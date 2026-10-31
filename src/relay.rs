//! Explicit local SOCKS5 TCP/UDP relay; no interception or payload rewriting.
mod hy2;
mod learning;
mod udp;
#[cfg(windows)]
mod native;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
pub use hy2::Config as Hy2Config;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock, atomic::{AtomicBool, AtomicU64, Ordering}};
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
    pub hy2_server_name: String,
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
    pub error: String,
    pub predictions: Vec<learning::Prediction>,
    pub interfaces: Vec<crate::routes::Candidate>,
    pub allowed_interfaces: Vec<String>,
}

struct Connection { id: u64, client: TcpStream, outbound: Option<Outbound>, udp_bound: Option<SocketAddr> }
struct Shared {
    mode: Mode,
    config: Hy2Config,
    udp_destinations: AtomicU64,
    running: AtomicBool,
    connections: Mutex<Vec<Connection>>,
    total: AtomicU64,
    bytes_tx: AtomicU64,
    bytes_rx: AtomicU64,
    error: Mutex<String>,
    learning: Mutex<learning::Learning>,
    interfaces: Mutex<Vec<crate::routes::Candidate>>,
    allowed_interfaces: RwLock<Vec<String>>,
    /// One authenticated HY2 connection shared by all flows, opened lazily on a worker.
    hy2: Mutex<Option<hy2::Client>>,
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
        let client = hy2::Client::connect(&self.config)?;
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

pub struct RelayServer { port: u16, run: Option<Run>, error: String, mode: Mode, config: Hy2Config }
impl RelayServer {
    pub fn new(port: u16) -> Self { Self { port, run: None, error: String::new(), mode: Mode::default(), config: Hy2Config::default() } }
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
        let shared = Arc::new(Shared {
            mode: self.mode, config: self.config.clone(), udp_destinations: AtomicU64::new(0),
            running: AtomicBool::new(true), connections: Mutex::new(Vec::new()), total: AtomicU64::new(0),
            bytes_tx: AtomicU64::new(0), bytes_rx: AtomicU64::new(0), error: Mutex::new(String::new()),
            learning: Mutex::new(learning::Learning::default()),
            interfaces: Mutex::new(Vec::new()), allowed_interfaces: RwLock::new(Vec::new()), hy2: Mutex::new(None),
        });
        let state = Arc::clone(&shared);
        let worker = thread::Builder::new().name("rapo-socks5".into()).spawn(move || serve(listener, state, address))?;
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
            return State { mode: self.mode, hy2_server: self.config.server.clone(), hy2_server_name: self.config.server_name.clone(),
                udp_route: self.udp_route().into(), address: format!("127.0.0.1:{}", self.port), error: self.error.clone(), ..State::default() };
        };
        let running = run.shared.running.load(Ordering::Acquire) && !run.worker.as_ref().is_some_and(JoinHandle::is_finished);
        // One guard: a second lock inside the State literal would self-deadlock.
        let (active_connections, udp_associations) = {
            let connections = run.shared.connections.lock().unwrap();
            (connections.len(), connections.iter().filter(|c| c.udp_bound.is_some()).count())
        };
        State {
            mode: self.mode, hy2_server: self.config.server.clone(), hy2_server_name: self.config.server_name.clone(),
            udp_associations, active_connections,
            udp_destinations: run.shared.udp_destinations.load(Ordering::Relaxed),
            udp_route: self.udp_route().into(),
            running, stopping: !running && run.worker.as_ref().is_some_and(|w| !w.is_finished()),
            address: run.address.to_string(),
            total_connections: run.shared.total.load(Ordering::Relaxed),
            bytes_tx: run.shared.bytes_tx.load(Ordering::Relaxed), bytes_rx: run.shared.bytes_rx.load(Ordering::Relaxed),
            error: if self.error.is_empty() { run.shared.error.lock().unwrap().clone() } else { self.error.clone() },
            predictions: run.shared.learning.lock().unwrap().predictions(Instant::now()),
            interfaces: run.shared.interfaces.lock().unwrap().clone(),
            allowed_interfaces: run.shared.allowed_interfaces.read().unwrap().clone(),
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

fn serve(listener: TcpListener, shared: Arc<Shared>, address: SocketAddr) {
    #[cfg(windows)]
    match crate::routes::native::candidates() {
        Ok(interfaces) => *shared.interfaces.lock().unwrap() = interfaces,
        Err(error) => *shared.error.lock().unwrap() = format!("Listar interfaces SOCKS5: {error}"),
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
                if state.running.load(Ordering::Acquire) { *state.error.lock().unwrap() = format!("Última conexão SOCKS5: {error}"); }
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
    for worker in workers { let _ = worker.join(); }
}
struct ConnectionGuard { shared: Arc<Shared>, id: u64 }
impl Drop for ConnectionGuard {
    fn drop(&mut self) { self.shared.connections.lock().unwrap().retain(|c| c.id != self.id); }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Target { Address(SocketAddr), Name(String, u16) }
fn handle(mut client: TcpStream, shared: &Shared, id: u64, listen: SocketAddr) -> io::Result<()> {
    client.set_read_timeout(Some(Duration::from_millis(250)))?;
    client.set_write_timeout(Some(Duration::from_millis(250)))?;
    client.set_nodelay(true)?;
    let handshake = Handshake { running: &shared.running, deadline: Instant::now() + HANDSHAKE_TIMEOUT };
    let mut greeting = [0; 2];
    handshake.read(&mut client, &mut greeting)?;
    if greeting[0] != 5 { return Err(io::ErrorKind::InvalidData.into()); }
    let mut methods = [0; 255];
    let count = greeting[1] as usize;
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
    let mut outbound = match connect_target(&target, shared, listen) {
        Ok(stream) => stream,
        Err(error) => { reply(&mut client, failure_code(&error), None)?; return Err(error); }
    };
    shared.register_outbound(id, &outbound)?;
    reply(&mut client, 0, outbound.local_addr()?)?;
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
fn connect_target(target: &Target, shared: &Shared, listen: SocketAddr) -> io::Result<Outbound> {
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
        let result = match choice.id.as_str() {
            "hy2" => shared.hy2_tcp(address).map(Outbound::Hy2),
            "direct" => TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map(Outbound::Direct),
            _ => connect(address, choice, &shared.running).map(Outbound::Direct),
        };
        shared.learning.lock().unwrap().observe(address, choice, start.elapsed(), result.is_ok(), Instant::now());
        match result { Ok(stream) => return Ok(stream), Err(error) => last = error }
    }
    Err(last)
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
                result => result?,
            };
            if count == 0 { return Ok(()); }
            let mut remaining = &buffer[..count];
            while !remaining.is_empty() {
                if !running.load(Ordering::Acquire) { return Ok(()); }
                let sent = match writer.write(remaining) {
                    Err(e) if matches!(e.kind(), io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
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
