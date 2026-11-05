use super::*;

fn relay() -> RelayServer {
    let mut server = RelayServer::new(0);
    server.start().unwrap();
    server
}
fn client(server: &RelayServer) -> TcpStream {
    let socket = TcpStream::connect(&server.state().address).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    socket.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    socket
}
fn greet(socket: &mut TcpStream) {
    for fragment in [&[5][..], &[2, 2][..], &[0][..]] { socket.write_all(fragment).unwrap(); }
    let mut reply = [0; 2]; socket.read_exact(&mut reply).unwrap(); assert_eq!(reply, [5, 0]);
}
fn request(socket: &mut TcpStream, target: SocketAddr, domain: bool) {
    let mut bytes = vec![5, 1, 0];
    if domain {
        bytes.extend_from_slice(&[3, 9]); bytes.extend_from_slice(b"localhost");
    } else {
        match target.ip() {
            IpAddr::V4(ip) => { bytes.push(1); bytes.extend_from_slice(&ip.octets()); }
            IpAddr::V6(ip) => { bytes.push(4); bytes.extend_from_slice(&ip.octets()); }
        }
    }
    bytes.extend_from_slice(&target.port().to_be_bytes());
    socket.write_all(&bytes).unwrap();
}
fn response(socket: &mut TcpStream) -> (u8, SocketAddr) {
    let mut header = [0; 4]; socket.read_exact(&mut header).unwrap();
    assert_eq!((header[0], header[2]), (5, 0));
    let ip = match header[3] {
        1 => { let mut ip = [0; 4]; socket.read_exact(&mut ip).unwrap(); Ipv4Addr::from(ip).into() }
        4 => { let mut ip = [0; 16]; socket.read_exact(&mut ip).unwrap(); Ipv6Addr::from(ip).into() }
        other => panic!("invalid BND address family {other}"),
    };
    let mut port = [0; 2]; socket.read_exact(&mut port).unwrap();
    (header[1], SocketAddr::new(ip, u16::from_be_bytes(port)))
}
// Generous deadline: under a fully parallel test run, QUIC/TLS startup can exceed a few seconds.
// `track_caller` makes the panic name the wait that expired instead of this helper's own line.
#[track_caller]
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "relay did not reach expected state");
        thread::sleep(Duration::from_millis(5));
    }
}
fn assert_closed(socket: &mut TcpStream) {
    let mut byte = [0];
    match socket.read(&mut byte) {
        Ok(0) => (),
        Err(error) if matches!(error.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted | io::ErrorKind::BrokenPipe) => (),
        result => panic!("expected a closed socket, received {result:?}"),
    }
}

#[test]
fn failed_bind_is_not_running_and_can_be_retried() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();
    let mut server = RelayServer::new(port);
    assert_eq!(server.start().unwrap_err().kind(), io::ErrorKind::AddrInUse);
    assert!(!server.state().running);
    drop(occupied);
    let occupied_udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    assert_eq!(server.start().unwrap_err().kind(), io::ErrorKind::AddrInUse, "a successful start must reserve UDP too");
    assert!(!server.state().running);
    drop(occupied_udp);
    server.start().unwrap();
    let mut socket = client(&server); greet(&mut socket);
    server.stop();
    until(|| { server.poll(); !server.state().stopping });
    assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
}

#[test]
fn ipv4_ipv6_and_dns_forward_binary_payload_and_preserve_half_close_and_bound_address() {
    for (host, domain) in [("127.0.0.1:0", false), ("[::1]:0", false), ("127.0.0.1:0", true)] {
        let listener = TcpListener::bind(host).unwrap();
        let target = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let echo = thread::spawn(move || {
            let (mut socket, peer) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            sender.send(peer).unwrap();
            let mut payload = Vec::new(); socket.read_to_end(&mut payload).unwrap();
            socket.write_all(&payload).unwrap();
            socket.shutdown(Shutdown::Write).unwrap();
        });
        let server = relay(); let mut socket = client(&server);
        greet(&mut socket); request(&mut socket, target, domain);
        let (status, bound) = response(&mut socket); assert_eq!(status, 0);
        assert_eq!(bound, receiver.recv_timeout(Duration::from_secs(5)).unwrap(), "BND must identify the actual upstream socket");
        let payload: Vec<_> = (0..32785).map(|i| (i % 256) as u8).collect();
        socket.write_all(&payload).unwrap(); socket.shutdown(Shutdown::Write).unwrap();
        let mut returned = Vec::new(); socket.read_to_end(&mut returned).unwrap();
        assert_eq!(returned, payload);
        echo.join().unwrap();
        until(|| server.state().active_connections == 0);
        assert_eq!((server.state().bytes_tx, server.state().bytes_rx), (payload.len() as u64, payload.len() as u64));
    }
}

