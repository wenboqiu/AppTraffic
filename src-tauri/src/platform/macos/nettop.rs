//! macOS 系统级数据源：`nettop` 的增量日志模式，不需要 root。
//! 每秒打印每个进程的字节数（含已经关掉的连接），后面跟着它打开的连接和所在网卡：
//!
//!   ,interface,state,bytes_in,bytes_out,
//!   curl.19127,,,4463,283,
//!   tcp4 127.0.0.1:57597<->127.0.0.1:7897,lo0,Established,39,443,
//!
//! 连接行把进程的字节拆成回环（连向本地代理 -> SysLoop）和按网卡的外部流量。
//! 两次采样之间关掉的连接没有行，按进程最后见到的连接构成分摊。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::ifaces::{self, InterfaceTable, SharedTable};
use super::processes::ps;
use crate::settings;
use crate::types::{SourceStatus, SysLoop, SysProc, SystemSample, SystemSource};

const NETTOP: &str = "/usr/bin/nettop";
const ARGS: [&str; 9] = ["-L", "0", "-s", "1", "-d", "-x", "-n", "-J", "interface,state,bytes_in,bytes_out"];
/// 这么久没有新样本，nettop 就是卡住或没了
const SILENCE: Duration = Duration::from_secs(6);
/// 进程这么多个样本都没有连接，就忘掉它的连接构成
const MIX_TTL: u32 = 60;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Addr {
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct NtFlow {
    pub local: Addr,
    pub remote: Addr,
    pub iface: String,
    pub state: String,
    pub down: i64,
    pub up: i64,
}

#[derive(Clone, Debug)]
pub struct NtProc {
    /// nettop 打印的名字：截到 15 个字符左右
    pub name: String,
    pub pid: i64,
    pub down: i64,
    pub up: i64,
    pub flows: Vec<NtFlow>,
}

/// "192.168.1.2:443"（v4）/ "fe80::1%en4.51016"、"::1.27017"（v6）/ "*:*"
fn parse_addr(s: &str, v6: bool) -> Addr {
    let Some(i) = s.rfind(if v6 { '.' } else { ':' }) else { return Addr::default() };
    let host = s[..i].split('%').next().unwrap_or("");
    let port = &s[i + 1..];
    Addr { host: (host != "*").then(|| host.to_string()), port: if port == "*" { None } else { port.parse().ok() } }
}

fn parse_flow(head: &str) -> Option<(bool, &str, &str)> {
    let rest = head.strip_prefix("tcp").or_else(|| head.strip_prefix("udp"))?;
    let v6 = match rest.chars().next()? {
        '4' => false,
        '6' => true,
        _ => return None,
    };
    let (l, r) = rest[1..].strip_prefix(' ')?.split_once("<->")?;
    Some((v6, l, r))
}

/// 一个样本（两个表头之间的行）
pub fn parse_sample(lines: &[String]) -> Vec<NtProc> {
    let mut out: Vec<NtProc> = vec![];
    for line in lines {
        // 从右边数字段：名字或连接本身可能带逗号
        let cols: Vec<&str> = line.split(',').collect();
        let n = cols.len();
        if n < 6 {
            continue;
        }
        let (iface, state, down, up) = (cols[n - 5], cols[n - 4], cols[n - 3], cols[n - 2]);
        let head = cols[..n - 5].join(",");
        let down = down.trim().parse().unwrap_or(0);
        let up = up.trim().parse().unwrap_or(0);
        if let Some((v6, l, r)) = parse_flow(&head) {
            if let Some(cur) = out.last_mut() {
                cur.flows.push(NtFlow { local: parse_addr(l, v6), remote: parse_addr(r, v6), iface: iface.into(), state: state.into(), down, up });
            }
            continue;
        }
        let Some((name, pid)) = head.rsplit_once('.') else { continue };
        let Ok(pid) = pid.parse() else { continue };
        out.push(NtProc { name: name.into(), pid, down, up, flows: vec![] });
    }
    out
}

fn is_loopback(f: &NtFlow) -> bool {
    let h = f.remote.host.as_deref().or(f.local.host.as_deref()).unwrap_or("");
    f.iface == "lo0" || h.starts_with("127.") || h == "::1" || h.starts_with("::ffff:127.")
}

#[derive(Clone, Default)]
struct Bytes {
    up: i64,
    down: i64,
}

#[derive(Clone)]
struct LoopAcc {
    server: i64,
    port: u16,
    up: i64,
    down: i64,
}

/// 进程最近一个有连接的样本里字节的去向，当权重用
#[derive(Clone, Default)]
struct Mix {
    /// key "<监听者 pid>:<端口>"
    loops: BTreeMap<String, LoopAcc>,
    /// 不是连向已知监听者的回环字节（本地代理的服务端那一侧……）
    local: Bytes,
    ifs: BTreeMap<String, Bytes>,
    hosts: BTreeMap<String, i64>,
    age: u32,
}

fn total(m: &Mix) -> Bytes {
    let mut t = m.local.clone();
    for v in m.loops.values() {
        t.up += v.up;
        t.down += v.down;
    }
    for v in m.ifs.values() {
        t.up += v.up;
        t.down += v.down;
    }
    t
}

pub struct AProc {
    pub pid: i64,
    pub up: i64,
    pub down: i64,
    pub hosts: Vec<(String, i64)>,
    pub ifs: Vec<(String, i64, i64)>,
}

pub struct ALoop {
    pub client: i64,
    pub server: i64,
    pub port: u16,
    pub up: i64,
    pub down: i64,
}

#[derive(Default)]
pub struct Attributed {
    pub procs: Vec<AProc>,
    pub loops: Vec<ALoop>,
}

/// 把连续的样本变成每个进程（按网卡）的外部字节，以及连向本地监听进程的回环流。
/// 这里进程还是 pid，数据源再把它们解析成路径。
#[derive(Default)]
pub struct Attributor {
    mixes: HashMap<i64, Mix>,
    /// 回环 / 通配监听的 TCP 端口 -> pid
    listeners: HashMap<u16, i64>,
}

impl Attributor {
    pub fn add(&mut self, sample: &[NtProc], dflt: &str) -> Attributed {
        for p in sample {
            for f in &p.flows {
                if f.state == "Listen" {
                    if let Some(port) = f.local.port {
                        self.listeners.insert(port, p.pid);
                    }
                }
            }
        }
        let mut procs = vec![];
        let mut loops: BTreeMap<String, ALoop> = BTreeMap::new();
        let mut seen = HashSet::new();

        for p in sample {
            seen.insert(p.pid);
            let mut now = Mix::default();
            for f in &p.flows {
                if f.up == 0 && f.down == 0 {
                    continue;
                }
                if is_loopback(f) {
                    let server = f.remote.port.and_then(|port| self.listeners.get(&port).copied());
                    let own = f.local.port.and_then(|port| self.listeners.get(&port)) == Some(&p.pid);
                    // 我们这一侧是另一个本地进程的客户端（应用 -> 本地代理）
                    if let (Some(server), Some(port), false) = (server, f.remote.port, own) {
                        if server != p.pid {
                            let l = now.loops.entry(format!("{server}:{port}")).or_insert(LoopAcc { server, port, up: 0, down: 0 });
                            l.up += f.up;
                            l.down += f.down;
                            continue;
                        }
                    }
                    now.local.up += f.up;
                    now.local.down += f.down;
                    continue;
                }
                let iface = if f.iface.is_empty() { dflt } else { &f.iface };
                let i = now.ifs.entry(iface.to_string()).or_default();
                i.up += f.up;
                i.down += f.down;
                if let Some(h) = &f.remote.host {
                    *now.hosts.entry(h.clone()).or_insert(0) += f.up + f.down;
                }
            }

            let seen_bytes = total(&now);
            let rest_up = (p.up - seen_bytes.up).max(0);
            let rest_down = (p.down - seen_bytes.down).max(0);
            let basis = if seen_bytes.up + seen_bytes.down > 0 {
                let mut b = now.clone();
                b.age = 0;
                self.mixes.insert(p.pid, b.clone());
                Some(b)
            } else if let Some(prev) = self.mixes.get_mut(&p.pid) {
                prev.age += 1;
                Some(prev.clone())
            } else {
                None
            };
            if rest_up + rest_down > 0 {
                spread(&mut now, basis.as_ref(), rest_up, rest_down, dflt);
            }

            for l in now.loops.values() {
                let acc = loops
                    .entry(format!("{}:{}:{}", p.pid, l.server, l.port))
                    .or_insert(ALoop { client: p.pid, server: l.server, port: l.port, up: 0, down: 0 });
                acc.up += l.up;
                acc.down += l.down;
            }
            let (mut up, mut down) = (0, 0);
            let mut ifs = vec![];
            for (name, v) in &now.ifs {
                if v.up == 0 && v.down == 0 {
                    continue;
                }
                ifs.push((name.clone(), v.up, v.down));
                up += v.up;
                down += v.down;
            }
            if up + down == 0 && now.loops.is_empty() {
                continue;
            }
            procs.push(AProc { pid: p.pid, up, down, hosts: now.hosts.into_iter().collect(), ifs });
        }

        self.mixes.retain(|pid, m| seen.contains(pid) && m.age <= MIX_TTL);
        Attributed { procs, loops: loops.into_values().collect() }
    }
}

/// 把没见到的连接的字节按 `mix` 记上；没有参照就记成默认网卡的外部流量
fn spread(into: &mut Mix, mix: Option<&Mix>, up: i64, down: i64, dflt: &str) {
    let t = mix.map(total);
    let (Some(mix), Some(t)) = (mix, t) else {
        let i = into.ifs.entry(dflt.to_string()).or_default();
        i.up += up;
        i.down += down;
        return;
    };
    if t.up + t.down == 0 {
        let i = into.ifs.entry(dflt.to_string()).or_default();
        i.up += up;
        i.down += down;
        return;
    }
    let share = |part: &Bytes| Bytes {
        up: if t.up > 0 { (up as f64 * part.up as f64 / t.up as f64).round() as i64 } else { 0 },
        down: if t.down > 0 { (down as f64 * part.down as f64 / t.down as f64).round() as i64 } else { 0 },
    };
    // 参照里从没用过的方向也得有个去处：外部
    let orphan_up = if t.up > 0 { 0 } else { up };
    let orphan_down = if t.down > 0 { 0 } else { down };
    for (key, l) in &mix.loops {
        let s = share(&Bytes { up: l.up, down: l.down });
        let acc = into.loops.entry(key.clone()).or_insert(LoopAcc { server: l.server, port: l.port, up: 0, down: 0 });
        acc.up += s.up;
        acc.down += s.down;
    }
    let fallback: BTreeMap<String, Bytes> = BTreeMap::from([(dflt.to_string(), Bytes::default())]);
    let ifaces = if mix.ifs.is_empty() { &fallback } else { &mix.ifs };
    for (i, (name, v)) in ifaces.iter().enumerate() {
        let s = share(v);
        let acc = into.ifs.entry(name.clone()).or_default();
        acc.up += s.up + if i == 0 { orphan_up } else { 0 };
        acc.down += s.down + if i == 0 { orphan_down } else { 0 };
    }
    for (h, n) in &mix.hosts {
        *into.hosts.entry(h.clone()).or_insert(0) += ((up + down) as f64 * *n as f64 / (t.up + t.down) as f64).round() as i64;
    }
}

/// pid -> 可执行文件路径，来自 `ps`；nettop 出现不认识的 pid 时重读
#[derive(Default)]
struct PathTable {
    paths: HashMap<i64, String>,
}

fn base_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

impl PathTable {
    fn resolve(&mut self, procs: &[(i64, String)]) {
        let stale = procs.iter().any(|(pid, name)| self.paths.get(pid).is_none_or(|k| !base_name(k).starts_with(name.as_str())));
        if !stale {
            return;
        }
        let Some(out) = ps(&["-axww", "-o", "pid=,comm="]) else { return };
        self.paths = out
            .lines()
            .filter_map(|l| {
                let (pid, comm) = l.trim_start().split_once(char::is_whitespace)?;
                Some((pid.parse().ok()?, comm.trim_start().to_string()))
            })
            .collect();
    }

    /// 完整路径；进程已经没了就用 nettop 的（被截断的）名字
    fn get(&self, pid: i64, name: &str) -> String {
        match self.paths.get(&pid) {
            Some(k) if base_name(k).starts_with(name) => k.clone(),
            _ => name.to_string(),
        }
    }
}

struct Inner {
    enabled: bool,
    child: Option<Child>,
    /// 区分新旧读取线程
    generation: u64,
    last_sample: Option<Instant>,
    procs: HashMap<String, SysProc>,
    loops: HashMap<String, SysLoop>,
    last_error: Option<String>,
}

pub struct NettopSource {
    inner: Arc<Mutex<Inner>>,
    table: SharedTable,
}

impl NettopSource {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                enabled: settings::get().nettop_enabled,
                child: None,
                generation: 0,
                last_sample: None,
                procs: HashMap::new(),
                loops: HashMap::new(),
                last_error: None,
            })),
            table: Arc::new(Mutex::new(InterfaceTable::new())),
        }
    }
}

