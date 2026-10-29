//! Clash 兼容 REST API（mihomo / Clash Meta、sing-box 的 clash_api）的最小客户端，
//! 走本地 socket（Windows 命名管道 / Unix socket）或 HTTP。端点由 discovery.rs 找。
//! 不引 HTTP 库：请求只有 GET，手写 HTTP/1.0 即可，服务端也就不会分块。

use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::discovery;

/// 没有可用控制接口时，最多隔这么久重新发现一次
const REDISCOVER: Duration = Duration::from_secs(10);
/// TUN 设置很少变，隔这么久再读一次
const CONFIG_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum Target {
    Socket(String),
    Http { host: String, port: u16 },
}

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub target: Target,
    pub secret: String,
    /// 运行内核的 GUI 客户端，如 "Clash Verge"
    pub client: String,
}

impl Endpoint {
    pub fn socket(path: &str, secret: &str, client: &str) -> Endpoint {
        Endpoint { target: Target::Socket(path.into()), secret: secret.into(), client: client.into() }
    }

    pub fn http(host: &str, port: u16, secret: &str, client: &str) -> Endpoint {
        Endpoint { target: Target::Http { host: host.into(), port }, secret: secret.into(), client: client.into() }
    }

    pub fn label(&self) -> String {
        match &self.target {
            Target::Socket(p) => p.clone(),
            Target::Http { host, port } => format!("http://{host}:{port}"),
        }
    }