#[test]
fn counters_update_during_live_session_and_stop_closes_both_sides() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target = listener.local_addr().unwrap();
    let echo = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut payload = [0; 7]; socket.read_exact(&mut payload).unwrap(); socket.write_all(&payload).unwrap();
        assert_closed(&mut socket);
    });
    let mut server = relay(); let mut socket = client(&server);
    greet(&mut socket); request(&mut socket, target, false); assert_eq!(response(&mut socket).0, 0);
    socket.write_all(b"\0relay\xff").unwrap();
    let mut echoed = [0; 7]; socket.read_exact(&mut echoed).unwrap(); assert_eq!(&echoed, b"\0relay\xff");
    // Each direction counts after its write returns, so the echo may land before the upload is tallied.
    until(|| { let state = server.state(); state.bytes_rx == 7 && state.bytes_tx == 7 });
    let state = server.state();
    assert_eq!((state.bytes_tx, state.active_connections, state.total_connections), (7, 1, 1));
    assert_eq!(state.predictions.len(), 1);
    server.stop();
    assert_closed(&mut socket);
    until(|| { server.poll(); !server.state().stopping });
    assert!(!server.state().running); assert_eq!(server.state().active_connections, 0);
    echo.join().unwrap();
}

#[test]
fn socks_failures_are_reported_and_recursion_and_unsupported_commands_are_rejected() {
    let server = relay();
    let mut socket = client(&server); socket.write_all(&[5, 1, 2]).unwrap();
    let mut method = [0; 2]; socket.read_exact(&mut method).unwrap(); assert_eq!(method, [5, 255]);
    for (bytes, expected) in [(&[5, 2, 0, 1][..], 7), (&[5, 1, 1, 1][..], 1), (&[5, 1, 0, 99][..], 8), (&[5, 1, 0, 3, 0][..], 8)] {
        let mut socket = client(&server); greet(&mut socket); socket.write_all(bytes).unwrap();
        assert_eq!(response(&mut socket).0, expected);
    }
    let mut socket = client(&server); greet(&mut socket);
    request(&mut socket, server.state().address.parse().unwrap(), false);
    assert_eq!(response(&mut socket).0, 2);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap(); let refused = listener.local_addr().unwrap(); drop(listener);
    let mut socket = client(&server); greet(&mut socket); request(&mut socket, refused, false);
    assert_eq!(response(&mut socket).0, 5);
}

#[test]
fn connections_are_bounded_and_stop_cancels_incomplete_handshakes() {
    let mut server = relay();
    let sockets: Vec<_> = (0..MAX_CONNECTIONS).map(|_| client(&server)).collect();
    until(|| server.state().active_connections == MAX_CONNECTIONS);
    let mut excess = client(&server);
    assert_closed(&mut excess);
    server.stop();
    until(|| { server.poll(); !server.state().stopping });
    assert_eq!(server.state().active_connections, 0);
    drop(sockets);
}

#[test]
fn additional_interfaces_require_exact_authorization_and_default_is_preserved() {
    let exits = || (0..3).map(|index| Egress { id: format!("route{index}"), authorization: format!("luid{index}:gateway"), interface: index,
        source: "192.0.2.1:0".parse().unwrap(), gateway: "192.0.2.254".into() }).collect::<Vec<_>>();
    let mut default = exits(); filter_egresses(&mut default, &[]);
    assert_eq!(default.iter().map(|e| e.interface).collect::<Vec<_>>(), [0]);
    let mut allowed = exits(); filter_egresses(&mut allowed, &["luid2:gateway".into(), "luid1:old-gateway".into()]);
    assert_eq!(allowed.iter().map(|e| e.interface).collect::<Vec<_>>(), [0, 2]);
}

