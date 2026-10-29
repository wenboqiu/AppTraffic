//! 从内核配置文件里读控制接口设置：mihomo / Clash Meta 的 YAML，或 sing-box 的 JSON（experimental.clash_api）。

use std::path::Path;

use crate::mihomo::Endpoint;

#[derive(Default)]
pub struct ControllerConfig {
    /// "127.0.0.1:9090"、":9090"…
    pub controller: Option<String>,
    /// Windows 命名管道（external-controller-pipe）或 Unix socket（external-controller-unix）
    pub socket: Option<String>,
    pub secret: Option<String>,
}

/// 只认顶层键（缩进的 "secret:" 属于某个代理节点）
fn yaml_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in text.lines() {
        let Some(rest) = line.strip_prefix(&prefix) else { continue };
        let rest = rest.trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest);
        let end = rest.find(['\'', '"', '#']).unwrap_or(rest.len());
        let v = rest[..end].trim();
        return (!v.is_empty()).then(|| v.to_string());
    }
    None
}

pub fn read_controller_config(file: &Path) -> ControllerConfig {
    let Ok(text) = std::fs::read_to_string(file) else { return ControllerConfig::default() };
    if file.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        let api = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.pointer("/experimental/clash_api").cloned());
        let Some(api) = api else { return ControllerConfig::default() };
        let s = |k: &str| api.get(k).and_then(|v| v.as_str()).map(String::from);
        return ControllerConfig { controller: s("external_controller"), secret: s("secret"), socket: None };
    }
    ControllerConfig {
        controller: yaml_field(&text, "external-controller"),
        socket: yaml_field(&text, "external-controller-pipe").or_else(|| yaml_field(&text, "external-controller-unix")),
        secret: yaml_field(&text, "secret"),
    }
}

/// 控制地址对应的 HTTP 端点；通配地址改走回环
pub fn http_endpoint(controller: Option<&str>, secret: &str, client: &str) -> Option<Endpoint> {
    let c = controller?;
    let (host, port) = c.rsplit_once(':')?;
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port.parse().ok()?;
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    let host = if ["", "0.0.0.0", "::", "127.0.0.1", "localhost"].contains(&host) { "127.0.0.1" } else { host };
    Some(Endpoint::http(host, port, secret, client))
}

/// 配置文件描述的端点，socket 在前（不要 secret，也不会撞端口）
pub fn endpoints_from_config(file: &Path, client: &str) -> Vec<Endpoint> {
    if !file.exists() {
        return vec![];
    }
    let cfg = read_controller_config(file);
    let secret = cfg.secret.clone().unwrap_or_default();
    let mut out = vec![];
    if let Some(s) = cfg.socket {
        out.push(Endpoint::socket(&s, &secret, client));
    }
    if let Some(ep) = http_endpoint(cfg.controller.as_deref(), &secret, client) {
        out.push(ep);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml() {
        let t = "mixed-port: 7897\nexternal-controller: 127.0.0.1:9097 # api\nsecret: 'abc'\nproxies:\n  - secret: nope\n";
        assert_eq!(yaml_field(t, "external-controller").as_deref(), Some("127.0.0.1:9097"));
        assert_eq!(yaml_field(t, "secret").as_deref(), Some("abc"));
        assert_eq!(yaml_field(t, "external-controller-pipe"), None);
    }

    #[test]
    fn http() {
        let ep = http_endpoint(Some(":9090"), "", "x").unwrap();
        assert_eq!(ep.label(), "http://127.0.0.1:9090");
        assert!(http_endpoint(Some("nonsense"), "", "x").is_none());
    }
}
