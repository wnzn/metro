"""A Metro-server stand-in and a recording HTTP proxy, on 127.0.0.1.

argv: server_port proxy_port log_file. The server answers "direct <Host>";
the proxy logs every request line it gets (CONNECT or absolute GET) and
refuses it, so a proxied fetch never reaches the server.
"""
import http.server, socketserver, sys, threading

LOG = sys.argv[3]


class Server(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = ("direct " + self.headers.get("Host", "")).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


class Proxy(Server):
    def handle_one_request(self):
        self.raw_requestline = self.rfile.readline(65537)
        if not self.raw_requestline:
            return
        with open(LOG, "a") as f:
            f.write(self.raw_requestline.decode(errors="replace").strip() + "\n")
        self.wfile.write(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 7\r\n\r\nproxied")
        self.close_connection = True


socketserver.ThreadingTCPServer.allow_reuse_address = True
for port, handler in ((int(sys.argv[1]), Server), (int(sys.argv[2]), Proxy)):
    srv = socketserver.ThreadingTCPServer(("127.0.0.1", port), handler)
    srv.daemon_threads = True
    threading.Thread(target=srv.serve_forever, daemon=True).start()
threading.Event().wait()
