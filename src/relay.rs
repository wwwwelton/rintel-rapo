//! Explicit local SOCKS5 TCP/UDP relay plus the transparent WFP framing used by the game redirector.
mod hy2;
pub mod wireguard;
pub mod openvpn;
mod learning;
mod prewarm;
pub mod redundancy;
pub use redundancy::{RedundancyConfig, RedundancyEngine};
mod udp;
#[cfg(windows)]
mod native;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
pub use hy2::Config as Hy2Config;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex, RwLock, atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering}};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Outbound policy, fixed for the lifetime of one relay run.
/// `Local` (default) is the pre-existing Flow over the Windows route plus authorized interfaces.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode { Direct, #[default] Local, Hy2, NeuralAuto }
#[derive(Clone)]
struct Egress {
    id: String,
    authorization: String,
    interface: u32,
    source: SocketAddr,
    gateway: String,
}

#[derive(Default, Serialize)]
pub struct State {
    pub mode: Mode,
    pub hy2_server: String,
    /// Protocol of the VPN profile carried by this run ("WireGuard"); empty for plain HY2, direct and local.
    pub tunnel: String,
    pub hy2_server_name: String,
    /// A QUIC session to the HY2 server (embedded loopback or remote) is open.
    pub hy2_connected: bool,
    /// Last HY2 connect failure; empty after a successful connect.
    pub hy2_error: String,
    /// No server was typed: the one sealed into this executable is used.
    pub hy2_builtin: bool,
    /// ICMP round trip to the HY2 server in ms; 0 while unmeasured or unanswered.
    pub hy2_ping_ms: u32,
    pub udp_associations: usize,
    pub udp_destinations: u64,
    pub udp_route: String,
    pub running: bool,
    pub stopping: bool,
    pub address: String,
    pub active_connections: usize,
    pub total_connections: u64,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    pub prewarm_hits: u64,
    pub prewarm_misses: u64,
    pub error: String,
    pub predictions: Vec<learning::Prediction>,
    pub interfaces: Vec<crate::routes::Candidate>,
    pub allowed_interfaces: Vec<String>,
    pub redundancy: RedundancyConfig,
}

struct Connection { id: u64, client: TcpStream, outbound: Option<Outbound>, udp_bound: Option<SocketAddr> }
struct Shared {
    mode: Mode,
    config: Hy2Config,
    /// The active VPN profile's tunnel. When set it is the only exit: no built-in or loopback fallback is ever tried.
    tunnel: Option<Arc<VpnTunnel>>,
    udp_destinations: AtomicU64,
    running: AtomicBool,
    connections: Mutex<Vec<Connection>>,
    total: AtomicU64,
    bytes_tx: AtomicU64,
    bytes_rx: AtomicU64,
    error: Mutex<String>,
    learning: Mutex<learning::Learning>,
    prewarm: Mutex<prewarm::Prewarm>,
    interfaces: Mutex<Vec<crate::routes::Candidate>>,
    allowed_interfaces: RwLock<Vec<String>>,
    /// One authenticated HY2 connection shared by all flows, opened lazily on a worker.
    hy2: Mutex<Option<hy2::Client>>,
    /// Last HY2 connect failure, shown in the GUI; cleared by a successful connect.
    hy2_error: Mutex<String>,
    hy2_ping_ms: AtomicU32,
    redundancy: RwLock<RedundancyConfig>,
    redundancy_engine: RedundancyEngine,
}
impl Shared {
    fn close_connections(&self) {
        for connection in self.connections.lock().unwrap().iter() {
            let _ = connection.client.shutdown(Shutdown::Both);
            if let Some(outbound) = &connection.outbound { let _ = outbound.shutdown(Shutdown::Both); }
        }
        // Flows keep their own client references; this only stops reuse for new flows.
        if let Ok(mut client) = self.hy2.try_lock() { client.take(); }
    }
    fn hy2_client(&self) -> io::Result<hy2::Client> {
        let mut cached = self.hy2.lock().unwrap();
        if let Some(client) = &*cached { return Ok(client.clone()); }
        if !self.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
        // A VPN profile owns the exit: a failing tunnel reports its error, it never falls back to another route.
        let connected = if let Some(tunnel) = &self.tunnel { tunnel.connect() } else {
            // Typed server first, then the server sealed into the exe; the loopback HY2 is only a fallback (a remote server is the only real ping gain).
            if !self.config.server.is_empty() { Ok(self.config.clone()) } else { hy2::builtin().map_or_else(hy2::local_server_config, Ok) }
                .and_then(|config| hy2::Client::connect(&config))
        };
        *self.hy2_error.lock().unwrap() = connected.as_ref().err().map(ToString::to_string).unwrap_or_default();
        let client = connected?;
        *cached = Some(client.clone());
        Ok(client)
    }
    /// Opens a HY2 stream, reconnecting once if the cached QUIC session died.
    fn hy2_tcp(&self, target: SocketAddr) -> io::Result<hy2::TcpStream> {
        match self.hy2_client()?.tcp(&target.to_string()) {
            Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => {
                self.hy2.lock().unwrap().take();
                self.hy2_client()?.tcp(&target.to_string())
            }
            result => result,
        }
    }
    fn register_outbound(&self, id: u64, socket: &Outbound) -> io::Result<()> {
        let mut connections = self.connections.lock().unwrap();
        if !self.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
        let connection = connections.iter_mut().find(|c| c.id == id).ok_or(io::ErrorKind::Interrupted)?;
        connection.outbound = Some(socket.try_clone()?);
        Ok(())
    }
}
struct Run { shared: Arc<Shared>, worker: Option<JoinHandle<()>>, address: SocketAddr }

