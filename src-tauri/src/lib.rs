mod analytics;
mod clash;
mod collector;
mod controller_config;
mod cores;
mod discovery;
mod merge;
mod mihomo;
mod platform;
mod popup;
mod settings;
mod store;
mod types;
mod util;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use types::{Range, Snapshot};

const TRAY_ICON: &[u8] = include_bytes!("../../resources/tray-icon.png");

fn fmt_bytes(b: f64) -> String {
    const K: f64 = 1024.0;
    if b >= K * K * K {
        format!("{:.2} GB", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.1} MB", b / (K * K))
    } else if b >= K {
        format!("{:.0} KB", b / K)
    } else {
        format!("{} B", b.round())
    }
}

fn fmt_speed(bps: f64) -> String {
    format!("{}/s", fmt_bytes(bps))
}

#[tauri::command]
fn get_snapshot(range: String) -> Snapshot {
    let r = Range::parse(&range);
    popup::set_range(r.as_str());
    collector::get().snapshot(r)
}

#[tauri::command]
fn source_action(id: String) {
    collector::get().run_source_action(&id);
}

#[tauri::command]
fn show_in_folder(path: String) {
    if !path.is_empty() {
        platform::reveal(&path);
    }
}

#[tauri::command]
fn open_data_folder() {
    platform::reveal(&store::store_path().to_string_lossy());
}

#[tauri::command]
fn close_popup(app: AppHandle) {
    popup::hide(&app);
}

#[tauri::command]
fn resize_popup(app: AppHandle, height: f64) {
    popup::set_height(&app, height);
}

/// 弹窗开着时才推快照，空闲时开销接近 0
fn push_snapshot(app: &AppHandle) {
    if popup::is_visible() {
        let _ = app.emit_to("popup", "snapshot", collector::get().snapshot(popup::range()));
    }
}

fn update_tooltip(app: &AppHandle, last: &mut String) {
    let Some(tray) = app.tray_by_id("main") else { return };
    let s = collector::get().snapshot(Range::Today);
    let text = if s.sources.iter().any(|x| x.state == "on") {
        format!(
            "AppTraffic\n今日 ↓{} ↑{}\n实时 ↓{} ↑{}",
            fmt_bytes(s.total_down),
            fmt_bytes(s.total_up),
            fmt_speed(s.speed_down),
            fmt_speed(s.speed_up)
        )
    } else {
        "AppTraffic\n未在统计：系统流量未启用，也未检测到代理".to_string()
    };
    if text != *last {
        let _ = tray.set_tooltip(Some(&text));
        *last = text;
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let open_i = MenuItem::with_id(app, "open", "打开 AppTraffic", true, None::<&str>)?;
    let auto_i = CheckMenuItem::with_id(app, "autostart", "开机自启", true, autostart_on, None::<&str>)?;
    let stats_i = CheckMenuItem::with_id(app, "analytics", "匿名使用统计", true, analytics::is_enabled(), None::<&str>)?;
    let data_i = MenuItem::with_id(app, "data", "打开数据目录", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open_i, &auto_i, &stats_i, &data_i, &PredefinedMenuItem::separator(app)?, &quit_i],
    )?;

    let (auto_c, stats_c) = (auto_i.clone(), stats_i.clone());
    TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(TRAY_ICON)?)
        .tooltip("AppTraffic")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, e| match e.id.as_ref() {
            "open" => {
                if !popup::is_visible() {
                    popup::toggle(app, app.tray_by_id("main").and_then(|t| t.rect().ok().flatten()));
                }
            }
            "autostart" => {
                let al = app.autolaunch();
                let _ = if al.is_enabled().unwrap_or(false) { al.disable() } else { al.enable() };
                let _ = auto_c.set_checked(al.is_enabled().unwrap_or(false));
            }
            "analytics" => {
                analytics::set_enabled(!analytics::is_enabled());
                let _ = stats_c.set_checked(analytics::is_enabled());
            }
            "data" => platform::reveal(&store::store_path().to_string_lossy()),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = e {
                popup::toggle(tray.app_handle(), Some(rect));
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if !popup::is_visible() {
                popup::toggle(app, app.tray_by_id("main").and_then(|t| t.rect().ok().flatten()));
            }
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            source_action,
            show_in_folder,
            open_data_folder,
            close_popup,
            resize_popup
        ])
        .setup(|app| {
            // 菜单栏应用：不要 Dock 图标
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            if let Ok(dir) = app.path().data_dir() {
                util::init_data_dir(dir);
            }
            store::load();

            let handle = app.handle().clone();
            popup::create(&handle)?;
            build_tray(&handle)?;

            let c = collector::get();
            let (h, last) = (handle.clone(), std::sync::Mutex::new(String::new()));
            c.on_tick(move || {
                push_snapshot(&h);
                update_tooltip(&h, &mut last.lock().unwrap());
            });
            c.start();
            analytics::start(&handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build app");

    app.run(|_app, e| match e {
        // 只有托盘：窗口都关了也不退出，只有菜单里的「退出」才退
        RunEvent::ExitRequested { api, code, .. } if code.is_none() => api.prevent_exit(),
        // 退出前把内存里的计数落盘
        RunEvent::Exit => collector::get().stop(),
        _ => {}
    });
}