fn push_address(bytes: &mut Vec<u8>, target: SocketAddr) {
    match target.ip() {
        IpAddr::V4(ip) => { bytes.push(1); bytes.extend_from_slice(&ip.octets()); }
        IpAddr::V6(ip) => { bytes.push(4); bytes.extend_from_slice(&ip.octets()); }
    }
    bytes.extend_from_slice(&target.port().to_be_bytes());
}
/// Sends UDP ASSOCIATE for `source`; returns the control stream and the reply.
fn associate(server: &RelayServer, source: SocketAddr) -> (TcpStream, u8, SocketAddr) {
    let mut control = client(server); greet(&mut control);
    let mut bytes = vec![5, 3, 0]; push_address(&mut bytes, source);
    control.write_all(&bytes).unwrap();
    let (status, bound) = response(&mut control);
    (control, status, bound)
}
fn datagram(target: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0]; push_address(&mut bytes, target); bytes.extend_from_slice(payload); bytes
}
fn udp_echo(host: &str) -> SocketAddr {
    let socket = std::net::UdpSocket::bind(host).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let address = socket.local_addr().unwrap();
    thread::spawn(move || {
        let mut buffer = [0; 65535];
        while let Ok((count, from)) = socket.recv_from(&mut buffer) { let _ = socket.send_to(&buffer[..count], from); }
    });
    address
}
fn udp_client() -> std::net::UdpSocket {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    socket
}
/// Returns the SOCKS-wrapped reply's source and payload, or `None` when nothing arrives.
fn udp_reply(socket: &std::net::UdpSocket, wait: Duration) -> Option<(SocketAddr, Vec<u8>)> {
    socket.set_read_timeout(Some(wait)).unwrap();
    let mut buffer = [0; 65535];
    let count = match socket.recv(&mut buffer) {
        Ok(count) => count,
        // Windows reports the ICMP unreachable from a closed relay port as a reset: no reply either way.
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::ConnectionReset) => return None,
        Err(e) => panic!("{e}"),
    };
    assert_eq!(&buffer[..3], [0, 0, 0], "replies never carry RSV/FRAG");
    let (from, start) = match buffer[3] {
        1 => (SocketAddr::new(Ipv4Addr::from(<[u8; 4]>::try_from(&buffer[4..8]).unwrap()).into(), u16::from_be_bytes([buffer[8], buffer[9]])), 10),
        4 => (SocketAddr::new(Ipv6Addr::from(<[u8; 16]>::try_from(&buffer[4..20]).unwrap()).into(), u16::from_be_bytes([buffer[20], buffer[21]])), 22),
        other => panic!("invalid reply family {other}"),
    };
    Some((from, buffer[start..count].to_vec()))
}

#[test]
fn udp_associate_relays_ipv4_ipv6_and_domain_datagrams_until_control_closes() {
    let mut server = relay();
    let (control, status, bound) = associate(&server, "0.0.0.0:0".parse().unwrap());
    assert_eq!(status, 0);
    assert!(bound.ip().is_loopback() && bound.port() != 0);
    let socket = udp_client();
    let resolved = ("localhost", 0).to_socket_addrs().unwrap().next().unwrap();
    let domain_echo = udp_echo(&SocketAddr::new(resolved.ip(), 0).to_string());
    let payload: Vec<u8> = (0..1400u32).map(|i| (i * 7) as u8).collect();
    for echo in [udp_echo("127.0.0.1:0"), udp_echo("[::1]:0")] {
        socket.send_to(&datagram(echo, &payload), bound).unwrap();
        assert_eq!(udp_reply(&socket, Duration::from_secs(5)), Some((echo, payload.clone())));
    }
    let mut named = vec![0, 0, 0, 3, 9]; named.extend_from_slice(b"localhost");
    named.extend_from_slice(&domain_echo.port().to_be_bytes()); named.extend_from_slice(b"named");
    socket.send_to(&named, bound).unwrap();
    assert_eq!(udp_reply(&socket, Duration::from_secs(5)), Some((domain_echo, b"named".to_vec())));
    until(|| server.state().udp_associations == 1 && server.state().bytes_rx == payload.len() as u64 * 2 + 5);
    drop(control);
    until(|| server.state().udp_associations == 0);
    socket.send_to(&datagram(domain_echo, b"late"), bound).unwrap();
    assert_eq!(udp_reply(&socket, Duration::from_millis(300)), None, "closing control ends the association");

    let (mut control, _, bound) = associate(&server, "0.0.0.0:0".parse().unwrap());
    socket.send_to(&datagram(domain_echo, b"x"), bound).unwrap();
    assert!(udp_reply(&socket, Duration::from_secs(5)).is_some());
    server.stop();
    assert_closed(&mut control);
    until(|| { server.poll(); !server.state().stopping });
}