/// Measures the ICMP round trip to the HY2 server until the relay stops; resolves off the UI thread.
#[cfg(windows)]
fn ping_server(shared: Arc<Shared>) {
    let server = if let Some(tunnel) = &shared.tunnel { Some(tunnel.endpoint().to_string()) } else if shared.config.server.is_empty() { hy2::builtin().map(|config| config.server) } else { Some(shared.config.server.clone()) };
    let ip = server.and_then(|server| server.to_socket_addrs().ok()?.find_map(|a| match a.ip() { IpAddr::V4(ip) if !ip.is_loopback() => Some(ip), _ => None }));
    let Some(ip) = ip else { return; };
    while shared.running.load(Ordering::Acquire) {
        shared.hy2_ping_ms.store(crate::process_ping::round_trip_ms(ip).unwrap_or(0), Ordering::Relaxed);
        for _ in 0..30 {
            if !shared.running.load(Ordering::Acquire) { break; }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// With an integrated server the relay opens in HY2: the transport block is locked while the relay runs, so a Local default left HY2 unreachable.
fn default_mode(integrated_server: bool) -> Mode { if integrated_server { Mode::Hy2 } else { Mode::Local } }

/// The tunnel for an active WireGuard profile.
pub fn wireguard_tunnel(profile: &crate::vpn::Profile) -> Result<wireguard::Tunnel, String> { wireguard::Tunnel::from_profile(profile) }

/// The tunnel for an active OpenVPN profile.
pub fn openvpn_tunnel(profile: &crate::vpn::Profile) -> Result<openvpn::Tunnel, String> { openvpn::Tunnel::from_profile(profile) }

/// Polymorphic VPN tunnel for either WireGuard or OpenVPN.
#[derive(Debug)]
pub enum VpnTunnel {
    Wireguard(wireguard::Tunnel),
    Openvpn(openvpn::Tunnel),
}
impl VpnTunnel {
    pub fn label(&self) -> &'static str {
        match self { Self::Wireguard(_) => "WireGuard", Self::Openvpn(_) => "OpenVPN" }
    }
    pub fn endpoint(&self) -> &str {
        match self { Self::Wireguard(t) => &t.endpoint, Self::Openvpn(t) => &t.endpoint }
    }
    pub fn connect(&self) -> io::Result<hy2::Client> {
        match self {
            Self::Wireguard(t) => hy2::Client::connect_wireguard(t.json()),
            Self::Openvpn(t) => hy2::Client::connect_openvpn(t.json()),
        }
    }
}

/// The HY2 settings for an active Hysteria 2 profile; the stored password becomes the session-only `auth`.
pub fn hy2_config(profile: &crate::vpn::Profile) -> Result<Hy2Config, String> {
    if profile.protocol != crate::vpn::Protocol::Hysteria2 { return Err("O perfil ativo não é Hysteria 2".into()); }
    profile.validate()?;
    let field = |name: &str| profile.fields.get(name).cloned().unwrap_or_default();
    let config = Hy2Config { server: field("server"), server_name: field("server_name"), ca_pem: field("ca_pem"), auth: profile.secrets.get("auth").cloned().unwrap_or_default() };
    config.validate().map_err(|e| e.to_string())?;
    Ok(config)
}

pub struct RelayServer { port: u16, run: Option<Run>, error: String, mode: Mode, config: Hy2Config, tunnel: Option<Arc<VpnTunnel>>, redundancy: RedundancyConfig, priors: Vec<crate::store::PathStat> }
impl RelayServer {
    pub fn new(port: u16) -> Self { Self { port, run: None, error: String::new(), mode: default_mode(hy2::builtin().is_some()), config: Hy2Config::default(), tunnel: None, redundancy: RedundancyConfig::default(), priors: Vec::new() } }
    /// Selects (or clears) the VPN tunnel used from the next `start()` on. While a tunnel is set the relay always
    /// runs in `Hy2` mode: everything goes through the tunnel and there is no direct fallback.
    pub fn set_tunnel(&mut self, tunnel: Option<VpnTunnel>) -> io::Result<()> {
        self.poll();
        if self.run.as_ref().is_some_and(|r| r.worker.is_some()) {
            let error = io::Error::new(io::ErrorKind::WouldBlock, "Pare o relay e aguarde todas as conexões encerrarem");
            self.error = error.to_string();
            return Err(error);
        }
        self.tunnel = tunnel.map(Arc::new);
        self.error.clear();
        Ok(())
    }
    fn tunnel_label(&self) -> String { self.tunnel.as_ref().map_or(String::new(), |t| t.label().into()) }
    pub fn set_redundancy(&mut self, config: RedundancyConfig) -> Result<(), &'static str> {
        config.validate()?;
        if let Some(run) = &self.run {
            *run.shared.redundancy.write().unwrap() = config;
        }
        self.redundancy = config;
        Ok(())
    }
    pub fn redundancy(&self) -> RedundancyConfig { self.redundancy }
    /// Exit statistics saved by earlier sessions; used from the next `start()` on.
    pub fn set_priors(&mut self, priors: Vec<crate::store::PathStat>) { self.priors = priors; }
    /// Destination/exit statistics measured by the running relay, for saving.
    pub fn paths(&self) -> Vec<crate::store::PathStat> {
        self.run.as_ref().map_or_else(Vec::new, |run| run.shared.learning.lock().unwrap().paths())
    }
    pub fn mode(&self) -> Mode { self.mode }
    pub fn configure(&mut self, mode: Mode, config: Hy2Config) -> io::Result<()> {
        self.poll();
        let result = if self.run.as_ref().is_some_and(|r| r.worker.is_some()) {
            Err(io::Error::new(io::ErrorKind::WouldBlock, "Pare o relay e aguarde todas as conexões encerrarem"))
        } else if matches!(mode, Mode::Hy2 | Mode::NeuralAuto) {
            config.validate()
        } else { Ok(()) };
        if let Err(error) = result {
            self.error = error.to_string();
            return Err(error);
        }
        self.mode = mode;
        self.config = config;
        self.error.clear();
        Ok(())
    }

    pub fn start(&mut self) -> io::Result<()> {
        self.poll();
        if let Some(run) = &self.run {
            if run.shared.running.load(Ordering::Acquire) { return Ok(()); }
            if run.worker.is_some() { return Err(io::Error::new(io::ErrorKind::WouldBlock, "Relay ainda encerrando conexões")); }
        }
        let result = self.launch();
        if let Err(error) = &result { self.error = format!("Não foi possível iniciar SOCKS5: {error}"); }
        result
    }
    fn launch(&mut self) -> io::Result<()> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port))?;
        let address = listener.local_addr()?;
        // Bound before start() returns, so a datagram sent right after it is never dropped.
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, address.port())).ok();
        let shared = Arc::new(Shared {
            mode: if self.tunnel.is_some() { Mode::Hy2 } else { self.mode }, config: self.config.clone(), tunnel: self.tunnel.clone(), udp_destinations: AtomicU64::new(0),
            running: AtomicBool::new(true), connections: Mutex::new(Vec::new()), total: AtomicU64::new(0),
            bytes_tx: AtomicU64::new(0), bytes_rx: AtomicU64::new(0), error: Mutex::new(String::new()),
            learning: Mutex::new(learning::Learning::with_prior(self.priors.clone())),
            prewarm: Mutex::new(prewarm::Prewarm::default()),
            interfaces: Mutex::new(Vec::new()), allowed_interfaces: RwLock::new(Vec::new()), hy2: Mutex::new(None), hy2_error: Mutex::new(String::new()), hy2_ping_ms: AtomicU32::new(0),
            redundancy: RwLock::new(self.redundancy), redundancy_engine: RedundancyEngine::new(),
        });
        let state = Arc::clone(&shared);
        let worker = thread::Builder::new().name("rapo-socks5".into()).spawn(move || serve(listener, udp, state, address))?;
        // HY2 modes open the QUIC session now: the GUI shows it working, and the first game connection skips the handshake.
        if self.tunnel.is_some() || matches!(self.mode, Mode::Hy2 | Mode::NeuralAuto) {
            let hy2 = Arc::clone(&shared);
            let _ = thread::Builder::new().name("rapo-hy2-connect".into()).spawn(move || {
                if hy2.hy2_client().is_err() && hy2.running.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(50));
                    let _ = hy2.hy2_client();
                }
            });
            #[cfg(windows)]
            {
                let shared = Arc::clone(&shared);
                let _ = thread::Builder::new().name("rapo-hy2-ping".into()).spawn(move || ping_server(shared));
            }
        }
        self.run = Some(Run { shared, worker: Some(worker), address });
        self.error.clear();
        Ok(())
    }
    /// Signals shutdown without making the UI wait for DNS or connection workers.
    pub fn stop(&mut self) {
        if let Some(run) = &self.run {
            if run.shared.running.swap(false, Ordering::AcqRel) {
                run.shared.close_connections();
                // Wake the blocking accept; the wakeup is never handled as a client.
                let _ = TcpStream::connect_timeout(&run.address, Duration::from_millis(100));
            }
        }
    }
    pub fn set_interfaces(&mut self, allowed: Vec<String>) {
        let Some(run) = &self.run else { return; };
        let interfaces = run.shared.interfaces.lock().unwrap();
        if allowed.iter().any(|id| !interfaces.iter().any(|interface| interface.id == *id)) {
            *run.shared.error.lock().unwrap() = "Interface SOCKS5 obsoleta; reinicie o relay para atualizar".into();
            return;
        }
        *run.shared.allowed_interfaces.write().unwrap() = allowed;
    }
    pub fn poll(&mut self) {
        if let Some(run) = &mut self.run {
            if run.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                run.shared.running.store(false, Ordering::Release);
                run.shared.close_connections();
                if run.worker.take().unwrap().join().is_err() { self.error = "Worker SOCKS5 encerrado inesperadamente".into(); }
            }
        }
    }
    pub fn state(&self) -> State {
        let Some(run) = &self.run else {
            return State { mode: if self.tunnel.is_some() { Mode::Hy2 } else { self.mode }, hy2_server: self.config.server.clone(), hy2_server_name: self.config.server_name.clone(), tunnel: self.tunnel_label(),
                hy2_builtin: self.tunnel.is_none() && self.config.server.is_empty() && hy2::builtin().is_some(),
                udp_route: self.udp_route().into(), address: format!("127.0.0.1:{}", self.port), error: self.error.clone(), redundancy: self.redundancy, ..State::default() };
        };
        let running = run.shared.running.load(Ordering::Acquire) && !run.worker.as_ref().is_some_and(JoinHandle::is_finished);
        // One guard: a second lock inside the State literal would self-deadlock.
        let (prewarm_hits, prewarm_misses) = { let pool = run.shared.prewarm.lock().unwrap(); (pool.hits, pool.misses) };
        let (active_connections, udp_associations) = {
            let connections = run.shared.connections.lock().unwrap();
            (connections.len(), connections.iter().filter(|c| c.udp_bound.is_some()).count())
        };
        State {
            mode: if self.tunnel.is_some() { Mode::Hy2 } else { self.mode }, hy2_server: self.config.server.clone(), hy2_server_name: self.config.server_name.clone(), tunnel: self.tunnel_label(),
            udp_associations, active_connections,
            udp_destinations: run.shared.udp_destinations.load(Ordering::Relaxed),
            udp_route: self.udp_route().into(),
            running, stopping: !running && run.worker.as_ref().is_some_and(|w| !w.is_finished()),
            address: run.address.to_string(),
            total_connections: run.shared.total.load(Ordering::Relaxed),
            bytes_tx: run.shared.bytes_tx.load(Ordering::Relaxed), bytes_rx: run.shared.bytes_rx.load(Ordering::Relaxed),
            error: if self.error.is_empty() { run.shared.error.lock().unwrap().clone() } else { self.error.clone() },
            prewarm_hits, prewarm_misses,
            // try_lock: a connect in progress holds the lock and must not stall the UI thread.
            hy2_connected: running && run.shared.hy2.try_lock().is_ok_and(|client| client.is_some()),
            hy2_error: run.shared.hy2_error.lock().unwrap().clone(),
            hy2_builtin: self.tunnel.is_none() && self.config.server.is_empty() && hy2::builtin().is_some(),
            hy2_ping_ms: if running { run.shared.hy2_ping_ms.load(Ordering::Relaxed) } else { 0 },
            predictions: run.shared.learning.lock().unwrap().predictions(Instant::now()),
            interfaces: run.shared.interfaces.lock().unwrap().clone(),
            allowed_interfaces: run.shared.allowed_interfaces.read().unwrap().clone(),
            redundancy: *run.shared.redundancy.read().unwrap(),
        }
    }
    /// UDP has no per-flow RTT signal here, so an association keeps one fixed route.
    fn udp_route(&self) -> &'static str {
        match self.mode { Mode::Direct | Mode::NeuralAuto => "direct", Mode::Local => "local", Mode::Hy2 => "hy2" }
    }
}
impl Drop for RelayServer {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.run.as_mut().and_then(|r| r.worker.take()) { let _ = worker.join(); }
    }
}

