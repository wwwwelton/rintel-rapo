use super::*;
use std::net::UdpSocket;

const MAX_DESTINATIONS: usize = 32;
const POLL: Duration = Duration::from_millis(5);

struct Destination { requested: Target, address: SocketAddr, socket: Option<UdpSocket> }
struct Destinations<'a> { entries: Vec<Destination>, count: &'a AtomicU64 }
impl Drop for Destinations<'_> {
    fn drop(&mut self) { self.count.fetch_sub(self.entries.len() as u64, Ordering::Relaxed); }
}

pub(super) fn associate(mut control: TcpStream, shared: &Shared, id: u64, source: Target, listen: SocketAddr) -> io::Result<()> {
    let peer = control.peer_addr()?;
    let setup = (|| {
        let expected = match source {
            Target::Address(address) if address.ip().is_unspecified() || address.ip() == peer.ip() => SocketAddr::new(peer.ip(), address.port()),
            Target::Name(name, port) if (name.as_str(), port).to_socket_addrs()?.any(|a| a.ip() == peer.ip()) => SocketAddr::new(peer.ip(), port),
            _ => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Origem UDP diferente da conexão de controle TCP")),
        };
        let socket = UdpSocket::bind(SocketAddr::new(control.local_addr()?.ip(), 0))?;
        socket.set_nonblocking(true)?;
        // The route is fixed for the whole association; it never switches mid-flow.
        let tunnel = if shared.mode == Mode::Hy2 {
            let session = match shared.hy2_client()?.udp() {
                Err(e) if e.kind() == io::ErrorKind::ConnectionAborted => {
                    shared.hy2.lock().unwrap().take();
                    shared.hy2_client()?.udp()?
                }
                result => result?,
            };
            session.set_read_timeout(Some(POLL))?;
            Some(session)
        } else { None };
        Ok((socket, expected, tunnel))
    })();
    let (socket, expected, tunnel) = match setup {
        Ok(value) => value,
        Err(error) => { reply(&mut control, failure_code(&error), None)?; return Err(error); }
    };
    let bound = socket.local_addr()?;
    {
        let mut connections = shared.connections.lock().unwrap();
        if !shared.running.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
        let connection = connections.iter_mut().find(|c| c.id == id).ok_or(io::ErrorKind::Interrupted)?;
        connection.udp_bound = Some(bound);
    }
    reply(&mut control, 0, Some(bound))?;
    control.set_nonblocking(true)?;
    let mut pinned = (expected.port() != 0).then_some(expected);
    let mut destinations = Destinations { entries: Vec::new(), count: &shared.udp_destinations };
    let mut packet = [0u8; 65535];
    let mut returned = [0u8; 65535];
    let mut wrapped = Vec::with_capacity(65535);
    // UDP has no general RTT signal. Neither TCP-open observations nor send time choose its route.
    while shared.running.load(Ordering::Acquire) {
        if !control_alive(&mut control)? { break; }
        for _ in 0..16 {
            let (length, sender) = match socket.recv_from(&mut packet) {
                Ok(value) => value,
                Err(e) if transient(&e) || icmp_unreachable(&e) => break,
                Err(e) => return Err(e),
            };
            if sender.ip() != expected.ip() || pinned.is_some_and(|source| source != sender) { continue; }
            let Some((target, payload)) = decode(&packet[..length]) else { continue; };
            let index = match destinations.entries.iter().position(|d| d.requested == target) {
                Some(index) => index,
                None => {
                    if destinations.entries.len() >= MAX_DESTINATIONS { continue; }
                    let destination = match destination(&target, shared, listen, bound, sender, tunnel.is_some()) {
                        Ok(destination) => destination,
                        Err(_) => continue,
                    };
                    destinations.entries.push(destination);
                    destinations.count.fetch_add(1, Ordering::Relaxed);
                    destinations.entries.len() - 1
                }
            };
            pinned = Some(sender);
            if !shared.running.load(Ordering::Acquire) || !control_alive(&mut control)? { return Ok(()); }
            let destination = &destinations.entries[index];
            let sent = if let Some(tunnel) = &tunnel {
                tunnel.send_to(payload, &destination.address.to_string())
            } else { destination.socket.as_ref().unwrap().send(payload) };
            match sent {
                Ok(count) if count == payload.len() => { shared.bytes_tx.fetch_add(count as u64, Ordering::Relaxed); }
                Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
                Err(e) if transient(&e) || icmp_unreachable(&e) => (),
                Err(e) => return Err(e),
            }
        }
        if let Some(tunnel) = &tunnel {
            match tunnel.recv_from(&mut returned) {
                Ok((count, from)) => {
                    if let Ok(from) = from.parse::<SocketAddr>() {
                        if destinations.entries.iter().any(|d| d.address == from) {
                            send_reply(&socket, pinned, from, &returned[..count], &mut wrapped, shared, &mut control)?;
                        }
                    }
                }
                Err(e) if transient(&e) => (),
                Err(e) => return Err(e),
            }
        } else {
            for destination in &destinations.entries {
                match destination.socket.as_ref().unwrap().recv_from(&mut returned) {
                    Ok((count, from)) if from == destination.address => {
                        send_reply(&socket, pinned, from, &returned[..count], &mut wrapped, shared, &mut control)?;
                    }
                    Ok(_) => (),
                    Err(e) if transient(&e) || icmp_unreachable(&e) => (),
                    Err(e) => return Err(e),
                }
            }
            thread::sleep(POLL);
        }
    }
    Ok(())
}