#[test]
fn udp_rejects_foreign_sources_spoofed_ports_fragments_and_reserved_bits() {
    let server = relay();
    let (_, status, _) = associate(&server, "192.0.2.1:9".parse().unwrap());
    assert_eq!(status, 2, "DST.ADDR must match the control connection's host");
    let socket = udp_client();
    let (_control, status, bound) = associate(&server, socket.local_addr().unwrap());
    assert_eq!(status, 0);
    let echo = udp_echo("127.0.0.1:0");
    let spoof = udp_client();
    spoof.send_to(&datagram(echo, b"spoof"), bound).unwrap();
    assert_eq!(udp_reply(&spoof, Duration::from_millis(300)), None, "a different source port is not the client");
    let mut fragment = datagram(echo, b"frag"); fragment[2] = 1;
    let mut reserved = datagram(echo, b"rsv"); reserved[0] = 1;
    for packet in [fragment, reserved] {
        socket.send_to(&packet, bound).unwrap();
        assert_eq!(udp_reply(&socket, Duration::from_millis(300)), None);
    }
    let recursive = datagram(bound, b"loop");
    socket.send_to(&recursive, bound).unwrap();
    assert_eq!(udp_reply(&socket, Duration::from_millis(300)), None, "the relay never targets its own UDP socket");
    socket.send_to(&datagram(echo, b"ok"), bound).unwrap();
    assert_eq!(udp_reply(&socket, Duration::from_secs(5)), Some((echo, b"ok".to_vec())), "rejections leave the association usable");
}

#[test]
fn an_integrated_server_makes_hy2_the_default_mode() {
    assert_eq!(default_mode(true), Mode::Hy2);
    assert_eq!(default_mode(false), Mode::Local, "without an integrated server the previous Flow stays the default");
}

