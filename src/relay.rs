//! Local, explicit SOCKS5 CONNECT relay. Never intercepts a process or rewrites payloads.
mod learning;
#[cfg(windows)]
mod native;
#[cfg(test)]
mod tests;

use serde::Serialize;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_CONNECTIONS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

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

struct Connection { id: u64, client: TcpStream, outbound: Option<TcpStream> }
struct Shared {
    running: AtomicBool,
    connections: Mutex<Vec<Connection>>,
    total: AtomicU64,
    bytes_tx: AtomicU64,
    bytes_rx: AtomicU64,
    error: Mutex<String>,
    learning: Mutex<learning::Learning>,
    interfaces: Mutex<Vec<crate::routes::Candidate>>,
    allowed_interfaces: RwLock<Vec<String>>,
}
impl Shared {
    fn close_connections(&self) {
        for connection in self.connections.lock().unwrap().iter() {
            let _ = connection.client.shutdown(Shutdown::Both);
            if let Some(outbound) = &connection.outbound { let _ = outbound.shutdown(Shutdown::Both); }
        }
    }
    fn register_outbound(&self, id: u64, socket: &TcpStream) -> io::Result<()> {
        let mut connections = self.connections.lock().unwrap();
        if !self.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
        let connection = connections.iter_mut().find(|c| c.id == id).ok_or(io::ErrorKind::Interrupted)?;
        connection.outbound = Some(socket.try_clone()?);
        Ok(())
    }
}
struct Run { shared: Arc<Shared>, worker: Option<JoinHandle<()>>, address: SocketAddr }

pub struct RelayServer { port: u16, run: Option<Run>, error: String }
impl RelayServer {
    pub fn new(port: u16) -> Self { Self { port, run: None, error: String::new() } }

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
            running: AtomicBool::new(true), connections: Mutex::new(Vec::new()), total: AtomicU64::new(0),
            bytes_tx: AtomicU64::new(0), bytes_rx: AtomicU64::new(0), error: Mutex::new(String::new()),
            learning: Mutex::new(learning::Learning::default()),
            interfaces: Mutex::new(Vec::new()), allowed_interfaces: RwLock::new(Vec::new()),
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
            return State { address: format!("127.0.0.1:{}", self.port), error: self.error.clone(), ..State::default() };
        };
        let running = run.shared.running.load(Ordering::Acquire) && !run.worker.as_ref().is_some_and(JoinHandle::is_finished);
        State {
            running, stopping: !running && run.worker.as_ref().is_some_and(|w| !w.is_finished()),
            address: run.address.to_string(), active_connections: run.shared.connections.lock().unwrap().len(),
            total_connections: run.shared.total.load(Ordering::Relaxed),
            bytes_tx: run.shared.bytes_tx.load(Ordering::Relaxed), bytes_rx: run.shared.bytes_rx.load(Ordering::Relaxed),
            error: if self.error.is_empty() { run.shared.error.lock().unwrap().clone() } else { self.error.clone() },
            predictions: run.shared.learning.lock().unwrap().predictions(Instant::now()),
            interfaces: run.shared.interfaces.lock().unwrap().clone(),
            allowed_interfaces: run.shared.allowed_interfaces.read().unwrap().clone(),
        }
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
        connections.push(Connection { id, client: control, outbound: None });
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
    if request[1] != 1 { return reply(&mut client, 7, None); }
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
    let result = match target {
        Target::Address(target) => connect_to(target, shared, listen),
        Target::Name(name, port) => (name.as_str(), port).to_socket_addrs().and_then(|targets| {
            let mut last_error = io::Error::new(io::ErrorKind::NotFound, "DNS sem endereço utilizável");
            for target in targets {
                if !shared.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
                match connect_to(target, shared, listen) { Ok(stream) => return Ok(stream), Err(e) => last_error = e }
            }
            Err(last_error)
        }),
    };
    let mut outbound = match result {
        Ok(stream) => stream,
        Err(error) => { reply(&mut client, failure_code(&error), None)?; return Err(error); }
    };
    shared.register_outbound(id, &outbound)?;
    reply(&mut client, 0, Some(outbound.local_addr()?))?;
    // Windows shutdown on a duplicated socket does not cancel another handle's blocked recv.
    for socket in [&client, &outbound] {
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        socket.set_write_timeout(Some(Duration::from_millis(250)))?;
    }
    outbound.set_nodelay(true)?;
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
fn connect_to(target: SocketAddr, shared: &Shared, listen: SocketAddr) -> io::Result<TcpStream> {
    let ip = match target.ip() { IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(target.ip()), ip => ip };
    if target.port() == 0 || ip.is_unspecified() || ip.is_multicast()
        || (target.port() == listen.port() && ip.is_loopback()) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Destino SOCKS5 inválido ou recursivo"));
    }
    if !shared.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
    let mut choices = egresses(target)?;
    filter_egresses(&mut choices, &shared.allowed_interfaces.read().unwrap());
    let index = shared.learning.lock().unwrap().choose(target, &choices, Instant::now());
    let choice = &choices[index];
    let start = Instant::now();
    let result = connect(target, choice, &shared.running);
    shared.learning.lock().unwrap().observe(target, choice, start.elapsed(), result.is_ok(), Instant::now());
    result
}
fn filter_egresses(choices: &mut Vec<Egress>, allowed: &[String]) {
    let mut index = 0;
    choices.retain(|choice| {
        let keep = index == 0 || allowed.contains(&choice.authorization);
        index += 1;
        keep
    });
}
fn forward(reader: &mut TcpStream, writer: &mut TcpStream, counter: &AtomicU64, running: &AtomicBool) -> io::Result<()> {
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
