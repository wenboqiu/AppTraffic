//! Windows：列出带父进程和命令行的进程（经 PowerShell 取 WMI），以及各 Clash 客户端存放控制接口的地方。

use std::path::PathBuf;

use serde::Deserialize;

use super::elevate::run_powershell;
use crate::controller_config::{endpoints_from_config, read_controller_config};
use crate::discovery::CoreProc;
use crate::mihomo::Endpoint;

/// 像内核控制接口的管道名
fn pipe_hint(n: &str) -> bool {
    let l = n.to_lowercase();
    ["mihomo", "clash", "verge", "sing-box", "singbox", "sparkle", "party"].iter().any(|h| l.contains(h))
}

/// 排除自己的管道（单实例锁、更新器）
fn pipe_ignore(n: &str) -> bool {
    n.to_lowercase().contains("traffic")
}

/// 按双引号拆 Windows 命令行
pub fn tokenize(cmd: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let (mut in_q, mut has) = (false, false);
    for c in cmd.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            c if c.is_whitespace() && !in_q => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

#[derive(Deserialize)]
struct Row {
    name: String,
    parent: Option<String>,
    cmd: Option<String>,
}

pub fn list_processes(names: &[&str]) -> Vec<CoreProc> {
    let wanted = names.iter().map(|n| format!("'{}'", n.to_lowercase().replace('\'', "''"))).collect::<Vec<_>>().join(",");
    let script = format!(
        r#"
$all = Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,Name,CommandLine
$names = @{{}}
foreach ($p in $all) {{ $names[[int]$p.ProcessId] = $p.Name }}
$wanted = @({wanted})
$found = @($all | Where-Object {{ $wanted -contains [IO.Path]::GetFileNameWithoutExtension($_.Name).ToLower() }} | ForEach-Object {{
  [pscustomobject]@{{ pid = [int]$_.ProcessId; name = $_.Name; parent = $names[[int]$_.ParentProcessId]; cmd = $_.CommandLine }}
}})
ConvertTo-Json -InputObject $found -Compress"#
    );
    let Ok(out) = run_powershell(&script, Some(std::time::Duration::from_secs(15))) else { return vec![] };
    let out = out.trim();
    if out.is_empty() {
        return vec![];
    }
    // 只有一个结果时 PowerShell 可能输出单个对象而不是数组
    let rows: Vec<Row> = match serde_json::from_str::<serde_json::Value>(out) {
        Ok(serde_json::Value::Array(a)) => a.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect(),
        Ok(v @ serde_json::Value::Object(_)) => serde_json::from_value(v).ok().into_iter().collect(),
        _ => vec![],
    };
    rows.into_iter()
        .map(|r| CoreProc { name: r.name, parent: r.parent, args: r.cmd.map(|c| tokenize(&c)) })
        .collect()
}

/// Clash Verge Rev 的配置，给内核以服务运行时用
pub fn known_endpoints() -> Vec<Endpoint> {
    let file = PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
        .join("io.github.clash-verge-rev.clash-verge-rev")
        .join("clash-verge.yaml");
    if !file.exists() {
        return vec![];
    }
    let mut eps = endpoints_from_config(&file, "Clash Verge");
    // Verge 不管配置里写没写，总会开这个管道
    if !eps.iter().any(|e| matches!(e.target, crate::mihomo::Target::Socket(_))) {
        let secret = read_controller_config(&file).secret.unwrap_or_default();
        eps.insert(0, Endpoint::socket(r"\\.\pipe\verge-mihomo", &secret, "Clash Verge"));
    }
    eps
}

pub fn socket_endpoints() -> Vec<Endpoint> {
    let Ok(rd) = std::fs::read_dir(r"\\.\pipe\") else { return vec![] };
    rd.filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| pipe_hint(n) && !pipe_ignore(n))
        .map(|n| {
            let l = n.to_lowercase();
            let client = if l.contains("verge") {
                "Clash Verge".to_string()
            } else if l.contains("party") {
                "Mihomo Party".to_string()
            } else if l.contains("sparkle") {
                "Sparkle".to_string()
            } else {
                n.clone()
            };
            Endpoint::socket(&format!(r"\\.\pipe\{n}"), "", &client)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert_eq!(
            tokenize(r#""C:\a b\mihomo.exe" -d "C:\x y" -secret abc"#),
            vec![r"C:\a b\mihomo.exe", "-d", r"C:\x y", "-secret", "abc"]
        );
    }
}