#[test]
fn configuration_requires_a_stopped_relay_and_never_exposes_the_hy2_secret() {
    let mut server = RelayServer::new(0);
    assert_eq!(server.mode(), Mode::Local, "the pre-existing Flow stays the default");
    let secret = Hy2Config { server: "127.0.0.1:443".into(), auth: "do-not-show".into(), ..Hy2Config::default() };
    assert_eq!(server.configure(Mode::Hy2, Hy2Config { auth: String::new(), ..secret.clone() }).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(server.mode(), Mode::Local, "rejected settings change nothing");
    server.configure(Mode::Hy2, secret.clone()).unwrap();
    server.start().unwrap();
    assert_eq!(server.configure(Mode::Direct, Hy2Config::default()).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    let json = serde_json::to_string(&server.state()).unwrap();
    assert!(json.contains("\"mode\":\"hy2\"") && json.contains("127.0.0.1:443") && !json.contains("do-not-show"), "{json}");
    server.stop();
    until(|| { server.poll(); !server.state().stopping });
    server.configure(Mode::Direct, Hy2Config::default()).unwrap();
    assert_eq!(server.mode(), Mode::Direct);
}

#[test]
fn hy2_mode_relays_tcp_and_udp_through_a_real_server_and_rejected_auth_is_never_success() {
    let fixture = hy2::fixture::start();
    let mut server = RelayServer::new(0);
    server.configure(Mode::Hy2, Hy2Config { auth: "wrong".into(), ..fixture.config() }).unwrap();
    server.start().unwrap();
    let target: SocketAddr = fixture.endpoints.tcp_echo.parse().unwrap();
    let mut socket = client(&server); greet(&mut socket); request(&mut socket, target, false);
    assert_eq!(response(&mut socket).0, 2, "HY2 auth failure is reported, not a fake success");
    until(|| { let state = server.state(); !state.hy2_error.is_empty() && !state.hy2_connected });
    server.stop();
    until(|| { server.poll(); !server.state().stopping });

    server.configure(Mode::Hy2, fixture.config()).unwrap();
    server.start().unwrap();
    for _ in 0..2 { // the second flow reuses the cached QUIC connection
        let mut socket = client(&server); greet(&mut socket); request(&mut socket, target, false);
        let (status, bound) = response(&mut socket);
        assert_eq!((status, bound), (0, "0.0.0.0:0".parse().unwrap()));
        let payload: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
        socket.write_all(&payload).unwrap();
        let mut echoed = vec![0; payload.len()]; socket.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, payload);
    }
    until(|| server.state().active_connections == 0);
    assert_eq!(server.state().error, "", "client-closed HY2 flows end cleanly, not as resets");
    let echo: SocketAddr = fixture.endpoints.udp_echo.parse().unwrap();
    let udp = udp_client();
    let (_control, status, bound) = associate(&server, "0.0.0.0:0".parse().unwrap());
    assert_eq!(status, 0);
    udp.send_to(&datagram(echo, b"through hy2"), bound).unwrap();
    assert_eq!(udp_reply(&udp, Duration::from_secs(5)), Some((echo, b"through hy2".to_vec())));
    let predictions = serde_json::to_string(&server.state().predictions).unwrap();
    assert!(predictions.contains("\"gateway\":\"hy2\"") && !predictions.contains("\"gateway\":\"system\""), "HY2 mode has no direct fallback: {predictions}");
}

#[test]
fn embedded_hy2_connects_at_start_and_relays_tcp_and_udp_without_a_server_address() {
    let mut server = RelayServer::new(0);
    server.configure(Mode::Hy2, Hy2Config::default()).unwrap();
    server.start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !server.state().hy2_connected {
        assert!(Instant::now() < deadline, "embedded HY2 never connected; hy2_error = {:?}", server.state().hy2_error);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(server.state().hy2_error, "", "the GUI reports HY2 working before any game connection");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let target = listener.local_addr().unwrap();
    thread::spawn(move || { let (mut c, _) = listener.accept().unwrap(); let mut b = [0; 5]; c.read_exact(&mut b).unwrap(); c.write_all(&b).unwrap(); });
    let mut socket = client(&server); greet(&mut socket); request(&mut socket, target, false);
    assert_eq!(response(&mut socket).0, 0, "TCP CONNECT through the embedded HY2 server");
    socket.write_all(b"hello").unwrap(); let mut reply = [0; 5]; socket.read_exact(&mut reply).unwrap(); assert_eq!(&reply, b"hello");
    let echo = udp_echo("127.0.0.1:0");
    let udp = udp_client();
    let (_control, status, bound) = associate(&server, "0.0.0.0:0".parse().unwrap());
    assert_eq!(status, 0);
    udp.send_to(&datagram(echo, b"embedded hy2"), bound).unwrap();
    assert_eq!(udp_reply(&udp, Duration::from_secs(5)), Some((echo, b"embedded hy2".to_vec())), "UDP through the embedded HY2 server");
    assert_eq!(server.state().udp_route, "hy2");
    server.stop();
    let deadline = Instant::now() + Duration::from_secs(20);
    while { server.poll(); server.state().stopping } {
        assert!(Instant::now() < deadline, "relay still stopping 20 s after stop(): {}", serde_json::to_string(&server.state()).unwrap());
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!server.state().hy2_connected, "a stopped relay never reports HY2 working");
}

#[test]
fn wfp_transparent_header_relays_directly_without_socks5_handshake() {
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let target_addr = target.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut conn, _) = target.accept().unwrap();
        let mut buf = [0; 5];
        conn.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        conn.write_all(b"world").unwrap();
    });

    let server = relay();
    let mut client = client(&server);
    let mut header = vec![0x23, 0xa4, 0xfa, 0x39];
    let ip = match target_addr.ip() { IpAddr::V4(ip) => ip.octets(), _ => unreachable!() };
    header.extend_from_slice(&ip);
    header.extend_from_slice(&target_addr.port().to_le_bytes());
    header.extend_from_slice(&[0, 0, 0, 0]);
    client.write_all(&header).unwrap();
    client.write_all(b"hello").unwrap();

    let mut reply = [0; 5];
    client.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"world");
    worker.join().unwrap();
}

