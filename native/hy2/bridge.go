// Package main is the c-shared HY2 client loaded by Rintel RAPO.
//
// ABI rules: handles are opaque registry ids; Go never retains a caller pointer
// after an export returns; every blocking call has a deadline; panics become -9.
package main

/*
#include <stddef.h>
#include <stdint.h>
*/
import "C"

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"io"
	"net"
	"reflect"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unsafe"

	"github.com/apernet/hysteria/core/v2/client"
	coreErrs "github.com/apernet/hysteria/core/v2/errors"
	"github.com/apernet/quic-go"
)

const (
	openTimeout = 3 * time.Second
	maxHandles  = 512
	maxDatagram = 65507
)

var (
	errInvalid  = errors.New("invalid HY2 input")
	errCapacity = errors.New("HY2 capacity reached")
	errTimeout  = errors.New("HY2 deadline exceeded")
	errResolve  = errors.New("HY2 server name did not resolve")
	errOverlong = errors.New("HY2 datagram exceeds caller buffer")
	clientSlots = make(chan struct{}, 16)
)

type config struct {
	Server     string `json:"server"`
	Auth       string `json:"auth"`
	ServerName string `json:"server_name"`
	CAPEM      string `json:"ca_pem"`
}

func validAddress(address string) bool {
	host, port, err := net.SplitHostPort(address)
	n, e := strconv.Atoi(port)
	return err == nil && e == nil && n > 0 && n <= 65535 && len(host) > 0 && len(host) <= 253 && !strings.ContainsAny(host, "\x00\r\n /\\")
}

func parseConfig(data []byte) (config, error) {
	var c config
	if json.Unmarshal(data, &c) != nil || len(c.Server) > 512 || !validAddress(c.Server) || len(c.Auth) == 0 || len(c.Auth) > 4096 ||
		len(c.ServerName) > 253 || len(c.CAPEM) > 65536 || strings.ContainsAny(c.Auth, "\x00\r\n") || strings.ContainsAny(c.ServerName, "\x00\r\n /\\") {
		return c, errInvalid
	}
	return c, nil
}

type packetFactory struct{ conn net.PacketConn }

func (f packetFactory) New(net.Addr) (net.PacketConn, error) { return f.conn, nil }

// owner is one authenticated QUIC connection shared by its TCP/UDP children.
type owner struct {
	client client.Client
	done   chan struct{}
	closed atomic.Bool
	once   sync.Once
}

func (o *owner) close() {
	o.once.Do(func() {
		o.closed.Store(true)
		close(o.done)
		_ = o.client.Close()
		<-clientSlots
	})
}

type datagram struct {
	data    []byte
	address string
	err     error
}

type entry struct {
	owner     *owner
	tcp       net.Conn
	udp       client.HyUDPConn
	packets   chan datagram
	done      chan struct{}
	once      sync.Once
	readGate  chan struct{}
	writeGate chan struct{}
}

func newEntry(o *owner) *entry {
	return &entry{owner: o, done: make(chan struct{}), readGate: make(chan struct{}, 1), writeGate: make(chan struct{}, 1)}
}

func (e *entry) isClient() bool { return e.tcp == nil && e.udp == nil }

func (e *entry) close() {
	e.once.Do(func() {
		close(e.done)
		if e.tcp != nil {
			abortRead(e.tcp)
			_ = e.tcp.Close()
		}
		if e.udp != nil {
			_ = e.udp.Close()
		}
	})
}

var handles = struct {
	sync.Mutex
	next    uint64
	entries map[uint64]*entry
}{entries: make(map[uint64]*entry)}

func register(e *entry) (uint64, error) {
	handles.Lock()
	defer handles.Unlock()
	if len(handles.entries) >= maxHandles || e.owner.closed.Load() {
		return 0, errCapacity
	}
	handles.next++
	handles.entries[handles.next] = e
	return handles.next, nil
}

func lookup(id uint64) (*entry, error) {
	handles.Lock()
	e := handles.entries[id]
	handles.Unlock()
	if e == nil || e.owner.closed.Load() {
		return nil, net.ErrClosed
	}
	select {
	case <-e.done:
		return nil, net.ErrClosed
	default:
		return e, nil
	}
}

// closeHandle is idempotent. Closing a client closes every child it owns.
func closeHandle(id uint64) {
	handles.Lock()
	e := handles.entries[id]
	delete(handles.entries, id)
	var children []*entry
	if e != nil && e.isClient() {
		// Mark before releasing the registry so in-flight opens cannot publish children.
		e.owner.closed.Store(true)
		for childID, child := range handles.entries {
			if child.owner == e.owner {
				delete(handles.entries, childID)
				children = append(children, child)
			}
		}
	}
	handles.Unlock()
	if e == nil {
		return
	}
	e.close()
	for _, child := range children {
		child.close()
	}
	if e.isClient() {
		e.owner.close()
	}
}

