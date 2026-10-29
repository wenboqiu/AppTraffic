//! 以 traffic-etw.exe 为数据源：一个提权的采集程序，用 ETW 数每个进程的 TCP/UDP 字节，
//! 经命名管道每秒推一次增量。采集程序在最后一个客户端断开约 30 秒后自己退出，
//! 所以这边只管启动它（见 elevate.rs）并一直读。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::windows::io::AsRawHandle;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::elevate::*;
use crate::settings;
use crate::types::*;

const PIPE: &str = r"\\.\pipe\apptraffic-etw";
/// 这么久没收到任何一行（连 2 秒一次的保活都没有）就当采集程序没了
const SILENCE: Duration = Duration::from_secs(6);
/// 启动后「正在启动」显示多久才算失败
const START_GRACE: Duration = Duration::from_secs(15);

#[derive(Deserialize, Default)]
#[serde(default)]
struct Line {
    procs: Vec<SysProc>,
    loops: Vec<SysLoop>,
    ifaces: Vec<SysIface>,
}

#[derive(Default)]
struct Inner {
    /// 当前管道连接的代数；读线程发现自己过期就退出
    generation: u64,
    connected: bool,
    ready: bool,
    last_line: Option<Instant>,
    handle: Option<isize>,
    procs: HashMap<String, SysProc>,
    loops: HashMap<String, SysLoop>,
    /// 见过的每块网卡的最新描述（序号 -> 信息）
    ifaces: HashMap<i64, SysIface>,
    /// 用户要系统级统计（持久化）
    enabled: bool,
    /// 在等 UAC 或计划任务启动
    busy: bool,
    /// UAC 窗口在（或即将在）屏幕上
    prompting: bool,
    launched_at: Option<Instant>,
    last_error: Option<String>,
}

pub struct EtwSource {
    inner: Arc<Mutex<Inner>>,
}

impl EtwSource {
    pub fn new() -> Self {
        let inner = Inner { enabled: settings::get().etw_enabled, ..Default::default() };
        Self { inner: Arc::new(Mutex::new(inner)) }
    }
}

fn drop_conn(s: &mut Inner) {
    if let Some(h) = s.handle.take() {
        // 读线程可能正阻塞在管道上，取消它的读
        unsafe { windows_sys::Win32::System::IO::CancelIoEx(h as _, std::ptr::null()) };
    }
    s.generation += 1;
    s.connected = false;
    s.ready = false;
}

fn connect(this: &Arc<Mutex<Inner>>, s: &mut Inner) {
    if s.connected {
        return;
    }
    // 管道不存在只说明采集程序没在跑，下个 tick 再试
    let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(PIPE) else { return };
    s.generation += 1;
    s.connected = true;
    s.handle = Some(file.as_raw_handle() as isize);
    let (gen, inner) = (s.generation, this.clone());
    std::thread::spawn(move || read_loop(inner, gen, file));
}

fn read_loop(inner: Arc<Mutex<Inner>>, gen: u64, file: File) {
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(line) = serde_json::from_slice::<Line>(&buf) else { continue };
        let mut s = inner.lock().unwrap();
        if s.generation != gen {
            return;
        }
        s.ready = true;
        s.last_line = Some(Instant::now());
        for p in line.procs {
            add_proc(&mut s, p);
        }
        for l in line.loops {
            add_loop(&mut s, l);
        }
        for f in line.ifaces {
            s.ifaces.insert(f.index, f);
        }
    }
    // 句柄在文件关闭前先从状态里摘掉，免得别人对着失效的句柄调 CancelIoEx
    let mut s = inner.lock().unwrap();
    if s.generation == gen {
        s.handle = None;
        drop_conn(&mut s);
    }
}

fn add_proc(s: &mut Inner, p: SysProc) {
    match s.procs.get_mut(&p.path) {
        None => {
            s.procs.insert(p.path.clone(), p);
        }
        Some(acc) => {
            acc.up += p.up;
            acc.down += p.down;
            acc.hosts.extend(p.hosts);
            for (index, up, down) in p.ifs {
                match acc.ifs.iter_mut().find(|x| x.0 == index) {
                    Some(f) => {
                        f.1 += up;
                        f.2 += down;
                    }
                    None => acc.ifs.push((index, up, down)),
                }
            }
        }
    }
}

fn add_loop(s: &mut Inner, l: SysLoop) {
    let key = format!("{}\0{}\0{}", l.client, l.server, l.port);
    match s.loops.get_mut(&key) {
        None => {
            s.loops.insert(key, l);
        }
        Some(acc) => {
            acc.up += l.up;
            acc.down += l.down;
        }
    }
}

