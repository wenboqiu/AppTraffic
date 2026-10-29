//! macOS 网卡：给 nettop 的行一个稳定的序号，认出哪些是 VPN，以及默认路由所在的网卡
//! （看不到的连接按它算）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::types::SysIface;
use crate::util::command;

const REFRESH: Duration = Duration::from_secs(10);
/// VPN 客户端建的点对点隧道（utun：Network Extension / WireGuard / Tailscale…）
const TUNNEL_PREFIXES: [&str; 6] = ["utun", "ppp", "ipsec", "tun", "tap", "wg"];

#[derive(Clone, Debug, Default)]
pub struct IfaceInfo {
    pub name: String,
    pub addrs: Vec<String>,
}

/// mihomo / sing-box TUN 的默认网段（fake-ip）
fn clash_tun_addr(a: &str) -> bool {
    a.starts_with("198.18.") || a.starts_with("198.19.")
}

fn is_tunnel_name(n: &str) -> bool {
    TUNNEL_PREFIXES.iter().any(|p| n.strip_prefix(p).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())))
}

/// `ifconfig` 输出 -> 网卡和它们的地址
pub fn parse_ifconfig(out: &str) -> Vec<IfaceInfo> {
    let mut list: Vec<IfaceInfo> = vec![];
    for line in out.lines() {
        if let Some((name, _)) = line.split_once(": flags=") {
            if !name.is_empty() && !name.starts_with(char::is_whitespace) {
                list.push(IfaceInfo { name: name.to_string(), addrs: vec![] });
                continue;
            }
        }
        let t = line.trim_start();
        if t.len() == line.len() {
            continue;
        }
        if let Some(rest) = t.strip_prefix("inet6 ").or_else(|| t.strip_prefix("inet ")) {
            let addr: String = rest.chars().take_while(|c| c.is_ascii_hexdigit() || *c == '.' || *c == ':').collect();
            if let (false, Some(last)) = (addr.is_empty(), list.last_mut()) {
                last.addrs.push(addr);
            }
        }
    }
    list
}

/// 真在走流量的隧道：有可路由的地址。macOS 自己还留着几个空闲 utun（iCloud、接力…），只有链路本地 IPv6。
pub fn is_vpn_iface(f: &IfaceInfo) -> bool {
    is_tunnel_name(&f.name) && f.addrs.iter().any(|a| !a.to_lowercase().starts_with("fe80:") && !a.starts_with("169.254."))
}

/// `scutil --nc list` 里已连接的 VPN 服务名
pub fn connected_vpn_names(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.contains("(Connected)"))
        .filter_map(|l| {
            let a = l.find('"')? + 1;
            let b = a + l[a..].find('"')?;
            Some(l[a..b].to_string())
        })
        .collect()
}

pub struct InterfaceTable {
    /// 出现的顺序，序号 = 下标 + 1
    order: Vec<String>,
    indices: HashMap<String, i64>,
    info: HashMap<String, IfaceInfo>,
    vpn_names: Vec<String>,
    dflt: String,
    last_refresh: Option<Instant>,
}

pub type SharedTable = Arc<Mutex<InterfaceTable>>;

impl InterfaceTable {
    pub fn new() -> Self {
        Self {
            order: vec![],
            indices: HashMap::new(),
            info: HashMap::new(),
            vpn_names: vec![],
            dflt: "en0".into(),
            last_refresh: None,
        }
    }

    pub fn index(&mut self, name: &str) -> i64 {
        if let Some(i) = self.indices.get(name) {
            return *i;
        }
        let i = self.order.len() as i64 + 1;
        self.order.push(name.to_string());
        self.indices.insert(name.to_string(), i);
        i
    }

    pub fn default_iface(&self) -> String {
        self.dflt.clone()
    }

    /// nettop 到目前为止提到过的网卡
    pub fn list(&self) -> Vec<SysIface> {
        let vpns = self
            .order
            .iter()
            .filter(|n| {
                self.info.get(*n).is_some_and(|f| is_vpn_iface(f) && !f.addrs.iter().any(|a| clash_tun_addr(a)))
            })
            .count();
        self.order
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let f = self.info.get(name);
                let vpn = f.is_some_and(is_vpn_iface);
                let clash_tun = f.is_some_and(|f| f.addrs.iter().any(|a| clash_tun_addr(a)));
                // 只有一个隧道在线、只连着一个 VPN 服务时，它们是同一个
                let alias = if vpn && !clash_tun && vpns == 1 && self.vpn_names.len() == 1 {
                    self.vpn_names[0].clone()
                } else {
                    name.clone()
                };
                let desc = if clash_tun { format!("{name} (Clash TUN)") } else { name.clone() };
                SysIface { index: i as i64 + 1, alias, desc, vpn }
            })
            .collect()
    }
}

fn run(file: &str, args: &[&str]) -> String {
    command(file).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default()
}

/// 后台重读网卡信息
pub fn refresh(table: &SharedTable) {
    table.lock().unwrap().last_refresh = Some(Instant::now());
    let table = table.clone();
    std::thread::spawn(move || {
        let ifconfig = run("/sbin/ifconfig", &[]);
        let nc = run("/usr/sbin/scutil", &["--nc", "list"]);
        let route = run("/sbin/route", &["-n", "get", "default"]);
        let mut t = table.lock().unwrap();
        if !ifconfig.is_empty() {
            t.info = parse_ifconfig(&ifconfig).into_iter().map(|f| (f.name.clone(), f)).collect();
        }
        t.vpn_names = connected_vpn_names(&nc);
        if let Some(i) = route.lines().find_map(|l| l.trim().strip_prefix("interface:")) {
            t.dflt = i.trim().to_string();
        }
    });
}

pub fn refresh_if_stale(table: &SharedTable) {
    let stale = table.lock().unwrap().last_refresh.is_none_or(|t| t.elapsed() > REFRESH);
    if stale {
        refresh(table);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "lo0: flags=8049<UP,LOOPBACK> mtu 16384\n\tinet 127.0.0.1 netmask 0xff000000\nutun1: flags=8051<UP> mtu 1380\n\tinet6 fe80::1%utun1 prefixlen 64\nutun4: flags=8051<UP> mtu 1500\n\tinet 10.8.0.2 --> 10.8.0.2 netmask 0xffffff00\n\tinet6 fe80::2%utun4 prefixlen 64\n";

    #[test]
    fn vpn_detection() {
        let list = parse_ifconfig(OUT);
        assert_eq!(list.len(), 3);
        assert!(!is_vpn_iface(&list[0]));
        assert!(!is_vpn_iface(&list[1])); // 只有链路本地地址
        assert!(is_vpn_iface(&list[2]));
        assert_eq!(list[2].addrs, vec!["10.8.0.2", "fe80::2"]);
    }

    #[test]
    fn nc_list() {
        let out = "* (Connected)      ABC-1 PPP --> x \"WireGuard\" [VPN:com.wireguard]\n* (Disconnected)   DEF \"Other\" [VPN]\n";
        assert_eq!(connected_vpn_names(out), vec!["WireGuard"]);
    }
}
