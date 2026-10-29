//! 前后端共用的数据结构（对应原 src/shared/types.ts）。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// 没经过任何代理 / VPN 的流量
pub const DIRECT: &str = "direct";
/// VPN 客户端（或 TUN 代理内核）自己的承载流量：已经算在各应用名下的字节的加密副本，不计入总量
pub const TUNNEL: &str = "tunnel";
/// 经 VPN 网卡的路线前缀："VPN · wg-home"
pub const VPN_ROUTE_PREFIX: &str = "VPN · ";

/// 全局计数里有、但没有出现在任何已轮询连接上的流量
pub const UNATTRIBUTED: &str = "(短连接/未归属)";
pub const UNKNOWN_PROCESS: &str = "(未识别进程)";
pub const CORE_INNER: &str = "(代理内核 · DNS 等)";

/// 单个进程在某一天里的累计流量（落盘）
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct ProcStat {
    pub path: String,
    pub up: u64,
    pub down: u64,
    pub conns: u64,
    /// 域名 -> 字节（上下行合计），只留前若干
    pub hosts: HashMap<String, u64>,
    /// 路线 -> 字节：DIRECT，或承载这些流量的代理客户端
    pub routes: HashMap<String, u64>,
}

pub type DayData = HashMap<String, ProcStat>;

#[derive(Serialize, Deserialize)]
pub struct StoreFile {
    pub version: u32,
    pub days: BTreeMap<String, DayData>,
}

impl Default for StoreFile {
    fn default() -> Self {
        Self { version: 2, days: BTreeMap::new() }
    }
}

/// 一次采样里某进程的增量
#[derive(Clone, Default, Debug)]
pub struct Delta {
    pub proc: String,
    pub path: String,
    pub up: i64,
    pub down: i64,
    /// up + down 中直连的部分；对账时可能为负，用来把 ETW 提前记成直连的字节挪到代理上
    pub direct: i64,
    /// 承载其余字节的代理客户端（"Clash Verge"…）；没经过代理时为空
    pub route: String,
    pub new_conns: i64,
    /// 域名 -> 字节；可能只覆盖 up + down 的一部分
    pub hosts: Vec<(String, i64)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
    Today,
    Days7,
    Days30,
    All,
}

impl Range {
    pub fn parse(s: &str) -> Range {
        match s {
            "7d" => Range::Days7,
            "30d" => Range::Days30,
            "all" => Range::All,
            _ => Range::Today,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Range::Today => "today",
            Range::Days7 => "7d",
            Range::Days30 => "30d",
            Range::All => "all",
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProcRow {
    pub name: String,
    pub path: String,
    pub up: f64,
    pub down: f64,
    pub direct: f64,
    pub tunnel: f64,
    pub routes: Vec<(String, f64)>,
    pub conns: u64,
    pub live_conns: u64,
    pub speed_up: f64,
    pub speed_down: f64,
    pub top_hosts: Vec<(String, f64)>,
}

#[derive(Serialize, Clone)]
pub struct SourceStatus {
    pub id: String,
    pub label: String,
    /// on：在出数据；off：没运行 / 没检测到（正常）；error：需要处理
    pub state: &'static str,
    pub detail: String,
    /// 能把这个来源打开 / 关闭的按钮文字
    pub action: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub range: &'static str,
    pub sources: Vec<SourceStatus>,
    pub total_up: f64,
    pub total_down: f64,
    pub total_direct: f64,
    pub total_tunnel: f64,
    pub routes: Vec<(String, f64)>,
    pub speed_up: f64,
    pub speed_down: f64,
    pub procs: Vec<ProcRow>,
    pub since: Option<String>,
}

// ── 数据源之间交换的采样 ──

/// Clash 连接到达内核的方式，决定它和 ETW 怎么重叠：
/// proxy：应用 -> 本地 HTTP/SOCKS 口（应用侧是回环，ETW 看不到）；
/// tun：被 TUN 网卡接走（ETW 也在真实应用名下看得到）；
/// inner：内核自己开的连接；gap：内核全局计数里没有任何连接解释的字节
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClashVia {
    Proxy,
    Tun,
    Inner,
    Gap,
}

#[derive(Clone, Debug)]
pub struct ClashDelta {
    pub d: Delta,
    pub via: ClashVia,
    /// mihomo 没能说出是哪个进程开的连接时为 false
    pub known: bool,
}

pub struct ClashSample {
    /// 代理流量的路线名：运行内核的客户端（"Clash Verge"…）
    pub route: String,
    /// 内核 TUN 网卡名（mihomo /configs），没开 TUN 时为 None
    pub tun_device: Option<String>,
    pub deltas: Vec<ClashDelta>,
    /// 每个进程当前打开的连接数
    pub live_conns: HashMap<String, u64>,
}

#[derive(Clone, Deserialize, Default, Debug)]
#[serde(default)]
pub struct SysProc {
    /// 可执行文件路径；打不开进程时为空
    pub path: String,
    pub up: i64,
    pub down: i64,
    pub hosts: Vec<(String, i64)>,
    /// [网卡序号, 上行, 下行]；序号 0 表示本地地址不在任何已知网卡上
    pub ifs: Vec<(i64, i64, i64)>,
}

#[derive(Clone, Deserialize, Default, Debug)]
#[serde(default)]
pub struct SysIface {
    pub index: i64,
    pub alias: String,
    pub desc: String,
    /// 把流量转去别处的虚拟网卡（VPN、TUN 代理、组网软件）
    pub vpn: bool,
}

/// 回环 TCP：client 连到监听 port 的 server 进程
#[derive(Clone, Deserialize, Default, Debug)]
#[serde(default)]
pub struct SysLoop {
    pub client: String,
    pub server: String,
    pub port: u16,
    pub up: i64,
    pub down: i64,
}

#[derive(Clone, Default)]
pub struct SystemSample {
    pub procs: Vec<SysProc>,
    pub loops: Vec<SysLoop>,
    pub ifaces: Vec<SysIface>,
}

impl SystemSample {
    pub fn join(mut self, other: SystemSample) -> SystemSample {
        self.procs.extend(other.procs);
        self.loops.extend(other.loops);
        self.ifaces.extend(other.ifaces);
        self
    }
}

/// 平台的系统级数据源（Windows：ETW 采集程序；macOS：nettop），可能要用户先打开
pub trait SystemSource: Send + Sync {
    fn id(&self) -> &'static str {
        "system"
    }
    /// 拿不到数据时返回 None
    fn poll(&self) -> Option<SystemSample>;
    fn reset(&self);
    fn status(&self) -> SourceStatus;
    /// 启动时：用户之前开过就静默恢复
    fn auto_start(&self);
    /// 状态栏上的按钮（启用 / 停用）
    fn toggle(&self);
}