fn starting(s: &Inner) -> bool {
    s.busy || (s.enabled && s.launched_at.is_some_and(|t| t.elapsed() < START_GRACE))
}

/// 启动采集程序：计划任务存在且指向它就静默启动；否则（仅交互时）弹一次 UAC 注册任务，
/// 未受保护的路径则直接提权运行。调用前 busy 已置位。
fn launch(inner: &Arc<Mutex<Inner>>, interactive: bool) {
    let exe = helper_path();
    let finish = |ok: bool| {
        let mut s = inner.lock().unwrap();
        if ok {
            s.enabled = true;
            s.launched_at = Some(Instant::now());
            settings::update(|st| st.etw_enabled = true);
        } else if interactive {
            s.last_error = Some("未获得管理员授权，系统流量统计未启动".into());
        }
        s.busy = false;
        s.prompting = false;
    };
    if !exe.exists() {
        let mut s = inner.lock().unwrap();
        s.last_error = Some(format!("找不到 {HELPER}"));
        s.busy = false;
        return;
    }
    inner.lock().unwrap().last_error = None;

    let ok = if is_protected_path(&exe) {
        let mut ok = task_executable().is_some_and(|c| same_path(&c, &exe)) && run_task();
        // 没有任务，或是装在别处时留下的：（重新）注册
        if !ok && interactive {
            inner.lock().unwrap().prompting = true;
            ok = register_and_run_task(&exe);
        }
        ok
    } else if interactive {
        inner.lock().unwrap().prompting = true;
        run_elevated(&exe.to_string_lossy(), &[], false)
    } else {
        false
    };
    finish(ok);
}

/// 停止采集并删除计划任务（任务存在时要一次 UAC 弹窗）。调用前 busy 已置位。
fn disable(inner: &Arc<Mutex<Inner>>) {
    {
        let mut s = inner.lock().unwrap();
        s.enabled = false;
        settings::update(|st| st.etw_enabled = false);
        // 没人连着时采集程序自己会退出
        drop_conn(&mut s);
        s.procs.clear();
        s.loops.clear();
    }
    if task_executable().is_some() {
        inner.lock().unwrap().prompting = true;
        unregister_task();
    }
    let mut s = inner.lock().unwrap();
    s.busy = false;
    s.prompting = false;
}

impl SystemSource for EtwSource {
    fn poll(&self) -> Option<SystemSample> {
        let mut s = self.inner.lock().unwrap();
        if s.ready && s.last_line.is_none_or(|t| t.elapsed() > SILENCE) {
            drop_conn(&mut s);
        }
        if !s.ready {
            if s.enabled {
                connect(&self.inner, &mut s);
            }
            return None;
        }
        let sample = SystemSample {
            procs: s.procs.values().cloned().collect(),
            loops: s.loops.values().cloned().collect(),
            ifaces: s.ifaces.values().cloned().collect(),
        };
        s.procs.clear();
        s.loops.clear();
        Some(sample)
    }

    fn reset(&self) {
        let mut s = self.inner.lock().unwrap();
        s.procs.clear();
        s.loops.clear();
    }

    fn auto_start(&self) {
        {
            let mut s = self.inner.lock().unwrap();
            if !s.enabled || s.busy {
                return;
            }
            s.busy = true;
        }
        let inner = self.inner.clone();
        std::thread::spawn(move || launch(&inner, false));
    }

    fn toggle(&self) {
        let disabling = {
            let mut s = self.inner.lock().unwrap();
            if s.busy {
                return;
            }
            let disabling = s.enabled && (s.ready || starting(&s));
            s.busy = true;
            disabling
        };
        let inner = self.inner.clone();
        std::thread::spawn(move || if disabling { disable(&inner) } else { launch(&inner, true) });
    }

    fn status(&self) -> SourceStatus {
        let s = self.inner.lock().unwrap();
        let mk = |state, detail: &str, action: Option<&str>| SourceStatus {
            id: "system".into(),
            label: "系统流量".into(),
            state,
            detail: detail.into(),
            action: action.map(String::from),
        };
        if s.ready {
            return mk("on", "已启用", Some("停用"));
        }
        if s.prompting {
            return mk("off", "请在管理员授权窗口中确认…", None);
        }
        if starting(&s) {
            return mk("off", "正在启动…", None);
        }
        if let Some(e) = &s.last_error {
            return mk("error", e, Some("启用"));
        }
        if s.enabled {
            return mk("error", "采集程序未运行", Some("启用"));
        }
        mk("off", "未启用：按应用统计全部流量需要管理员授权", Some("启用"))
    }
}
