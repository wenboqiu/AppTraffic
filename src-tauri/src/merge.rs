//! 把 Clash 连接列表（L2）和系统级 ETW 计数（L0）对账，使每个字节只记一次，记在引起它的应用名下。
//!
//! ETW 不把回环算作流量，但会把它报成「客户端 -> 本机服务端」的流。这给出每个应用的两种精确体量：
//! - external：应用自己连向外部的 socket（直连流量，以及被 TUN 接走的流量，因为 TUN 网卡对应用来说只是又一块网卡）
//! - via core：应用和代理内核本地端口之间交换的字节（系统代理、SOCKS、显式 -x …），
//!   包括短到 Clash 一秒轮询看不到的连接
//!
//! Clash 可达时，内核自己的 socket 丢掉（它们是上面两者的上游一侧），Clash 逐连接的数字只用来区分
//! 代理 / 直连和命名域名；它的 "gap" 桶此时是多余的。ETW 看不到的 Clash 字节（比如它看不到 TUN 网卡）
//! 过一段宽限期后记入。
//!
//! L1：本地代理没有 Clash API 时，代理进程的上游字节按各回环客户端交换的量比例分给它们。
//!
//! VPN：ETW 还知道每个字节走了哪块网卡。VPN 网卡上的字节记给那个 VPN；VPN 客户端自己的流量
//! （同样字节的加密承载）记到 TUNNEL 路线，总量不算它。

use std::collections::{HashMap, HashSet};

use crate::cores::{is_core, is_vpn_client, proc_name, stem};
use crate::types::*;

