//! 按天、按进程的流量桶，落盘到 data_dir/traffic.json。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::types::*;
use crate::util::{data_dir, day_key, day_key_back};

const KEEP_DAYS: i64 = 90;
const HOSTS_KEEP: usize = 100;
const HOSTS_TRIM_AT: usize = 200;

struct Store {
    data: StoreFile,
    dirty: bool,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

pub fn store_path() -> PathBuf {
    data_dir().join("traffic.json")
}

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock().unwrap();
    f(g.get_or_insert_with(|| Store { data: StoreFile::default(), dirty: false }))
}

pub fn load() {
    let file = store_path();
    let Ok(text) = std::fs::read_to_string(&file) else { return };
    match serde_json::from_str::<StoreFile>(&text) {
        Ok(parsed) if parsed.version == 2 => with(|s| s.data = parsed),
        _ => {
            // 读不懂就留一份备份再从头开始，不直接覆盖
            let _ = std::fs::copy(&file, file.with_extension("json.bad"));
        }
    }
}

pub fn save(force: bool) {
    with(|s| {
        if !s.dirty && !force {
            return;
        }
        let cut = day_key_back(KEEP_DAYS);
        s.data.days.retain(|k, _| *k >= cut);
        let file = store_path();
        let _ = std::fs::create_dir_all(data_dir());
        // 先写临时文件再改名，写到一半崩溃也不会弄坏历史
        let tmp = file.with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string(&s.data) {
            if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &file).is_ok() {
                s.dirty = false;
            }
        }
    })
}

fn add_route(stat: &mut ProcStat, route: &str, bytes: i64) {
    if bytes == 0 {
        return;
    }
    let next = (stat.routes.get(route).copied().unwrap_or(0) as i64 + bytes).max(0) as u64;
    if next > 0 {
        stat.routes.insert(route.to_string(), next);
    } else {
        stat.routes.remove(route);
    }
}

fn top_n(map: HashMap<String, u64>, n: usize) -> HashMap<String, u64> {
    let mut v: Vec<_> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(n);
    v.into_iter().collect()
}

pub fn add_delta(d: &Delta) {
    with(|s| {
        let day = s.data.days.entry(day_key()).or_default();
        let stat = day.entry(d.proc.clone()).or_default();
        if stat.path.is_empty() && !d.path.is_empty() {
            stat.path = d.path.clone();
        }
        stat.up += d.up.max(0) as u64;
        stat.down += d.down.max(0) as u64;
        // direct 为负时，把 ETW 提前记成直连的字节挪到代理上（见 merge.rs）
        add_route(stat, DIRECT, d.direct);
        let route = if d.route.is_empty() { "proxy" } else { &d.route };
        add_route(stat, route, d.up + d.down - d.direct);
        stat.conns += d.new_conns.max(0) as u64;
        for (host, bytes) in &d.hosts {
            if host.is_empty() || *bytes <= 0 {
                continue;
            }
            *stat.hosts.entry(host.clone()).or_insert(0) += *bytes as u64;
        }
        if stat.hosts.len() > HOSTS_TRIM_AT {
            stat.hosts = top_n(std::mem::take(&mut stat.hosts), HOSTS_KEEP);
        }
        s.dirty = true;
    })
}

fn range_start_key(range: Range) -> Option<String> {
    match range {
        Range::All => None,
        Range::Today => Some(day_key()),
        Range::Days7 => Some(day_key_back(6)),
        Range::Days30 => Some(day_key_back(29)),
    }
}

/// 把范围内所有天的桶合成一张进程表
pub fn aggregate(range: Range) -> DayData {
    let start = range_start_key(range);
    let mut out = DayData::new();
    with(|s| {
        for (k, day) in &s.data.days {
            if start.as_ref().is_some_and(|st| k < st) {
                continue;
            }
            for (proc, st) in day {
                let acc = out.entry(proc.clone()).or_default();
                if acc.path.is_empty() && !st.path.is_empty() {
                    acc.path = st.path.clone();
                }
                acc.up += st.up;
                acc.down += st.down;
                acc.conns += st.conns;
                for (r, b) in &st.routes {
                    *acc.routes.entry(r.clone()).or_insert(0) += b;
                }
                for (h, b) in &st.hosts {
                    *acc.hosts.entry(h.clone()).or_insert(0) += b;
                }
            }
        }
    });
    out
}

pub fn earliest_day() -> Option<String> {
    with(|s| s.data.days.keys().next().cloned())
}