func connect(c config) (*owner, error) {
	select {
	case clientSlots <- struct{}{}:
	default:
		return nil, errCapacity
	}
	success := false
	defer func() {
		if !success {
			<-clientSlots
		}
	}()
	ctx, cancel := context.WithTimeout(context.Background(), openTimeout)
	defer cancel()
	host, port, _ := net.SplitHostPort(c.Server)
	ips, err := net.DefaultResolver.LookupIPAddr(ctx, host)
	if err != nil || len(ips) == 0 {
		return nil, errResolve
	}
	p, _ := strconv.Atoi(port)
	addr := &net.UDPAddr{IP: ips[0].IP, Zone: ips[0].Zone, Port: p}
	var roots *x509.CertPool // nil keeps the operating system trust store
	if c.CAPEM != "" {
		roots = x509.NewCertPool()
		if !roots.AppendCertsFromPEM([]byte(c.CAPEM)) {
			return nil, errInvalid
		}
	}
	name := c.ServerName
	if name == "" {
		name = host
	}
	conn, err := net.ListenUDP("udp", nil)
	if err != nil {
		return nil, err
	}
	// Upstream has no context argument; closing its PacketConn cancels TLS/auth I/O.
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	cl, _, err := client.NewClient(&client.Config{
		ServerAddr:  addr,
		Auth:        c.Auth,
		ConnFactory: packetFactory{conn},
		TLSConfig:   client.TLSConfig{ServerName: name, RootCAs: roots},
		FastOpen:    false, // success is reported only after the server dialed the target
	})
	stopped := stop()
	if err != nil {
		_ = conn.Close()
		if !stopped {
			return nil, errTimeout
		}
		return nil, err
	}
	if !stopped || ctx.Err() != nil {
		_ = cl.Close()
		return nil, errTimeout
	}
	success = true
	return &owner{client: cl, done: make(chan struct{})}, nil
}

func openTCP(e *entry, address string) (*entry, error) {
	if !e.isClient() || !validAddress(address) {
		return nil, errInvalid
	}
	type result struct {
		conn net.Conn
		err  error
	}
	results := make(chan result, 1)
	go func() {
		defer func() {
			if recover() != nil {
				results <- result{err: errInvalid}
			}
		}()
		conn, err := e.owner.client.TCP(address)
		results <- result{conn, err}
	}()
	timer := time.NewTimer(openTimeout)
	defer timer.Stop()
	select {
	case r := <-results:
		if r.err != nil {
			return nil, r.err
		}
		child := newEntry(e.owner)
		child.tcp = r.conn
		return child, nil
	case <-timer.C:
		// Upstream TCP has no open deadline: abandon this stream only, never the shared client.
		go func() {
			if r := <-results; r.conn != nil {
				_ = r.conn.Close()
			}
		}()
		return nil, errTimeout
	case <-e.owner.done:
		go func() {
			if r := <-results; r.conn != nil {
				_ = r.conn.Close()
			}
		}()
		return nil, net.ErrClosed
	}
}

func openUDP(e *entry) (*entry, error) {
	if !e.isClient() {
		return nil, errInvalid
	}
	conn, err := e.owner.client.UDP()
	if err != nil {
		return nil, err
	}
	child := newEntry(e.owner)
	child.udp = conn
	child.packets = make(chan datagram, 16)
	go func() {
		defer child.close()
		defer func() { _ = recover() }()
		for {
			data, address, err := conn.Receive()
			select {
			case child.packets <- datagram{data, address, err}:
			case <-child.done:
				return
			case <-child.owner.done:
				return
			}
			if err != nil {
				return
			}
		}
	}()
	return child, nil
}

func duration(ms uint32) time.Duration {
	if ms == 0 || ms > 30000 {
		ms = 30000
	}
	return time.Duration(ms) * time.Millisecond
}

func acquire(e *entry, gate chan struct{}, deadline time.Time) error {
	timer := time.NewTimer(time.Until(deadline))
	defer timer.Stop()
	select {
	case gate <- struct{}{}:
		return nil
	case <-e.done:
		return net.ErrClosed
	case <-e.owner.done:
		return net.ErrClosed
	case <-timer.C:
		return errTimeout
	}
}

func tcpRead(e *entry, data []byte, ms uint32) (int, error) {
	if e.tcp == nil {
		return 0, errInvalid
	}
	deadline := time.Now().Add(duration(ms))
	if err := acquire(e, e.readGate, deadline); err != nil {
		return 0, err
	}
	defer func() { <-e.readGate }()
	if err := e.tcp.SetReadDeadline(deadline); err != nil {
		return 0, err
	}
	return e.tcp.Read(data)
}