/// Clash 内核给 TUN 网卡起的名字
fn looks_like_clash_tun(s: &str) -> bool {
    let l = s.to_lowercase();
    if l.contains("mihomo") || l.contains("clash") || l.contains("singbox") || l.contains("sing-box") {
        return true;
    }
    // \bmeta\b
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    l.match_indices("meta").any(|(i, m)| {
        let before = l[..i].chars().next_back();
        let after = l[i + m.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

/// VPN 网卡：序号 -> 别名。Clash API 可达时，内核自己的 TUN 网卡不算（Clash 的连接列表说明了走了它的是什么）
fn vpn_adapters(etw: &SystemSample, clash: Option<&ClashSample>) -> HashMap<i64, String> {
    let mut out = HashMap::new();
    let tun = clash.and_then(|c| c.tun_device.as_ref()).map(|t| t.to_lowercase());
    for f in &etw.ifaces {
        if !f.vpn {
            continue;
        }
        if clash.is_some()
            && (tun.as_deref() == Some(f.alias.to_lowercase().as_str()) || looks_like_clash_tun(&format!("{} {}", f.alias, f.desc)))
        {
            continue;
        }
        out.insert(f.index, f.alias.clone());
    }
    out
}

/// 账本上这么多个 tick 没有 ETW 字节，才把 Clash 没对上的字节记进去
const GRACE_TICKS: u32 = 5;
/// 进程这么久没有活动就忘掉它的账本
const IDLE_FORGET_TICKS: u32 = 120;
/// 身份不明的代理字节和无人认领的内核客户端互相等待的 tick 数
const UNKNOWN_HOLD_TICKS: u32 = 3;

#[derive(Clone, Copy, Default)]
struct Volume {
    up: i64,
    down: i64,
}

struct Settled {
    up: i64,
    down: i64,
    proxied: i64,
    /// 只有在 ETW 缺席时记入 Clash 字节才有
    fallback_hosts: Option<Vec<(String, i64)>>,
}

/// 一个进程在一条路径（external 或 via core）上的累计：ETW 一到就给出体量，Clash 给出代理占比，可能晚到
struct Ledger {
    proxied_by_ratio: bool,
    etw_up: i64,
    etw_down: i64,
    clash_up: i64,
    clash_down: i64,
    clash_proxied: i64,
    booked_up: i64,
    booked_down: i64,
    booked_proxied: i64,
    since_etw: u32,
    clash_hosts: HashMap<String, i64>,
}

impl Ledger {
    /// proxied_by_ratio：这条路径上的一切都走了 Clash，所以 Clash 漏掉的字节按它看到的比例分；
    /// 否则（external 路径还有非 Clash 流量）只有 Clash 报的字节才算代理
    fn new(proxied_by_ratio: bool) -> Self {
        Self {
            proxied_by_ratio,
            etw_up: 0,
            etw_down: 0,
            clash_up: 0,
            clash_down: 0,
            clash_proxied: 0,
            booked_up: 0,
            booked_down: 0,
            booked_proxied: 0,
            since_etw: 0,
            clash_hosts: HashMap::new(),
        }
    }

    fn step(&mut self, etw: Option<Volume>, clash: &[ClashDelta]) -> Settled {
        match etw {
            Some(v) if v.up + v.down > 0 => {
                self.etw_up += v.up;
                self.etw_down += v.down;
                self.since_etw = 0;
                self.clash_hosts.clear();
            }
            _ => self.since_etw += 1,
        }
        for c in clash {
            self.clash_up += c.d.up;
            self.clash_down += c.d.down;
            self.clash_proxied += c.d.up + c.d.down - c.d.direct;
            for (h, n) in &c.d.hosts {
                *self.clash_hosts.entry(h.clone()).or_insert(0) += n;
            }
        }

        let mut target_up = self.etw_up;
        let mut target_down = self.etw_down;
        let fallback = self.since_etw >= GRACE_TICKS;
        if fallback {
            target_up = target_up.max(self.clash_up);
            target_down = target_down.max(self.clash_down);
        }
        let up = (target_up - self.booked_up).max(0);
        let down = (target_down - self.booked_down).max(0);
        self.booked_up += up;
        self.booked_down += down;
        let mut fallback_hosts = None;
        if fallback && up + down > 0 {
            fallback_hosts = Some(self.clash_hosts.drain().collect());
        }

        // 代理字节可能在 ETW 已经记成直连之后才被报告；下一个 delta 就带负的 direct 把它们挪过去
        let booked = self.booked_up + self.booked_down;
        let clash_total = self.clash_up + self.clash_down;
        let target = if self.proxied_by_ratio {
            if clash_total > 0 {
                ((booked as f64 * self.clash_proxied as f64) / clash_total as f64).round() as i64
            } else {
                0
            }
        } else {
            self.clash_proxied.min(booked)
        };
        let proxied = target - self.booked_proxied;
        self.booked_proxied = target;
        Settled { up, down, proxied, fallback_hosts }
    }
}

struct ProcState {
    path: String,
    external: Ledger,
    via_core: Ledger,
    idle: u32,
}

struct ViaCore {
    up: i64,
    down: i64,
    path: String,
}

struct CoreClient {
    path: String,
    bytes: i64,
    age: u32,
}

fn rnd(x: f64) -> i64 {
    x.round() as i64
}

#[derive(Default)]
pub struct Reconciler {
    procs: HashMap<String, ProcState>,
    /// 最近见过的 Clash API 的客户端名，API 不可达时用来给它的内核贴标签
    last_route: String,
    /// 等本地应用来认领的、身份不明的代理连接
    pending_unknown: Vec<(ClashDelta, u32)>,
    /// 最近和内核有过交换、但没有已识别 Clash 连接的应用
    core_clients: HashMap<String, CoreClient>,
}

impl Reconciler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.procs.clear();
        self.pending_unknown.clear();
        self.core_clients.clear();
    }

    pub fn merge(&mut self, clash: Option<&ClashSample>, etw: Option<&SystemSample>, proxy_ports: &HashSet<u16>) -> Vec<Delta> {
        let Some(etw) = etw else {
            self.procs.clear();
            return clash.map(|c| c.deltas.iter().map(|d| d.d.clone()).collect()).unwrap_or_default();
        };

        let mut out: Vec<Delta> = vec![];
        let vpn_ifaces = vpn_adapters(etw, clash);
        let mut vpn_active = false;

        // 每个进程名的 ETW external 字节。VPN 网卡上的字节立刻记给那个 VPN，剩下的（物理网卡、Clash 自己的 TUN）继续往下走
        let mut external: HashMap<String, Delta> = HashMap::new();
        for p in &etw.procs {
            let name = proc_name(&p.path);
            let (mut up, mut down) = (p.up, p.down);
            let mut via_vpn: Option<usize> = None;
            for &(index, if_up, if_down) in &p.ifs {
                let Some(alias) = vpn_ifaces.get(&index) else { continue };
                if if_up + if_down == 0 {
                    continue;
                }
                vpn_active = true;
                up -= if_up;
                down -= if_down;
                out.push(Delta {
                    proc: name.clone(),
                    path: p.path.clone(),
                    up: if_up,
                    down: if_down,
                    route: format!("{VPN_ROUTE_PREFIX}{alias}"),
                    ..Default::default()
                });
                via_vpn = Some(out.len() - 1);
            }
            if up + down <= 0 {
                if let Some(i) = via_vpn {
                    out[i].hosts = p.hosts.clone();
                }
                continue;
            }
            let d = external
                .entry(name.clone())
                .or_insert_with(|| Delta { proc: name.clone(), path: p.path.clone(), ..Default::default() });
            d.up += up.max(0);
            d.down += down.max(0);
            d.hosts.extend(p.hosts.iter().cloned());
        }
        // VPN 客户端自己的流量把上面的字节又带了一遍，加密的
        let vpn_clients: Vec<String> = external.keys().filter(|n| is_vpn_client(n)).cloned().collect();
        for name in vpn_clients {
            if let Some(d) = external.remove(&name) {
                out.push(Delta { direct: 0, route: TUNNEL.into(), ..d });
            }
        }

        let Some(clash) = clash else {
            self.procs.clear();
            out.extend(self.attribute_local_proxies(&mut external, &etw.loops, proxy_ports, false));
            for d in external.into_values() {
                if is_core(&d.proc) {
                    // 内核上剩下的仍是内核自己的，不是应用的：TUN 网卡流量的承载，或它的 DNS 和更新
                    let route = if vpn_active { TUNNEL } else { "" };
                    let direct = if route.is_empty() { d.up + d.down } else { 0 };
                    out.push(Delta { proc: CORE_INNER.into(), path: String::new(), direct, route: route.into(), ..d });
                } else {
                    let direct = d.up + d.down;
                    out.push(Delta { direct, ..d });
                }
            }
            return out;
        };

        self.last_route = clash.route.clone();
        external.retain(|name, _| !is_core(name));
        out.extend(self.attribute_local_proxies(&mut external, &etw.loops, proxy_ports, true));

        // 各应用和内核本地端口交换了多少
        let mut via_core: HashMap<String, ViaCore> = HashMap::new();
        for l in &etw.loops {
            if !is_core(&proc_name(&l.server)) {
                continue;
            }
            let name = proc_name(&l.client);
            if is_core(&name) {
                continue;
            }
            let v = via_core.entry(name).or_insert_with(|| ViaCore { up: 0, down: 0, path: l.client.clone() });
            v.up += l.up;
            v.down += l.down;
        }

        let mut tun_by_proc: HashMap<String, Vec<ClashDelta>> = HashMap::new();
        let mut proxy_by_proc: HashMap<String, Vec<ClashDelta>> = HashMap::new();
        let mut unknown_proxy: Vec<ClashDelta> = vec![];
        for d in &clash.deltas {
            if d.via == ClashVia::Inner {
                out.push(d.d.clone());
            }
            // gap 是 Clash 轮询漏掉的短连接，ETW 已经量过了
            if d.via == ClashVia::Inner || d.via == ClashVia::Gap {
                continue;
            }
            if !d.known {
                // 身份不明的 Tun 流量在 ETW 里已经记在真实进程名下了
                if d.via == ClashVia::Proxy {
                    unknown_proxy.push(d.clone());
                }
                continue;
            }
            let map = if d.via == ClashVia::Tun { &mut tun_by_proc } else { &mut proxy_by_proc };
            map.entry(d.d.proc.clone()).or_default().push(d.clone());
        }
        out.extend(self.claim_unknown_proxy(unknown_proxy, &via_core, &mut proxy_by_proc));

        let mut names: HashSet<String> = HashSet::new();
        names.extend(external.keys().cloned());
        names.extend(via_core.keys().cloned());
        names.extend(tun_by_proc.keys().cloned());
        names.extend(proxy_by_proc.keys().cloned());
        names.extend(self.procs.keys().cloned());
        for name in names {
            let ext = external.get(&name);
            let core = via_core.get(&name);
            let tun = tun_by_proc.get(&name).map(Vec::as_slice).unwrap_or(&[]);
            let prx = proxy_by_proc.get(&name).map(Vec::as_slice).unwrap_or(&[]);
            let st = self.procs.entry(name.clone()).or_insert_with(|| {
                let path = ext
                    .map(|e| e.path.clone())
                    .filter(|p| !p.is_empty())
                    .or_else(|| core.map(|c| c.path.clone()).filter(|p| !p.is_empty()))
                    .or_else(|| tun.first().map(|d| d.d.path.clone()).filter(|p| !p.is_empty()))
                    .or_else(|| prx.first().map(|d| d.d.path.clone()).filter(|p| !p.is_empty()))
                    .unwrap_or_default();
                ProcState { path, external: Ledger::new(false), via_core: Ledger::new(true), idle: 0 }
            });
            let active = ext.is_some() || core.is_some() || !tun.is_empty() || !prx.is_empty();
            st.idle = if active { 0 } else { st.idle + 1 };

            let a = st.external.step(ext.map(|e| Volume { up: e.up, down: e.down }), tun);
            let b = st.via_core.step(core.map(|c| Volume { up: c.up, down: c.down }), prx);
            let up = a.up + b.up;
            let down = a.down + b.down;
            let proxied = a.proxied + b.proxied;
            let new_conns: i64 = tun.iter().chain(prx).map(|d| d.d.new_conns).sum();
            // ETW 知道 external 的域名（DNS）；代理连接的域名只有 Clash 知道
            let mut hosts: Vec<(String, i64)> = ext.map(|e| e.hosts.clone()).unwrap_or_default();
            hosts.extend(prx.iter().flat_map(|d| d.d.hosts.iter().cloned()));
            hosts.extend(a.fallback_hosts.unwrap_or_default());

            let (idle, path) = (st.idle, st.path.clone());
            if idle >= IDLE_FORGET_TICKS {
                self.procs.remove(&name);
            }
            if up + down == 0 && proxied == 0 && new_conns == 0 {
                continue;
            }
            out.push(Delta {
                proc: name,
                path,
                up,
                down,
                direct: up + down - proxied,
                route: clash.route.clone(),
                new_conns,
                hosts,
            });
        }
        out
    }

    /// 没能给出进程名的代理连接（find-process-mode 关闭）：最近有本地应用和内核交换了字节、
    /// 又没有已识别的连接能解释它们时，这些连接就是那些应用的——按回环体量分摊并加进 proxy_by_proc。
    /// 两边可能差一两个 tick 才到，所以无人认领的字节等 UNKNOWN_HOLD_TICKS；过了以后认为客户端是
    /// 局域网设备（allow-lan），原样返回，按身份不明记账。
    fn claim_unknown_proxy(
        &mut self,
        unknown: Vec<ClashDelta>,
        via_core: &HashMap<String, ViaCore>,
        proxy_by_proc: &mut HashMap<String, Vec<ClashDelta>>,
    ) -> Vec<Delta> {
        for c in self.core_clients.values_mut() {
            c.age += 1;
        }
        for (name, v) in via_core {
            if proxy_by_proc.contains_key(name) || v.up + v.down == 0 {
                continue;
            }
            let c = self.core_clients.entry(name.clone()).or_insert_with(|| CoreClient { path: v.path.clone(), bytes: 0, age: 0 });
            c.bytes += v.up + v.down;
            c.age = 0;
        }
        self.core_clients.retain(|name, c| !(c.age >= UNKNOWN_HOLD_TICKS || proxy_by_proc.contains_key(name)));

        let waiting = std::mem::take(&mut self.pending_unknown);
        if waiting.is_empty() && unknown.is_empty() {
            return vec![];
        }
        if self.core_clients.is_empty() {
            let mut out = vec![];
            let mut keep = vec![];
            for (d, age) in waiting.into_iter().chain(unknown.into_iter().map(|d| (d, 0))) {
                let age = age + 1;
                if age >= UNKNOWN_HOLD_TICKS {
                    out.push(d.d);
                } else {
                    keep.push((d, age));
                }
            }
            self.pending_unknown = keep;
            return out;
        }

        let pending: Vec<ClashDelta> = waiting.into_iter().map(|(d, _)| d).chain(unknown).collect();
        let sum: i64 = self.core_clients.values().map(|c| c.bytes).sum();
        for (name, c) in &self.core_clients {
            let share = c.bytes as f64 / sum as f64;
            let scaled = pending.iter().map(|d| ClashDelta {
                d: Delta {
                    proc: name.clone(),
                    path: c.path.clone(),
                    up: rnd(d.d.up as f64 * share),
                    down: rnd(d.d.down as f64 * share),
                    direct: rnd(d.d.direct as f64 * share),
                    route: d.d.route.clone(),
                    new_conns: rnd(d.d.new_conns as f64 * share),
                    hosts: d.d.hosts.iter().map(|(h, n)| (h.clone(), rnd(*n as f64 * share))).collect(),
                },
                via: d.via,
                known: true,
            });
            proxy_by_proc.entry(name.clone()).or_default().extend(scaled);
        }
        vec![]
    }

    /// L1：把每个本地代理的上游字节，按这个 tick 里给它发过回环流量的各应用分摊。
    /// Clash 可达时 Clash 内核跳过，因为它的连接列表是精确的。
    fn attribute_local_proxies(
        &self,
        external: &mut HashMap<String, Delta>,
        loops: &[SysLoop],
        proxy_ports: &HashSet<u16>,
        clash_up: bool,
    ) -> Vec<Delta> {
        let mut by_server: HashMap<String, Vec<&SysLoop>> = HashMap::new();
        for l in loops {
            if !proxy_ports.contains(&l.port) {
                continue;
            }
            let server = proc_name(&l.server);
            if clash_up && is_core(&server) {
                continue;
            }
            if proc_name(&l.client) == server {
                continue;
            }
            by_server.entry(server).or_default().push(l);
        }

        let mut out = vec![];
        for (server, list) in by_server {
            let Some(ext) = external.get_mut(&server) else { continue };
            let sum_up: i64 = list.iter().map(|l| l.up).sum();
            let sum_down: i64 = list.iter().map(|l| l.down).sum();
            let (mut given_up, mut given_down) = (0, 0);
            for l in list {
                let up = if sum_up > 0 { rnd(ext.up as f64 * l.up as f64 / sum_up as f64) } else { 0 };
                let down = if sum_down > 0 { rnd(ext.down as f64 * l.down as f64 / sum_down as f64) } else { 0 };
                if up + down == 0 {
                    continue;
                }
                given_up += up;
                given_down += down;
                let route = if is_core(&server) && !self.last_route.is_empty() { self.last_route.clone() } else { stem(&server).to_string() };
                out.push(Delta {
                    proc: proc_name(&l.client),
                    path: l.client.clone(),
                    up,
                    down,
                    direct: 0,
                    route,
                    new_conns: 0,
                    hosts: vec![(format!("经 {server} 代理"), up + down)],
                });
            }
            let before = ext.up + ext.down;
            ext.up = (ext.up - given_up).max(0);
            ext.down = (ext.down - given_down).max(0);
            let ratio = if before > 0 { (ext.up + ext.down) as f64 / before as f64 } else { 0.0 };
            for h in ext.hosts.iter_mut() {
                h.1 = rnd(h.1 as f64 * ratio);
            }
            if ext.up + ext.down == 0 {
                external.remove(&server);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(path: &str, up: i64, down: i64) -> SysProc {
        SysProc { path: path.into(), up, down, ..Default::default() }
    }

    fn total(ds: &[Delta]) -> i64 {
        ds.iter().map(|d| d.up + d.down).sum()
    }

    #[test]
    fn plain_direct_without_clash() {
        let mut r = Reconciler::new();
        let etw = SystemSample { procs: vec![sys(r"C:\a\chrome.exe", 10, 90)], ..Default::default() };
        let out = r.merge(None, Some(&etw), &HashSet::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].proc, "chrome.exe");
        assert_eq!((out[0].up, out[0].down, out[0].direct), (10, 90, 100));
    }

    #[test]
    fn local_proxy_shared_by_clients() {
        let mut r = Reconciler::new();
        let etw = SystemSample {
            procs: vec![sys(r"C:\p\proxy.exe", 100, 1000)],
            loops: vec![
                SysLoop { client: r"C:\a\a.exe".into(), server: r"C:\p\proxy.exe".into(), port: 8080, up: 10, down: 300 },
                SysLoop { client: r"C:\b\b.exe".into(), server: r"C:\p\proxy.exe".into(), port: 8080, up: 30, down: 100 },
            ],
            ..Default::default()
        };
        let ports: HashSet<u16> = [8080].into();
        let out = r.merge(None, Some(&etw), &ports);
        assert_eq!(total(&out), 1100);
        let a = out.iter().find(|d| d.proc == "a.exe").unwrap();
        assert_eq!((a.up, a.down), (25, 750));
        assert_eq!(a.route, "proxy");
    }

    #[test]
    fn vpn_route() {
        let mut r = Reconciler::new();
        let mut p = sys(r"C:\a\a.exe", 10, 20);
        p.ifs = vec![(7, 10, 20)];
        let etw = SystemSample {
            procs: vec![p],
            ifaces: vec![SysIface { index: 7, alias: "wg-home".into(), desc: "WireGuard".into(), vpn: true }],
            ..Default::default()
        };
        let out = r.merge(None, Some(&etw), &HashSet::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].route, "VPN · wg-home");
    }

    #[test]
    fn tun_hint() {
        assert!(looks_like_clash_tun("Meta Tunnel"));
        assert!(looks_like_clash_tun("Mihomo"));
        assert!(!looks_like_clash_tun("Metadata adapter"));
    }
}
