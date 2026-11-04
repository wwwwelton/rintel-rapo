package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// setupOpenVPNServer starts a local OpenVPN 2.7.8 server in the test directory with a fresh PKI and tls-crypt.
// It returns the client config and a cleanup function.
func setupOpenVPNServer(t *testing.T) ([]byte, func()) {
	t.Helper()
	dir := t.TempDir()
	genPKI := func() {
		sh := fmt.Sprintf(`
set -e
cd "%s"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -keyout ca.key -out ca.crt -subj "/CN=rapo-ca" -days 1 2>/dev/null
for n in server client; do openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -keyout $n.key -out $n.csr -subj "/CN=$n" 2>/dev/null; done
printf "basicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1,DNS:localhost\n" > server.ext
printf "basicConstraints=CA:FALSE\nextendedKeyUsage=clientAuth\n" > client.ext
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 1 -extfile server.ext 2>/dev/null
openssl x509 -req -in client.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out client.crt -days 1 -extfile client.ext 2>/dev/null
openvpn --genkey tls-crypt tc.key 2>/dev/null
`, dir)
		out, err := exec.Command("sh", "-c", sh).CombinedOutput()
		if err != nil {
			t.Fatalf("PKI generation failed: %v\n%s", err, out)
		}
	}
	genPKI()

	// Pick an available port.
	ln, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := ln.LocalAddr().(*net.UDPAddr).Port
	_ = ln.Close()

	srvConf := fmt.Sprintf(`
port %d
proto udp4
local 127.0.0.1
dev null
tls-server
ca ca.crt
cert server.crt
key server.key
dh none
tls-crypt tc.key
topology subnet
server 10.88.99.0 255.255.255.0
verb 3
explicit-exit-notify 0
`, port)
	if err := os.WriteFile(filepath.Join(dir, "server.conf"), []byte(srvConf), 0o600); err != nil {
		t.Fatal(err)
	}

	cmd := exec.Command("openvpn", "--config", filepath.Join(dir, "server.conf"))
	cmd.Dir = dir
	if err := cmd.Start(); err != nil {
		t.Skipf("cannot start openvpn server: %v", err)
	}

	read := func(name string) string {
		data, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			t.Fatal(err)
		}
		return strings.TrimSpace(string(data))
	}

	// Wait up to 5 seconds for the server to bind.
	time.Sleep(1 * time.Second)

	clientJSON, err := json.Marshal(map[string]string{
		"remote":    "127.0.0.1",
		"port":      fmt.Sprintf("%d", port),
		"proto":     "udp",
		"ca":        read("ca.crt"),
		"cert":      read("client.crt"),
		"key":       read("client.key"),
		"tls_crypt": read("tc.key"),
	})
	if err != nil {
		t.Fatal(err)
	}

	cleanup := func() {
		if cmd.Process != nil {
			_ = cmd.Process.Kill()
			_ = cmd.Wait()
		}
	}
	return clientJSON, cleanup
}

func TestOpenVPNHandshakeAgainstRealServer(t *testing.T) {
	if _, err := exec.LookPath("openvpn"); err != nil {
		t.Skip("openvpn binary not available in test environment")
	}
	clientJSON, cleanup := setupOpenVPNServer(t)
	defer cleanup()

	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()

	tunnel, err := startOpenVPN(ctx, clientJSON)
	if err != nil {
		t.Fatalf("OpenVPN handshake failed: %v", err)
	}
	defer tunnel.Close()

	if tunnel.cli == nil || tunnel.net == nil {
		t.Fatal("tunnel must have active client and netstack")
	}
}

func TestOpenVPNConfigValidation(t *testing.T) {
	ctx := context.Background()
	for name, data := range map[string][]byte{
		"empty":          []byte("{}"),
		"bad json":       []byte("{"),
		"missing remote": []byte(`{"ca":"-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----"}`),
		"missing ca":     []byte(`{"remote":"1.2.3.4","port":"1194"}`),
		"invalid ca":     []byte(`{"remote":"1.2.3.4","ca":"not a pem"}`),
		"invalid port":   []byte(`{"remote":"1.2.3.4","port":"99999","ca":"-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----"}`),
		"oversized":      make([]byte, 70000),
	} {
		if _, err := startOpenVPN(ctx, data); err == nil {
			t.Errorf("%s: must fail validation", name)
		}
	}
}
