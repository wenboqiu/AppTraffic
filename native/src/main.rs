//! traffic-etw: per-process network byte counter for AppTraffic.
//!
//! Runs elevated (ETW real-time sessions need admin), subscribes to
//! Microsoft-Windows-Kernel-Network for TCP/UDP send/recv sizes and to
//! Microsoft-Windows-DNS-Client to map remote IPs back to host names.
//! Every second it pushes one JSON line of per-executable deltas to whoever is
//! connected on the named pipe; it exits on its own once nobody has been
//! connected for a while, so the tray app controls its lifetime.

#![windows_subsystem = "windows"]

use std::collections::HashMap;
use std::ffi::c_void;
use std::io::Write as _;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ferrisetw::parser::Parser;
use ferrisetw::provider::Provider;
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::{stop_trace_by_name, UserTrace};
use ferrisetw::EventRecord;
use serde_json::json;

const SESSION: &str = "AppTrafficEtw";
const PIPE_NAME: &str = r"\\.\pipe\apptraffic-etw";
const KERNEL_NETWORK: &str = "7dd42a49-5329-4832-8dfd-43d979153a88";
const DNS_CLIENT: &str = "1c95126e-7eea-49a9-a3fe-a378b03ddb4d";
/// KERNEL_NETWORK_KEYWORD_IPV4 | KERNEL_NETWORK_KEYWORD_IPV6
const NET_KEYWORDS: u64 = 0x10 | 0x20;
/// Exit when no client has been connected for this long (also the start-up grace period).
const IDLE_EXIT: Duration = Duration::from_secs(30);
const PATH_TTL: Duration = Duration::from_secs(30);
const DNS_CACHE_MAX: usize = 50_000;
const HOSTS_PER_PROC: usize = 20;

#[derive(Default)]
struct PidAgg {
    up: u64,
    down: u64,
    hosts: HashMap<IpAddr, u64>,
}

struct State {
    pending: Mutex<HashMap<u32, PidAgg>>,
    dns: Mutex<HashMap<IpAddr, String>>,
    connected: AtomicBool,
    last_client_ms: AtomicU64,
    probe: Option<Mutex<Probe>>,
}

