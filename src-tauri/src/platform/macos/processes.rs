//! macOS：列出带父进程和命令行的进程（`ps`），以及各 Clash 客户端存放控制接口的地方。

use std::path::{Path, PathBuf};

use crate::controller_config::{endpoints_from_config, read_controller_config};
use crate::discovery::CoreProc;
use crate::mihomo::Endpoint;
use crate::util::command;

/// 客户端放控制 socket 的地方
const SOCKET_DIRS: [&str; 2] = ["/tmp", "/tmp/verge"];

pub fn ps(args: &[&str]) -> Option<String> {
    let o = command("/bin/ps").args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

fn base_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// "  754   555 /Applications/X.app/Contents/MacOS/x" -> (754, 555, 路径)
fn parse_row(line: &str) -> Option<(i64, i64, &str)> {
    let t = line.trim_start();
    let (pid, rest) = t.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (ppid, exe) = rest.split_once(char::is_whitespace)?;
    let exe = exe.trim_start();
    (!exe.is_empty()).then_some(())?;
    Some((pid.parse().ok()?, ppid.parse().ok()?, exe))
}

/// 空白之后紧跟 `-flag` / `--flag`（其后是空白、= 或结尾）
fn flag_at(s: &str) -> bool {
    let b = s.as_bytes();
    let dashes = b.iter().take_while(|c| **c == b'-').count();
    if dashes == 0 || dashes > 2 || !b.get(dashes).is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rest = &s[dashes..];
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(rest.len());
    rest[end..].chars().next().is_none_or(|c| c.is_whitespace() || c == '=')
}

/// 把 `ps -o args` 的输出拆回参数。ps 用空格拼 argv，带空格的路径和两个参数分不开；
/// 但 argv[0] 已知（就是可执行文件），值又跟在 flag 后面，所以在下一个 " -flag" 处切开。内核的命令行够用了。
pub fn split_args(cmd: &str, exe: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = cmd;
    if let Some(r) = cmd.strip_prefix(exe) {
        out.push(exe.to_string());
        rest = r;
    }
    // 在「空白串 + 紧跟 flag」处切
    let mut chunks = vec![];
    let mut start = 0;
    let mut i = 0;
    let bytes = rest.as_bytes();
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && flag_at(&rest[j..]) {
                chunks.push(&rest[start..i]);
                start = j;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    chunks.push(&rest[start..]);

    for c in chunks {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        if flag_at(c) {
            let dashes = c.bytes().take_while(|b| *b == b'-').count();
            let name_end = c[dashes..]
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
                .map_or(c.len(), |n| n + dashes);
            out.push(c[..name_end].to_string());
            let value = c[name_end..].trim_start_matches(|ch: char| ch == '=' || ch.is_whitespace());
            if !value.is_empty() {
                out.push(value.to_string());
            }
        } else {
            out.push(c.to_string());
        }
    }
    out
}

pub fn list_processes(names: &[&str]) -> Vec<CoreProc> {
    let wanted: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let Some(table) = ps(&["-axww", "-o", "pid=,ppid=,comm="]) else { return vec![] };
    let rows: Vec<_> = table.lines().filter_map(parse_row).collect();
    rows.iter()
        .filter(|(_, _, exe)| wanted.contains(&base_name(exe).to_lowercase()))
        .map(|(pid, ppid, exe)| {
            let args = ps(&["-ww", "-o", "args=", "-p", &pid.to_string()])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(|cmd| split_args(&cmd, exe));
            let parent = rows.iter().find(|(p, _, _)| p == ppid).map(|(_, _, e)| base_name(e).to_string());
            CoreProc { name: base_name(exe).to_string(), parent, args }
        })
        .collect()
}

/// Clash Verge Rev 的配置，给内核以服务运行时用
pub fn known_endpoints() -> Vec<Endpoint> {
    let file = PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/clash-verge.yaml");
    if !file.exists() {
        return vec![];
    }
    let mut eps = endpoints_from_config(&file, "Clash Verge");
    if !eps.iter().any(|e| matches!(e.target, crate::mihomo::Target::Socket(_))) {
        let secret = read_controller_config(&file).secret.unwrap_or_default();
        eps.insert(0, Endpoint::socket("/tmp/verge/verge-mihomo.sock", &secret, "Clash Verge"));
    }
    eps
}

fn hint(n: &str) -> bool {
    let l = n.to_lowercase();
    l.ends_with(".sock") && ["mihomo", "clash", "verge", "sing-box", "singbox", "sparkle", "party"].iter().any(|h| l.contains(h))
}

pub fn socket_endpoints() -> Vec<Endpoint> {
    use std::os::unix::fs::FileTypeExt;
    let mut dirs: Vec<PathBuf> = SOCKET_DIRS.iter().map(PathBuf::from).collect();
    let tmp = std::env::temp_dir();
    if !dirs.contains(&tmp) {
        dirs.push(tmp);
    }
    let mut out = vec![];
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let n = e.file_name().to_string_lossy().to_string();
            if !hint(&n) || !e.file_type().is_ok_and(|t| t.is_socket()) {
                continue;
            }
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
            let path: &Path = &dir.join(&n);
            out.push(Endpoint::socket(&path.to_string_lossy(), "", &client));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows() {
        assert_eq!(parse_row("  754   555 /Applications/X Y.app/Contents/MacOS/x"), Some((754, 555, "/Applications/X Y.app/Contents/MacOS/x")));
    }

    #[test]
    fn args() {
        let exe = "/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo";
        let cmd = format!("{exe} -d /Users/a b/Library/Application Support/x -ext-ctl-unix /tmp/verge/verge-mihomo.sock");
        assert_eq!(
            split_args(&cmd, exe),
            vec![exe, "-d", "/Users/a b/Library/Application Support/x", "-ext-ctl-unix", "/tmp/verge/verge-mihomo.sock"]
        );
        assert_eq!(split_args("mihomo --secret=abc -f cfg.yaml", "mihomo"), vec!["mihomo", "--secret", "abc", "-f", "cfg.yaml"]);
    }
}
