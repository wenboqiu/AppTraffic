//! 读 macOS 系统代理（`scutil --proxy`），好把回环流认成「应用 -> 本地代理」这一跳。

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::util::command;

const REFRESH: Duration = Duration::from_secs(15);
const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "::1"];

struct State {
    ports: Vec<u16>,
    last_refresh: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State { ports: Vec::new(), last_refresh: None });

/// "  HTTPEnable : 1" 这种行里的值
fn value<'a>(out: &'a str, key: &str) -> Option<&'a str> {
    out.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim())
    })
}

fn parse(out: &str) -> HashSet<u16> {
    let mut ports = HashSet::new();
    for scheme in ["HTTP", "HTTPS", "SOCKS"] {
        if value(out, &format!("{scheme}Enable")) != Some("1") {
            continue;
        }
        let host = value(out, &format!("{scheme}Proxy")).unwrap_or("").to_lowercase();
        let port: u16 = value(out, &format!("{scheme}Port")).and_then(|p| p.parse().ok()).unwrap_or(0);
        if LOOPBACK.contains(&host.as_str()) && port != 0 {
            ports.insert(port);
        }
    }
    ports
}

fn refresh() {
    std::thread::spawn(|| {
        let Ok(o) = command("/usr/sbin/scutil").arg("--proxy").output() else { return };
        if o.status.success() {
            STATE.lock().unwrap().ports = parse(&String::from_utf8_lossy(&o.stdout)).into_iter().collect();
        }
    });
}

/// 系统代理当前指向的回环端口（后台刷新）
pub fn system_proxy_ports() -> HashSet<u16> {
    let mut s = STATE.lock().unwrap();
    if s.last_refresh.is_none_or(|t| t.elapsed() > REFRESH) {
        s.last_refresh = Some(Instant::now());
        refresh();
    }
    s.ports.iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scutil_output() {
        let out = "<dictionary> {\n  HTTPEnable : 1\n  HTTPPort : 7897\n  HTTPProxy : 127.0.0.1\n  HTTPSEnable : 0\n  HTTPSPort : 1\n  HTTPSProxy : 127.0.0.1\n  SOCKSEnable : 1\n  SOCKSPort : 7891\n  SOCKSProxy : 10.0.0.2\n}";
        assert_eq!(parse(out), HashSet::from([7897]));
    }
}
