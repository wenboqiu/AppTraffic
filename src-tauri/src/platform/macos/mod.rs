mod ifaces;
mod nettop;
mod processes;
mod sysproxy;

use std::sync::Arc;

use crate::types::SystemSource;

pub use processes::{known_endpoints, list_processes, socket_endpoints};
pub use sysproxy::system_proxy_ports;

pub fn create_system_source() -> Arc<dyn SystemSource> {
    Arc::new(nettop::NettopSource::new())
}

/// 在访达里选中这个文件
pub fn reveal(path: &str) {
    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
}
