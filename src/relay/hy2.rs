//! In-process HY2 client. The embedded Go library is written to a fresh private
//! file, verified while write-locked, loaded once and never unloaded (Go cannot be).
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::sync::{Arc, LazyLock, atomic::{AtomicBool, AtomicU32, Ordering}};
use std::time::Duration;

static LIBRARY: &[u8] = include_bytes!(env!("RAPO_HY2_EMBED"));
const MAX_WAIT_MS: u32 = 30_000;

/// Session-only HY2 settings. `auth` is a secret: never logged, displayed or persisted.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub server: String,
    pub auth: String,
    pub server_name: String,
    pub ca_pem: String,
}
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config").field("server", &self.server).field("auth", &"<redacted>")
            .field("server_name", &self.server_name).field("ca_pem", &format_args!("{} bytes", self.ca_pem.len())).finish()
    }
}
impl Config {
    pub fn validate(&self) -> io::Result<()> {
        let invalid = |message: &str| Err(io::Error::new(io::ErrorKind::InvalidInput, message.to_owned()));
        let host_ok = |host: &str| !host.is_empty() && host.len() <= 253 && !host.contains(['\0', '\r', '\n', ' ', '/', '\\']);
        let Some((host, port)) = self.server.rsplit_once(':') else { return invalid("Servidor HY2 deve ser host:porta"); };
        let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        if self.server.len() > 512 || !host_ok(host) || port.parse::<u16>().map_or(true, |p| p == 0) {
            return invalid("Servidor HY2 deve ser host:porta");
        }
        if self.auth.is_empty() || self.auth.len() > 4096 || self.auth.contains(['\0', '\r', '\n']) {
            return invalid("Autenticação HY2 ausente ou inválida");
        }
        if !self.server_name.is_empty() && !host_ok(&self.server_name) { return invalid("Nome TLS HY2 inválido"); }
        if self.ca_pem.len() > 65_536 || (!self.ca_pem.is_empty() && !self.ca_pem.contains("-----BEGIN CERTIFICATE-----")) {
            return invalid("CA PEM HY2 inválida");
        }
        Ok(())
    }
}

type Open = unsafe extern "C" fn(u64, *mut u64) -> i32;
type Io = unsafe extern "C" fn(u64, *mut u8, usize, u32, *mut usize) -> i32;
/// Declares `Api` and its loader together; field `x` resolves export `rapo_hy2_x`.
macro_rules! api {
    ($($field:ident: $ty:ty),* $(,)?) => {
        struct Api { $($field: $ty),* }
        unsafe fn load() -> io::Result<Api> {
            let symbol = platform::open()?;
            Ok(Api { $($field: std::mem::transmute::<*const (), $ty>(symbol(concat!("rapo_hy2_", stringify!($field), "\0"))?)),* })
        }
    };
}
api! {
    connect: unsafe extern "C" fn(*const u8, usize, *mut u64) -> i32,
    tcp: unsafe extern "C" fn(u64, *const u8, usize, *mut u64) -> i32,
    udp: Open,
    read: Io,
    write: Io,
    shutdown: unsafe extern "C" fn(u64, i32) -> i32,
    udp_send: unsafe extern "C" fn(u64, *const u8, usize, *const u8, usize) -> i32,
    udp_recv: unsafe extern "C" fn(u64, *mut u8, usize, u32, *mut usize, *mut u8, usize, *mut usize) -> i32,
    close: unsafe extern "C" fn(u64),
}

static API: LazyLock<Result<Api, String>> = LazyLock::new(|| unsafe { load() }.map_err(|e| e.to_string()));
fn api() -> io::Result<&'static Api> {
    API.as_ref().map_err(|e| io::Error::other(format!("Biblioteca HY2 indisponível: {e}")))
}

