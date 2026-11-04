package main

// A real userspace WireGuard peer for the relay tests: its own device and network stack, a loopback UDP socket, and
// TCP/UDP echoes that exist only on its tunnel address. A byte comes back only if it crossed the encrypted tunnel.
// Test-only; never shipped.

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"io"
	"net"
	"net/netip"
	"strings"

	"golang.org/x/crypto/curve25519"
	"golang.zx2c4.com/wireguard/conn"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun/netstack"
)

type wgKeys struct{ private, public [32]byte }

func newWGKeys() wgKeys {
	var k wgKeys
	must(rand.Read(k.private[:]))
	k.private[0] &= 248
	k.private[31] = (k.private[31] & 127) | 64
	copy(k.public[:], must(curve25519.X25519(k.private[:], curve25519.Basepoint)))
	return k
}

func b64(b [32]byte) string { return base64.StdEncoding.EncodeToString(b[:]) }

// startWireGuardPeer returns the client profile (the JSON fields the app's profile uses) and a stop function.
func startWireGuardPeer() (map[string]string, func()) {
	server, client := newWGKeys(), newWGKeys()
	tunnelIP := netip.MustParseAddr("10.77.0.1")
	tunDev, tnet, err := netstack.CreateNetTUN([]netip.Addr{tunnelIP}, nil, 1420)
	if err != nil {
		panic(err)
	}
	dev := device.NewDevice(tunDev, conn.NewDefaultBind(), device.NewLogger(device.LogLevelSilent, ""))
	if err := dev.IpcSet(fmt.Sprintf("private_key=%s\nlisten_port=0\npublic_key=%s\nallowed_ip=10.77.0.2/32\n",
		hex.EncodeToString(server.private[:]), hex.EncodeToString(client.public[:]))); err != nil {
		panic(err)
	}
	if err := dev.Up(); err != nil {
		panic(err)
	}
	port := 0
	for _, line := range strings.Split(must(dev.IpcGet()), "\n") {
		if _, err := fmt.Sscanf(line, "listen_port=%d", &port); err == nil {
			break
		}
	}
	tcp := must(tnet.ListenTCP(&net.TCPAddr{Port: 7}))
	go func() {
		for {
			c, err := tcp.Accept()
			if err != nil {
				return
			}
			go func() { defer c.Close(); _, _ = io.Copy(c, c) }()
		}
	}()
	udp := must(tnet.ListenUDP(&net.UDPAddr{IP: tunnelIP.AsSlice(), Port: 7}))
	go func() {
		buffer := make([]byte, 2048)
		for {
			n, from, err := udp.ReadFrom(buffer)
			if err != nil {
				return
			}
			_, _ = udp.WriteTo(buffer[:n], from)
		}
	}()
	return map[string]string{
		"wg_private_key": b64(client.private), "wg_public_key": b64(server.public),
		"wg_address": "10.77.0.2/32", "wg_endpoint": fmt.Sprintf("127.0.0.1:%d", port),
		"wg_allowed_ips": "10.77.0.0/24", "wg_keepalive": "1", "wg_tcp_echo": net.JoinHostPort(tunnelIP.String(), "7"),
		"wg_udp_echo": net.JoinHostPort(tunnelIP.String(), "7"),
	}, func() {
		_ = tcp.Close()
		_ = udp.Close()
		dev.Close()
	}
}