fn ready(s: &Inner) -> bool {
    s.child.is_some() && s.last_sample.is_some_and(|t| t.elapsed() < SILENCE)
}

fn stop(s: &mut Inner) {
    if let Some(mut c) = s.child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    s.generation += 1;
    s.last_sample = None;
}

fn start(inner: &Arc<Mutex<Inner>>, table: &SharedTable, s: &mut Inner) {
    if s.child.is_some() {
        return;
    }
    s.last_error = None;
    s.last_sample = None;
    ifaces::refresh(table);
    let spawned = Command::new(NETTOP).args(ARGS).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            s.last_error = Some(format!("无法运行 nettop：{e}"));
            return;
        }
    };
    let Some(stdout) = child.stdout.take() else { return };
    s.generation += 1;
    s.child = Some(child);
    let (gen, inner, table) = (s.generation, inner.clone(), table.clone());
    std::thread::spawn(move || read_loop(stdout, gen, &inner, &table));
}

fn read_loop(stdout: std::process::ChildStdout, gen: u64, inner: &Arc<Mutex<Inner>>, table: &SharedTable) {
    let mut reader = BufReader::new(stdout);
    let mut buf = vec![];
    let mut lines: Vec<String> = vec![];
    let mut headers = 0;
    let mut attributor = Attributor::default();
    let mut paths = PathTable::default();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']).to_string();
        if !line.starts_with(',') {
            lines.push(line);
            continue;
        }
        // 表头开始下一个样本，所以前一个完整了
        let done = std::mem::take(&mut lines);
        headers += 1;
        // 第一个表头前面没有东西；第一个样本（第二个表头收尾）是进程启动以来的总数，不是增量
        if headers <= 2 {
            continue;
        }
        let sample = parse_sample(&done);
        {
            let mut s = inner.lock().unwrap();
            if s.generation != gen {
                return;
            }
            s.last_sample = Some(Instant::now());
        }
        ifaces::refresh_if_stale(table);
        ingest(&sample, &mut attributor, &mut paths, inner, table, gen);
    }
    let mut s = inner.lock().unwrap();
    if s.generation == gen {
        if let Some(mut c) = s.child.take() {
            let _ = c.wait();
        }
        s.last_sample = None;
    }
}

