"""Local Range/206 index and pack server for the launcher demo."""
import argparse, http.server, json, os, secrets

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get('content-length', '0'))
        body = json.loads(self.rfile.read(length) or b'{}')
        if self.path == '/auth/email/start':
            self.server.codes[body.get('email', '')] = '123456'; print('demo verification code: 123456', flush=True); return self.send_json({'ok': True})
        if self.path == '/auth/email/verify':
            if self.server.codes.get(body.get('email')) != body.get('code'): return self.send_error(401, 'invalid code')
            token = secrets.token_urlsafe(24); self.server.sessions.add(token); payload = json.dumps({'ok': True}).encode(); self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Set-Cookie', f'session={token}; Path=/; HttpOnly'); self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload); return
        if self.path.startswith('/apps/') and self.path.endswith('/redeem'):
            if not self._session(): return self.send_error(401, 'login required')
            app_id = self.path.split('/')[2]
            if body.get('code') != 'DEMO-PAID-2026': return self.send_error(400, 'invalid redeem code')
            self.server.grants.add((self._token(), app_id)); return self.send_json({'ok': True, 'app_id': app_id})
        if self.path.startswith('/apps/') and self.path.endswith('/key'):
            if not self._session(): return self.send_error(401, 'login required')
            app_id = self.path.split('/')[2]
            if (self._token(), app_id) not in self.server.grants: return self.send_error(403, 'grant required')
            return self.send_json({'app_id': app_id, 'key_id': 1, 'depot_key_hex': '11' * 32})
        self.send_error(404)
    def do_GET(self):
        if self.path == '/me/grants':
            if not self._session(): return self.send_error(401, 'login required')
            return self.send_json({'grants': sorted(app for token, app in self.server.grants if token == self._token())})
        return super().do_GET()
    def _session(self):
        return self._token() in self.server.sessions
    def _token(self):
        return self.headers.get('Cookie','').replace('session=', '').split(';')[0]
    def send_json(self, value):
        payload = json.dumps(value).encode(); self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Content-Length', str(len(payload))); self.end_headers(); self.wfile.write(payload)
    def send_head(self):
        path = self.translate_path(self.path.split('?',1)[0])
        if not os.path.isfile(path): return super().send_head()
        source = open(path, 'rb')
        size = os.path.getsize(path); start, end = 0, size - 1; value = self.headers.get('Range')
        if value and value.startswith('bytes='):
            start_s, end_s = value[6:].split('-', 1); start = int(start_s); end = int(end_s) if end_s else end; end = min(end, size - 1); self.send_response(206); self.send_header('Content-Range', f'bytes {start}-{end}/{size}')
        else: self.send_response(200)
        self.send_header('Content-Length', str(max(0, end - start + 1))); self.send_header('Accept-Ranges', 'bytes'); self.end_headers(); self._range = (start, end); return source
    def copyfile(self, source, outputfile):
        start, end = getattr(self, '_range', (0, None)); source.seek(start); outputfile.write(source.read(None if end is None else end - start + 1))

if __name__ == '__main__':
    parser = argparse.ArgumentParser(); parser.add_argument('--root', required=True); parser.add_argument('--port', type=int, default=3210); args = parser.parse_args(); os.chdir(args.root); server = http.server.ThreadingHTTPServer(('127.0.0.1', args.port), Handler); server.codes = {}; server.sessions = set(); server.grants = set(); print(f'Vesper demo index: http://127.0.0.1:{args.port}', flush=True); server.serve_forever()