#[test]
fn hot_destination_gets_a_prewarmed_spare_that_a_later_connection_uses() {
    let target = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = target.local_addr().unwrap();
    target.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let echo = thread::spawn(move || {
        let mut held: Vec<TcpStream> = Vec::new();
        while !flag.load(Ordering::Relaxed) {
            if let Ok((conn, _)) = target.accept() { conn.set_nonblocking(true).unwrap(); held.push(conn); }
            for conn in &mut held {
                let mut buf = [0u8; 4];
                if let Ok(4) = conn.read(&mut buf) { let _ = conn.write_all(&buf); }
            }
            thread::sleep(Duration::from_millis(2));
        }
    });
    let server = relay();
    let mut header = vec![0x23, 0xa4, 0xfa, 0x39, 127, 0, 0, 1];
    header.extend_from_slice(&addr.port().to_le_bytes());
    header.extend_from_slice(&[0, 0, 0, 0]);
    for round in 0..4 {
        let mut c = client(&server);
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        c.write_all(&header).unwrap();
        c.write_all(b"ping").unwrap();
        let mut reply = [0u8; 4];
        c.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"ping", "round {round} still relays correctly");
        thread::sleep(Duration::from_millis(150));
    }
    stop.store(true, Ordering::Relaxed);
    echo.join().unwrap();
    assert!(server.state().prewarm_hits >= 1, "a later connection must have reused a spare");
}

#[test]
#[ignore]
fn wfp_udp_round_trip_latency() {
    let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let target = echo.local_addr().unwrap();
    echo.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let stop = Arc::new(AtomicBool::new(false)); let flag = Arc::clone(&stop);
    let t = thread::spawn(move || { let mut b = [0u8; 2048]; while !flag.load(Ordering::Relaxed) { if let Ok((n, from)) = echo.recv_from(&mut b) { let _ = echo.send_to(&b[..n], from); } } });
    let mut server = RelayServer::new(0); server.start().unwrap();
    let port: u16 = server.state().address.rsplit(':').next().unwrap().parse().unwrap();
    let game = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    game.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut pkt = vec![0x23, 0xa4, 0xfa, 0x39, 127, 0, 0, 1];
    pkt.extend_from_slice(&target.port().to_le_bytes()); pkt.extend_from_slice(&[0, 0, 0, 0]); pkt.extend_from_slice(b"skill");
    let mut rtts = Vec::new();
    for _ in 0..200 {
        let s = Instant::now();
        game.send_to(&pkt, (Ipv4Addr::LOCALHOST, port)).unwrap();
        let mut b = [0u8; 2048];
        if game.recv_from(&mut b).is_ok() { rtts.push(s.elapsed().as_micros() as f64 / 1000.0); }
        thread::sleep(Duration::from_millis(7)); // game sends a few times per frame, not back-to-back
    }
    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!("udp via relay: n={} min={:.2} mediana={:.2} p90={:.2} max={:.2} ms", rtts.len(), rtts[0], rtts[rtts.len()/2], rtts[rtts.len()*9/10], rtts[rtts.len()-1]);
    stop.store(true, Ordering::Relaxed); t.join().unwrap();
}

