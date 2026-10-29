//! 启动需要管理员权限的 ETW 采集程序。
//!
//! 首选：计划任务 "\AppTraffic\ETW Collector"（最高权限），当前用户可以不弹窗地运行它。
//! 注册任务要弹一次 UAC，由 etw-task.ps1 在用户启用系统级统计时完成。因为任务是提权运行的，
//! 只会指向 Program Files 里的采集程序（非管理员换不了它）。其他位置（开发构建、每用户安装）
//! 每次都弹 UAC 启动。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::Engine;

use crate::util::command;

const TASK_PATH: &str = r"\AppTraffic\";
const TASK_NAME: &str = "ETW Collector";
pub const HELPER: &str = "traffic-etw.exe";

/// 运行 PowerShell 脚本并返回输出；timeout 为 None 时一直等（UAC 窗口可以停很久）。非零退出码算失败。
pub fn run_powershell(script: &str, timeout: Option<Duration>) -> Result<String, String> {
    let text = format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8\n{script}");
    let utf16: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let mut child = command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let buf = match timeout {
        Some(t) => match rx.recv_timeout(t) {
            Ok(b) => b,
            Err(_) => {
                let _ = child.kill();
                return Err("timeout".into());
            }
        },
        None => rx.recv().map_err(|e| e.to_string())?,
    };
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("exit {:?}", status.code()));
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// PowerShell 单引号字符串
fn psq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
}

/// 安装包把采集程序和脚本放在主程序旁边；开发时回到源码树里找
fn find_resource(name: &str, dev_rel: &str) -> PathBuf {
    let beside = exe_dir().join(name);
    if beside.exists() || !cfg!(debug_assertions) {
        return beside;
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(dev_rel)
}

pub fn helper_path() -> PathBuf {
    find_resource(HELPER, "native/target/release/traffic-etw.exe")
}

fn task_script_path() -> PathBuf {
    find_resource("etw-task.ps1", "resources/etw-task.ps1")
}

fn norm(p: &str) -> String {
    p.trim_matches('"').trim_start_matches(r"\\?\").replace('/', r"\").to_lowercase()
}

/// 在 Program Files 里，即只有管理员能替换它
pub fn is_protected_path(p: &Path) -> bool {
    let full = norm(&p.to_string_lossy());
    ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|d| format!("{}\\", norm(&d).trim_end_matches('\\')))
        .any(|d| full.starts_with(&d))
}

pub fn same_path(a: &str, b: &Path) -> bool {
    norm(a) == norm(&b.to_string_lossy())
}

/// 计划任务运行的程序；没有任务（或读不了）时为 None
pub fn task_executable() -> Option<String> {
    let script = format!(
        "$t = Get-ScheduledTask -TaskPath {} -TaskName {} -ErrorAction Stop\n$t.Actions[0].Execute",
        psq(TASK_PATH),
        psq(TASK_NAME)
    );
    let out = run_powershell(&script, Some(Duration::from_secs(15))).ok()?;
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_string())
}

/// 不弹窗启动任务（任务给了当前用户运行权限）
pub fn run_task() -> bool {
    command("schtasks.exe")
        .args(["/Run", "/TN", &format!("{TASK_PATH}{TASK_NAME}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// 提权运行一个程序（UAC 弹窗）。用户拒绝时返回 false；wait 时等程序退出，且只有退出码为 0 才算成功
pub fn run_elevated(file: &str, args: &[String], wait: bool) -> bool {
    // Start-Process 用空格拼 ArgumentList，所以带空格的参数要加引号
    let list = args
        .iter()
        .map(|a| psq(&if a.contains(char::is_whitespace) { format!("\"{a}\"") } else { a.clone() }))
        .collect::<Vec<_>>()
        .join(",");
    let arg_part = if args.is_empty() { String::new() } else { format!(" -ArgumentList @({list})") };
    let script = if wait {
        format!(
            "$p = Start-Process -FilePath {}{arg_part} -Verb RunAs -WindowStyle Hidden -PassThru -Wait\nexit $p.ExitCode",
            psq(file)
        )
    } else {
        format!("Start-Process -FilePath {}{arg_part} -Verb RunAs", psq(file))
    };
    run_powershell(&script, None).is_ok()
}

fn run_task_script(extra: &[&str]) -> bool {
    let mut args: Vec<String> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"].iter().map(|s| s.to_string()).collect();
    args.push(task_script_path().to_string_lossy().to_string());
    args.extend(extra.iter().map(|s| s.to_string()));
    run_elevated("powershell.exe", &args, true)
}

/// 为 exe 注册任务并启动（一次 UAC 弹窗）
pub fn register_and_run_task(exe: &Path) -> bool {
    run_task_script(&["-ExePath", &exe.to_string_lossy(), "-Run"])
}

/// 删除任务（一次 UAC 弹窗）
pub fn unregister_task() -> bool {
    run_task_script(&["-Unregister"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert!(same_path("\"C:\\Program Files\\AppTraffic\\traffic-etw.exe\"", Path::new("c:/program files/apptraffic/TRAFFIC-ETW.exe")));
        assert_eq!(psq("a'b"), "'a''b'");
    }
}
