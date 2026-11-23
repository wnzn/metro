// egress is an HTTP/1 CONNECT proxy for the GitHub Actions runner.
package main

import (
	"bufio"
	"context"
	"errors"
	"io"
	"log"
	"net"
	"net/http"
	"net/netip"
	"os"
	"strings"
	"time"
)

const (
	port          = "3128"
	setupTimeout  = 10 * time.Second
	maxTunnelAge  = time.Hour
	maxConcurrent = 64
)

// https://docs.github.com/en/actions/reference/runners/self-hosted-runners#accessible-domains-by-function
func githubHost(host string) bool {
	switch host {
	case "github.com", "api.github.com", "codeload.github.com":
		return true
	}
	// This includes results-receiver.actions.githubusercontent.com. Only the
	// documented Actions namespace is allowed, not all user-content hosts.
	return strings.HasSuffix(host, ".actions.githubusercontent.com")
}

func connectHost(target string) (string, bool) {
	host, ok := strings.CutSuffix(target, ":443")
	if !ok || len(host) == 0 || len(host) > 253 {
		return "", false
	}
	for label := range strings.SplitSeq(host, ".") {
		if len(label) == 0 || len(label) > 63 || label[0] == '-' || label[len(label)-1] == '-' {
			return "", false
		}
		for i := range len(label) {
			c := label[i]
			if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '-') {
				return "", false
			}
		}
	}
	host = strings.ToLower(host)
	return host, githubHost(host)
}

// Global unicast alone includes private and special-purpose addresses.
// Deny the IANA special-purpose ranges (including globally reachable
// exceptions), plus non-unicast/reserved space and Azure's host wire server.
var forbiddenNetworks = []netip.Prefix{
	netip.MustParsePrefix("0.0.0.0/8"),
	netip.MustParsePrefix("10.0.0.0/8"),
	netip.MustParsePrefix("100.64.0.0/10"),
	netip.MustParsePrefix("127.0.0.0/8"),
	netip.MustParsePrefix("168.63.129.16/32"),
	netip.MustParsePrefix("169.254.0.0/16"),
	netip.MustParsePrefix("172.16.0.0/12"),
	netip.MustParsePrefix("192.0.0.0/24"),
	netip.MustParsePrefix("192.0.2.0/24"),
	netip.MustParsePrefix("192.31.196.0/24"),
	netip.MustParsePrefix("192.52.193.0/24"),
	netip.MustParsePrefix("192.88.99.0/24"),
	netip.MustParsePrefix("192.168.0.0/16"),
	netip.MustParsePrefix("192.175.48.0/24"),
	netip.MustParsePrefix("198.18.0.0/15"),
	netip.MustParsePrefix("198.51.100.0/24"),
	netip.MustParsePrefix("203.0.113.0/24"),
	netip.MustParsePrefix("224.0.0.0/3"), // multicast and reserved
	netip.MustParsePrefix("2001::/23"),
	netip.MustParsePrefix("2001:db8::/32"),
	netip.MustParsePrefix("2002::/16"),
	netip.MustParsePrefix("2620:4f:8000::/48"),
	netip.MustParsePrefix("3fff::/20"),
}

var globalIPv6 = netip.MustParsePrefix("2000::/3")

func publicAddress(ip netip.Addr) bool {
	if !ip.IsValid() || ip.Zone() != "" || !(ip.Is4() || globalIPv6.Contains(ip)) {
		return false
	}
	for _, prefix := range forbiddenNetworks {
		if prefix.Contains(ip) {
			return false
		}
	}
	return true
}

// Keep the trust boundary testable: a resolver yields typed addresses and
// a dialer only accepts a typed, already-validated numeric socket address.
type proxy struct {
	resolve func(context.Context, string, string) ([]netip.Addr, error)
	dial    func(context.Context, netip.AddrPort) (net.Conn, error)
	slots   chan struct{}
}