#[test]
fn wfp_udp_replies_reach_the_sender_and_stop_releases_the_flow_threads() {
    let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let target = echo.local_addr().unwrap();
    echo.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let stop = Arc::new(AtomicBool::new(false)); let flag = Arc::clone(&stop);
    let t = thread::spawn(move || { let mut b = [0u8; 2048]; while !flag.load(Ordering::Relaxed) { if let Ok((n, from)) = echo.recv_from(&mut b) { let _ = echo.send_to(&b[..n], from); } } });
    let mut server = RelayServer::new(0); server.start().unwrap();
    let port: u16 = server.state().address.rsplit(':').next().unwrap().parse().unwrap();
    let game = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    game.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut pkt = vec![0x23, 0xa4, 0xfa, 0x39, 127, 0, 0, 1];
    pkt.extend_from_slice(&target.port().to_le_bytes()); pkt.extend_from_slice(&[0, 0, 0, 0]); pkt.extend_from_slice(b"skill");
    game.send_to(&pkt, (Ipv4Addr::LOCALHOST, port)).unwrap();
    let mut b = [0u8; 64];
    let (n, _) = game.recv_from(&mut b).unwrap();
    assert_eq!(&b[..4], &[0x23, 0xa4, 0xfa, 0x39], "reply keeps the WFP framing");
    assert_eq!(&b[8..10], &target.port().to_le_bytes(), "reply names the destination it came from");
    assert_eq!(&b[14..n], b"skill");
    until(|| server.state().udp_destinations == 1);
    until(|| { let state = server.state(); state.bytes_tx == 5 && state.bytes_rx == 5 });
    let state = server.state();
    assert_eq!(state.total_connections, 0, "UDP datagrams are not TCP connections");
    assert_eq!(state.udp_destinations, 1);
    let mut mud_pkt = vec![0x10, 0x8b, 0x6d, 0x7e, 1, 2, 3, 4, 127, 0, 0, 1];
    mud_pkt.extend_from_slice(&target.port().to_be_bytes()); mud_pkt.extend_from_slice(b"cast");
    game.send_to(&mud_pkt, (Ipv4Addr::LOCALHOST, port)).unwrap();
    let (n2, _) = game.recv_from(&mut b).unwrap();
    assert_eq!(&b[..n2], b"cast", "mudwfp_proxy 0x7e6d8b10 UDP expects raw payload reply");
    server.stop();
    until(|| { server.poll(); !server.state().stopping });
    let before = thread_count();
    stop.store(true, Ordering::Relaxed); t.join().unwrap();
    thread::sleep(Duration::from_millis(1500)); // reader threads wake on their 1 s read timeout and see `running == false`
    assert!(thread_count() <= before, "UDP reader threads must not outlive the relay");
}
#[cfg(target_os = "linux")]
fn thread_count() -> usize { std::fs::read_dir("/proc/self/task").unwrap().count() }
#[cfg(not(target_os = "linux"))]
fn thread_count() -> usize { 0 }

#[test]
fn wireguard_profile_relays_tcp_and_udp_through_the_tunnel_and_never_falls_back() {
    let fixture = hy2::fixture::start();
    let mut server = RelayServer::new(0);
    server.set_tunnel(Some(VpnTunnel::Wireguard(wireguard::Tunnel::from_profile(&fixture.wireguard_profile()).unwrap()))).unwrap();
    // A tunnel forces the "everything through the exit" policy even if another mode was chosen.
    server.configure(Mode::Direct, Hy2Config::default()).unwrap();
    server.start().unwrap();
    assert_eq!(server.state().mode, Mode::Hy2, "a VPN profile always runs the no-fallback policy");
    assert_eq!(server.state().tunnel, "WireGuard");

    // The echo lives only on the peer's tunnel address (10.77.0.1): a reply proves the bytes crossed the encrypted tunnel.
    let target: SocketAddr = fixture.endpoints.wg_tcp_echo.parse().unwrap();
    for _ in 0..2 { // the second flow reuses the same tunnel
        let mut socket = client(&server); greet(&mut socket); request(&mut socket, target, false);
        assert_eq!(response(&mut socket).0, 0, "the tunnel must connect to its peer's private address");
        let payload: Vec<u8> = (0..20_000u32).map(|i| (i * 7) as u8).collect();
        socket.write_all(&payload).unwrap();
        let mut echoed = vec![0; payload.len()]; socket.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, payload);
    }
    let udp_target: SocketAddr = fixture.endpoints.wg_udp_echo.parse().unwrap();
    let udp = udp_client();
    let (_control, status, bound) = associate(&server, "0.0.0.0:0".parse().unwrap());
    assert_eq!(status, 0);
    udp.send_to(&datagram(udp_target, b"through wireguard"), bound).unwrap();
    assert_eq!(udp_reply(&udp, Duration::from_secs(10)), Some((udp_target, b"through wireguard".to_vec())));
    let state = serde_json::to_string(&server.state()).unwrap();
    assert!(!state.contains(&fixture.endpoints.wg_private_key) && !state.contains("private_key"), "the key must never reach the GUI state: {state}");
    assert!(state.contains("\"gateway\":\"hy2\"") && !state.contains("\"gateway\":\"system\""), "no direct fallback: {state}");
    server.stop();
    until(|| { server.poll(); !server.state().stopping });
}

