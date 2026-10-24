//! traffic-etw: per-process network byte counter for AppTraffic.
//!
//! Runs elevated (ETW real-time sessions need admin), subscribes to
//! Microsoft-Windows-Kernel-Network for TCP/UDP send/recv sizes and to
//! Microsoft-Windows-DNS-Client to map remote IPs back to host names, and to
//! Microsoft-Windows-Kernel-Process to learn each new process's image path.
//! Events arrive with ~1 s of buffering delay, by which time a short-lived
//! process (curl, an updater) has often exited and can no longer be opened.
//! Loopback TCP is not counted as traffic but reported as client -> local server
//! flows, so the app can attribute a local proxy's upstream bytes to its clients.
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
const KERNEL_PROCESS: &str = "22fb2cd6-0e7b-422b-a0c7-2fad1fd0e716";
/// WINEVENT_KEYWORD_PROCESS: process start/stop
const PROCESS_KEYWORDS: u64 = 0x10;
const STARTED_MAX: usize = 8192;
/// KERNEL_NETWORK_KEYWORD_IPV4 | KERNEL_NETWORK_KEYWORD_IPV6
const NET_KEYWORDS: u64 = 0x10 | 0x20;
/// Exit when no client has been connected for this long (also the start-up grace period).
const IDLE_EXIT: Duration = Duration::from_secs(30);
const PATH_TTL: Duration = Duration::from_secs(30);
const DNS_CACHE_MAX: usize = 50_000;
const HOSTS_PER_PROC: usize = 20;
const LOOPS_MAX: usize = 50;

#[derive(Default)]
struct PidAgg {
    /// Resolved on the first event, while the process still exists
    path: String,
    up: u64,
    down: u64,
    hosts: HashMap<IpAddr, u64>,
    /// Loopback TCP by remote (server) port: (sent, received)
    loops: HashMap<u16, (u64, u64)>,
}

struct State {
    pending: Mutex<HashMap<u32, PidAgg>>,
    paths: Mutex<HashMap<u32, (String, Instant)>>,
    /// PID -> image path from process-start events (survives the process exiting)
    started: Mutex<HashMap<u32, String>>,
    /// NT device prefix ("\Device\HarddiskVolume3") -> drive ("C:")
    drives: Vec<(String, String)>,
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

impl State {
    fn path_of(&self, pid: u32) -> String {
        if let Some(p) = self.started.lock().unwrap().get(&pid) {
            return p.clone();
        }
        let mut paths = self.paths.lock().unwrap();
        match paths.get(&pid) {
            Some((p, at)) if at.elapsed() < PATH_TTL => p.clone(),
            _ => {
                let p = process_path(pid);
                // Don't cache a miss: the start event may still be on its way
                if !p.is_empty() {
                    paths.insert(pid, (p.clone(), Instant::now()));
                }
                p
            }
        }
    }

    fn dos_path(&self, nt: &str) -> String {
        for (device, drive) in &self.drives {
            if let Some(rest) = nt.strip_prefix(device.as_str())
                && rest.starts_with('\\')
            {
                return format!("{drive}{rest}");
            }
        }
        nt.to_string()
    }

    /// Pending counters, with an entry for `pid` whose path is already resolved.
    /// Short-lived processes (curl, updaters) are often gone by the next flush.
    fn pending_for(&self, pid: u32) -> std::sync::MutexGuard<'_, HashMap<u32, PidAgg>> {
        let pending = self.pending.lock().unwrap();
        if pending.contains_key(&pid) {
            return pending;
        }
        drop(pending);
        let path = self.path_of(pid);
        let mut pending = self.pending.lock().unwrap();
        pending.entry(pid).or_insert_with(|| PidAgg { path, ..Default::default() });
        pending
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