    /// 去重用
    pub fn key(&self) -> String {
        match &self.target {
            Target::Socket(p) => p.to_lowercase(),
            Target::Http { host, port } => format!("{host}:{port}"),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Meta {
    /// mihomo："HTTP"、"Tun"、"Inner"…；sing-box："<入站类型>/<tag>"
    #[serde(rename = "type")]
    pub kind: String,
    pub host: String,
    #[serde(rename = "destinationIP")]
    pub destination_ip: String,
    /// 仅 mihomo；sing-box 只给 processPath
    pub process: Option<String>,
    pub process_path: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Connection {
    pub id: String,
    pub upload: u64,
    pub download: u64,
    pub chains: Vec<String>,
    pub metadata: Meta,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ConnectionsResponse {
    pub upload_total: u64,
    pub download_total: u64,
    pub connections: Option<Vec<Connection>>,
}

fn http_get_blocking(ep: &Endpoint, path: &str) -> Result<Vec<u8>, String> {
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: localhost\r\nAccept: application/json\r\n{}Connection: close\r\n\r\n",
        if ep.secret.is_empty() { String::new() } else { format!("Authorization: Bearer {}\r\n", ep.secret) }
    );
    let mut raw = Vec::new();
    match &ep.target {
        Target::Http { host, port } => {
            let mut s = std::net::TcpStream::connect((host.as_str(), *port)).map_err(|e| e.to_string())?;
            s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
            s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        }
        #[cfg(windows)]
        Target::Socket(p) => {
            let mut f = std::fs::OpenOptions::new().read(true).write(true).open(p).map_err(|e| e.to_string())?;
            f.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
            f.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        Target::Socket(p) => {
            let mut s = std::os::unix::net::UnixStream::connect(p).map_err(|e| e.to_string())?;
            s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
            s.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        }
    }
    parse_response(&raw)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 取出 200 响应的正文（兼容服务端仍然分块的情况）
fn parse_response(raw: &[u8]) -> Result<Vec<u8>, String> {
    let split = find(raw, b"\r\n\r\n").ok_or("bad response")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
    if status != "200" {
        return Err(format!("HTTP {status}"));
    }
    let body = &raw[split + 4..];
    if !head.to_lowercase().contains("transfer-encoding: chunked") {
        return Ok(body.to_vec());
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let Some(nl) = find(rest, b"\r\n") else { break };
        let size = usize::from_str_radix(String::from_utf8_lossy(&rest[..nl]).split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "bad chunk")?;
        rest = &rest[nl + 2..];
        if size == 0 || rest.len() < size {
            break;
        }
        out.extend_from_slice(&rest[..size]);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
    Ok(out)
}

/// 命名管道读不能设超时，所以放到线程里，超时就放弃它
pub fn request<T: for<'de> Deserialize<'de> + Send + 'static>(
    ep: &Endpoint,
    path: &str,
    timeout: Duration,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    let (ep, path) = (ep.clone(), path.to_string());
    std::thread::spawn(move || {
        let r = http_get_blocking(&ep, &path)
            .and_then(|b| serde_json::from_slice::<T>(&b).map_err(|e| e.to_string()));
        let _ = tx.send(r);
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| Err("timeout".into()))
}

pub const NOT_DETECTED: &str = "未检测到 Clash 兼容的代理";

#[derive(Default)]
struct State {
    endpoint: Option<Endpoint>,
    version: Option<String>,
    last_discovery: Option<Instant>,
    discovering: bool,
    tun: Option<(Instant, Option<String>)>,
}

#[derive(Clone, Default)]
pub struct MihomoClient {
    st: Arc<Mutex<State>>,
}

impl MihomoClient {
    pub fn client(&self) -> Option<String> {
        self.st.lock().unwrap().endpoint.as_ref().map(|e| e.client.clone())
    }

    pub fn version(&self) -> Option<String> {
        self.st.lock().unwrap().version.clone()
    }

    /// 正在找，且还没找到
    pub fn searching(&self) -> bool {
        let s = self.st.lock().unwrap();
        s.discovering && s.endpoint.is_none()
    }

    /// 发现要起 PowerShell，可能一秒多，放到后台，不拖住 tick
    fn kick_discovery(&self) {
        {
            let mut s = self.st.lock().unwrap();
            if s.discovering || s.last_discovery.is_some_and(|t| t.elapsed() < REDISCOVER) {
                return;
            }
            s.last_discovery = Some(Instant::now());
            s.discovering = true;
        }
        let st = self.st.clone();
        std::thread::spawn(move || {
            let found = discovery::discover();
            let mut s = st.lock().unwrap();
            s.discovering = false;
            if let Some((ep, version)) = found {
                s.endpoint = Some(ep);
                s.version = Some(version);
            }
        });
    }

    /// 内核 TUN 网卡名；没开 TUN 或不知道（sing-box）时为 None
    pub fn tun_device(&self) -> Option<String> {
        let ep = {
            let s = self.st.lock().unwrap();
            if let Some((at, dev)) = &s.tun {
                if at.elapsed() < CONFIG_TTL {
                    return dev.clone();
                }
            }
            s.endpoint.clone()?
        };
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Tun {
            enable: bool,
            device: String,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Cfg {
            tun: Tun,
        }
        let mut s = self.st.lock().unwrap();
        let prev = s.tun.as_ref().and_then(|t| t.1.clone());
        s.tun = Some((Instant::now(), prev));
        drop(s);
        if let Ok(cfg) = request::<Cfg>(&ep, "/configs", Duration::from_secs(3)) {
            let dev = (cfg.tun.enable && !cfg.tun.device.is_empty()).then_some(cfg.tun.device);
            self.st.lock().unwrap().tun = Some((Instant::now(), dev.clone()));
            return dev;
        }
        self.st.lock().unwrap().tun.as_ref().and_then(|t| t.1.clone())
    }

    pub fn get_connections(&self) -> Result<ConnectionsResponse, String> {
        let ep = self.st.lock().unwrap().endpoint.clone();
        let Some(ep) = ep else {
            self.kick_discovery();
            return Err(NOT_DETECTED.into());
        };
        match request::<ConnectionsResponse>(&ep, "/connections", Duration::from_secs(3)) {
            Ok(r) => Ok(r),
            Err(e) => {
                // 丢掉端点，下次轮询重新发现（内核重启、端口变了、换了客户端…）
                let mut s = self.st.lock().unwrap();
                s.endpoint = None;
                s.version = None;
                s.last_discovery = None;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response() {
        let raw = b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"a\":1}";
        assert_eq!(parse_response(raw).unwrap(), b"{\"a\":1}");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n4\r\n\":1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).unwrap(), b"{\"a\":1}");
        assert!(parse_response(b"HTTP/1.1 401 Unauthorized\r\n\r\n").is_err());
    }
}
