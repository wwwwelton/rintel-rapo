use super::{CONNECT_TIMEOUT, Egress};
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::os::windows::io::{AsRawSocket, FromRawSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Networking::WinSock::*;

pub(super) fn egresses(target: SocketAddr) -> io::Result<Vec<Egress>> {
    crate::routes::native::egresses(target).map(|exits| exits.into_iter().map(|(route, source)| Egress {
        id: route.id, authorization: route.candidate, interface: route.interface, source, gateway: route.gateway,
    }).collect()).map_err(io::Error::other)
}
fn address(socket: SocketAddr) -> (SOCKADDR_INET, i32) {
    unsafe {
        let mut value: SOCKADDR_INET = std::mem::zeroed();
        let length = match socket {
            SocketAddr::V4(socket) => {
                value.Ipv4.sin_family = AF_INET;
                value.Ipv4.sin_port = socket.port().to_be();
                value.Ipv4.sin_addr.S_un.S_addr = u32::from_ne_bytes(socket.ip().octets());
                std::mem::size_of::<SOCKADDR_IN>()
            }
            SocketAddr::V6(socket) => {
                value.Ipv6.sin6_family = AF_INET6;
                value.Ipv6.sin6_port = socket.port().to_be();
                value.Ipv6.sin6_addr.u.Byte = socket.ip().octets();
                value.Ipv6.Anonymous.sin6_scope_id = socket.scope_id();
                std::mem::size_of::<SOCKADDR_IN6>()
            }
        };
        (value, length as i32)
    }
}
fn last_error() -> io::Error { io::Error::from_raw_os_error(unsafe { WSAGetLastError() }) }

pub(super) fn connect(target: SocketAddr, exit: &Egress, running: &AtomicBool) -> io::Result<TcpStream> {
    if target.is_ipv4() != exit.source.is_ipv4() || exit.source.ip().is_unspecified() || exit.interface == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Saída SOCKS5 sem interface/origem válida"));
    }
    // The listener initializes Winsock before any client worker can reach this function.
    let raw = unsafe { WSASocketW(if target.is_ipv4() { AF_INET } else { AF_INET6 } as i32,
        SOCK_STREAM, IPPROTO_TCP, std::ptr::null(), 0, WSA_FLAG_NO_HANDLE_INHERIT | WSA_FLAG_OVERLAPPED) };
    if raw == INVALID_SOCKET { return Err(last_error()); }
    let socket = unsafe { TcpStream::from_raw_socket(raw as _) };
    let (level, option, index) = if target.is_ipv4() { (IPPROTO_IP, IP_UNICAST_IF, exit.interface.to_be()) }
        else { (IPPROTO_IPV6, IPV6_UNICAST_IF, exit.interface) };
    if unsafe { setsockopt(raw, level, option, std::ptr::addr_of!(index).cast(), std::mem::size_of_val(&index) as i32) } == SOCKET_ERROR {
        return Err(last_error());
    }
    let (local, length) = address(exit.source);
    if unsafe { bind(raw, std::ptr::addr_of!(local).cast(), length) } == SOCKET_ERROR { return Err(last_error()); }
    socket.set_nonblocking(true)?;
    let (remote, length) = address(target);
    let result = unsafe { windows_sys::Win32::Networking::WinSock::connect(raw, std::ptr::addr_of!(remote).cast(), length) };
    if result == SOCKET_ERROR {
        let error = last_error();
        if !matches!(error.raw_os_error(), Some(WSAEWOULDBLOCK | WSAEINPROGRESS)) { return Err(error); }
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            if !running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
            let mut fd = WSAPOLLFD { fd: socket.as_raw_socket() as _, events: POLLWRNORM, revents: 0 };
            let ready = unsafe { WSAPoll(&mut fd, 1, remaining.min(Duration::from_millis(50)).as_millis().max(1) as i32) };
            if ready == SOCKET_ERROR { return Err(last_error()); }
            if ready == 0 { continue; }
            if let Some(error) = socket.take_error()? { return Err(error); }
            break;
        }
    }
    socket.set_nonblocking(false)?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, TcpListener};
    #[test]
    fn binds_selected_source_and_interface_and_never_silently_ignores_invalid_bind() {
        for host in ["127.0.0.1:0", "[::1]:0"] {
            let listener = TcpListener::bind(host).unwrap();
            let target = listener.local_addr().unwrap();
            let exit = egresses(target).unwrap().remove(0);
            let running = AtomicBool::new(true);
            let stream = connect(target, &exit, &running).unwrap();
            assert_eq!(stream.local_addr().unwrap().ip(), exit.source.ip());
            let raw = stream.as_raw_socket() as _;
            let mut index = 0u32; let mut length = 4;
            let (level, option) = if target.is_ipv4() { (IPPROTO_IP, IP_UNICAST_IF) } else { (IPPROTO_IPV6, IPV6_UNICAST_IF) };
            assert_eq!(unsafe { getsockopt(raw, level, option, std::ptr::addr_of_mut!(index).cast(), &mut length) }, 0);
            assert_eq!(index, exit.interface, "Windows reports the selected outgoing interface");
            let mut invalid = exit.clone();
            invalid.source = SocketAddr::new(if target.is_ipv4() { "192.0.2.123".parse::<IpAddr>().unwrap() } else { "2001:db8::123".parse().unwrap() }, 0);
            assert!(connect(target, &invalid, &running).is_err(), "invalid source must not fall back to an unbound socket");
        }
    }
}
