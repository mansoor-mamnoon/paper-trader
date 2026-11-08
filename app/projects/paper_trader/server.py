"""Read-only web dashboard and a single supervised five-minute worker."""

import csv
import fcntl
import io
import json
import signal
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import cast
from urllib.parse import urlsplit

from . import engine, ledger, market

STATIC = Path(__file__).parent / "static"


def worker(path, stop):
    """Observe and trade until stopped; failures freeze trading and remain visible."""
    while not stop.is_set():
        try:
            engine.cycle(path, market.observe())
        except Exception as error:
            ledger.failure(path, str(error))
        stop.wait(300)


class Dashboard(ThreadingHTTPServer):
    """HTTP server with explicit read-only application configuration."""

    daemon_threads = True

    def __init__(self, address, path):
        """Bind the dashboard to an address and durable account ledger."""
        super().__init__(address, Handler)
        self.path = path


class Handler(BaseHTTPRequestHandler):
    """Serve only allowlisted static resources and read-only ledger exports."""

    def log_message(self, format, *args):
        """Suppress routine access logs; operational errors live in the ledger."""

    def do_POST(self):
        """Reject mutation requests, including those sent from other websites."""
        self.send_error(405, "Read-only paper account")

    # HTTP method names are required by BaseHTTPRequestHandler.
    do_PUT = do_POST  # noqa: N815
    do_DELETE = do_POST  # noqa: N815
    do_PATCH = do_POST  # noqa: N815

    def do_GET(self):
        """Return the dashboard, a consistent JSON snapshot or a full CSV export."""
        server = cast(Dashboard, self.server)
        path = urlsplit(self.path).path
        static = {
            "/": ("index.html", "text/html"),
            "/app.js": ("app.js", "text/javascript"),
            "/style.css": ("style.css", "text/css"),
        }
        if path in static:
            name, mime = static[path]
            self.respond((STATIC / name).read_bytes(), mime)
        elif path in ("/api/state", "/api/health"):
            state = ledger.report(server.path)
            data = state if path == "/api/state" else state["health"]
            self.respond(json.dumps(data, allow_nan=False).encode(), "application/json")
        elif path in ("/export/fills.csv", "/export/decisions.csv", "/export/equity.csv"):
            table = path.split("/")[-1].split(".")[0]
            db = ledger.connect(server.path)
            try:
                cursor = db.execute("SELECT * FROM " + table + " ORDER BY id")
                output = io.StringIO()
                writer = csv.writer(output)
                writer.writerow([column[0] for column in cursor.description])
                writer.writerows(cursor)
                self.respond(output.getvalue().encode(), "text/csv")
            finally:
                db.close()
        else:
            self.send_error(404)

    def respond(self, body, mime):
        """Send a bounded response with restrictive browser security headers."""
        self.send_response(200)
        self.send_header("Content-Type", mime + "; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; "
            "img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'",
        )
        self.end_headers()
        self.wfile.write(body)


def serve(path, host="127.0.0.1", port=8766, no_worker=False):
    """Run one account process; an OS file lock prevents duplicate workers."""
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    with open(str(path) + ".lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        db = ledger.connect(path)
        db.close()
        if not no_worker:
            ledger.claim_mode(path, "live")
        stop = threading.Event()
        signal.signal(signal.SIGTERM, lambda *_: stop.set())
        signal.signal(signal.SIGINT, lambda *_: stop.set())
        server = Dashboard((host, port), path)
        server.timeout = 1
        thread = threading.Thread(target=worker, args=(path, stop), daemon=True)
        if not no_worker:
            thread.start()
        print(
            json.dumps(
                {"url": f"http://{host}:{server.server_port}", "pid": __import__("os").getpid()}
            ),
            flush=True,
        )
        try:
            while not stop.is_set():
                server.handle_request()
        finally:
            stop.set()
            server.server_close()
            if thread.is_alive():
                thread.join(timeout=150)
