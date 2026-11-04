package main

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/netip"
	"strings"
	"testing"
	"time"

	"golang.org/x/crypto/curve25519"
	"golang.zx2c4.com/wireguard/conn"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun/netstack"
)

type keyPair struct{ private, public [32]byte }

func newKeyPair(t *testing.T) keyPair {
	var k keyPair
	if _, err := rand.Read(k.private[:]); err != nil {
		t.Fatal(err)
	}
	k.private[0] &= 248
	k.private[31] = (k.private[31] & 127) | 64
	pub, err := curve25519.X25519(k.private[:], curve25519.Basepoint)
	if err != nil {
		t.Fatal(err)
	}
	copy(k.public[:], pub)
	return k
}

func b64(b [32]byte) string { return base64.StdEncoding.EncodeToString(b[:]) }

// testServer is a real WireGuard peer: its own device, its own userspace stack and a loopback UDP socket.
// It answers TCP and UDP echo on its tunnel address, so a byte only comes back if it crossed the encrypted tunnel.
type testServer struct {
	dev      *device.Device
	port     int
	tunnelIP netip.Addr
	keys     keyPair
}

func startTestServer(t *testing.T, clientPublic [32]byte, psk string) *testServer {
	keys := newKeyPair(t)
	tunnelIP := netip.MustParseAddr("10.77.0.1")
	tunDev, tnet, err := netstack.CreateNetTUN([]netip.Addr{tunnelIP}, nil, 1420)
	if err != nil {
		t.Fatal(err)
	}
	dev := device.NewDevice(tunDev, conn.NewDefaultBind(), device.NewLogger(device.LogLevelSilent, ""))
	extra := ""
	if psk != "" {
		raw, _ := base64.StdEncoding.DecodeString(psk)
		extra = "preshared_key=" + hex.EncodeToString(raw) + "\n"
	}
	if err := dev.IpcSet(fmt.Sprintf("private_key=%s\nlisten_port=0\npublic_key=%s\nallowed_ip=10.77.0.2/32\n%s",
		hex.EncodeToString(keys.private[:]), hex.EncodeToString(clientPublic[:]), extra)); err != nil {
		t.Fatal(err)
	}
	if err := dev.Up(); err != nil {
		t.Fatal(err)
	}
	// The kernel picked the UDP port; read it back from the device.
	cfg, err := dev.IpcGet()
	if err != nil {
		t.Fatal(err)
	}
	var port int
	for _, line := range strings.Split(cfg, "\n") {
		if _, err := fmt.Sscanf(line, "listen_port=%d", &port); err == nil {
			break
		}
	}
	if port == 0 {
		t.Fatalf("no listen_port in %q", cfg)
	}
	tcp, err := tnet.ListenTCP(&net.TCPAddr{Port: 7})
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		for {
			c, err := tcp.Accept()
			if err != nil {
				return
			}
			go func() { defer c.Close(); _, _ = io.Copy(c, c) }()
		}
	}()
	udp, err := tnet.ListenUDP(&net.UDPAddr{Port: 7})
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		buf := make([]byte, 2048)
		for {
			n, from, err := udp.ReadFrom(buf)
			if err != nil {
				return
			}
			_, _ = udp.WriteTo(buf[:n], from)
		}
	}()
	t.Cleanup(func() { tcp.Close(); udp.Close(); dev.Close() })
	return &testServer{dev: dev, port: port, tunnelIP: tunnelIP, keys: keys}
}

func clientConfig(server *testServer, client keyPair, extra map[string]string) []byte {
	cfg := map[string]string{
		"private_key": b64(client.private), "public_key": b64(server.keys.public),
		"address": "10.77.0.2/32", "endpoint": fmt.Sprintf("127.0.0.1:%d", server.port),
		"allowed_ips": "10.77.0.0/24", "keepalive": "1",
	}
	for k, v := range extra {
		cfg[k] = v
	}
	data, _ := json.Marshal(cfg)
	return data
}

func TestWireGuardTunnelCarriesTCPAndUDPEndToEnd(t *testing.T) {
	clientKeys := newKeyPair(t)
	server := startTestServer(t, clientKeys.public, "")
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	tunnel, err := startWireGuard(ctx, clientConfig(server, clientKeys, nil))
	if err != nil {
		t.Fatal(err)
	}
	defer tunnel.Close()

	// TCP: the payload must come back from the server's *tunnel* address, so it crossed the encrypted tunnel.
	c, err := tunnel.TCP(net.JoinHostPort(server.tunnelIP.String(), "7"))
	if err != nil {
		t.Fatalf("tcp through tunnel: %v", err)
	}
	_ = c.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := c.Write([]byte("ping-through-wireguard")); err != nil {
		t.Fatal(err)
	}
	got := make([]byte, len("ping-through-wireguard"))
	if _, err := io.ReadFull(c, got); err != nil || string(got) != "ping-through-wireguard" {
		t.Fatalf("tcp echo: %q %v", got, err)
	}
	c.Close()

	// UDP: one datagram out, the echo comes back from the same tunnel address and port.
	u, err := tunnel.UDP()
	if err != nil {
		t.Fatal(err)
	}
	defer u.Close()
	if err := u.Send([]byte("udp-through-wireguard"), net.JoinHostPort(server.tunnelIP.String(), "7")); err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() {
		data, from, err := u.Receive()
		if err == nil && (string(data) != "udp-through-wireguard" || from != net.JoinHostPort(server.tunnelIP.String(), "7")) {
			err = fmt.Errorf("udp echo %q from %s", data, from)
		}
		done <- err
	}()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("udp echo timed out")
	}
}

