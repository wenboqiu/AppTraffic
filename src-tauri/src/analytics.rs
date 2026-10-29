//! 匿名使用统计：自建 Umami，直接调 /api/send。
//! 约定见 D:\Play\docs\umami-integration-guide.md：固定 ip / UA、安装 ID、启动和每日首次活跃补发页面浏览、
//! 失败静默丢弃、开发模式默认不发。只发随机安装 ID、版本号和系统，从不发流量数字、域名或应用名。

use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tauri::AppHandle;

use crate::settings;
use crate::util::day_key;

const ENDPOINT: &str = "https://umami.wondergarden.top/api/send";
const WEBSITE_ID: &str = "d2ec23fe-003a-4689-9ef0-6f2cd5a0d3d9";
const HOSTNAME: &str = "apptraffic.app";
const ACTIVE_CHECK: Duration = Duration::from_secs(60 * 60);

// 固定、不含版本号：访客只由安装 ID 决定
const FIXED_IP: &str = "127.0.0.1";
#[cfg(target_os = "macos")]
const CLIENT: (&str, &str) = ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppTraffic", "Mac OS");
#[cfg(not(target_os = "macos"))]
const CLIENT: (&str, &str) = ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppTraffic", "Windows 10");

struct Ctx {
    screen: String,
    language: String,
    version: String,
}

static CTX: OnceLock<Arc<Ctx>> = OnceLock::new();

/// 开发模式默认不发，设 APPTRAFFIC_ANALYTICS=1 才发
fn allowed_build() -> bool {
    !cfg!(debug_assertions) || std::env::var("APPTRAFFIC_ANALYTICS").as_deref() == Ok("1")
}

fn active() -> Option<Arc<Ctx>> {
    let c = CTX.get()?;
    (allowed_build() && settings::get().analytics_enabled).then(|| c.clone())
}

pub fn is_enabled() -> bool {
    settings::get().analytics_enabled
}

/// 随机 UUID v4（不为它单独引依赖：系统时间 + 进程 ID 混入 RandomState）
fn new_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut b = [0u8; 16];
    for (i, chunk) in b.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let x: String = b.iter().map(|v| format!("{v:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

fn install_id() -> String {
    if let Some(id) = settings::get().install_id {
        return id;
    }
    let id = new_uuid();
    settings::update(|s| s.install_id = Some(id.clone()));
    id
}

/// name 为 None 是页面浏览（Umami 概览只统计至少有一次页面浏览的访客）；在后台线程发，失败直接丢弃
fn send(c: &Arc<Ctx>, name: Option<&str>) {
    let mut payload = json!({
        "website": WEBSITE_ID,
        "hostname": HOSTNAME,
        "url": "/app",
        "title": "AppTraffic",
        "language": c.language,
        "screen": c.screen,
        "id": install_id(),
        "ip": FIXED_IP,
        "userAgent": CLIENT.0,
        "os": CLIENT.1,
        "browser": "AppTraffic",
        "device": "desktop",
    });
    if let Some(n) = name {
        payload["name"] = json!(n);
        payload["data"] = json!({ "version": c.version, "os": std::env::consts::OS });
    }
    let body = json!({ "type": "event", "payload": payload }).to_string();
    std::thread::spawn(move || {
        let Ok(tls) = native_tls::TlsConnector::new() else { return };
        let agent = ureq::AgentBuilder::new().tls_connector(Arc::new(tls)).timeout(Duration::from_secs(10)).build();
        let _r = agent
            .post(ENDPOINT)
            .set("Content-Type", "application/json")
            // Umami 会丢掉没有浏览器样 UA 的请求
            .set("User-Agent", CLIENT.0)
            .send_string(&body);
        #[cfg(debug_assertions)]
        eprintln!("[umami] {} -> {:?}", body, _r.map(|r| r.into_string().unwrap_or_default()));
    });
}

/// 每个本地日期只报一次，不管程序开了多久
fn check_active(c: &Arc<Ctx>, with_pageview: bool) {
    let day = day_key();
    if settings::get().last_active_day.as_deref() == Some(&day) {
        return;
    }
    settings::update(|s| s.last_active_day = Some(day));
    if with_pageview {
        send(c, None);
    }
    send(c, Some("app_active"));
}

/// setup 里调用一次
pub fn start(app: &AppHandle) {
    let screen = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| format!("{}x{}", m.size().width, m.size().height))
        .unwrap_or_default();
    let ctx = Arc::new(Ctx {
        screen,
        language: sys_locale::get_locale().unwrap_or_else(|| "zh-CN".into()),
        version: app.package_info().version.to_string(),
    });
    if CTX.set(ctx).is_err() {
        return;
    }

    if let Some(c) = active() {
        if !settings::get().install_reported {
            settings::update(|s| s.install_reported = true);
            send(&c, Some("app_install"));
        }
        send(&c, None);
        send(&c, Some("app_start"));
        check_active(&c, false); // 刚发过页面浏览，不重复发
    }

    // 常驻托盘：每小时看一次是否换了一天
    std::thread::spawn(|| loop {
        std::thread::sleep(ACTIVE_CHECK);
        if let Some(c) = active() {
            check_active(&c, true);
        }
    });
}

/// 托盘菜单「匿名使用统计」开关
pub fn set_enabled(on: bool) {
    settings::update(|s| s.analytics_enabled = on);
    if on {
        if let Some(c) = active() {
            check_active(&c, true);
        }
    }
}
