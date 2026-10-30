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
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
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
    until(|| server.state().bytes_rx == 7);
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
    for (bytes, expected) in [(&[5, 3, 0, 1][..], 7), (&[5, 1, 1, 1][..], 1), (&[5, 1, 0, 99][..], 8), (&[5, 1, 0, 3, 0][..], 8)] {
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
