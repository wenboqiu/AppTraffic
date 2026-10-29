//! 点托盘图标弹出的无边框窗口：贴着托盘图标摆（下边任务栏的上方、上边任务栏或 macOS 菜单栏的下方），失去焦点就隐藏。
//! 窗口启动时建好一次、只做显示 / 隐藏，所以打开是瞬间的，内容高度也已经是上次量好的。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Rect, Size, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};

const WIDTH: f64 = 420.0;
const INITIAL_HEIGHT: f64 = 600.0; // 渲染进程报告内容高度之前先用这个
const MIN_HEIGHT: f64 = 200.0;
/// 点托盘图标会先让弹窗失焦隐藏，紧接着才收到点击；这段时间内的点击不再重新弹出
const REOPEN_GUARD: Duration = Duration::from_millis(250);

struct State {
    visible: bool,
    /// 逻辑像素
    height: f64,
    /// 托盘在屏幕上半部：从那里往下长
    anchor_top: bool,
    /// 窗口左边缘 / 工作区（物理像素）和缩放，resize 时沿用
    x: i32,
    work: (i32, i32, u32, u32),
    scale: f64,
    hid_at: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    visible: false,
    height: INITIAL_HEIGHT,
    anchor_top: false,
    x: 0,
    work: (0, 0, 0, 0),
    scale: 1.0,
    hid_at: None,
});

/// 当前选的时间范围（今天 / 7 天…），推送快照用
static RANGE: Mutex<&'static str> = Mutex::new("today");

pub fn set_range(r: &'static str) {
    *RANGE.lock().unwrap() = r;
}

pub fn range() -> crate::types::Range {
    crate::types::Range::parse(&RANGE.lock().unwrap())
}

pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let w = WebviewWindowBuilder::new(app, "popup", WebviewUrl::App("index.html".into()))
        .title("AppTraffic")
        .inner_size(WIDTH, INITIAL_HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        .visible(false)
        .focused(false)
        .build()?;
    let handle = app.clone();
    w.on_window_event(move |e| {
        if let WindowEvent::Focused(false) = e {
            hide(&handle);
        }
    });
    Ok(w)
}

pub fn is_visible() -> bool {
    STATE.lock().unwrap().visible
}

pub fn hide(app: &AppHandle) {
    {
        let mut s = STATE.lock().unwrap();
        if !s.visible {
            return;
        }
        s.visible = false;
        s.hid_at = Some(Instant::now());
    }
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.hide();
    }
}

pub fn toggle(app: &AppHandle, tray_rect: Option<Rect>) {
    {
        let s = STATE.lock().unwrap();
        if s.visible {
            drop(s);
            hide(app);
            return;
        }
        if s.hid_at.is_some_and(|t| t.elapsed() < REOPEN_GUARD) {
            return;
        }
    }
    show(app, tray_rect);
}

fn physical(p: &Position, scale: f64) -> (f64, f64) {
    match p {
        Position::Physical(p) => (p.x as f64, p.y as f64),
        Position::Logical(p) => (p.x * scale, p.y * scale),
    }
}

fn physical_size(s: &Size, scale: f64) -> (f64, f64) {
    match s {
        Size::Physical(s) => (s.width as f64, s.height as f64),
        Size::Logical(s) => (s.width * scale, s.height * scale),
    }
}

fn show(app: &AppHandle, tray_rect: Option<Rect>) {
    let Some(w) = app.get_webview_window("popup") else { return };
    let primary_scale = app.primary_monitor().ok().flatten().map_or(1.0, |m| m.scale_factor());

    // 托盘图标的物理矩形；不知道位置（比如第二次启动唤起）时用主屏右下角
    let (tx, ty, tw, th, has_rect) = match &tray_rect {
        Some(r) => {
            let (x, y) = physical(&r.position, primary_scale);
            let (w, h) = physical_size(&r.size, primary_scale);
            (x, y, w, h, true)
        }
        None => (f64::MAX, f64::MAX, 0.0, 0.0, false),
    };
    let monitor = if has_rect {
        app.monitor_from_point(tx, ty).ok().flatten()
    } else {
        None
    }
    .or_else(|| app.primary_monitor().ok().flatten());
    let Some(m) = monitor else { return };
    let scale = m.scale_factor();
    let wa = m.work_area();
    let (wx, wy, ww, wh) = (wa.position.x, wa.position.y, wa.size.width, wa.size.height);

    let mut s = STATE.lock().unwrap();
    s.scale = scale;
    s.work = (wx, wy, ww, wh);
    s.anchor_top = has_rect && ty < m.position().y as f64 + m.size().height as f64 / 2.0;
    let width = (WIDTH * scale).round() as i32;
    // 以托盘图标为中心，再夹在工作区内
    let center = if has_rect { tx + tw / 2.0 } else { (wx + ww as i32) as f64 - width as f64 / 2.0 - 12.0 * scale };
    let _ = th;
    s.x = ((center - width as f64 / 2.0).round() as i32).clamp(wx, (wx + ww as i32 - width).max(wx));
    s.height = s.height.clamp(MIN_HEIGHT, (wh as f64 / scale - 8.0).max(MIN_HEIGHT));
    s.visible = true;
    let (x, y, size) = layout(&s);
    drop(s);

    let _ = w.set_size(size);
    let _ = w.set_position(PhysicalPosition::new(x, y));
    let _ = w.show();
    let _ = w.set_focus();
    let _ = app.emit_to("popup", "snapshot", crate::collector::get().snapshot(range()));
}

/// 当前状态下窗口该在的位置和大小（物理像素）
fn layout(s: &State) -> (i32, i32, PhysicalSize<u32>) {
    let h = (s.height * s.scale).round() as i32;
    let gap = (4.0 * s.scale).round() as i32;
    let (_, wy, _, wh) = s.work;
    let y = if s.anchor_top { wy + gap } else { wy + wh as i32 - h - gap };
    (s.x, y, PhysicalSize::new((WIDTH * s.scale).round() as u32, h as u32))
}

/// 内容高度变了：窗口跟着变，贴着托盘那一侧不动
pub fn set_height(app: &AppHandle, height: f64) {
    let mut s = STATE.lock().unwrap();
    let max = if s.work.3 > 0 { s.work.3 as f64 / s.scale - 8.0 } else { f64::MAX };
    let h = height.round().clamp(MIN_HEIGHT, max.max(MIN_HEIGHT));
    if (h - s.height).abs() < 0.5 {
        return;
    }
    s.height = h;
    if !s.visible {
        return;
    }
    let (x, y, size) = layout(&s);
    drop(s);
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.set_size(size);
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
}
