//! 每个 tick 轮询所有数据源，把它们各进程的增量记进按天的存储，并维护界面要显示的实时速度和连接数。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::clash::ClashSource;
use crate::merge::Reconciler;
use crate::platform;
use crate::store;
use crate::types::*;

const POLL: Duration = Duration::from_secs(1);
const SAVE_EVERY: Duration = Duration::from_secs(30);
const TOP_HOSTS: usize = 8;

#[derive(Default)]
struct Live {
    live_conns: HashMap<String, u64>,
    speed: (f64, f64),
    proc_speed: HashMap<String, (f64, f64)>,
}

pub struct Collector {
    clash: ClashSource,
    system: Arc<dyn SystemSource>,
    live: Mutex<Live>,
    stop: AtomicBool,
    listeners: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
}

static COLLECTOR: OnceLock<Arc<Collector>> = OnceLock::new();

pub fn get() -> &'static Arc<Collector> {
    COLLECTOR.get_or_init(|| {
        Arc::new(Collector {
            clash: ClashSource::new(),
            system: platform::create_system_source(),
            live: Mutex::new(Live::default()),
            stop: AtomicBool::new(false),
            listeners: Mutex::new(vec![]),
        })
    })
}

impl Collector {
    pub fn on_tick(&self, f: impl Fn() + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(f));
    }

    fn notify(&self) {
        for f in self.listeners.lock().unwrap().iter() {
            f();
        }
    }

    pub fn start(self: &Arc<Self>) {
        self.system.auto_start();
        let me = self.clone();
        std::thread::spawn(move || {
            let mut reconciler = Reconciler::new();
            let mut st = TickState::default();
            let mut next = Instant::now();
            let mut last_save = Instant::now();
            while !me.stop.load(Ordering::Relaxed) {
                me.tick(&mut reconciler, &mut st);
                if last_save.elapsed() >= SAVE_EVERY {
                    store::save(false);
                    last_save = Instant::now();
                }
                // 按固定节奏走；某次轮询拖久了就跳过错过的，不连着补
                next += POLL;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        });
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        store::save(true);
    }

    /// 运行数据源的按钮动作（目前只有系统级统计的启用 / 停用）
    pub fn run_source_action(&self, id: &str) {
        if id == self.system.id() {
            self.system.toggle();
        }
        self.notify();
    }

    fn tick(&self, reconciler: &mut Reconciler, st: &mut TickState) {
        let c = match self.clash.poll() {
            Ok(c) => Some(c),
            Err(_) => {
                self.clash.reset();
                None
            }
        };
        let polled = self.system.poll();
        if polled.is_none() {
            self.system.reset();
        }
        let now = Instant::now();
        // 在确定有没有 Clash API 之前，内核的字节和应用的分不开：先扣住，等结果出来一起对账
        let mut e = polled;
        if e.is_some() && self.clash.searching() {
            let cur = e.take().unwrap();
            st.held = Some(match st.held.take() {
                Some(h) => h.join(cur),
                None => cur,
            });
        } else if let Some(h) = st.held.take() {
            e = Some(match e {
                Some(cur) => h.join(cur),
                None => h,
            });
        }

        {
            let mut live = self.live.lock().unwrap();
            live.live_conns = c.as_ref().map(|c| c.live_conns.clone()).unwrap_or_default();
        }

        if c.is_none() && e.is_none() {
            reconciler.reset();
            st.last_tick = None;
            let mut live = self.live.lock().unwrap();
            live.speed = (0.0, 0.0);
            live.proc_speed.clear();
            drop(live);
            self.notify();
            return;
        }

        let mut tick_speed: HashMap<String, (f64, f64)> = HashMap::new();
        // 总速度不含 VPN 承载流量，那是已经算在应用名下的字节的重复
        let (mut tick_up, mut tick_down) = (0f64, 0f64);
        for d in reconciler.merge(c.as_ref(), e.as_ref(), &platform::system_proxy_ports()) {
            store::add_delta(&d);
            let s = tick_speed.entry(d.proc.clone()).or_default();
            s.0 += d.up as f64;
            s.1 += d.down as f64;
            if d.route != TUNNEL {
                tick_up += d.up as f64;
                tick_down += d.down as f64;
            }
        }

        // 刚（重新）连上后的第一个 tick 只是设基线，还没有间隔可除
        if let Some(prev) = st.last_tick {
            let secs = now.duration_since(prev).as_secs_f64().max(0.001);
            let mut live = self.live.lock().unwrap();
            live.speed = (tick_up / secs, tick_down / secs);
            live.proc_speed = tick_speed.into_iter().map(|(k, s)| (k, (s.0 / secs, s.1 / secs))).collect();
        }
        st.last_tick = Some(now);
        self.notify();
    }

    pub fn snapshot(&self, range: Range) -> Snapshot {
        let agg = store::aggregate(range);
        let live = self.live.lock().unwrap();

        let mut procs: Vec<ProcRow> = agg
            .into_iter()
            .map(|(name, s)| {
                let get = |r: &str| s.routes.get(r).copied().unwrap_or(0) as f64;
                let mut routes: Vec<(String, f64)> = s
                    .routes
                    .iter()
                    .filter(|(r, _)| r.as_str() != DIRECT && r.as_str() != TUNNEL)
                    .map(|(r, b)| (r.clone(), *b as f64))
                    .collect();
                sort_desc(&mut routes);
                let mut top_hosts: Vec<(String, f64)> = s.hosts.iter().map(|(h, b)| (h.clone(), *b as f64)).collect();
                sort_desc(&mut top_hosts);
                top_hosts.truncate(TOP_HOSTS);
                let sp = live.proc_speed.get(&name).copied().unwrap_or((0.0, 0.0));
                ProcRow {
                    live_conns: live.live_conns.get(&name).copied().unwrap_or(0),
                    speed_up: sp.0,
                    speed_down: sp.1,
                    direct: get(DIRECT),
                    tunnel: get(TUNNEL),
                    up: s.up as f64,
                    down: s.down as f64,
                    path: s.path,
                    conns: s.conns,
                    routes,
                    top_hosts,
                    name,
                }
            })
            .filter(|p| p.up + p.down > 0.0 || p.live_conns > 0)
            .collect();
        procs.sort_by(|a, b| (b.up + b.down).total_cmp(&(a.up + a.down)).then_with(|| a.name.cmp(&b.name)));

        let mut route_totals: HashMap<String, f64> = HashMap::new();
        let (mut total_up, mut total_down) = (0.0, 0.0);
        for p in &procs {
            for (r, b) in &p.routes {
                *route_totals.entry(r.clone()).or_insert(0.0) += b;
            }
            // 路线不分上下行，所以隧道那份从两边按比例扣
            let total = p.up + p.down;
            let keep = if total > 0.0 { 1.0 - p.tunnel / total } else { 1.0 };
            total_up += p.up * keep;
            total_down += p.down * keep;
        }
        let mut routes: Vec<(String, f64)> = route_totals.into_iter().collect();
        sort_desc(&mut routes);

        Snapshot {
            range: range.as_str(),
            sources: vec![self.clash.status(), self.system.status()],
            total_up,
            total_down,
            total_tunnel: procs.iter().map(|p| p.tunnel).sum(),
            total_direct: procs.iter().map(|p| p.direct).sum(),
            routes,
            speed_up: live.speed.0,
            speed_down: live.speed.1,
            procs,
            since: store::earliest_day(),
        }
    }
}

fn sort_desc(v: &mut [(String, f64)]) {
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
}

#[derive(Default)]
struct TickState {
    last_tick: Option<Instant>,
    /// 代理 API 还在发现时扣住的系统采样
    held: Option<SystemSample>,
}