    if let Some(probe) = &state.probe
        && state.path_of(pid).is_empty()
    {
        let mut p = probe.lock().unwrap();
        if p.net_lines < 120 {
            p.net_lines += 1;
            let _ = writeln!(
                p.out,
                "UNRESOLVED id={} pid={} hdrpid={} size={} saddr={:?} sport={:?} daddr={:?} dport={:?}",
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

    // Loopback hops (app -> local proxy port, IPC over TCP) are not internet traffic,
    // but TCP ones are kept per server port for proxy attribution.
    if daddr.as_ref().is_some_and(is_local_only) || saddr.as_ref().is_some_and(is_local_only) {
        if is_tcp && let Ok(dport) = parser.try_parse::<u16>("dport") {
            let mut pending = state.pending_for(pid);
            let entry = pending.get_mut(&pid).unwrap().loops.entry(u16::from_be(dport)).or_default();
            if is_send {
                entry.0 += size as u64;
            } else {
                entry.1 += size as u64;
            }
        }
        return;
    }
    // TCP events report the connection's endpoints (saddr = local, daddr = remote)
    // in both directions; UDP events report the datagram's own source/destination.
    // PID is the owning process; the header PID of a receive is often 4 (System).
    let remote = if is_tcp || is_send { daddr } else { saddr };

    let mut pending = state.pending_for(pid);
    let agg = pending.get_mut(&pid).unwrap();
    if is_send {
        agg.up += size as u64;
    } else {
        agg.down += size as u64;
    }
    if let Some(ip) = remote {
        *agg.hosts.entry(canonical(ip)).or_default() += size as u64;
    }
}

fn on_process(state: &State, record: &EventRecord, locator: &SchemaLocator) {
    // 1 = ProcessStart; ImageName is an NT device path
    if record.event_id() != 1 {
        return;
    }
    let Ok(schema) = locator.event_schema(record) else { return };
    let parser = Parser::create(record, &schema);
    let (Ok(pid), Ok(image)) = (parser.try_parse::<u32>("ProcessID"), parser.try_parse::<String>("ImageName")) else {
        return;
    };
    let path = state.dos_path(&image);
    let mut started = state.started.lock().unwrap();
    if started.len() > STARTED_MAX {
        started.clear();
    }
    // A reused PID simply overwrites the previous owner
    started.insert(pid, path);
}

/// Map NT device names to drive letters for converting Kernel-Process image paths.
fn drive_map() -> Vec<(String, String)> {
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
    let mut out = Vec::new();
    for letter in b'A'..=b'Z' {
        let drive = format!("{}:", letter as char);
        let name: Vec<u16> = format!("{drive}\0").encode_utf16().collect();
        let mut buf = [0u16; 512];
        let n = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if n == 0 {
            continue;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(n as usize);
        out.push((String::from_utf16_lossy(&buf[..len]), drive));
    }
    out
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

/// Executable file name from a process snapshot; works for protected processes
/// that refuse OpenProcess even to an elevated caller. Empty if the PID is gone.
fn process_name_snapshot(pid: u32) -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return String::new();
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut name = String::new();
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            if e.th32ProcessID == pid {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                name = String::from_utf16_lossy(&e.szExeFile[..len]);
                break;
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
        name
    }
}

/// Executable path for a PID; just the file name for protected processes, empty when gone.
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
            return process_name_snapshot(pid);
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return process_name_snapshot(pid);
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

/// Local TCP port -> PID of the process listening on it (IPv4 and IPv6).
fn tcp_listeners() -> HashMap<u16, u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
    let mut out = HashMap::new();
    for af in [AF_INET, AF_INET6] {
        let mut size = 0u32;
        unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, af as u32, TCP_TABLE_OWNER_PID_LISTENER, 0) };
        if size == 0 {
            continue;
        }
        // Some slack in case listeners appear between the two calls; u64 for alignment
        size += 4096;
        let mut buf = vec![0u64; size as usize / 8 + 1];
        let ret = unsafe {
            GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, af as u32, TCP_TABLE_OWNER_PID_LISTENER, 0)
        };
        if ret != 0 {
            continue;
        }
        // dwLocalPort holds the port in network byte order in its low 16 bits
        unsafe {
            if af == AF_INET {
                let t = buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
                let rows = std::slice::from_raw_parts((*t).table.as_ptr(), (*t).dwNumEntries as usize);
                for r in rows {
                    out.entry(u16::from_be(r.dwLocalPort as u16)).or_insert(r.dwOwningPid);
                }
            } else {
                let t = buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID;
                let rows = std::slice::from_raw_parts((*t).table.as_ptr(), (*t).dwNumEntries as usize);
                for r in rows {
                    out.entry(u16::from_be(r.dwLocalPort as u16)).or_insert(r.dwOwningPid);
                }
            }
        }
    }
    out
}

/// Drain the pending per-PID counters into one JSON line grouped by executable.
fn flush(state: &State) -> Option<String> {
    let drained: HashMap<u32, PidAgg> = std::mem::take(&mut *state.pending.lock().unwrap());
    if drained.is_empty() {
        return None;
    }
    // Only the client side of a loopback connection resolves to a listener; the
    // server side's remote port is the client's ephemeral one.
    let listeners = if drained.values().any(|a| !a.loops.is_empty()) { tcp_listeners() } else { HashMap::new() };
    let mut loops: HashMap<(String, String, u16), (u64, u64)> = HashMap::new();
    for (pid, agg) in &drained {
        for (port, (sent, recv)) in &agg.loops {
            let Some(&server) = listeners.get(port) else { continue };
            if server == *pid {
                continue;
            }
            let key = (agg.path.clone(), state.path_of(server), *port);
            let e = loops.entry(key).or_default();
            e.0 += sent;
            e.1 += recv;
        }
    }

    let dns = state.dns.lock().unwrap();
    let mut by_path: HashMap<String, PidAgg> = HashMap::new();
    let mut host_names: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for (pid, agg) in drained {
        if agg.up + agg.down == 0 {
            continue;
        }
        // Retry a miss: the process-start event may have been processed since
        let path = if agg.path.is_empty() { state.path_of(pid) } else { agg.path };
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
    let mut paths = state.paths.lock().unwrap();
    if paths.len() > 4096 {
        paths.retain(|_, (_, at)| at.elapsed() < PATH_TTL);
    }
    drop(paths);

    let procs: Vec<_> = by_path
        .into_iter()
        .map(|(path, agg)| {
            let mut hosts: Vec<(String, u64)> = host_names.remove(&path).unwrap_or_default().into_iter().collect();
            hosts.sort_by(|a, b| b.1.cmp(&a.1));
            hosts.truncate(HOSTS_PER_PROC);
            json!({ "path": path, "up": agg.up, "down": agg.down, "hosts": hosts })
        })
        .collect();
    let mut loops: Vec<_> = loops.into_iter().collect();
    loops.sort_by(|a, b| (b.1.0 + b.1.1).cmp(&(a.1.0 + a.1.1)));
    loops.truncate(LOOPS_MAX);
    let loops: Vec<_> = loops
        .into_iter()
        .map(|((client, server, port), (up, down))| {
            json!({ "client": client, "server": server, "port": port, "up": up, "down": down })
        })
        .collect();
    Some(json!({ "procs": procs, "loops": loops }).to_string())
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
        paths: Mutex::new(HashMap::new()),
        started: Mutex::new(HashMap::new()),
        drives: drive_map(),
        dns: Mutex::new(HashMap::new()),
        connected: AtomicBool::new(false),
        last_client_ms: AtomicU64::new(now_ms()),
        probe,
    });

