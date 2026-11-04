package main

// Userspace WireGuard tunnel (wireguard-go + gVisor netstack). It implements the same two interfaces the HY2 client
// does, so the bridge registry, deadlines, capacity limits and every exported rapo_hy2_* function work unchanged.
// Nothing here touches the host network stack: no TUN adapter, no routes, no driver and no administrator rights.
// Only the encrypted UDP socket to the peer leaves the process; everything inside the tunnel stays in userspace.

import (
	"context"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/apernet/hysteria/core/v2/client"
	"golang.zx2c4.com/wireguard/conn"
	"golang.zx2c4.com/wireguard/device"
	"golang.zx2c4.com/wireguard/tun/netstack"
)

// wgConfig mirrors the secret-free/secret split of the Rust profile. Fields are the wg-quick names in snake_case.
type wgConfig struct {
	PrivateKey   string `json:"private_key"`
	PublicKey    string `json:"public_key"`
	PresharedKey string `json:"preshared_key"`
	Address      string `json:"address"`
	DNS          string `json:"dns"`
	MTU          string `json:"mtu"`
	Endpoint     string `json:"endpoint"`
	AllowedIPs   string `json:"allowed_ips"`
	Keepalive    string `json:"keepalive"`
}

var errWGConfig = errors.New("invalid WireGuard config")

// keyHex converts a base64 WireGuard key to the lowercase hex the UAPI wants. It accepts only 32-byte keys.
func keyHex(value string) (string, error) {
	raw, err := base64.StdEncoding.DecodeString(strings.TrimSpace(value))
	if err != nil || len(raw) != 32 {
		return "", errWGConfig
	}
	return hex.EncodeToString(raw), nil
}

// resolvePeer turns host:port into ip:port. The UAPI only accepts a literal address, and resolving here (before the
// tunnel exists) uses the system resolver, exactly like any WireGuard client resolving its own endpoint.
func resolvePeer(ctx context.Context, endpoint string) (string, error) {
	host, port, err := net.SplitHostPort(endpoint)
	if err != nil {
		return "", errWGConfig
	}
	if _, err := strconv.ParseUint(port, 10, 16); err != nil {
		return "", errWGConfig
	}
	if ip, err := netip.ParseAddr(host); err == nil {
		return net.JoinHostPort(ip.String(), port), nil
	}
	ips, err := net.DefaultResolver.LookupIPAddr(ctx, host)
	if err != nil || len(ips) == 0 {
		return "", errResolve
	}
	return net.JoinHostPort(ips[0].IP.String(), port), nil
}

func parseAddrs(list string) ([]netip.Addr, error) {
	var out []netip.Addr
	for _, part := range strings.Split(list, ",") {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		prefix, err := netip.ParsePrefix(part)
		if err != nil {
			ip, ipErr := netip.ParseAddr(part)
			if ipErr != nil {
				return nil, errWGConfig
			}
			out = append(out, ip)
			continue
		}
		out = append(out, prefix.Addr())
	}
	return out, nil
}

// wgTunnel is a running WireGuard device plus its userspace network stack.
type wgTunnel struct {
	dev   *device.Device
	net   *netstack.Net
	local netip.Addr // the tunnel's own address: gVisor needs a valid one to choose the IP protocol for a datagram socket
	once  sync.Once
}

