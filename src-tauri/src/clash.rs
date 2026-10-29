//! 以 Clash 兼容的 /connections API 为数据源（Clash Verge / Mihomo Party / FlClash 里的 mihomo，
//! 或 sing-box 的 clash_api）。连接短命，关闭后就从列表里消失，所以记住每个 id 上次的读数，只报差值。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::cores::proc_name;
use crate::mihomo::{Connection, MihomoClient, NOT_DETECTED};
use crate::types::*;

struct LiveConn {
    proc: String,
    up: u64,
    down: u64,
}

#[derive(Default)]
struct State {
    live: HashMap<String, LiveConn>,
    last_global: Option<(u64, u64)>,
    connected: bool,
    last_error: Option<String>,
}

pub struct ClashSource {
    client: MihomoClient,
    st: Mutex<State>,
}

impl ClashSource {
    pub fn new() -> Self {
        Self { client: MihomoClient::default(), st: Mutex::new(State::default()) }
    }

    pub fn id(&self) -> &'static str {
        "clash"
    }

    /// 正在找控制接口还没找到：其他数据源应该等一等，否则内核的字节会被当成应用的
    pub fn searching(&self) -> bool {
        self.client.searching()
    }

    pub fn reset(&self) {
        let mut s = self.st.lock().unwrap();
        s.live.clear();
        s.last_global = None;
    }

    pub fn poll(&self) -> Result<ClashSample, String> {
        let resp = match self.client.get_connections() {
            Ok(r) => r,
            Err(e) => {
                let mut s = self.st.lock().unwrap();
                s.connected = false;
                s.last_error = (e != NOT_DETECTED).then(|| e.clone());
                return Err(e);
            }
        };
        let route = self.client.client().unwrap_or_else(|| "Clash".into());
        let mut s = self.st.lock().unwrap();
        s.connected = true;
        s.last_error = None;

        let mut deltas = vec![];
        let mut seen = HashSet::new();
        let (mut attributed_up, mut attributed_down) = (0i64, 0i64);
        // 启动 / 重连后的第一次读数：已经打开的连接带着之前的字节（可能上一轮已记过），只记基线
        let baseline = s.last_global.is_none();

        for c in resp.connections.iter().flatten() {
            seen.insert(c.id.clone());
            // mihomo 把自己下载规则集 / GeoIP 的连接标成进程 "mihomo" 且没有路径
            let own = c.metadata.process.as_deref() == Some("mihomo") && c.metadata.process_path.as_deref().unwrap_or("").is_empty();
            let via = if own { ClashVia::Inner } else { via_of(c) };
            // 和系统数据源的行一样按路径取名（macOS 应用包）；sing-box 只给 processPath
            let exe = if own {
                String::new()
            } else if let Some(p) = c.metadata.process_path.as_deref().filter(|p| !p.is_empty()) {
                proc_name(p)
            } else {
                c.metadata.process.clone().unwrap_or_default()
            };
            // "inner" 是内核自己开的连接（DNS 上游、规则集更新…）
            let proc = if !exe.is_empty() {
                exe.clone()
            } else if via == ClashVia::Inner {
                CORE_INNER.to_string()
            } else {
                UNKNOWN_PROCESS.to_string()
            };
            if baseline {
                s.live.insert(c.id.clone(), LiveConn { proc, up: c.upload, down: c.download });
                continue;
            }
            let prev = s.live.get(&c.id);
            let d_up = c.upload.saturating_sub(prev.map_or(0, |p| p.up)) as i64;
            let d_down = c.download.saturating_sub(prev.map_or(0, |p| p.down)) as i64;
            let is_new = prev.is_none();
            s.live.insert(c.id.clone(), LiveConn { proc: proc.clone(), up: c.upload, down: c.download });

            attributed_up += d_up;
            attributed_down += d_down;
            if is_new || d_up > 0 || d_down > 0 {
                let host = if c.metadata.host.is_empty() { &c.metadata.destination_ip } else { &c.metadata.host };
                deltas.push(ClashDelta {
                    d: Delta {
                        proc,
                        path: c.metadata.process_path.clone().unwrap_or_default(),
                        up: d_up,
                        down: d_down,
                        direct: if is_direct(c) { d_up + d_down } else { 0 },
                        route: route.clone(),
                        new_conns: if is_new { 1 } else { 0 },
                        hosts: vec![(host.clone(), d_up + d_down)],
                    },
                    via,
                    known: !exe.is_empty(),
                });
            }
        }
        s.live.retain(|id, _| seen.contains(id));

        // 内核的全局计数还包含两次轮询之间开了又关的连接，以及刚关闭连接的最后一批字节。
        // 把差额记下来，使每天的总量和 Clash 自己报的一致
        if let Some((lu, ld)) = s.last_global {
            if resp.upload_total >= lu && resp.download_total >= ld {
                let gap_up = (resp.upload_total - lu) as i64 - attributed_up;
                let gap_down = (resp.download_total - ld) as i64 - attributed_down;
                if gap_up > 0 || gap_down > 0 {
                    deltas.push(ClashDelta {
                        d: Delta {
                            proc: UNATTRIBUTED.into(),
                            up: gap_up.max(0),
                            down: gap_down.max(0),
                            route: route.clone(),
                            ..Default::default()
                        },
                        via: ClashVia::Gap,
                        known: false,
                    });
                }
            }
        }
        // 计数变小说明内核重启了，下次轮询和这次读数比
        s.last_global = Some((resp.upload_total, resp.download_total));

        let mut live_conns: HashMap<String, u64> = HashMap::new();
        for c in s.live.values() {
            *live_conns.entry(c.proc.clone()).or_insert(0) += 1;
        }
        drop(s);
        Ok(ClashSample { route, tun_device: self.client.tun_device(), deltas, live_conns })
    }

    pub fn status(&self) -> SourceStatus {
        let s = self.st.lock().unwrap();
        let client = self.client.client().unwrap_or_else(|| "Clash".into());
        let base = |state, detail: String| SourceStatus {
            id: self.id().into(),
            label: "代理".into(),
            state,
            detail,
            action: None,
        };
        if s.connected {
            let version = self.client.version().map(|v| format!(" · {v}")).unwrap_or_default();
            return base("on", format!("{client}{version}"));
        }
        if let Some(e) = &s.last_error {
            return base("error", format!("{client} 连接失败：{e}"));
        }
        let detail = if self.client.searching() { "正在检测代理…" } else { NOT_DETECTED };
        base("off", detail.into())
    }
}

/// mihomo："HTTP"、"Socks5"、"Tun"、"Inner"…；sing-box："mixed/mixed-in"、"tun/tun-in"…
fn via_of(c: &Connection) -> ClashVia {
    let kind = c.metadata.kind.to_lowercase();
    if kind == "inner" {
        ClashVia::Inner
    } else if kind == "tun" || kind.starts_with("tun/") {
        ClashVia::Tun
    } else {
        ClashVia::Proxy
    }
}

/// chain 列出出站（和选中它的策略组）；有 DIRECT 就是没走代理节点
fn is_direct(c: &Connection) -> bool {
    c.chains.iter().any(|n| n.eq_ignore_ascii_case("direct"))
}
