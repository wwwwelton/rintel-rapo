package main

// Userspace OpenVPN tunnel (github.com/n0madic/go-openvpn + pkg/netstack). Like wireguard.go, it implements the
// client.Client and client.HyUDPConn interfaces, so the bridge registry, deadlines and every exported rapo_hy2_*
// function work unchanged.
// No TUN adapter, no driver, no routes and no administrator rights: traffic leaves the process only as the outer
// encrypted UDP/TCP socket to the OpenVPN server. Everything inside stays in userspace.

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"net"
	"net/netip"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/apernet/hysteria/core/v2/client"
	"github.com/n0madic/go-openvpn"
	"github.com/n0madic/go-openvpn/pkg/netstack"
	"gvisor.dev/gvisor/pkg/tcpip/adapters/gonet"
	"gvisor.dev/gvisor/pkg/tcpip/network/ipv4"
)

type ovpnConfig struct {
	Remote       string `json:"remote"`
	Port         string `json:"port"`
	Proto        string `json:"proto"`
	Cipher       string `json:"cipher"`
	Auth         string `json:"auth"`
	CA           string `json:"ca"`
	Cert         string `json:"cert"`
	Key          string `json:"key"`
	Username     string `json:"username"`
	Password     string `json:"password"`
	TLSAuth      string `json:"tls_auth"`
	TLSCrypt     string `json:"tls_crypt"`
	TLSCryptV2   string `json:"tls_crypt_v2"`
	KeyDirection string `json:"key_direction"`
}

var errOVPNConfig = errors.New("invalid OpenVPN config")

func buildTLSConfig(c *ovpnConfig, serverName string) (*tls.Config, error) {
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM([]byte(c.CA)) {
		return nil, errOVPNConfig
	}
	tlsConfig := &tls.Config{RootCAs: roots, ServerName: serverName}
	if c.Cert != "" && c.Key != "" {
		cert, err := tls.X509KeyPair([]byte(c.Cert), []byte(c.Key))
		if err != nil {
			return nil, errOVPNConfig
		}
		tlsConfig.Certificates = []tls.Certificate{cert}
	}
	return tlsConfig, nil
}

type ovpnTunnel struct {
	cli  *openvpn.Client
	net  *netstack.Net
	once sync.Once
}

func startOpenVPN(ctx context.Context, data []byte) (*ovpnTunnel, error) {
	var c ovpnConfig
	if json.Unmarshal(data, &c) != nil || len(data) > 65536 {
		return nil, errOVPNConfig
	}
	if strings.TrimSpace(c.Remote) == "" || strings.TrimSpace(c.CA) == "" {
		return nil, errOVPNConfig
	}
	port := strings.TrimSpace(c.Port)
	if port == "" {
		port = "1194"
	}
	if _, err := strconv.ParseUint(port, 10, 16); err != nil {
		return nil, errOVPNConfig
	}
	remote := net.JoinHostPort(strings.TrimSpace(c.Remote), port)
	proto := strings.ToLower(strings.TrimSpace(c.Proto))
	if proto == "" {
		proto = "udp"
	}
	tlsCfg, err := buildTLSConfig(&c, strings.TrimSpace(c.Remote))
	if err != nil {
		return nil, err
	}
	cfg := &openvpn.Config{
		Network:    proto,
		RemoteAddr: remote,
		TLSConfig:  tlsCfg,
		Username:   c.Username,
		Password:   c.Password,
		Auth:       c.Auth,
	}
	if c.Cipher != "" {
		cfg.Ciphers = []string{strings.ToUpper(strings.TrimSpace(c.Cipher))}
	}
	if c.TLSCrypt != "" {
		cfg.TLSCryptV1 = []byte(c.TLSCrypt)
	} else if c.TLSCryptV2 != "" {
		cfg.TLSCryptV2 = []byte(c.TLSCryptV2)
	} else if c.TLSAuth != "" {
		cfg.TLSAuth = []byte(c.TLSAuth)
		if c.KeyDirection == "0" {
			cfg.KeyDirection = 0
		} else {
			cfg.KeyDirection = 1
		}
	}
	cfg.HandshakeTimeout = openTimeout
	cli, err := openvpn.Dial(ctx, cfg)
	if err != nil {
		return nil, err
	}
	ns, err := netstack.New(cli)
	if err != nil {
		_ = cli.Close()
		return nil, err
	}
	return &ovpnTunnel{cli: cli, net: ns}, nil
}

func (t *ovpnTunnel) TCP(addr string) (net.Conn, error) {
	ctx, cancel := context.WithTimeout(context.Background(), openTimeout)
	defer cancel()
	return t.net.DialContext(ctx, "tcp", addr)
}

func (t *ovpnTunnel) UDP() (client.HyUDPConn, error) {
	conn, err := gonet.DialUDP(t.net.Stack(), nil, nil, ipv4.ProtocolNumber)
	if err != nil {
		return nil, err
	}
	return &ovpnUDP{conn: conn, net: t.net}, nil
}

func (t *ovpnTunnel) Close() error {
	t.once.Do(func() { _ = t.cli.Close() })
	return nil
}

type ovpnUDP struct {
	conn net.PacketConn
	net  *netstack.Net
}

func (u *ovpnUDP) Receive() ([]byte, string, error) {
	buffer := make([]byte, 65535)
	n, from, err := u.conn.ReadFrom(buffer)
	if err != nil {
		return nil, "", err
	}
	return buffer[:n], from.String(), nil
}

func (u *ovpnUDP) Send(data []byte, address string) error {
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
		ips, lookupErr := net.DefaultResolver.LookupIPAddr(ctx, host)
		if lookupErr != nil || len(ips) == 0 {
			return errResolve
		}
		ip, err = netip.ParseAddr(ips[0].IP.String())
		if err != nil {
			return err
		}
	}
	_, err = u.conn.WriteTo(data, &net.UDPAddr{IP: ip.AsSlice(), Port: int(number)})
	return err
}

func (u *ovpnUDP) Close() error { return u.conn.Close() }

var _ client.Client = (*ovpnTunnel)(nil)
var _ client.HyUDPConn = (*ovpnUDP)(nil)
