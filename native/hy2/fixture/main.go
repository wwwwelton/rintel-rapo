// Test-only HY2 server fixture; never shipped. It prints one JSON line with its
// endpoints and exits when stdin closes, so a crashed test cannot leak it.
package main

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"io"
	"math/big"
	"net"
	"os"
	"time"

	"github.com/apernet/hysteria/core/v2/server"
)

const secret = "rapo-fixture-secret"

type auth struct{}

func (auth) Authenticate(_ net.Addr, given string, _ uint64) (bool, string) {
	return given == secret, "fixture"
}

func must[T any](value T, err error) T {
	if err != nil {
		panic(err)
	}
	return value
}

func certificate() (tls.Certificate, string) {
	key := must(ecdsa.GenerateKey(elliptic.P256(), rand.Reader))
	template := &x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "rapo hy2 fixture"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(time.Hour),
		IsCA:                  true,
		BasicConstraintsValid: true,
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		DNSNames:              []string{"localhost"},
		IPAddresses:           []net.IP{net.IPv4(127, 0, 0, 1)},
	}
	der := must(x509.CreateCertificate(rand.Reader, template, template, &key.PublicKey, key))
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key},
		string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}))
}

func tcpEcho() string {
	listener := must(net.Listen("tcp", "127.0.0.1:0"))
	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				return
			}
			go func() { defer conn.Close(); _, _ = io.Copy(conn, conn) }()
		}
	}()
	return listener.Addr().String()
}

func udpEcho(network, address string) string {
	conn, err := net.ListenPacket(network, address)
	if err != nil {
		return ""
	}
	go func() {
		buffer := make([]byte, 65535)
		for {
			n, from, err := conn.ReadFrom(buffer)
			if err != nil {
				return
			}
			_, _ = conn.WriteTo(buffer[:n], from)
		}
	}()
	return conn.LocalAddr().String()
}

func main() {
	cert, caPEM := certificate()
	conn := must(net.ListenPacket("udp", "127.0.0.1:0"))
	hy2 := must(server.NewServer(&server.Config{
		TLSConfig:     server.TLSConfig{Certificates: []tls.Certificate{cert}},
		Conn:          conn,
		Authenticator: auth{},
	}))
	go func() { _ = hy2.Serve() }()
	_ = json.NewEncoder(os.Stdout).Encode(map[string]string{
		"server": conn.LocalAddr().String(), "ca_pem": caPEM, "auth": secret,
		"tcp_echo": tcpEcho(), "udp_echo": udpEcho("udp4", "127.0.0.1:0"), "udp_echo6": udpEcho("udp6", "[::1]:0"),
	})
	_, _ = io.Copy(io.Discard, os.Stdin)
	_ = hy2.Close()
}