func TestWireGuardWithPresharedKeyAndAFailingPeer(t *testing.T) {
	clientKeys := newKeyPair(t)
	psk := b64(newKeyPair(t).private)
	server := startTestServer(t, clientKeys.public, psk)
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	// Correct preshared key: works.
	tunnel, err := startWireGuard(ctx, clientConfig(server, clientKeys, map[string]string{"preshared_key": psk}))
	if err != nil {
		t.Fatal(err)
	}
	c, err := tunnel.TCP(net.JoinHostPort(server.tunnelIP.String(), "7"))
	if err != nil {
		t.Fatalf("matching psk must connect: %v", err)
	}
	c.Close()
	tunnel.Close()
	// Wrong preshared key: the handshake is never accepted, so the dial must fail (not hang forever and not succeed).
	wrong, err := startWireGuard(ctx, clientConfig(server, clientKeys, map[string]string{"preshared_key": b64(newKeyPair(t).private)}))
	if err != nil {
		t.Fatal(err)
	}
	defer wrong.Close()
	start := time.Now()
	if c, err := wrong.TCP(net.JoinHostPort(server.tunnelIP.String(), "7")); err == nil {
		c.Close()
		t.Fatal("a wrong preshared key must not produce a working tunnel")
	}
	if time.Since(start) > openTimeout+2*time.Second {
		t.Fatalf("failure took %v; it must be bounded by openTimeout", time.Since(start))
	}
}

func TestWireGuardConfigBoundaries(t *testing.T) {
	good := newKeyPair(t)
	base := map[string]string{"private_key": b64(good.private), "public_key": b64(good.public), "address": "10.0.0.2/32", "endpoint": "127.0.0.1:51820"}
	with := func(k, v string) []byte {
		m := map[string]string{}
		for a, b := range base {
			m[a] = b
		}
		m[k] = v
		d, _ := json.Marshal(m)
		return d
	}
	ctx := context.Background()
	for name, data := range map[string][]byte{
		"not json":         []byte("{"),
		"short key":        with("private_key", "AAAA"),
		"bad base64":       with("public_key", "!!!!"),
		"no address":       with("address", ""),
		"bad address":      with("address", "not-an-ip"),
		"mtu too small":    with("mtu", "100"),
		"mtu not number":   with("mtu", "x"),
		"endpoint no port": with("endpoint", "127.0.0.1"),
		"endpoint port 0x": with("endpoint", "127.0.0.1:99999"),
		"bad allowed ip":   with("allowed_ips", "10.0.0.0/99"),
		"bad keepalive":    with("keepalive", "-1"),
		"bad psk":          with("preshared_key", "short"),
	} {
		if tunnel, err := startWireGuard(ctx, data); err == nil {
			tunnel.Close()
			t.Errorf("%s: must be rejected", name)
		}
	}
	// Oversized input is rejected before any parsing work.
	if tunnel, err := startWireGuard(ctx, make([]byte, 70000)); err == nil {
		tunnel.Close()
		t.Error("oversized config must be rejected")
	}
}

// The bridge layer below the C ABI: register/lookup/openTCP/closeHandle with a WireGuard owner. Slot accounting
// matters because the cap is shared with HY2 clients: a leak would eventually refuse every new connection.
func TestWireGuardOwnerLifecycleThroughTheBridge(t *testing.T) {
	clientKeys := newKeyPair(t)
	server := startTestServer(t, clientKeys.public, "")
	before := len(clientSlots)
	o, err := connectWireGuard(clientConfig(server, clientKeys, nil))
	if err != nil {
		t.Fatal(err)
	}
	if len(clientSlots) != before+1 {
		t.Fatalf("an owner must take one shared slot: %d -> %d", before, len(clientSlots))
	}
	id, err := register(newEntry(o))
	if err != nil {
		t.Fatal(err)
	}
	e, err := lookup(id)
	if err != nil {
		t.Fatal(err)
	}
	child, err := openTCP(e, net.JoinHostPort(server.tunnelIP.String(), "7"))
	if err != nil {
		t.Fatalf("openTCP through a WireGuard owner: %v", err)
	}
	childID, err := register(child)
	if err != nil {
		t.Fatal(err)
	}
	_ = child.tcp.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := child.tcp.Write([]byte("bridge")); err != nil {
		t.Fatal(err)
	}
	got := make([]byte, 6)
	if _, err := io.ReadFull(child.tcp, got); err != nil || string(got) != "bridge" {
		t.Fatalf("echo through the bridge: %q %v", got, err)
	}
	// An address the bridge validation rejects never reaches the tunnel.
	if _, err := openTCP(e, "not an address"); err == nil {
		t.Fatal("an invalid address must be rejected before dialing")
	}
	// Closing the client closes its children and releases the slot exactly once (idempotent).
	closeHandle(id)
	closeHandle(id)
	if _, err := lookup(childID); err == nil {
		t.Fatal("a child must be unusable after its client closed")
	}
	if _, err := child.tcp.Write([]byte("x")); err == nil {
		t.Fatal("the child stream must be closed with its client")
	}
	if len(clientSlots) != before {
		t.Fatalf("the slot must be released once: %d, want %d", len(clientSlots), before)
	}
}

func TestWireGuardFailedStartReleasesItsSlot(t *testing.T) {
	before := len(clientSlots)
	for i := 0; i < 3; i++ {
		if _, err := connectWireGuard([]byte(`{"private_key":"x"}`)); err == nil {
			t.Fatal("a bad config must fail")
		}
	}
	if len(clientSlots) != before {
		t.Fatalf("failed starts leaked slots: %d, want %d", len(clientSlots), before)
	}
}