fn serve(listener: TcpListener, udp: Option<UdpSocket>, shared: Arc<Shared>, address: SocketAddr) {
    #[cfg(windows)]
    match crate::routes::native::candidates() {
        Ok(interfaces) => {
            let ids: Vec<String> = interfaces.iter().map(|c| c.id.clone()).collect();
            *shared.allowed_interfaces.write().unwrap() = ids;
            *shared.interfaces.lock().unwrap() = interfaces;
        }
        Err(error) => *shared.error.lock().unwrap() = format!("Listar interfaces SOCKS5: {error}"),
    }
    let swapped_port = address.port().swap_bytes();
    let swapped_udp = (swapped_port != 0 && swapped_port != address.port())
        .then(|| UdpSocket::bind((Ipv4Addr::LOCALHOST, swapped_port)).ok())
        .flatten();
    let mut udp_workers = Vec::new();
    for sock in [udp, swapped_udp].into_iter().flatten() {
        let state = Arc::clone(&shared);
        if let Ok(w) = thread::Builder::new().name("rapo-wfp-udp".into()).spawn(move || serve_wfp_udp(sock, state)) {
            udp_workers.push(w);
        }
    }
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while shared.running.load(Ordering::Acquire) {
        let incoming = listener.accept();
        if !shared.running.load(Ordering::Acquire) { break; }
        let (client, peer) = match incoming {
            Ok(connection) => connection,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => { *shared.error.lock().unwrap() = format!("SOCKS5 accept: {e}"); break; }
        };
        if !peer.ip().is_loopback() { continue; }
        let mut i = 0;
        while i < workers.len() {
            if workers[i].is_finished() { let _ = workers.swap_remove(i).join(); } else { i += 1; }
        }
        let mut connections = shared.connections.lock().unwrap();
        if !shared.running.load(Ordering::Acquire) { break; }
        if connections.len() >= MAX_CONNECTIONS {
            *shared.error.lock().unwrap() = format!("Limite local de {MAX_CONNECTIONS} conexões SOCKS5 atingido");
            continue;
        }
        let Ok(control) = client.try_clone() else { continue; };
        let id = shared.total.fetch_add(1, Ordering::Relaxed) + 1;
        connections.push(Connection { id, client: control, outbound: None, udp_bound: None });
        drop(connections);
        let state = Arc::clone(&shared);
        match thread::Builder::new().name("rapo-socks5-client".into()).spawn(move || {
            let guard = ConnectionGuard { shared: state, id };
            let state = &guard.shared;
            if let Err(error) = handle(client, state, id, address) {
                if state.running.load(Ordering::Acquire) && !matches!(error.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted) {
                    *state.error.lock().unwrap() = format!("Última conexão SOCKS5: {error}");
                }
            }
        }) {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                shared.connections.lock().unwrap().retain(|c| c.id != id);
                *shared.error.lock().unwrap() = format!("Worker SOCKS5: {error}");
            }
        }
    }
    shared.running.store(false, Ordering::Release);
    shared.close_connections();
    drop(listener);
    for w in udp_workers { let _ = w.join(); }
    for worker in workers { let _ = worker.join(); }
}