fn ingest(sample: &[NtProc], attributor: &mut Attributor, paths: &mut PathTable, inner: &Arc<Mutex<Inner>>, table: &SharedTable, gen: u64) {
    let dflt = table.lock().unwrap().default_iface();
    let a = attributor.add(sample, &dflt);
    let names: HashMap<i64, &str> = sample.iter().map(|p| (p.pid, p.name.as_str())).collect();
    let mut pids: Vec<i64> = a.procs.iter().map(|p| p.pid).chain(a.loops.iter().flat_map(|l| [l.client, l.server])).collect();
    pids.sort_unstable();
    pids.dedup();
    let wanted: Vec<(i64, String)> = pids.iter().map(|pid| (*pid, names.get(pid).copied().unwrap_or("").to_string())).collect();
    paths.resolve(&wanted);
    let path_of = |pid: i64| paths.get(pid, names.get(&pid).copied().unwrap_or(""));

    let mut s = inner.lock().unwrap();
    if s.generation != gen {
        return;
    }
    let mut t = table.lock().unwrap();
    for p in &a.procs {
        let ifs: Vec<(i64, i64, i64)> = p.ifs.iter().map(|(name, up, down)| (t.index(name), *up, *down)).collect();
        add_proc(&mut s.procs, SysProc { path: path_of(p.pid), up: p.up, down: p.down, hosts: p.hosts.clone(), ifs });
    }
    for l in &a.loops {
        add_loop(&mut s.loops, SysLoop { client: path_of(l.client), server: path_of(l.server), port: l.port, up: l.up, down: l.down });
    }
}

