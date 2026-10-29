//! 暴露 Clash 兼容 REST API 的代理内核、启动它们的 GUI 客户端、VPN 客户端。
//! 名字比较时去掉 Windows 的 .exe 后缀并忽略大小写，一份列表通用各平台。

use crate::types::UNKNOWN_PROCESS;

/// 自己的 socket 是应用流量上游一侧的内核可执行文件名
pub const CORE_NAMES: &[&str] = &[
    "verge-mihomo",
    "verge-mihomo-alpha",
    "mihomo",
    "mihomo-alpha",
    "mihomo-windows-amd64",
    "mihomo-darwin-arm64",
    "mihomo-darwin-amd64",
    "clash",
    "clash-meta",
    "clash-meta-alpha",
    "FlClashCore",
    "sing-box",
];

/// VPN 客户端：自己的流量是应用经 VPN 网卡发出的字节的加密承载，即同样的字节再算一遍
const VPN_CLIENTS: &[&str] = &[
    "openvpn",
    "openvpnserv",
    "wireguard",
    "wireguard-go",
    "tailscaled",
    "io.tailscale.ipn.macsys.network-extension",
    "zerotier-one",
    "zerotier-one_x64",
    "zerotier-one_x86",
    "vpnagent", // Cisco AnyConnect / Secure Client
    "vpnagentd",
    "PanGPS", // GlobalProtect
    "FortiSSLVPNdaemon",
    "openconnect",
    "hamachi-2",
    "nordvpn-service",
    "expressvpnd",
    "ProtonVPN.WireGuardService",
    "netbird",
    "easytier-core",
];

/// 启动内核的父进程 -> 显示名
const CLIENT_LABELS: &[(&str, &str)] = &[
    ("clash-verge", "Clash Verge"),
    ("clash-verge-service", "Clash Verge"),
    ("mihomo-party", "Mihomo Party"),
    ("sparkle", "Sparkle"),
    ("flclash", "FlClash"),
    ("flclashhelperservice", "FlClash"),
    ("v2rayn", "v2rayN"),
    ("v2rayu", "V2rayU"),
    ("nekoray", "NekoRay"),
    ("nekobox", "NekoBox"),
    ("hiddify", "Hiddify"),
    ("clash-for-windows", "Clash for Windows"),
    ("clashx", "ClashX"),
    ("clashx-meta", "ClashX Meta"),
    ("gui.for.clash", "GUI.for.Clash"),
    ("gui.for.singbox", "GUI.for.SingBox"),
];

/// "verge-mihomo.exe" -> "verge-mihomo"
pub fn stem(name: &str) -> &str {
    let n = name.len();
    if n >= 4 && name.is_char_boundary(n - 4) && name[n - 4..].eq_ignore_ascii_case(".exe") {
        &name[..n - 4]
    } else {
        name
    }
}

fn key(name: &str) -> String {
    stem(name).to_lowercase()
}

pub fn is_core(name: &str) -> bool {
    let k = key(name);
    CORE_NAMES.iter().any(|c| c.to_lowercase() == k)
}

pub fn is_vpn_client(name: &str) -> bool {
    let k = key(name);
    VPN_CLIENTS.iter().any(|c| c.to_lowercase() == k)
}

/// 可执行文件路径的显示名：文件名；macOS 上 .app 包里的一切都算这个应用
/// （"Google Chrome Helper" 等归到 "Google Chrome"）。内核和 VPN 客户端保留自己的名字：
/// 要靠名字识别，它们的字节也不是应用的。
pub fn proc_name(exe_path: &str) -> String {
    if exe_path.is_empty() {
        return UNKNOWN_PROCESS.to_string();
    }
    let file = exe_path.rsplit(['\\', '/']).next().filter(|s| !s.is_empty()).unwrap_or(exe_path);
    if is_core(file) || is_vpn_client(file) {
        return file.to_string();
    }
    if exe_path.starts_with('/') {
        let parts: Vec<&str> = exe_path.split('/').collect();
        for p in &parts[..parts.len().saturating_sub(1)] {
            if let Some(app) = p.strip_suffix(".app") {
                if !app.is_empty() {
                    return app.to_string();
                }
            }
        }
    }
    file.to_string()
}

/// 内核由 clash-verge(.exe) 启动时是 "Clash Verge"，否则取内核自己的名字
pub fn client_label(parent: Option<&str>, core: &str) -> String {
    if let Some(p) = parent {
        let k = key(p).split_whitespace().collect::<Vec<_>>().join("-").replace('_', "-");
        if let Some((_, label)) = CLIENT_LABELS.iter().find(|(n, _)| *n == k) {
            return label.to_string();
        }
    }
    stem(core).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(proc_name("C:\\a\\Chrome.exe"), "Chrome.exe");
        assert_eq!(proc_name("/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Helper"), "Google Chrome");
        assert_eq!(proc_name("C:\\x\\verge-mihomo.exe"), "verge-mihomo.exe");
        assert!(is_core("Verge-Mihomo.EXE"));
        assert_eq!(client_label(Some("clash-verge.exe"), "verge-mihomo.exe"), "Clash Verge");
        assert_eq!(client_label(None, "mihomo.exe"), "mihomo");
    }
}
