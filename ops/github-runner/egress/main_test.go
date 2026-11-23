package main

import (
	"bufio"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestAuthority(t *testing.T) {
	for _, tc := range []struct {
		target, want string
	}{
		{"GITHUB.com:443", "github.com"},
		{"api.github.com:443", "api.github.com"},
		{"codeload.github.com:443", "codeload.github.com"},
		{"pipelines.actions.githubusercontent.com:443", "pipelines.actions.githubusercontent.com"},
		{"results-receiver.actions.githubusercontent.com:443", "results-receiver.actions.githubusercontent.com"},
		{"github.com", ""},
		{"github.com:80", ""},
		{"github.com:0443", ""},
		{"github.com.:443", ""},
		{"example.org:443", ""},
		{"raw.githubusercontent.com:443", ""},
		{"actions.githubusercontent.com:443", ""},
		{"otheractions.githubusercontent.com:443", ""},
		{"pipelines.actions.githubusercontent.com.example.org:443", ""},
		{"acct.blob.core.windows.net:443", ""},
		{"acct.azure.com:443", ""},
		{"bucket.s3.amazonaws.com:443", ""},
		{"127.0.0.1:443", ""},
		{"140.82.112.3:443", ""},
		{"[::1]:443", ""},
		{"[2606:50c0:8000::153]:443", ""},
		{"https://github.com:443", ""},
		{"github.com:443/path", ""},
		{"@github.com:443", ""},
		{"x..actions.githubusercontent.com:443", ""},
		{"-x.actions.githubusercontent.com:443", ""},
		{"a_b.actions.githubusercontent.com:443", ""},
		{strings.Repeat("a", 64) + ".actions.githubusercontent.com:443", ""},
	} {
		t.Run(tc.target, func(t *testing.T) {
			host, ok := connectHost(tc.target)
			if ok != (tc.want != "") || ok && host != tc.want {
				t.Fatalf("got (%q, %v), want %q", host, ok, tc.want)
			}
		})
	}
}

func TestPublicAddresses(t *testing.T) {
	for _, s := range []string{
		"0.0.0.1", "10.5.0.1", "100.100.100.200", "127.9.0.1", "168.63.129.16",
		"169.254.169.254", "172.31.0.1", "192.0.0.9", "192.0.2.9", "192.31.196.1",
		"192.52.193.1", "192.88.99.2", "192.168.0.1", "192.175.48.1", "198.18.0.1",
		"198.51.100.1", "203.0.113.1", "224.1.1.1", "241.0.0.1", "255.255.255.255",
		"::", "::1", "::ffff:169.254.169.254", "::ffff:140.82.112.3", "64:ff9b::a00:1",
		"100::1", "2001::1", "2001:db8::1", "2002::1", "2620:4f:8000::1",
		"3fff::1", "5f00::1", "fd00:ec2::254", "fe80::1", "ff02::1", "2606:50c0::1%eth0",
	} {
		t.Run(s, func(t *testing.T) {
			if publicAddress(netip.MustParseAddr(s)) {
				t.Fatal("special-use address was eligible")
			}
		})
	}
	if publicAddress(netip.Addr{}) {
		t.Fatal("invalid address was eligible")
	}
	for _, s := range []string{"140.82.112.3", "2606:50c0:8000::153"} {
		if !publicAddress(netip.MustParseAddr(s)) {
			t.Fatalf("public address %s was rejected", s)
		}
	}
}

func tcpRequest(t *testing.T, server *httptest.Server, request string) (net.Conn, *bufio.Reader) {
	t.Helper()
	c, err := net.DialTimeout("tcp", server.Listener.Addr().String(), time.Second)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { c.Close() })
	if err := c.SetDeadline(time.Now().Add(3 * time.Second)); err != nil {
		t.Fatal(err)
	}
	if _, err := io.WriteString(c, request); err != nil {
		t.Fatal(err)
	}
	return c, bufio.NewReader(c)
}

func connectRequest(authority string) string {
	return "CONNECT " + authority + " HTTP/1.1\r\nHost: " + authority + "\r\n\r\n"
}

func expectStatus(t *testing.T, r *bufio.Reader, status int) {
	t.Helper()
	res, err := http.ReadResponse(r, &http.Request{Method: http.MethodConnect})
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode != status {
		t.Fatalf("got HTTP %d, want %d", res.StatusCode, status)
	}
}

func TestDeniedRequestsDoNotResolveOrDial(t *testing.T) {
	var resolves, dials atomic.Int32
	s := httptest.NewServer(&proxy{
		resolve: func(context.Context, string, string) ([]netip.Addr, error) {
			resolves.Add(1)
			return nil, errors.New("unexpected DNS")
		},
		dial: func(context.Context, netip.AddrPort) (net.Conn, error) {
			dials.Add(1)
			return nil, errors.New("unexpected dial")
		},
		slots: make(chan struct{}, 1),
	})
	defer s.Close()
	for _, tc := range []struct {
		name, request string
		status        int
	}{
		{"forward HTTP", "GET http://github.com/ HTTP/1.1\r\nHost: github.com\r\n\r\n", 405},
		{"health without CONNECT", "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n", 405},
		{"wrong port", connectRequest("github.com:80"), 403},
		{"private hostname", connectRequest("metadata.google.internal:443"), 403},
		{"public IP literal", connectRequest("140.82.112.3:443"), 403},
		{"IPv6 literal", connectRequest("[::1]:443"), 403},
		{"disabled storage", connectRequest("acct.blob.core.windows.net:443"), 403},
		{"body", "CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\nContent-Length: 1\r\n\r\nx", 403},
		{"chunked", "CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n", 403},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, r := tcpRequest(t, s, tc.request)
			expectStatus(t, r, tc.status)
		})
	}
	if resolves.Load() != 0 || dials.Load() != 0 {
		t.Fatal("denied requests reached the resolver or dialer")
	}
}