#[test]
fn a_destination_outside_the_tunnel_fails_instead_of_leaving_through_the_system_route() {
    let fixture = hy2::fixture::start();
    let mut server = RelayServer::new(0);
    server.set_tunnel(Some(VpnTunnel::Wireguard(wireguard::Tunnel::from_profile(&fixture.wireguard_profile()).unwrap()))).unwrap();
    server.start().unwrap();
    // The real loopback TCP echo of the fixture is reachable from this host, but it is NOT inside the tunnel's
    // allowed IPs (10.77.0.0/24). A VPN must refuse it rather than silently use the system route.
    let outside: SocketAddr = fixture.endpoints.tcp_echo.parse().unwrap();
    let mut socket = client(&server); greet(&mut socket); request(&mut socket, outside, false);
    let status = response(&mut socket).0;
    assert_ne!(status, 0, "a destination outside the tunnel must not connect through the system route");
    server.stop();
    until(|| { server.poll(); !server.state().stopping });
}

#[test]
fn a_tunnel_cannot_be_swapped_while_the_relay_runs_and_clearing_it_restores_the_previous_policy() {
    let fixture = hy2::fixture::start();
    let mut server = RelayServer::new(0);
    server.set_tunnel(Some(VpnTunnel::Wireguard(wireguard::Tunnel::from_profile(&fixture.wireguard_profile()).unwrap()))).unwrap();
    server.start().unwrap();
    assert_eq!(server.set_tunnel(None).unwrap_err().kind(), io::ErrorKind::WouldBlock, "the exit cannot change under live flows");
    assert_eq!(server.state().tunnel, "WireGuard");
    server.stop();
    until(|| { server.poll(); !server.state().stopping });
    server.set_tunnel(None).unwrap();
    assert_eq!((server.state().tunnel.as_str(), server.state().mode), ("", Mode::Local), "no tunnel: the default policy is back");
}

#[test]
fn udp_redundancy_replicates_packets_at_configured_ratio() {
    let recv_sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    recv_sock.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let target_addr = recv_sock.local_addr().unwrap();

    let mut server = RelayServer::new(0);
    server.set_redundancy(RedundancyConfig { enabled: true, x: 2, y: 3 }).unwrap();
    server.start().unwrap();

    let udp = udp_client();
    let (_control, status, bound) = associate(&server, "0.0.0.0:0".parse().unwrap());
    assert_eq!(status, 0);

    // Send 2 original packets (X=2, Y=3 -> total 5 packets expected at target_addr).
    udp.send_to(&datagram(target_addr, b"pkt1"), bound).unwrap();
    udp.send_to(&datagram(target_addr, b"pkt2"), bound).unwrap();

    let mut received = 0;
    let mut buf = [0u8; 64];
    while let Ok((n, _)) = recv_sock.recv_from(&mut buf) {
        assert!(matches!(&buf[..n], b"pkt1" | b"pkt2"));
        received += 1;
        if received == 5 { break; }
    }
    assert_eq!(received, 5, "2 original packets with 2:3 redundancy must arrive as 5 datagrams");

    // Now disable redundancy on the live relay and send 2 more -> only 2 should arrive.
    server.set_redundancy(RedundancyConfig { enabled: false, x: 2, y: 3 }).unwrap();
    udp.send_to(&datagram(target_addr, b"pkt3"), bound).unwrap();
    udp.send_to(&datagram(target_addr, b"pkt4"), bound).unwrap();

    let mut received_after = 0;
    while let Ok((n, _)) = recv_sock.recv_from(&mut buf) {
        assert!(matches!(&buf[..n], b"pkt3" | b"pkt4"));
        received_after += 1;
    }
    assert_eq!(received_after, 2, "disabled redundancy sends 1 datagram per packet");

    server.stop();
    until(|| { server.poll(); !server.state().stopping });
}