fn unique_name(extension: &str) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("rapo-hy2-{}-{nanos}.{extension}", std::process::id())
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

    /// Returns a symbol resolver for the loaded library. The module is intentionally leaked.
    pub(super) unsafe fn open() -> io::Result<impl Fn(&str) -> io::Result<*const ()>> {
        // %LOCALAPPDATA% is per-user ACL-protected; a fresh create_new name defeats planted files.
        let base = std::env::var_os("LOCALAPPDATA").ok_or_else(|| io::Error::other("LOCALAPPDATA ausente"))?;
        let directory = std::path::PathBuf::from(base).join("Rintel RAPO").join("hy2");
        std::fs::create_dir_all(&directory)?;
        if let Ok(entries) = std::fs::read_dir(&directory) {
            // Libraries still loaded by other instances stay locked and survive this sweep.
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("rapo-hy2-") { let _ = std::fs::remove_file(entry.path()); }
            }
        }
        let path = directory.join(unique_name("dll"));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        file.write_all(LIBRARY)?;
        file.sync_all()?;
        drop(file);
        // Deny writers and deletion until the image section is mapped, then verify the bytes.
        let mut lock = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path)?;
        let mut written = Vec::with_capacity(LIBRARY.len());
        lock.read_to_end(&mut written)?;
        if written != LIBRARY { return Err(io::Error::other("conteúdo extraído divergente")); }
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let module = LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
        drop(lock);
        if module.is_null() { return Err(io::Error::last_os_error()); }
        Ok(move |name: &str| GetProcAddress(module, name.as_ptr()).map(|f| f as *const ())
            .ok_or_else(|| io::Error::other(format!("símbolo ausente {}", name.trim_end_matches('\0')))))
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::ffi::{c_char, c_int, c_void};
    use std::os::unix::{ffi::OsStrExt, fs::{DirBuilderExt, OpenOptionsExt}};
    extern "C" {
        fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    const RTLD_NOW: c_int = 2;

    pub(super) unsafe fn open() -> io::Result<impl Fn(&str) -> io::Result<*const ()>> {
        // A fresh 0700 directory (non-recursive create fails on pre-existing paths).
        let directory = std::env::temp_dir().join(unique_name("d"));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let path = directory.join("librapo_hy2.so");
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o500).open(&path)?;
        file.write_all(LIBRARY)?;
        drop(file);
        let c_path: Vec<u8> = path.as_os_str().as_bytes().iter().copied().chain([0]).collect();
        let module = dlopen(c_path.as_ptr().cast(), RTLD_NOW);
        if module.is_null() { return Err(io::Error::other("dlopen falhou")); }
        let module = module as usize;
        Ok(move |name: &str| {
            let address = dlsym(module as *mut c_void, name.as_ptr().cast());
            if address.is_null() { Err(io::Error::other(format!("símbolo ausente {}", name.trim_end_matches('\0')))) } else { Ok(address as *const ()) }
        })
    }
}

fn error(code: i32) -> io::Error {
    use io::ErrorKind::*;
    let (kind, message) = match code {
        -1 => (InvalidInput, "Entrada HY2 inválida"),
        -2 => (ConnectionAborted, "Sessão HY2 encerrada"),
        -3 => (TimedOut, "Tempo limite HY2"),
        -4 => (Other, "Limite de sessões HY2 atingido"),
        -6 => (PermissionDenied, "Autenticação HY2 recusada"),
        -7 => (InvalidData, "Certificado TLS do servidor HY2 não confiável"),
        -8 => (NotFound, "Servidor HY2 não resolvido"),
        -9 => (Other, "Falha interna HY2"),
        -10 => (ConnectionRefused, "Servidor HY2 não conseguiu conectar ao destino"),
        -11 => (InvalidData, "Datagrama HY2 excede o buffer"),
        -12 => (ConnectionReset, "Fluxo HY2 reiniciado pelo servidor"),
        _ => (Other, "Falha de conexão HY2"),
    };
    io::Error::new(kind, message)
}
fn check(code: i32) -> io::Result<()> { if code == 0 { Ok(()) } else { Err(error(code)) } }
fn wait_ms(timeout: Option<Duration>) -> io::Result<u32> {
    match timeout {
        None => Ok(0),
        Some(d) if d.is_zero() => Err(io::Error::new(io::ErrorKind::InvalidInput, "timeout zero")),
        Some(d) => Ok(d.as_millis().clamp(1, MAX_WAIT_MS as u128) as u32),
    }
}

/// Closes its Go handle on drop; closing a client also closes its children.
struct Handle(u64);
impl Drop for Handle {
    fn drop(&mut self) { if let Ok(api) = api() { unsafe { (api.close)(self.0) } } }
}

/// One authenticated QUIC connection. Clones share it; it closes with its last owner.
#[derive(Clone)]
pub struct Client(Arc<Handle>);
impl Client {
    /// Blocking TLS + auth handshake bounded to 3 s; call from a worker thread only.
    pub fn connect(config: &Config) -> io::Result<Self> {
        config.validate()?;
        let api = api()?;
        let json = serde_json::to_vec(config).map_err(io::Error::other)?;
        let mut id = 0;
        check(unsafe { (api.connect)(json.as_ptr(), json.len(), &mut id) })?;
        Ok(Self(Arc::new(Handle(id))))
    }
    /// Opens a stream; success means the server already connected to `target` (`host:port`).
    pub fn tcp(&self, target: &str) -> io::Result<TcpStream> {
        let api = api()?;
        let mut id = 0;
        check(unsafe { (api.tcp)(self.0 .0, target.as_ptr(), target.len(), &mut id) })?;
        Ok(TcpStream { stream: Arc::new(Stream { handle: Handle(id), write_closed: AtomicBool::new(false) }), _client: self.clone(),
            read_ms: AtomicU32::new(0), write_ms: AtomicU32::new(0) })
    }
    pub fn udp(&self) -> io::Result<UdpSession> {
        let api = api()?;
        let mut id = 0;
        check(unsafe { (api.udp)(self.0 .0, &mut id) })?;
        Ok(UdpSession { handle: Handle(id), _client: self.clone(), read_ms: AtomicU32::new(250) })
    }
}