/// `--probe <file>`: dump raw samples for checking field semantics, then exit.
struct Probe {
    out: std::fs::File,
    net_lines: usize,
    dns_lines: usize,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn is_local_only(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_unspecified(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Fold IPv4-mapped IPv6 addresses back to plain IPv4 so DNS lookups match.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}

fn on_network(state: &State, record: &EventRecord, locator: &SchemaLocator) {
    // TCP send/recv: 10/11 (v4), 26/27 (v6); UDP send/recv: 42/43 (v4), 58/59 (v6)
    let (is_send, is_tcp) = match record.event_id() {
        10 | 26 => (true, true),
        11 | 27 => (false, true),
        42 | 58 => (true, false),
        43 | 59 => (false, false),
        _ => return,
    };
    let Ok(schema) = locator.event_schema(record) else { return };
    let parser = Parser::create(record, &schema);
    let (Ok(pid), Ok(size)) = (parser.try_parse::<u32>("PID"), parser.try_parse::<u32>("size")) else {
        return;
    };
    let daddr: Option<IpAddr> = parser.try_parse("daddr").ok();
    let saddr: Option<IpAddr> = parser.try_parse("saddr").ok();

    if let Some(probe) = &state.probe {
        let mut p = probe.lock().unwrap();
        if p.net_lines < 120 {
            p.net_lines += 1;
            let _ = writeln!(
                p.out,
                "NET id={} pid={} hdrpid={} size={} saddr={:?} sport={:?} daddr={:?} dport={:?}",
                record.event_id(),
                pid,
                record.process_id(),
                size,
                saddr,
                parser.try_parse::<u16>("sport").ok().map(u16::from_be),
                daddr,
                parser.try_parse::<u16>("dport").ok().map(u16::from_be),
            );
        }
    }

    // Loopback hops (app -> local proxy port, IPC over TCP) are not internet traffic.
    if daddr.as_ref().is_some_and(is_local_only) || saddr.as_ref().is_some_and(is_local_only) {
        return;
    }
    // TCP events report the connection's endpoints (saddr = local, daddr = remote)
    // in both directions; UDP events report the datagram's own source/destination.
    // PID is the owning process; the header PID of a receive is often 4 (System).
    let remote = if is_tcp || is_send { daddr } else { saddr };

    let mut pending = state.pending.lock().unwrap();
    let agg = pending.entry(pid).or_default();
    if is_send {
        agg.up += size as u64;
    } else {
        agg.down += size as u64;
    }
    if let Some(ip) = remote {
        *agg.hosts.entry(canonical(ip)).or_default() += size as u64;
    }
}

fn on_dns(state: &State, record: &EventRecord, locator: &SchemaLocator) {
    // 3008 = query completed, carries the answer list
    if record.event_id() != 3008 {
        return;
    }
    let Ok(schema) = locator.event_schema(record) else { return };
    let parser = Parser::create(record, &schema);
    let (Ok(name), Ok(results)) = (parser.try_parse::<String>("QueryName"), parser.try_parse::<String>("QueryResults"))
    else {
        return;
    };
    if name.is_empty() || results.is_empty() {
        return;
    }
    if let Some(probe) = &state.probe {
        let mut p = probe.lock().unwrap();
        if p.dns_lines < 20 {
            p.dns_lines += 1;
            let _ = writeln!(p.out, "DNS {name} => {results}");
        }
    }
    // Results look like "type:  5 cname.example;::ffff:1.2.3.4;2606:...;"
    let mut dns = state.dns.lock().unwrap();
    if dns.len() > DNS_CACHE_MAX {
        dns.clear();
    }
    for part in results.split(';') {
        if let Ok(ip) = part.trim().parse::<IpAddr>() {
            dns.insert(canonical(ip), name.clone());
        }
    }
}

/// Executable path for a PID; empty when the process can't be opened (gone or protected).
fn process_path(pid: u32) -> String {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    match pid {
        0 => return "Idle".into(),
        4 => return "System".into(),
        _ => {}
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

/// Drain the pending per-PID counters into one JSON line grouped by executable.
fn flush(state: &State, paths: &mut HashMap<u32, (String, Instant)>) -> Option<String> {
    let drained: HashMap<u32, PidAgg> = std::mem::take(&mut *state.pending.lock().unwrap());
    if drained.is_empty() {
        return None;
    }
    let dns = state.dns.lock().unwrap();
    let mut by_path: HashMap<String, PidAgg> = HashMap::new();
    let mut host_names: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for (pid, agg) in drained {
        let path = match paths.get(&pid) {
            Some((p, at)) if at.elapsed() < PATH_TTL => p.clone(),
            _ => {
                let p = process_path(pid);
                paths.insert(pid, (p.clone(), Instant::now()));
                p
            }
        };
        let entry = by_path.entry(path.clone()).or_default();
        entry.up += agg.up;
        entry.down += agg.down;
        let names = host_names.entry(path).or_default();
        for (ip, bytes) in agg.hosts {
            let host = dns.get(&ip).cloned().unwrap_or_else(|| ip.to_string());
            *names.entry(host).or_default() += bytes;
        }
    }
    drop(dns);
    if paths.len() > 4096 {
        paths.retain(|_, (_, at)| at.elapsed() < PATH_TTL);
    }

    let procs: Vec<_> = by_path
        .into_iter()
        .map(|(path, agg)| {
            let mut hosts: Vec<(String, u64)> = host_names.remove(&path).unwrap_or_default().into_iter().collect();
            hosts.sort_by(|a, b| b.1.cmp(&a.1));
            hosts.truncate(HOSTS_PER_PROC);
            json!({ "path": path, "up": agg.up, "down": agg.down, "hosts": hosts })
        })
        .collect();
    Some(json!({ "procs": procs }).to_string())
}

// ── Named pipe server ────────────────────────────────────────────────────────

struct PipeHandle(*mut c_void);
unsafe impl Send for PipeHandle {}

fn create_pipe_instance() -> Option<PipeHandle> {
    use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    // SYSTEM, Administrators and interactive users get access; the low mandatory
    // label lets the non-elevated tray app (medium integrity) open a pipe created
    // by this elevated process. Duplex because Node opens pipes read/write.
    let sddl: Vec<u16> = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)S:(ML;;NW;;;LW)\0".encode_utf16().collect();
    let name: Vec<u16> = format!("{PIPE_NAME}\0").encode_utf16().collect();
    unsafe {
        let mut sd: *mut c_void = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            return None;
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let h = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            4096,
            0,
            &sa,
        );
        LocalFree(sd);
        if h == INVALID_HANDLE_VALUE { None } else { Some(PipeHandle(h)) }
    }
}

fn write_all(h: &PipeHandle, data: &[u8]) -> bool {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    let mut off = 0;
    while off < data.len() {
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(h.0, data[off..].as_ptr(), (data.len() - off) as u32, &mut written, std::ptr::null_mut())
        };
        if ok == 0 || written == 0 {
            return false;
        }
        off += written as usize;
    }
    true
}

fn serve_pipe(state: Arc<State>, lines: Receiver<String>) {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GetLastError};
    use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, DisconnectNamedPipe};
    loop {
        let Some(pipe) = create_pipe_instance() else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let ok = unsafe { ConnectNamedPipe(pipe.0, std::ptr::null_mut()) };
        if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            unsafe { CloseHandle(pipe.0) };
            continue;
        }
        // Lines produced while nobody was listening are stale
        while lines.try_recv().is_ok() {}
        state.connected.store(true, Ordering::SeqCst);
        // Greeting doubles as a liveness check for the client
        let mut alive = write_all(&pipe, b"{\"hello\":1}\n");
        while alive {
            match lines.recv_timeout(Duration::from_secs(2)) {
                Ok(line) => alive = write_all(&pipe, format!("{line}\n").as_bytes()),
                // Keep-alive so a vanished client is noticed even when the network is idle
                Err(RecvTimeoutError::Timeout) => alive = write_all(&pipe, b"{}\n"),
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        state.connected.store(false, Ordering::SeqCst);
        state.last_client_ms.store(now_ms(), Ordering::SeqCst);
        unsafe {
            DisconnectNamedPipe(pipe.0);
            CloseHandle(pipe.0);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let probe = args.iter().position(|a| a == "--probe").and_then(|i| args.get(i + 1)).map(|path| {
        Mutex::new(Probe { out: std::fs::File::create(path).expect("probe file"), net_lines: 0, dns_lines: 0 })
    });
    let probing = probe.is_some();

    let state = Arc::new(State {
        pending: Mutex::new(HashMap::new()),
        dns: Mutex::new(HashMap::new()),
        connected: AtomicBool::new(false),
        last_client_ms: AtomicU64::new(now_ms()),
        probe,
    });

    // A previous instance that was killed leaves its session behind
    let _ = stop_trace_by_name(SESSION);

    let s1 = state.clone();
    let net = Provider::by_guid(KERNEL_NETWORK)
        .any(NET_KEYWORDS)
        .add_callback(move |r: &EventRecord, l: &SchemaLocator| on_network(&s1, r, l))
        .build();
    let s2 = state.clone();
    let dns = Provider::by_guid(DNS_CLIENT)
        .add_callback(move |r: &EventRecord, l: &SchemaLocator| on_dns(&s2, r, l))
        .build();
    let trace = match UserTrace::new().named(SESSION.to_string()).enable(net).enable(dns).start_and_process() {
        Ok(t) => t,
        Err(e) => {
            if let Some(p) = &state.probe {
                let _ = writeln!(p.lock().unwrap().out, "ERROR starting trace: {e:?}");
            }
            std::process::exit(2);
        }
    };

    if probing {
        std::thread::sleep(Duration::from_secs(8));
        let mut paths = HashMap::new();
        let line = flush(&state, &mut paths).unwrap_or_default();
        if let Some(p) = &state.probe {
            let _ = writeln!(p.lock().unwrap().out, "FLUSH {line}");
        }
        let _ = trace.stop();
        return;
    }

    let (tx, rx) = mpsc::channel::<String>();
    let ps = state.clone();
    std::thread::spawn(move || serve_pipe(ps, rx));

    let mut paths: HashMap<u32, (String, Instant)> = HashMap::new();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let line = flush(&state, &mut paths);
        if state.connected.load(Ordering::SeqCst) {
            if let Some(line) = line {
                let _ = tx.send(line);
            }
        } else if now_ms().saturating_sub(state.last_client_ms.load(Ordering::SeqCst)) > IDLE_EXIT.as_millis() as u64 {
            break;
        }
    }
    let _ = trace.stop();
}