func (p *proxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Connection", "close")
	if r.Method != http.MethodConnect {
		w.Header().Set("Allow", "CONNECT")
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	host, ok := connectHost(r.RequestURI)
	if !ok || r.ContentLength != 0 || len(r.TransferEncoding) != 0 {
		http.Error(w, "forbidden", http.StatusForbidden)
		return
	}
	select {
	case p.slots <- struct{}{}:
		defer func() { <-p.slots }()
	default:
		http.Error(w, "busy", http.StatusServiceUnavailable)
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), setupTimeout)
	defer cancel()
	// Absolute DNS name avoids search suffixes. Reject the entire response
	// when any address is unsafe, even if a public answer came first.
	ips, err := p.resolve(ctx, "ip", host+".")
	if err != nil || len(ips) == 0 {
		http.Error(w, "unavailable", http.StatusBadGateway)
		return
	}
	for _, ip := range ips {
		if !publicAddress(ip) {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}
	}
	for _, ip := range ips {
		upstream, err := p.dial(ctx, netip.AddrPortFrom(ip, 443))
		if err == nil {
			cancel()
			defer upstream.Close()
			tunnel(w, upstream)
			return
		}
	}
	http.Error(w, "unavailable", http.StatusBadGateway)
}

func tunnel(w http.ResponseWriter, upstream net.Conn) {
	client, rw, err := http.NewResponseController(w).Hijack()
	if err != nil {
		http.Error(w, "unavailable", http.StatusBadGateway)
		return
	}
	defer client.Close()
	deadline := time.Now().Add(maxTunnelAge)
	if client.SetDeadline(deadline) != nil || upstream.SetDeadline(deadline) != nil {
		return
	}
	if _, err := rw.WriteString("HTTP/1.1 200 Connection Established\r\n\r\n"); err != nil || rw.Flush() != nil {
		return
	}
	done := make(chan struct{})
	copyTo := func(dst net.Conn, src io.Reader) {
		if _, err := io.Copy(dst, src); err == nil {
			if tcp, ok := dst.(interface{ CloseWrite() error }); ok && tcp.CloseWrite() == nil {
				return
			}
		}
		// On an error, close both halves and unblock the other pump.
		client.Close()
		upstream.Close()
	}
	go func() {
		defer close(done)
		// The HTTP parser may already have buffered part of the TLS stream.
		copyTo(upstream, rw.Reader)
	}()
	copyTo(client, upstream)
	<-done
}

func healthy() bool {
	c, err := net.DialTimeout("tcp4", "127.0.0.1:"+port, time.Second)
	if err != nil {
		return false
	}
	defer c.Close()
	if err := c.SetDeadline(time.Now().Add(time.Second)); err != nil {
		return false
	}
	// An IP literal must be denied before any DNS or dial.
	if _, err := io.WriteString(c, "CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n"); err != nil {
		return false
	}
	res, err := http.ReadResponse(bufio.NewReader(c), &http.Request{Method: http.MethodConnect})
	if err != nil {
		return false
	}
	defer res.Body.Close()
	return res.StatusCode == http.StatusForbidden
}

func main() {
	if len(os.Args) == 2 && os.Args[1] == "-healthcheck" {
		if !healthy() {
			os.Exit(1)
		}
		return
	}
	if len(os.Args) != 1 {
		log.Fatal("usage: egress [-healthcheck]")
	}
	resolver := &net.Resolver{PreferGo: true, StrictErrors: true}
	dialer := &net.Dialer{Timeout: 3 * time.Second}
	s := &http.Server{
		Addr: ":" + port,
		Handler: &proxy{
			resolve: resolver.LookupNetIP,
			dial: func(ctx context.Context, addr netip.AddrPort) (net.Conn, error) {
				return dialer.DialContext(ctx, "tcp", addr.String())
			},
			slots: make(chan struct{}, maxConcurrent),
		},
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       setupTimeout,
		WriteTimeout:      setupTimeout + 5*time.Second,
		MaxHeaderBytes:    8 << 10,
		ErrorLog:          log.New(io.Discard, "", 0),
	}
	if err := s.ListenAndServe(); !errors.Is(err, http.ErrServerClosed) {
		log.Fatal(err)
	}
}
