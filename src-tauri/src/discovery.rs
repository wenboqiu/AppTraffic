//! 不知道是哪个 GUI 客户端启动的，也要找到正在运行的 Clash 兼容 REST API（mihomo、Clash Meta、sing-box 的 clash_api）：
//!
//! 1. 运行中的内核进程：命令行里有配置文件（mihomo 的 -f/-d，sing-box 的 -c/-D），有时直接给出控制接口
//!    （-ext-ctl、-ext-ctl-pipe / -ext-ctl-unix、-secret）；父进程名就是客户端名。
//! 2. 各客户端的已知配置位置，给命令行读不到的内核（以服务 / 其他用户运行）。
//! 3. 看起来像内核控制接口的本地 socket。
//!
//! 每个候选都用 GET /version 确认。系统相关的部分（列进程、配置和 socket 在哪）在 platform/ 里。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::controller_config::{http_endpoint, read_controller_config};
use crate::cores::{client_label, CORE_NAMES};
use crate::mihomo::{request, Endpoint};
use crate::platform;

pub struct CoreProc {
    pub name: String,
    /// 父进程的可执行文件名，用来命名 GUI 客户端
    pub parent: Option<String>,
    /// 拆开的命令行（argv[0] 在前）；读不到时为 None
    pub args: Option<Vec<String>>,
}

fn flag_values(args: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for i in 0..args.len().saturating_sub(1) {
        let a = &args[i];
        let mut cs = a.chars();
        let flag = match (cs.next(), cs.next(), cs.next()) {
            (Some('-'), Some(c), _) if c.is_ascii_alphabetic() => true,
            (Some('-'), Some('-'), Some(c)) => c.is_ascii_alphabetic(),
            _ => false,
        };
        if flag && !out.contains_key(a) {
            // \\?\ 是有些启动器加的 Windows 长路径前缀
            out.insert(a.clone(), args[i + 1].strip_prefix(r"\\?\").unwrap_or(&args[i + 1]).to_string());
        }
    }
    out
}

pub fn endpoints_from_core(core: &CoreProc) -> Vec<Endpoint> {
    let Some(args) = &core.args else { return vec![] };
    let flags = flag_values(args);
    let get = |ks: &[&str]| ks.iter().find_map(|k| flags.get(*k)).cloned();
    let dir = get(&["-d", "-D", "--directory"]);
    let mut file = get(&["-f", "-c", "--config"]).map(PathBuf::from);
    match (&file, &dir) {
        (None, Some(d)) => file = Some(Path::new(d).join("config.yaml")),
        (Some(f), Some(d)) if !f.is_absolute() => file = Some(Path::new(d).join(f)),
        _ => {}
    }
    let cfg = file.as_deref().map(read_controller_config).unwrap_or_default();

    let client = client_label(core.parent.as_deref(), &core.name);
    let secret = flags.get("-secret").cloned().or(cfg.secret).unwrap_or_default();
    let mut out = vec![];
    let socket = get(&["-ext-ctl-pipe", "-ext-ctl-unix"]).or(cfg.socket);
    if let Some(s) = socket {
        out.push(Endpoint::socket(&s, &secret, &client));
    }
    let controller = flags.get("-ext-ctl").cloned().or(cfg.controller);
    if let Some(ep) = http_endpoint(controller.as_deref(), &secret, &client) {
        out.push(ep);
    }
    out
}

/// 按可信度依次试所有候选，返回第一个应答的
pub fn discover() -> Option<(Endpoint, String)> {
    let mut candidates: Vec<Endpoint> = vec![];
    let mut seen = HashSet::new();
    let mut add = |eps: Vec<Endpoint>| {
        for ep in eps {
            if seen.insert(ep.key()) {
                candidates.push(ep);
            }
        }
    };
    for core in platform::list_processes(CORE_NAMES) {
        add(endpoints_from_core(&core));
    }
    add(platform::known_endpoints());
    add(platform::socket_endpoints());

    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Version {
        version: Option<String>,
    }
    for ep in candidates {
        if let Ok(Version { version: Some(v) }) = request::<Version>(&ep, "/version", Duration::from_millis(1500)) {
            return Some((ep, v));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags() {
        let args: Vec<String> = ["mihomo.exe", "-d", r"\\?\C:\x", "-ext-ctl-pipe", r"\\.\pipe\verge-mihomo", "-v"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let f = flag_values(&args);
        assert_eq!(f.get("-d").unwrap(), r"C:\x");
        let core = CoreProc { name: "verge-mihomo.exe".into(), parent: Some("clash-verge.exe".into()), args: Some(args) };
        let eps = endpoints_from_core(&core);
        assert_eq!(eps[0].label(), r"\\.\pipe\verge-mihomo");
        assert_eq!(eps[0].client, "Clash Verge");
    }
}