fn add_proc(procs: &mut HashMap<String, SysProc>, p: SysProc) {
    if p.up + p.down == 0 {
        return;
    }
    match procs.get_mut(&p.path) {
        None => {
            procs.insert(p.path.clone(), p);
        }
        Some(acc) => {
            acc.up += p.up;
            acc.down += p.down;
            acc.hosts.extend(p.hosts);
            for (index, up, down) in p.ifs {
                match acc.ifs.iter_mut().find(|x| x.0 == index) {
                    Some(f) => {
                        f.1 += up;
                        f.2 += down;
                    }
                    None => acc.ifs.push((index, up, down)),
                }
            }
        }
    }
}

fn add_loop(loops: &mut HashMap<String, SysLoop>, l: SysLoop) {
    if l.up + l.down == 0 {
        return;
    }
    let key = format!("{}\0{}\0{}", l.client, l.server, l.port);
    match loops.get_mut(&key) {
        None => {
            loops.insert(key, l);
        }
        Some(acc) => {
            acc.up += l.up;
            acc.down += l.down;
        }
    }
}

impl SystemSource for NettopSource {
    fn poll(&self) -> Option<SystemSample> {
        let mut s = self.inner.lock().unwrap();
        if !s.enabled {
            return None;
        }
        if s.child.is_some() && s.last_sample.is_some_and(|t| t.elapsed() > SILENCE) {
            stop(&mut s);
        }
        if s.child.is_none() {
            start(&self.inner, &self.table, &mut s);
        }
        if !ready(&s) {
            return None;
        }
        let sample = SystemSample {
            procs: s.procs.values().cloned().collect(),
            loops: s.loops.values().cloned().collect(),
            ifaces: self.table.lock().unwrap().list(),
        };
        s.procs.clear();
        s.loops.clear();
        Some(sample)
    }

