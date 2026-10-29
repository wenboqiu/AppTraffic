//! 用户偏好，落盘到 data_dir/settings.json（字段名沿用 Electron 版）。

use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

use crate::util::data_dir;

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// 用户开过系统级统计：启动时拉起 ETW 采集程序
    pub etw_enabled: bool,
    /// macOS：nettop 不需要授权，默认开
    pub nettop_enabled: bool,
    /// 匿名使用统计（见 analytics.rs），托盘菜单可关
    pub analytics_enabled: bool,
    pub install_id: Option<String>,
    pub install_reported: bool,
    pub last_active_day: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            etw_enabled: false,
            nettop_enabled: true,
            analytics_enabled: true,
            install_id: None,
            install_reported: false,
            last_active_day: None,
        }
    }
}

static CACHE: OnceLock<Mutex<Settings>> = OnceLock::new();

fn cache() -> &'static Mutex<Settings> {
    CACHE.get_or_init(|| {
        let s = std::fs::read_to_string(data_dir().join("settings.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Mutex::new(s)
    })
}

pub fn get() -> Settings {
    cache().lock().unwrap().clone()
}

pub fn update(f: impl FnOnce(&mut Settings)) {
    let mut s = cache().lock().unwrap();
    f(&mut s);
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(text) = serde_json::to_string_pretty(&*s) {
        let _ = std::fs::write(dir.join("settings.json"), text);
    }
}