/// Shared by clones of one stream.
struct Stream { handle: Handle, write_closed: AtomicBool }
/// HY2 TCP stream with `std::net::TcpStream`-like timeout and shutdown semantics.
pub struct TcpStream { stream: Arc<Stream>, _client: Client, read_ms: AtomicU32, write_ms: AtomicU32 }
impl TcpStream {
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { stream: Arc::clone(&self.stream), _client: self._client.clone(),
            read_ms: AtomicU32::new(self.read_ms.load(Ordering::Relaxed)), write_ms: AtomicU32::new(self.write_ms.load(Ordering::Relaxed)) })
    }
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> { self.read_ms.store(wait_ms(timeout)?, Ordering::Relaxed); Ok(()) }
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> { self.write_ms.store(wait_ms(timeout)?, Ordering::Relaxed); Ok(()) }
    /// `Write` sends FIN; `Read`/`Both` discard inbound data and wake blocked readers.
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        if how != Shutdown::Read { self.stream.write_closed.store(true, Ordering::Release); }
        let how = match how { Shutdown::Read => 1, Shutdown::Write => 2, Shutdown::Both => 3 };
        match unsafe { (api()?.shutdown)(self.stream.handle.0, how) } { 0 | -2 => Ok(()), code => Err(error(code)) }
    }
    fn io(&self, call: Io, buffer: *mut u8, length: usize, timeout: &AtomicU32) -> io::Result<usize> {
        loop {
            let ms = timeout.load(Ordering::Relaxed);
            let mut count = 0;
            match unsafe { call(self.stream.handle.0, buffer, length, if ms == 0 { MAX_WAIT_MS } else { ms }, &mut count) } {
                0 => return Ok(count),
                1 => return Ok(0),
                -3 if ms == 0 => continue, // no timeout configured: keep blocking
                -3 => return Err(io::ErrorKind::WouldBlock.into()),
                // Upstream servers reset the stream after our FIN instead of half-closing: that is its normal end.
                -12 if self.stream.write_closed.load(Ordering::Acquire) => return Ok(0),
                code => return Err(error(code)),
            }
        }
    }
}
impl Read for TcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() { return Ok(0); }
        self.io(api()?.read, buffer.as_mut_ptr(), buffer.len(), &self.read_ms)
    }
}
impl Write for TcpStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() { return Ok(0); }
        // The Go side copies before writing and never writes through this pointer.
        self.io(api()?.write, buffer.as_ptr().cast_mut(), buffer.len(), &self.write_ms)
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

/// HY2 UDP session; datagrams keep their boundaries and are never truncated.
pub struct UdpSession { handle: Handle, _client: Client, read_ms: AtomicU32 }
impl UdpSession {
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> { self.read_ms.store(wait_ms(timeout)?, Ordering::Relaxed); Ok(()) }
    pub fn send_to(&self, data: &[u8], target: &str) -> io::Result<usize> {
        check(unsafe { (api()?.udp_send)(self.handle.0, data.as_ptr(), data.len(), target.as_ptr(), target.len()) })?;
        Ok(data.len())
    }
    /// Waits up to the read timeout (250 ms by default); returns the sender as `host:port`.
    pub fn recv_from(&self, buffer: &mut [u8]) -> io::Result<(usize, String)> {
        let api = api()?;
        let mut address = [0u8; 300];
        loop {
            let ms = self.read_ms.load(Ordering::Relaxed);
            let (mut count, mut length) = (0, 0);
            let code = unsafe { (api.udp_recv)(self.handle.0, buffer.as_mut_ptr(), buffer.len(), if ms == 0 { MAX_WAIT_MS } else { ms },
                &mut count, address.as_mut_ptr(), address.len(), &mut length) };
            match code {
                0 => return Ok((count, String::from_utf8_lossy(&address[..length]).into_owned())),
                -3 if ms == 0 => continue,
                -3 => return Err(io::ErrorKind::WouldBlock.into()),
                code => return Err(error(code)),
            }
        }
    }
}

#[cfg(test)]
pub(super) mod fixture {
    use std::io::BufRead;
    use std::process::{Child, Command, Stdio};