    // A previous instance that was killed leaves its session behind
    let session = if probing { format!("{SESSION}Probe") } else { SESSION.to_string() };
    let _ = stop_trace_by_name(&session);

    let s1 = state.clone();
    let net = Provider::by_guid(KERNEL_NETWORK)
        .any(NET_KEYWORDS)
        .add_callback(move |r: &EventRecord, l: &SchemaLocator| on_network(&s1, r, l))
        .build();
    let s2 = state.clone();
    let dns = Provider::by_guid(DNS_CLIENT)
        .add_callback(move |r: &EventRecord, l: &SchemaLocator| on_dns(&s2, r, l))
        .build();
    let s3 = state.clone();
    let procs = Provider::by_guid(KERNEL_PROCESS)
        .any(PROCESS_KEYWORDS)
        .add_callback(move |r: &EventRecord, l: &SchemaLocator| on_process(&s3, r, l))
        .build();
    let trace = match UserTrace::new().named(session).enable(net).enable(dns).enable(procs).start_and_process() {
        Ok(t) => t,
        Err(e) => {
            if let Some(p) = &state.probe {
                let _ = writeln!(p.lock().unwrap().out, "ERROR starting trace: {e:?}");
            }
            std::process::exit(2);
        }
    };

    if probing {
        std::thread::sleep(Duration::from_secs(15));
        let line = flush(&state).unwrap_or_default();
        if let Some(p) = &state.probe {
            let _ = writeln!(p.lock().unwrap().out, "FLUSH {line}");
        }
        let _ = trace.stop();
        return;
    }

    let (tx, rx) = mpsc::channel::<String>();
    let ps = state.clone();
    std::thread::spawn(move || serve_pipe(ps, rx));

    loop {
        std::thread::sleep(Duration::from_secs(1));
        let line = flush(&state);
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