fn control_alive(control: &mut TcpStream) -> io::Result<bool> {
    match control.read(&mut [0; 1]) {
        Ok(_) => Ok(false), // ASSOCIATE control carries no application data; EOF terminates it.
        Err(e) if transient(&e) => Ok(true),
        Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted) => Ok(false),
        Err(e) => Err(e),
    }
}
fn transient(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
}
/// A queued ICMP unreachable (Windows reports it as a reset on the next UDP call) drops one datagram, not the association.
fn icmp_unreachable(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset)
}
fn destination(target: &Target, shared: &Shared, listen: SocketAddr, bound: SocketAddr, source: SocketAddr, tunneled: bool) -> io::Result<Destination> {
    let address = match target {
        Target::Address(address) => *address,
        Target::Name(name, port) => (name.as_str(), *port).to_socket_addrs()?.next().ok_or(io::ErrorKind::NotFound)?,
    };
    validate_target(address, listen)?;
    if address == bound || address == source || shared.connections.lock().unwrap().iter().any(|c| c.udp_bound == Some(address)) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Destino UDP recursivo"));
    }
    let socket = if tunneled { None } else {
        let socket = if shared.mode == Mode::Local {
            let mut choices = egresses(address)?;
            filter_egresses(&mut choices, &shared.allowed_interfaces.read().unwrap());
            let choice = choices.first().ok_or(io::ErrorKind::NetworkUnreachable)?;
            #[cfg(windows)]
            let socket = native::udp_socket(address, choice)?;
            #[cfg(not(windows))]
            let socket = UdpSocket::bind(choice.source)?;
            socket
        } else {
            UdpSocket::bind(system_egress(address).source)?
        };
        socket.connect(address)?;
        socket.set_nonblocking(true)?;
        Some(socket)
    };
    Ok(Destination { requested: target.clone(), address, socket })
}
fn send_reply(socket: &UdpSocket, client: Option<SocketAddr>, from: SocketAddr, payload: &[u8], wrapped: &mut Vec<u8>, shared: &Shared, control: &mut TcpStream) -> io::Result<()> {
    let Some(client) = client else { return Ok(()); };
    if !shared.running.load(Ordering::Acquire) || !control_alive(control)? { return Ok(()); }
    wrapped.clear();
    wrapped.extend_from_slice(&[0, 0, 0]);
    match from.ip() {
        IpAddr::V4(ip) => { wrapped.push(1); wrapped.extend_from_slice(&ip.octets()); }
        IpAddr::V6(ip) => { wrapped.push(4); wrapped.extend_from_slice(&ip.octets()); }
    }
    wrapped.extend_from_slice(&from.port().to_be_bytes());
    wrapped.extend_from_slice(payload);
    // A UDP datagram must never be split or silently truncated to fit SOCKS framing.
    if wrapped.len() > if client.is_ipv4() { 65507 } else { 65527 } { return Ok(()); }
    match socket.send_to(wrapped, client) {
        Ok(count) if count == wrapped.len() => { shared.bytes_rx.fetch_add(payload.len() as u64, Ordering::Relaxed); Ok(()) }
        Ok(_) => Err(io::ErrorKind::WriteZero.into()),
        Err(e) if transient(&e) || icmp_unreachable(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

fn decode(packet: &[u8]) -> Option<(Target, &[u8])> {
    if packet.get(..3)? != [0, 0, 0] { return None; }
    let (target, end) = match *packet.get(3)? {
        1 => {
            let ip = Ipv4Addr::from(<[u8; 4]>::try_from(packet.get(4..8)?).ok()?);
            let port = u16::from_be_bytes(packet.get(8..10)?.try_into().ok()?);
            (Target::Address(SocketAddr::new(ip.into(), port)), 10)
        }
        4 => {
            let ip = Ipv6Addr::from(<[u8; 16]>::try_from(packet.get(4..20)?).ok()?);
            let port = u16::from_be_bytes(packet.get(20..22)?.try_into().ok()?);
            (Target::Address(SocketAddr::new(ip.into(), port)), 22)
        }
        3 => {
            let length = *packet.get(4)? as usize;
            let name = std::str::from_utf8(packet.get(5..5 + length)?).ok()?;
            if name.is_empty() || name.contains('\0') { return None; }
            let port = u16::from_be_bytes(packet.get(5 + length..7 + length)?.try_into().ok()?);
            (Target::Name(name.into(), port), 7 + length)
        }
        _ => return None,
    };
    Some((target, packet.get(end..)?))
}