/// UDP flows of the WFP redirector. Each destination owns a socket and a reader thread that blocks
/// on it, so a reply goes back as soon as it arrives instead of waiting for a poll tick.
fn serve_wfp_udp(socket: UdpSocket, shared: Arc<Shared>) {
    // Only bounds how long shutdown waits; incoming datagrams wake the loop immediately.
    let _ = socket.set_read_timeout(Some(Duration::from_millis(250)));
    let reply_socket = match socket.try_clone() { Ok(s) => Arc::new(s), Err(_) => return };
    struct Flow { out: Arc<UdpSocket>, client: Arc<Mutex<SocketAddr>>, raw: Arc<AtomicBool>, sent_at: Arc<Mutex<Instant>>, last_seen: Instant }
    let mut flows: HashMap<SocketAddr, Flow> = HashMap::new();
    let mut buf = [0u8; 65535];

    while shared.running.load(Ordering::Acquire) {
        let (n, src) = match socket.recv_from(&mut buf) {
            Ok(res) => res,
            Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                let now = Instant::now();
                flows.retain(|_, f| {
                    let keep = now.duration_since(f.last_seen) <= Duration::from_secs(60);
                    if !keep { shared.udp_destinations.fetch_sub(1, Ordering::Relaxed); }
                    keep // dropping the Flow drops `out`; its reader exits on the next read error
                });
                continue;
            }
            // Windows reports WSAECONNRESET here when an earlier send hit a dead local port; it must not stop the relay.
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
            Err(_) => break,
        };
        if !src.ip().is_loopback() || n < 14 { continue; }
        // mudwfp_proxy.exe (`mudwfp_udp.c` 0x40df79) prefixes 14 bytes: magic 0x7e6d8b10 LE
        // (`[0x10, 0x8b, 0x6d, 0x7e]`), 4-byte token, 4-byte IPv4, 2-byte big-endian port, and expects raw payload back.
        let (dst, raw_reply) = if buf[..4] == [0x10, 0x8b, 0x6d, 0x7e] {
            (SocketAddr::new(Ipv4Addr::new(buf[8], buf[9], buf[10], buf[11]).into(), u16::from_be_bytes([buf[12], buf[13]])), true)
        } else if buf[..4] == [0x23, 0xa4, 0xfa, 0x39] {
            (SocketAddr::new(Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]).into(), u16::from_le_bytes([buf[8], buf[9]])), false)
        } else {
            continue;
        };

        let now = Instant::now();
        let is_new = !flows.contains_key(&dst);
        if is_new {
            let Ok(out) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { continue };
            let _ = out.set_read_timeout(Some(Duration::from_secs(1)));
            let out = Arc::new(out);
            let client = Arc::new(Mutex::new(src));
            let raw = Arc::new(AtomicBool::new(raw_reply));
            let sent_at = Arc::new(Mutex::new(now));
            shared.udp_destinations.fetch_add(1, Ordering::Relaxed);
            let (reader, back, to, is_raw, stamp, state) = (Arc::clone(&out), Arc::clone(&reply_socket), Arc::clone(&client), Arc::clone(&raw), Arc::clone(&sent_at), Arc::clone(&shared));
            let spawned = thread::Builder::new().name("rapo-wfp-udp-rx".into()).spawn(move || {
                let mut reply = [0u8; 65535];
                reply[..4].copy_from_slice(&[0x23, 0xa4, 0xfa, 0x39]);
                if let IpAddr::V4(ip) = dst.ip() { reply[4..8].copy_from_slice(&ip.octets()); }
                reply[8..10].copy_from_slice(&dst.port().to_le_bytes());
                reply[10..14].copy_from_slice(&[0; 4]);
                // Strong count 1 means the flow was dropped and only this thread still holds the socket.
                while state.running.load(Ordering::Acquire) && Arc::strong_count(&reader) > 1 {
                    let Ok((rn, _)) = reader.recv_from(&mut reply[14..]) else { continue };
                    let rtt = Instant::now().saturating_duration_since(*stamp.lock().unwrap());
                    state.learning.lock().unwrap().observe(dst, &system_egress(dst), rtt, true, Instant::now());
                    let payload = if is_raw.load(Ordering::Relaxed) { &reply[14..14 + rn] } else { &reply[..14 + rn] };
                    let _ = back.send_to(payload, *to.lock().unwrap());
                    state.bytes_rx.fetch_add(rn as u64, Ordering::Relaxed);
                }
            });
            if spawned.is_err() { shared.udp_destinations.fetch_sub(1, Ordering::Relaxed); continue; }
            flows.insert(dst, Flow { out, client, raw, sent_at, last_seen: now });
        }
        let Some(flow) = flows.get_mut(&dst) else { continue };
        *flow.client.lock().unwrap() = src;
        flow.raw.store(raw_reply, Ordering::Relaxed);
        *flow.sent_at.lock().unwrap() = now;
        flow.last_seen = now;
        let red_cfg = *shared.redundancy.read().unwrap();
        let times = shared.redundancy_engine.duplicates_for_next(&red_cfg);
        for copy_idx in 0..times {
            if let Ok(sent) = flow.out.send_to(&buf[14..n], dst) {
                if copy_idx == 0 { shared.bytes_tx.fetch_add(sent as u64, Ordering::Relaxed); }
            }
        }
    }
}
struct ConnectionGuard { shared: Arc<Shared>, id: u64 }
impl Drop for ConnectionGuard {
    fn drop(&mut self) { self.shared.connections.lock().unwrap().retain(|c| c.id != self.id); }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target { Address(SocketAddr), Name(String, u16) }
fn handle(mut client: TcpStream, shared: &Arc<Shared>, id: u64, listen: SocketAddr) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_millis(250)))?;
    client.set_write_timeout(Some(Duration::from_millis(250)))?;
    client.set_nodelay(true)?;
    let handshake = Handshake { running: &shared.running, deadline: Instant::now() + HANDSHAKE_TIMEOUT };
    let mut first = [0; 1];
    handshake.read(&mut client, &mut first)?;

    let (target, is_wfp) = if first[0] == 0x23 {
        // WFP kernel transparent redirect: read remaining 13 bytes of 14-byte header
        let mut rem = [0; 13];
        handshake.read(&mut client, &mut rem)?;
        if rem[0] != 0xa4 || rem[1] != 0xfa || rem[2] != 0x39 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let ip = Ipv4Addr::new(rem[3], rem[4], rem[5], rem[6]);
        let port = u16::from_le_bytes([rem[7], rem[8]]);
        (Target::Address(SocketAddr::new(ip.into(), port)), true)
    } else if first[0] == 5 {
        // Standard RFC 1928 SOCKS5 greeting
        let mut count_buf = [0; 1];
        handshake.read(&mut client, &mut count_buf)?;
        let count = count_buf[0] as usize;
        let mut methods = [0; 255];
        handshake.read(&mut client, &mut methods[..count])?;
        if !methods[..count].contains(&0) { client.write_all(&[5, 255])?; return Ok(()); }
        client.write_all(&[5, 0])?;
        let mut request = [0; 4];
        handshake.read(&mut client, &mut request)?;
        if request[0] != 5 || request[2] != 0 { return reply(&mut client, 1, None); }
        if !matches!(request[1], 1 | 3) { return reply(&mut client, 7, None); }
        let target = match request[3] {
            1 => {
                let mut ip = [0; 4]; handshake.read(&mut client, &mut ip)?;
                Target::Address(SocketAddr::new(Ipv4Addr::from(ip).into(), handshake.port(&mut client)?))
            }
            4 => {
                let mut ip = [0; 16]; handshake.read(&mut client, &mut ip)?;
                Target::Address(SocketAddr::new(Ipv6Addr::from(ip).into(), handshake.port(&mut client)?))
            }
            3 => {
                let mut length = [0]; handshake.read(&mut client, &mut length)?;
                let mut bytes = [0; 255]; handshake.read(&mut client, &mut bytes[..length[0] as usize])?;
                let name = match std::str::from_utf8(&bytes[..length[0] as usize]) {
                    Ok(name) if !name.is_empty() && !name.contains('\0') => name.to_owned(),
                    _ => return reply(&mut client, 8, None),
                };
                Target::Name(name, handshake.port(&mut client)?)
            }
            _ => return reply(&mut client, 8, None),
        };
        if request[1] == 3 { return udp::associate(client, shared, id, target, listen); }
        (target, false)
    } else {
        return Err(io::ErrorKind::InvalidData.into());
    };

    let mut outbound = match connect_target(&target, shared, listen) {
        Ok(stream) => stream,
        Err(error) => {
            if !is_wfp { reply(&mut client, failure_code(&error), None)?; }
            return Err(error);
        }
    };
    shared.register_outbound(id, &outbound)?;
    {
        let mut err = shared.error.lock().unwrap();
        if err.starts_with("Última conexão SOCKS5:") { err.clear(); }
    }
    if !is_wfp {
        reply(&mut client, 0, outbound.local_addr()?)?;
    }
    client.set_read_timeout(Some(Duration::from_millis(250)))?;
    client.set_write_timeout(Some(Duration::from_millis(250)))?;
    outbound.set_timeouts()?;
    let mut client = Outbound::Direct(client);
    let mut upload_client = client.try_clone()?;
    let mut upload_outbound = outbound.try_clone()?;
    thread::scope(|scope| {
        let upload = thread::Builder::new().name("rapo-socks5-upload".into()).spawn_scoped(scope, || {
            forward(&mut upload_client, &mut upload_outbound, &shared.bytes_tx, &shared.running)
        })?;
        let download = forward(&mut outbound, &mut client, &shared.bytes_rx, &shared.running);
        // A broken direction must wake the other, while EOF preserves TCP half-close.
        if download.is_err() { let _ = client.shutdown(Shutdown::Both); let _ = outbound.shutdown(Shutdown::Both); }
        let upload = upload.join().map_err(|_| io::Error::other("Worker de envio SOCKS5 falhou"))?;
        download.and(upload)
    })
}
struct Handshake<'a> { running: &'a AtomicBool, deadline: Instant }
impl Handshake<'_> {
    fn read(&self, socket: &mut TcpStream, mut buffer: &mut [u8]) -> io::Result<()> {
        while !buffer.is_empty() {
            if !self.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
            if Instant::now() >= self.deadline { return Err(io::ErrorKind::TimedOut.into()); }
            match socket.read(buffer) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => buffer = &mut buffer[count..],
                Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn port(&self, socket: &mut TcpStream) -> io::Result<u16> {
        let mut port = [0; 2]; self.read(socket, &mut port)?; Ok(u16::from_be_bytes(port))
    }
}
fn reply(client: &mut TcpStream, status: u8, bound: Option<SocketAddr>) -> io::Result<()> {
    let bound = bound.unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 0)));
    let mut bytes = [0; 22];
    bytes[..3].copy_from_slice(&[5, status, 0]);
    let end = match bound.ip() {
        IpAddr::V4(ip) => { bytes[3] = 1; bytes[4..8].copy_from_slice(&ip.octets()); 8 }
        IpAddr::V6(ip) => { bytes[3] = 4; bytes[4..20].copy_from_slice(&ip.octets()); 20 }
    };
    bytes[end..end + 2].copy_from_slice(&bound.port().to_be_bytes());
    client.write_all(&bytes[..end + 2])
}
fn failure_code(error: &io::Error) -> u8 {
    match error.kind() {
        io::ErrorKind::PermissionDenied => 2,
        io::ErrorKind::NetworkUnreachable => 3,
        io::ErrorKind::HostUnreachable | io::ErrorKind::NotFound | io::ErrorKind::TimedOut => 4,
        io::ErrorKind::ConnectionRefused => 5,
        _ => 1,
    }
}
fn validate_target(target: SocketAddr, listen: SocketAddr) -> io::Result<()> {
    let ip = match target.ip() { IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(target.ip()), ip => ip };
    if target.port() == 0 || ip.is_unspecified() || ip.is_multicast() || ip == IpAddr::V4(Ipv4Addr::BROADCAST)
        || (target.port() == listen.port() && ip.is_loopback()) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Destino SOCKS5 inválido ou recursivo"));
    }
    Ok(())
}
fn system_egress(target: SocketAddr) -> Egress {
    Egress { id: "direct".into(), authorization: "system".into(), interface: 0,
        source: SocketAddr::new(if target.is_ipv4() { Ipv4Addr::UNSPECIFIED.into() } else { Ipv6Addr::UNSPECIFIED.into() }, 0),
        gateway: "system".into() }
}
fn connect_target(target: &Target, shared: &Arc<Shared>, listen: SocketAddr) -> io::Result<Outbound> {
    let addresses: Vec<_> = match target {
        Target::Address(address) => vec![*address],
        Target::Name(name, port) => (name.as_str(), *port).to_socket_addrs()?.take(16).collect(),
    };
    let mut last = io::Error::new(io::ErrorKind::NotFound, "DNS sem endereço utilizável");
    for address in addresses {
        if !shared.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
        validate_target(address, listen)?;
        let hy2_exit = || Egress { id: "hy2".into(), authorization: "hy2".into(), gateway: "hy2".into(), ..system_egress(address) };
        let choices = match shared.mode {
            Mode::Direct => vec![system_egress(address)],
            Mode::Local => {
                let mut exits = egresses(address)?;
                filter_egresses(&mut exits, &shared.allowed_interfaces.read().unwrap());
                exits
            }
            Mode::Hy2 => vec![hy2_exit()],
            // The Flow compares TCP open time of the system route and HY2 for new connections only.
            Mode::NeuralAuto => vec![system_egress(address), hy2_exit()],
        };
        if choices.is_empty() { return Err(io::Error::new(io::ErrorKind::NetworkUnreachable, "Sem saída SOCKS5 autorizada")); }
        let index = shared.learning.lock().unwrap().choose(address, &choices, Instant::now());
        let choice = &choices[index];
        let start = Instant::now();
        // A spare opened ahead of time skips the handshake. It is only used for the default (first)
        // non-HY2 exit: the spare is opened by the OS route, so it never stands in for another exit.
        let default_exit = index == 0 && choice.id != "hy2";
        let spare = if default_exit { shared.prewarm.lock().unwrap().take(address, start) } else { None };
        let warmed = spare.is_some();
        let result = match (spare, choice.id.as_str()) {
            (Some(stream), _) => Ok(Outbound::Direct(stream)),
            (None, "hy2") => shared.hy2_tcp(address).map(Outbound::Hy2),
            (None, "direct") => TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map(Outbound::Direct),
            _ => connect(address, choice, &shared.running).map(Outbound::Direct),
        };
        // A reused spare has no handshake to measure, so it must not teach the Flow a 0 ms path.
        if !warmed { shared.learning.lock().unwrap().observe(address, choice, start.elapsed(), result.is_ok(), Instant::now()); }
        if result.is_ok() && default_exit && shared.prewarm.lock().unwrap().used(address, Instant::now()) {
            refill(shared, address);
        }
        match result { Ok(stream) => return Ok(stream), Err(error) => last = error }
    }
    Err(last)
}
/// Opens the next spare off the hot path, on a short-lived thread that holds its own Arc.
fn refill(shared: &Arc<Shared>, target: SocketAddr) {
    let shared = Arc::clone(shared);
    let _ = thread::Builder::new().name("rapo-prewarm".into()).spawn(move || {
        if !shared.running.load(Ordering::Acquire) { return; }
        if let Ok(stream) = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
            let _ = stream.set_nodelay(true);
            if shared.running.load(Ordering::Acquire) { shared.prewarm.lock().unwrap().store(target, stream, Instant::now()); }
        }
    });
}
fn filter_egresses(choices: &mut Vec<Egress>, allowed: &[String]) {
    let mut index = 0;
    choices.retain(|choice| {
        let keep = index == 0 || allowed.contains(&choice.authorization);
        index += 1;
        keep
    });
}
fn forward(reader: &mut Outbound, writer: &mut Outbound, counter: &AtomicU64, running: &AtomicBool) -> io::Result<()> {
    let mut buffer = [0; 16 * 1024];
    let result = (|| {
        loop {
            if !running.load(Ordering::Acquire) { return Ok(()); }
            let count = match reader.read(&mut buffer) {
                Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
                Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted) => return Ok(()),
                result => result?,
            };
            if count == 0 { return Ok(()); }
            let mut remaining = &buffer[..count];
            while !remaining.is_empty() {
                if !running.load(Ordering::Acquire) { return Ok(()); }
                let sent = match writer.write(remaining) {
                    Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
                    Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted) => return Ok(()),
                    result => result?,
                };
                if sent == 0 { return Err(io::ErrorKind::WriteZero.into()); }
                counter.fetch_add(sent as u64, Ordering::Relaxed);
                remaining = &remaining[sent..];
            }
        }
    })();
    let _ = writer.shutdown(if result.is_ok() { Shutdown::Write } else { Shutdown::Both });
    if result.is_err() { let _ = reader.shutdown(Shutdown::Both); }
    result
}