    fn reset(&self) {
        let mut s = self.inner.lock().unwrap();
        s.procs.clear();
        s.loops.clear();
    }

    fn auto_start(&self) {
        let mut s = self.inner.lock().unwrap();
        if s.enabled {
            start(&self.inner, &self.table, &mut s);
        }
    }

    fn toggle(&self) {
        let mut s = self.inner.lock().unwrap();
        s.enabled = !s.enabled;
        let on = s.enabled;
        settings::update(|x| x.nettop_enabled = on);
        if on {
            start(&self.inner, &self.table, &mut s);
        } else {
            stop(&mut s);
            s.procs.clear();
            s.loops.clear();
        }
    }

    fn status(&self) -> SourceStatus {
        let s = self.inner.lock().unwrap();
        let mk = |state, detail: &str, action: Option<&str>| SourceStatus {
            id: "system".into(),
            label: "系统流量".into(),
            state,
            detail: detail.into(),
            action: action.map(String::from),
        };
        if !s.enabled {
            return mk("off", "已停用", Some("启用"));
        }
        if let Some(e) = &s.last_error {
            return mk("error", e, None);
        }
        if ready(&s) {
            return mk("on", "已启用（nettop）", Some("停用"));
        }
        mk("off", "正在启动…", None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn parses_procs_and_flows() {
        let p = parse_sample(&lines(
            "curl.19127,,,4463,283,\ntcp4 127.0.0.1:57597<->127.0.0.1:7897,lo0,Established,39,443,\ntcp6 fe80::1%en0.5000<->2001:db8::1.443,en0,Established,10,20,\nudp4 *:5353<->*:*,en0,,1,2,\n",
        ));
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].name.as_str(), p[0].pid, p[0].down, p[0].up), ("curl", 19127, 4463, 283));
        assert_eq!(p[0].flows.len(), 3);
        assert_eq!(p[0].flows[0].remote, Addr { host: Some("127.0.0.1".into()), port: Some(7897) });
        assert_eq!(p[0].flows[1].remote, Addr { host: Some("2001:db8::1".into()), port: Some(443) });
        assert_eq!(p[0].flows[2].local, Addr { host: None, port: Some(5353) });
    }

    #[test]
    fn loopback_to_listener_and_external() {
        let mut a = Attributor::default();
        let sample = parse_sample(&lines(
            "mihomo.100,,,0,0,\ntcp4 127.0.0.1:7897<->*:*,lo0,Listen,0,0,\ncurl.200,,,1000,100,\ntcp4 127.0.0.1:50000<->127.0.0.1:7897,lo0,Established,1000,100,\n",
        ));
        let r = a.add(&sample, "en0");
        assert_eq!(r.loops.len(), 1);
        assert_eq!((r.loops[0].client, r.loops[0].server, r.loops[0].port, r.loops[0].down, r.loops[0].up), (200, 100, 7897, 1000, 100));
    }

    #[test]
    fn closed_connection_bytes_follow_last_mix() {
        let mut a = Attributor::default();
        let s1 = parse_sample(&lines("curl.200,,,100,10,\ntcp4 1.1.1.1:1<->2.2.2.2:443,en0,Established,100,10,\n"));
        a.add(&s1, "en0");
        // 连接关了：进程有字节但没有连接行
        let s2 = parse_sample(&lines("curl.200,,,500,50,\n"));
        let r = a.add(&s2, "en0");
        assert_eq!(r.procs.len(), 1);
        assert_eq!((r.procs[0].down, r.procs[0].up), (500, 50));
        assert_eq!(r.procs[0].ifs[0].0, "en0");
    }
}
