"""Loopback PAC fixture: only example.com selects the test sentinel."""
import http.server
import json
import sys

proxy_port = int(sys.argv[1])
assert 0 < proxy_port < 65536
script = ('function FindProxyForURL(url, host) { '
          'if (host === "example.com") return "PROXY 127.0.0.1:%d"; '
          'return "DIRECT"; }' % proxy_port).encode('ascii')


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != '/lightvm.pac':
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'application/x-ns-proxy-autoconfig')
        self.send_header('Content-Length', str(len(script)))
        self.send_header('Cache-Control', 'no-store')
        self.end_headers()
        self.wfile.write(script)

    def log_message(self, *_args):
        pass


with http.server.HTTPServer(('127.0.0.1', 0), Handler) as server:
    print(json.dumps(dict(port=server.server_port)), flush=True)
    server.serve_forever()
