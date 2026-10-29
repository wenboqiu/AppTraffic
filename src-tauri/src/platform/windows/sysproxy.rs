//! 读 WinINet 系统代理，好把 ETW 里连到那个端口的回环流认成「应用 -> 本地代理」这一跳。

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::util::command;

const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const REFRESH: Duration = Duration::from_secs(15);

struct State {
    ports: Vec<u16>,
    last_refresh: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State { ports: Vec::new(), last_refresh: None });

/// `reg query` 输出里某个值："    ProxyEnable    REG_DWORD    0x1"
fn value<'a>(out: &'a str, name: &str) -> Option<&'a str> {
    out.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        if it.next()? != name {
            return None;
        }
        it.next()?; // REG_xxx
        let rest = l.trim_start().strip_prefix(name)?.trim_start();
        Some(rest.split_once(char::is_whitespace)?.1.trim())
    })
}

fn parse(out: &str) -> HashSet<u16> {
    let mut ports = HashSet::new();
    let enabled = value(out, "ProxyEnable").is_some_and(|v| v.strip_prefix("0x").map_or(v.parse::<u32>().ok(), |h| u32::from_str_radix(h, 16).ok()) == Some(1));
    if !enabled {
        return ports;
    }
    // "127.0.0.1:7897" 或按协议分的 "http=127.0.0.1:7897;https=…"
    for entry in value(out, "ProxyServer").unwrap_or("").split(';') {
        let host_port = entry.split_once('=').map_or(entry, |(_, v)| v).trim();
        if let Some((host, port)) = host_port.rsplit_once(':') {
            if ["127.0.0.1", "localhost", "::1", "[::1]"].contains(&host.to_lowercase().as_str()) {
                if let Ok(p) = port.parse() {
                    ports.insert(p);
                }
            }
        }
    }
    ports
}

fn refresh() {
    std::thread::spawn(|| {
        let Ok(o) = command("reg.exe").args(["query", KEY]).output() else { return };
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
    fn reg_output() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\...\\Internet Settings\r\n    ProxyEnable    REG_DWORD    0x1\r\n    ProxyServer    REG_SZ    http=127.0.0.1:7897;https=127.0.0.1:7897\r\n";
        assert_eq!(parse(out), HashSet::from([7897]));
        let off = "    ProxyEnable    REG_DWORD    0x0\r\n    ProxyServer    REG_SZ    127.0.0.1:1\r\n";
        assert!(parse(off).is_empty());
    }
}