func tcpWrite(e *entry, data []byte, ms uint32) (int, error) {
	if e.tcp == nil {
		return 0, errInvalid
	}
	deadline := time.Now().Add(duration(ms))
	if err := acquire(e, e.writeGate, deadline); err != nil {
		return 0, err
	}
	defer func() { <-e.writeGate }()
	if err := e.tcp.SetWriteDeadline(deadline); err != nil {
		return 0, err
	}
	return e.tcp.Write(data)
}

// abortRead discards unread stream data and wakes blocked readers.
func abortRead(conn net.Conn) {
	_ = conn.SetReadDeadline(time.Now())
	v := reflect.ValueOf(conn)
	if v.Kind() == reflect.Pointer && !v.IsNil() {
		v = v.Elem()
	}
	if v.Kind() != reflect.Struct {
		return
	}
	if field := v.FieldByName("Orig"); field.IsValid() && field.CanInterface() {
		if stream, ok := field.Interface().(interface{ CancelRead(quic.StreamErrorCode) }); ok {
			stream.CancelRead(0)
		}
	}
}

func udpReceive(e *entry, ms uint32) (datagram, error) {
	if e.udp == nil {
		return datagram{}, errInvalid
	}
	timer := time.NewTimer(duration(ms))
	defer timer.Stop()
	select {
	case p := <-e.packets:
		return p, p.err
	case <-e.done:
		return datagram{}, net.ErrClosed
	case <-e.owner.done:
		return datagram{}, net.ErrClosed
	case <-timer.C:
		return datagram{}, errTimeout
	}
}

func udpSend(e *entry, data []byte, address string) error {
	if e.udp == nil || !validAddress(address) || len(data) > maxDatagram {
		return errInvalid
	}
	if err := acquire(e, e.writeGate, time.Now().Add(openTimeout)); err != nil {
		return err
	}
	defer func() { <-e.writeGate }()
	// Upstream Send serializes into one reusable buffer and is not thread-safe.
	return e.udp.Send(data, address)
}

// Status codes shared with src/relay/hy2.rs. Never include secrets in errors.
func status(err error) C.int32_t {
	var auth coreErrs.AuthError
	var dial coreErrs.DialError
	var unknown x509.UnknownAuthorityError
	var hostname x509.HostnameError
	var invalid x509.CertificateInvalidError
	var verify *tls.CertificateVerificationError
	var transport *quic.TransportError
	var reset *quic.StreamError
	var ne net.Error
	switch {
	case err == nil || errors.Is(err, io.EOF):
		return 0
	case errors.Is(err, errInvalid):
		return -1
	case errors.As(err, &auth):
		return -6
	case errors.As(err, &unknown) || errors.As(err, &hostname) || errors.As(err, &invalid) || errors.As(err, &verify):
		return -7
	// quic-go reports non-alert TLS errors as INTERNAL_ERROR keeping only the text; this prefix is
	// crypto/tls's fixed CertificateVerificationError format.
	case errors.As(err, &transport) && strings.HasPrefix(transport.ErrorMessage, "tls: failed to verify certificate"):
		return -7
	case errors.As(err, &reset):
		return -12
	case errors.Is(err, errResolve):
		return -8
	case errors.As(err, &dial):
		return -10
	case errors.Is(err, errOverlong):
		return -11
	case errors.Is(err, errCapacity):
		return -4
	case errors.Is(err, errTimeout) || errors.Is(err, context.DeadlineExceeded) || (errors.As(err, &ne) && ne.Timeout()):
		return -3
	case errors.Is(err, net.ErrClosed):
		return -2
	default:
		return -5
	}
}

func guard(code *C.int32_t) {
	if recover() != nil {
		*code = -9
	}
}

func goBytes(data *C.uint8_t, length C.size_t) []byte {
	if length == 0 {
		return nil
	}
	return unsafe.Slice((*byte)(unsafe.Pointer(data)), int(length))
}

//export rapo_hy2_connect
func rapo_hy2_connect(cfg *C.uint8_t, cfgLength C.size_t, out *C.uint64_t) (code C.int32_t) {
	defer guard(&code)
	if cfg == nil || out == nil || cfgLength > 80000 {
		return -1
	}
	c, err := parseConfig(append([]byte(nil), goBytes(cfg, cfgLength)...))
	if err != nil {
		return status(err)
	}
	o, err := connect(c)
	if err != nil {
		return status(err)
	}
	id, err := register(newEntry(o))
	if err != nil {
		o.close()
		return status(err)
	}
	*out = C.uint64_t(id)
	return 0
}