    #[derive(serde::Deserialize)]
    pub struct Endpoints { pub server: String, pub ca_pem: String, pub auth: String, pub tcp_echo: String, pub udp_echo: String, pub udp_echo6: String }
    /// Real upstream HY2 server (native/hy2/fixture). Exits when its stdin closes.
    pub struct Fixture { child: Child, pub endpoints: Endpoints }
    impl Drop for Fixture { fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); } }
    impl Fixture {
        pub fn config(&self) -> super::Config {
            super::Config { server: self.endpoints.server.clone(), auth: self.endpoints.auth.clone(), server_name: String::new(), ca_pem: self.endpoints.ca_pem.clone() }
        }
    }
    pub fn start() -> Fixture {
        static BINARY: std::sync::LazyLock<std::path::PathBuf> = std::sync::LazyLock::new(|| {
            // Prebuilt fixture for hosts without Go (pairs with RAPO_HY2_LIBRARY).
            if let Some(prebuilt) = std::env::var_os("RAPO_HY2_FIXTURE") { return prebuilt.into(); }
            let path = std::env::temp_dir().join(format!("rapo-hy2-fixture-{}{}", std::process::id(), std::env::consts::EXE_SUFFIX));
            let status = Command::new(std::env::var_os("GO").unwrap_or("go".into())).args(["build", "-o"]).arg(&path).arg("./fixture")
                .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/native/hy2")).status().expect("Go toolchain is required for HY2 interop tests");
            assert!(status.success(), "building the HY2 fixture failed");
            path
        });
        let mut child = Command::new(&*BINARY).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        Fixture { endpoints: serde_json::from_str(&line).expect("fixture endpoints"), child }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn config_validation_and_debug_never_expose_the_secret() {
        let config = Config { server: "hy2.example:443".into(), auth: "s3cret-token".into(), ..Config::default() };
        config.validate().unwrap();
        assert!(!format!("{config:?}").contains("s3cret-token"));
        for broken in [Config { server: "hy2.example".into(), ..config.clone() }, Config { server: "hy2.example:0".into(), ..config.clone() },
            Config { auth: String::new(), ..config.clone() }, Config { auth: "a\nb".into(), ..config.clone() },
            Config { ca_pem: "not pem".into(), ..config.clone() }, Config { server_name: "bad name".into(), ..config.clone() }] {
            let error = broken.validate().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(!error.to_string().contains("s3cret-token"));
        }
        Config { server: "[2001:db8::1]:443".into(), ..config }.validate().unwrap();
    }

    #[test]
    fn tcp_and_udp_round_trip_through_a_real_hy2_server() {
        let fixture = fixture::start();
        let client = Client::connect(&fixture.config()).unwrap();
        let mut stream = client.tcp(&fixture.endpoints.tcp_echo).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        stream.write_all(b"rapo-hy2-tcp").unwrap();
        let mut echoed = [0u8; 12];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"rapo-hy2-tcp");
        // Upstream servers reset the stream after FIN (no TCP half-close); that ends it cleanly, promptly.
        stream.shutdown(Shutdown::Write).unwrap();
        let ended = Instant::now();
        assert_eq!(stream.read(&mut echoed).unwrap(), 0);
        assert!(ended.elapsed() < Duration::from_secs(4));

        let session = client.udp().unwrap();
        session.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        for target in [&fixture.endpoints.udp_echo, &fixture.endpoints.udp_echo6].into_iter().filter(|t| !t.is_empty()) {
            let payload: Vec<u8> = (0..1200u32).map(|i| i as u8).collect();
            session.send_to(&payload, target).unwrap();
            let mut buffer = [0u8; 65535];
            let (count, from) = session.recv_from(&mut buffer).unwrap();
            assert_eq!((&buffer[..count], from.as_str()), (&payload[..], target.as_str()));
        }
    }

    #[test]
    fn rejects_untrusted_certificates_and_wrong_credentials() {
        let fixture = fixture::start();
        let untrusted = Client::connect(&Config { ca_pem: String::new(), ..fixture.config() }).err().expect("system roots must reject the fixture CA");
        assert_eq!(untrusted.kind(), io::ErrorKind::InvalidData);
        let wrong_name = Client::connect(&Config { server_name: "example.invalid".into(), ..fixture.config() }).err().unwrap();
        assert_eq!(wrong_name.kind(), io::ErrorKind::InvalidData);
        let denied = Client::connect(&Config { auth: "wrong".into(), ..fixture.config() }).err().unwrap();
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
        assert!(!denied.to_string().contains("wrong"));
    }

    #[test]
    fn shutdown_from_another_owner_unblocks_a_reader_without_timeout() {
        let fixture = fixture::start();
        let client = Client::connect(&fixture.config()).unwrap();
        let mut stream = client.tcp(&fixture.endpoints.tcp_echo).unwrap();
        let control = stream.try_clone().unwrap();
        let started = Instant::now();
        let reader = std::thread::spawn(move || stream.read(&mut [0; 16]));
        std::thread::sleep(Duration::from_millis(200));
        control.shutdown(Shutdown::Both).unwrap();
        let _ = reader.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(client);
        assert!(control.try_clone().unwrap().write(b"x").is_err(), "stream is unusable after abort");
    }
}