enum Outbound { Direct(TcpStream), Hy2(hy2::TcpStream) }
impl Outbound {
    fn try_clone(&self) -> io::Result<Self> {
        match self { Self::Direct(s) => s.try_clone().map(Self::Direct), Self::Hy2(s) => s.try_clone().map(Self::Hy2) }
    }
    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self { Self::Direct(s) => s.shutdown(how), Self::Hy2(s) => s.shutdown(how) }
    }
    /// HY2 has no meaningful local socket address; SOCKS replies 0.0.0.0:0 then.
    fn local_addr(&self) -> io::Result<Option<SocketAddr>> {
        match self { Self::Direct(s) => s.local_addr().map(Some), Self::Hy2(_) => Ok(None) }
    }
    fn set_timeouts(&self) -> io::Result<()> {
        let timeout = Some(Duration::from_millis(250));
        match self {
            Self::Direct(s) => { s.set_nodelay(true)?; s.set_read_timeout(timeout)?; s.set_write_timeout(timeout) }
            Self::Hy2(s) => { s.set_read_timeout(timeout)?; s.set_write_timeout(timeout) }
        }
    }
}
impl Read for Outbound {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self { Self::Direct(s) => s.read(bytes), Self::Hy2(s) => s.read(bytes) }
    }
}
impl Write for Outbound {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self { Self::Direct(s) => s.write(bytes), Self::Hy2(s) => s.write(bytes) }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self { Self::Direct(s) => s.flush(), Self::Hy2(s) => s.flush() }
    }
}
#[cfg(windows)]
use native::{connect, egresses};
#[cfg(not(windows))]
fn egresses(target: SocketAddr) -> io::Result<Vec<Egress>> {
    Ok(vec![Egress { id: "system".into(), authorization: "system".into(), interface: 0, source: SocketAddr::new(if target.is_ipv4() { Ipv4Addr::UNSPECIFIED.into() } else { Ipv6Addr::UNSPECIFIED.into() }, 0), gateway: "system".into() }])
}
#[cfg(not(windows))]
fn connect(target: SocketAddr, _: &Egress, running: &AtomicBool) -> io::Result<TcpStream> {
    if !running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
    TcpStream::connect_timeout(&target, CONNECT_TIMEOUT)
}
