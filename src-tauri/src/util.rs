//! 小工具：本地日期（不引 chrono）、毫秒时间、隐藏窗口的子进程。

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 数据目录（traffic.json / settings.json），沿用 Electron 版的 app-traffic，历史记录直接接上
pub fn init_data_dir(base: PathBuf) {
    // APPTRAFFIC_DATA：测试时指向别处，免得和装着的版本抢同一份历史
    let dir = std::env::var_os("APPTRAFFIC_DATA").map(PathBuf::from).unwrap_or_else(|| base.join("app-traffic"));
    let _ = DATA_DIR.set(dir);
}

pub fn data_dir() -> PathBuf {
    DATA_DIR.get().cloned().unwrap_or_else(|| std::env::temp_dir().join("app-traffic"))
}

/// 本地日期 (年, 月, 日)
pub fn local_ymd() -> (i32, u32, u32) {
    #[cfg(windows)]
    {
        let mut t = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
        (t.wYear as i32, t.wMonth as u32, t.wDay as u32)
    }
    #[cfg(not(windows))]
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm = std::mem::zeroed::<libc::tm>();
        libc::localtime_r(&now, &mut tm);
        (tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32)
    }
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = (if m <= 2 { y - 1 } else { y }) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    ((y + if m <= 2 { 1 } else { 0 }) as i32, m, d)
}

fn key(ymd: (i32, u32, u32)) -> String {
    format!("{:04}-{:02}-{:02}", ymd.0, ymd.1, ymd.2)
}

/// 今天的 YYYY-MM-DD
pub fn day_key() -> String {
    key(local_ymd())
}

/// 今天往前 n 天的 YYYY-MM-DD
pub fn day_key_back(n: i64) -> String {
    let (y, m, d) = local_ymd();
    key(civil_from_days(days_from_civil(y, m, d) - n))
}

/// 不弹黑窗的子进程（Windows）
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip() {
        for (y, m, d) in [(2026, 10, 29), (2024, 2, 29), (2000, 1, 1), (1999, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(civil_from_days(days_from_civil(2026, 3, 1) - 1), (2026, 2, 28));
    }
}