//export rapo_hy2_tcp
func rapo_hy2_tcp(clientID C.uint64_t, address *C.uint8_t, addressLength C.size_t, out *C.uint64_t) (code C.int32_t) {
	defer guard(&code)
	if address == nil || out == nil || addressLength > 512 {
		return -1
	}
	e, err := lookup(uint64(clientID))
	if err != nil {
		return status(err)
	}
	child, err := openTCP(e, string(goBytes(address, addressLength)))
	if err != nil {
		return status(err)
	}
	id, err := register(child)
	if err != nil {
		child.close()
		return status(err)
	}
	*out = C.uint64_t(id)
	return 0
}

//export rapo_hy2_udp
func rapo_hy2_udp(clientID C.uint64_t, out *C.uint64_t) (code C.int32_t) {
	defer guard(&code)
	if out == nil {
		return -1
	}
	e, err := lookup(uint64(clientID))
	if err != nil {
		return status(err)
	}
	child, err := openUDP(e)
	if err != nil {
		return status(err)
	}
	id, err := register(child)
	if err != nil {
		child.close()
		return status(err)
	}
	*out = C.uint64_t(id)
	return 0
}

//export rapo_hy2_read
func rapo_hy2_read(id C.uint64_t, buffer *C.uint8_t, length C.size_t, ms C.uint32_t, count *C.size_t) (code C.int32_t) {
	defer guard(&code)
	if buffer == nil || count == nil || length == 0 {
		return -1
	}
	*count = 0
	e, err := lookup(uint64(id))
	if err != nil {
		return status(err)
	}
	// Read copies into the caller buffer synchronously; nothing keeps it afterwards.
	n, err := tcpRead(e, goBytes(buffer, length), uint32(ms))
	*count = C.size_t(n)
	if n > 0 {
		return 0
	}
	if errors.Is(err, io.EOF) {
		return 1
	}
	if err == nil {
		return -3 // empty read without EOF: caller retries
	}
	return status(err)
}

//export rapo_hy2_write
func rapo_hy2_write(id C.uint64_t, buffer *C.uint8_t, length C.size_t, ms C.uint32_t, count *C.size_t) (code C.int32_t) {
	defer guard(&code)
	if buffer == nil || count == nil || length == 0 {
		return -1
	}
	*count = 0
	e, err := lookup(uint64(id))
	if err != nil {
		return status(err)
	}
	// QUIC may keep queued slices after a deadline; give it Go-owned memory.
	n, err := tcpWrite(e, append([]byte(nil), goBytes(buffer, length)...), uint32(ms))
	*count = C.size_t(n)
	if n > 0 {
		return 0
	}
	return status(err)
}

// how: 1 read, 2 write (FIN), 3 both.
//
//export rapo_hy2_shutdown
func rapo_hy2_shutdown(id C.uint64_t, how C.int32_t) (code C.int32_t) {
	defer guard(&code)
	e, err := lookup(uint64(id))
	if err != nil {
		return status(err)
	}
	if e.tcp == nil || how < 1 || how > 3 {
		return -1
	}
	if how&1 != 0 {
		abortRead(e.tcp)
	}
	if how&2 != 0 {
		_ = e.tcp.SetWriteDeadline(time.Now())
		_ = e.tcp.Close() // quic Stream.Close sends FIN for the write side only
	}
	return 0
}

//export rapo_hy2_udp_send
func rapo_hy2_udp_send(id C.uint64_t, buffer *C.uint8_t, length C.size_t, address *C.uint8_t, addressLength C.size_t) (code C.int32_t) {
	defer guard(&code)
	if (buffer == nil && length != 0) || address == nil || addressLength > 512 {
		return -1
	}
	e, err := lookup(uint64(id))
	if err != nil {
		return status(err)
	}
	return status(udpSend(e, goBytes(buffer, length), string(goBytes(address, addressLength))))
}

//export rapo_hy2_udp_recv
func rapo_hy2_udp_recv(id C.uint64_t, buffer *C.uint8_t, capacity C.size_t, ms C.uint32_t, count *C.size_t,
	address *C.uint8_t, addressCapacity C.size_t, addressLength *C.size_t) (code C.int32_t) {
	defer guard(&code)
	if buffer == nil || count == nil || address == nil || addressLength == nil {
		return -1
	}
	*count, *addressLength = 0, 0
	e, err := lookup(uint64(id))
	if err != nil {
		return status(err)
	}
	p, err := udpReceive(e, uint32(ms))
	if err != nil {
		return status(err)
	}
	if len(p.data) > int(capacity) || len(p.address) > int(addressCapacity) {
		return status(errOverlong) // reject; never truncate a datagram
	}
	copy(goBytes(buffer, capacity), p.data)
	copy(goBytes(address, addressCapacity), p.address)
	*count, *addressLength = C.size_t(len(p.data)), C.size_t(len(p.address))
	return 0
}

//export rapo_hy2_close
func rapo_hy2_close(id C.uint64_t) {
	defer func() { _ = recover() }()
	closeHandle(uint64(id))
}

func main() {}