// startWireGuard builds the tunnel from the JSON profile. It returns only after the device is up; the handshake with
// the peer happens lazily on the first packet, like the reference implementation.
func startWireGuard(ctx context.Context, data []byte) (*wgTunnel, error) {
	var c wgConfig
	if json.Unmarshal(data, &c) != nil || len(data) > 65536 {
		return nil, errWGConfig
	}
	priv, err := keyHex(c.PrivateKey)
	if err != nil {
		return nil, err
	}
	pub, err := keyHex(c.PublicKey)
	if err != nil {
		return nil, err
	}
	local, err := parseAddrs(c.Address)
	if err != nil || len(local) == 0 {
		return nil, errWGConfig
	}
	dns, err := parseAddrs(c.DNS)
	if err != nil {
		return nil, errWGConfig
	}
	mtu := 1420
	if c.MTU != "" {
		if mtu, err = strconv.Atoi(c.MTU); err != nil || mtu < 576 || mtu > 9000 {
			return nil, errWGConfig
		}
	}
	peer, err := resolvePeer(ctx, c.Endpoint)
	if err != nil {
		return nil, err
	}
	allowed := strings.TrimSpace(c.AllowedIPs)
	if allowed == "" {
		allowed = "0.0.0.0/0,::/0"
	}
	var uapi strings.Builder
	fmt.Fprintf(&uapi, "private_key=%s\npublic_key=%s\nendpoint=%s\nreplace_allowed_ips=true\n", priv, pub, peer)
	for _, prefix := range strings.Split(allowed, ",") {
		prefix = strings.TrimSpace(prefix)
		if _, err := netip.ParsePrefix(prefix); err != nil {
			return nil, errWGConfig
		}
		fmt.Fprintf(&uapi, "allowed_ip=%s\n", prefix)
	}
	if c.PresharedKey != "" {
		psk, err := keyHex(c.PresharedKey)
		if err != nil {
			return nil, err
		}
		fmt.Fprintf(&uapi, "preshared_key=%s\n", psk)
	}
	if c.Keepalive != "" {
		secs, err := strconv.ParseUint(c.Keepalive, 10, 16)
		if err != nil {
			return nil, errWGConfig
		}
		fmt.Fprintf(&uapi, "persistent_keepalive_interval=%d\n", secs)
	}
	tunDev, tnet, err := netstack.CreateNetTUN(local, dns, mtu)
	if err != nil {
		return nil, err
	}
	dev := device.NewDevice(tunDev, conn.NewDefaultBind(), device.NewLogger(device.LogLevelSilent, ""))
	if err := dev.IpcSet(uapi.String()); err != nil {
		dev.Close()
		return nil, errWGConfig
	}
	if err := dev.Up(); err != nil {
		dev.Close()
		return nil, err
	}
	return &wgTunnel{dev: dev, net: tnet, local: local[0]}, nil
}

// TCP dials through the tunnel. A hostname is resolved by the tunnel's DNS, not by the local resolver.
func (t *wgTunnel) TCP(addr string) (net.Conn, error) {
	ctx, cancel := context.WithTimeout(context.Background(), openTimeout)
	defer cancel()
	return t.net.DialContext(ctx, "tcp", addr)
}

func (t *wgTunnel) UDP() (client.HyUDPConn, error) {
	// Port 0 picks an ephemeral port; an empty address would leave the protocol unset and panic inside gVisor.
	conn, err := t.net.ListenUDP(&net.UDPAddr{IP: t.local.AsSlice()})
	if err != nil {
		return nil, err
	}
	return &wgUDP{conn: conn, net: t.net}, nil
}

func (t *wgTunnel) Close() error {
	t.once.Do(func() { t.dev.Close() })
	return nil
}

// wgUDP adapts an unconnected netstack datagram socket to the destination-per-call API of HyUDPConn.
type wgUDP struct {
	conn net.PacketConn
	net  *netstack.Net
}

func (u *wgUDP) Receive() ([]byte, string, error) {
	buffer := make([]byte, 65535)
	n, from, err := u.conn.ReadFrom(buffer)
	if err != nil {
		return nil, "", err
	}
	return buffer[:n], from.String(), nil
}

func (u *wgUDP) Send(data []byte, address string) error {
	host, port, err := net.SplitHostPort(address)
	if err != nil {
		return err
	}
	number, err := strconv.ParseUint(port, 10, 16)
	if err != nil {
		return err
	}
	ip, err := netip.ParseAddr(host)
	if err != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		ips, lookupErr := u.net.LookupContextHost(ctx, host)
		if lookupErr != nil || len(ips) == 0 {
			return errResolve
		}
		if ip, err = netip.ParseAddr(ips[0]); err != nil {
			return err
		}
	}
	_, err = u.conn.WriteTo(data, &net.UDPAddr{IP: ip.AsSlice(), Port: int(number)})
	return err
}

func (u *wgUDP) Close() error { return u.conn.Close() }

var _ client.Client = (*wgTunnel)(nil)
var _ client.HyUDPConn = (*wgUDP)(nil)