func TestUnsafeDNSAnswersNeverDial(t *testing.T) {
	for _, tc := range []struct {
		name string
		ips  []string
		err  error
		code int
	}{
		{"metadata", []string{"169.254.169.254"}, nil, 403},
		{"private before public", []string{"172.20.0.2", "140.82.112.3"}, nil, 403},
		{"public before private", []string{"140.82.112.3", "fd00:ec2::254"}, nil, 403},
		{"mapped private", []string{"::ffff:127.0.0.1"}, nil, 403},
		{"special use", []string{"192.0.2.1"}, nil, 403},
		{"no records", nil, nil, 502},
		{"partial lookup failure", []string{"140.82.112.3"}, errors.New("DNS failure"), 502},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var dials atomic.Int32
			s := httptest.NewServer(&proxy{
				resolve: func(context.Context, string, string) ([]netip.Addr, error) {
					var ips []netip.Addr
					for _, ip := range tc.ips {
						ips = append(ips, netip.MustParseAddr(ip))
					}
					return ips, tc.err
				},
				dial: func(context.Context, netip.AddrPort) (net.Conn, error) {
					dials.Add(1)
					return nil, errors.New("unexpected dial")
				},
				slots: make(chan struct{}, 1),
			})
			defer s.Close()
			_, r := tcpRequest(t, s, connectRequest("github.com:443"))
			expectStatus(t, r, tc.code)
			if dials.Load() != 0 {
				t.Fatal("unsafe DNS answer reached the dialer")
			}
		})
	}
}

func TestTunnelsTLSUsingOnlyValidatedNumericAddresses(t *testing.T) {
	upstream := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.TLS == nil || r.Host != "github.com" || r.RequestURI != "/local-fixture" {
			t.Error("unexpected upstream request")
		}
		io.WriteString(w, "local fixture")
	}))
	defer upstream.Close()
	dns := make(chan string, 2)
	attempted := make(chan netip.AddrPort, 3)
	first := netip.MustParseAddrPort("140.82.112.3:443")
	second := netip.MustParseAddrPort("[2606:50c0:8000::153]:443")
	p := httptest.NewServer(&proxy{
		resolve: func(_ context.Context, family, host string) ([]netip.Addr, error) {
			dns <- family + " " + host
			return []netip.Addr{first.Addr(), second.Addr()}, nil
		},
		dial: func(ctx context.Context, address netip.AddrPort) (net.Conn, error) {
			attempted <- address
			if address != second {
				return nil, errors.New("fixture's first IP is unavailable")
			}
			// Only the test dialer maps this validated address to a real
			// loopback socket. Production dials the numeric address itself.
			return (&net.Dialer{}).DialContext(ctx, "tcp", upstream.Listener.Addr().String())
		},
		slots: make(chan struct{}, 1),
	})
	defer p.Close()
	proxyURL, err := url.Parse(p.URL)
	if err != nil {
		t.Fatal(err)
	}
	tlsConfig := upstream.Client().Transport.(*http.Transport).TLSClientConfig.Clone()
	tlsConfig.ServerName = "example.com" // Name in Go's test fixture certificate.
	transport := &http.Transport{Proxy: http.ProxyURL(proxyURL), TLSClientConfig: tlsConfig}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 3 * time.Second}
	res, err := client.Get("https://github.com/local-fixture")
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	body, err := io.ReadAll(res.Body)
	if err != nil || res.StatusCode != 200 || string(body) != "local fixture" {
		t.Fatalf("unexpected TLS response: HTTP %d: %q (%v)", res.StatusCode, body, err)
	}
	if len(dns) != 1 || <-dns != "ip github.com." {
		t.Fatal("expected one absolute DNS lookup")
	}
	if len(attempted) != 2 || <-attempted != first || <-attempted != second {
		t.Fatal("expected only the two validated numeric socket addresses, in order")
	}
}

func TestTunnelPreservesBufferedPayloadAndHalfClose(t *testing.T) {
	l, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	l.SetDeadline(time.Now().Add(3 * time.Second))
	go func() {
		c, err := l.AcceptTCP()
		if err != nil {
			return
		}
		defer c.Close()
		c.SetDeadline(time.Now().Add(3 * time.Second))
		payload, err := io.ReadAll(c)
		if err == nil && string(payload) == "client-finished" {
			io.WriteString(c, "server-finished")
		}
	}()
	s := httptest.NewServer(&proxy{
		resolve: func(context.Context, string, string) ([]netip.Addr, error) {
			return []netip.Addr{netip.MustParseAddr("140.82.112.3")}, nil
		},
		dial: func(ctx context.Context, _ netip.AddrPort) (net.Conn, error) {
			return (&net.Dialer{}).DialContext(ctx, "tcp", l.Addr().String())
		},
		slots: make(chan struct{}, 1),
	})
	defer s.Close()
	c, r := tcpRequest(t, s, connectRequest("github.com:443")+"client-finished")
	if status, err := r.ReadString('\n'); err != nil || status != "HTTP/1.1 200 Connection Established\r\n" {
		t.Fatalf("unexpected CONNECT status: %q (%v)", status, err)
	}
	if blank, err := r.ReadString('\n'); err != nil || blank != "\r\n" {
		t.Fatalf("unexpected CONNECT headers: %q (%v)", blank, err)
	}
	if err := c.(*net.TCPConn).CloseWrite(); err != nil {
		t.Fatal(err)
	}
	reply, err := io.ReadAll(r)
	if err != nil || string(reply) != "server-finished" {
		t.Fatalf("unexpected payload: %q (%v)", reply, err)
	}
}
