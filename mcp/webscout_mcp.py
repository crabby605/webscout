#!/usr/bin/env python3
"""WebScout MCP server (persistent-daemon mode).

A zero-dependency Model Context Protocol server (stdio / JSON-RPC 2.0) that
fronts the WebScout headless-browser engine.

Operating model — "treat it as one real browser tab":
  * On startup, this script spawns one long-running `webscout serve` child on
    an OS-assigned port and keeps it alive for the lifetime of the MCP server.
  * Every tool call dispatches to that single daemon over HTTP/JSON.
  * All calls default to the same named session (`mcp`), so cookies + browser
    identity persist across tool calls. The remote site sees one continuous
    visitor — not a fresh fingerprint every request.
  * Callers can override `session` on any call to isolate a workflow, or use
    the `session_*` tools to inspect / clear.

Zero third-party deps — only the Python standard library. Python 3.8+.

Binary resolution:
  1. $WEBSCOUT_BIN
  2. <repo>/target/release/webscout   (relative to this file)
  3. <repo>/target/debug/webscout
  4. `webscout` on $PATH
"""

import atexit
import base64
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from typing import Any, Optional

PROTOCOL_VERSION = "2024-11-05"
SERVER_NAME = "webscout"
SERVER_VERSION = "0.2.0"

# Default session that every tool call uses unless overridden. This is what
# gives us "real browser" continuity — same identity + same cookie jar across
# all calls in this MCP server's lifetime.
DEFAULT_SESSION = os.environ.get("WEBSCOUT_MCP_SESSION", "mcp")

# How long we'll wait for the daemon to bind and announce its port.
DAEMON_STARTUP_TIMEOUT = 8.0

# Per-request HTTP timeout when talking to the daemon.
HTTP_TIMEOUT = 120

_ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

# Binary discovery

def find_binary() -> Optional[str]:
    env = os.environ.get("WEBSCOUT_BIN")
    if env and os.path.isfile(env) and os.access(env, os.X_OK):
        return env

    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    for candidate in (
        os.path.join(repo, "target", "release", "webscout"),
        os.path.join(repo, "target", "debug", "webscout"),
    ):
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate

    return shutil.which("webscout")

# Daemon lifecycle

class Daemon:
    """Spawns and supervises a single long-running `webscout serve` process."""

    def __init__(self) -> None:
        self.proc: Optional[subprocess.Popen] = None
        self.base_url: Optional[str] = None
        self._lock = threading.Lock()
        atexit.register(self.stop)

    def ensure_running(self) -> str:
        """Start the daemon if it isn't running. Returns its base URL."""
        with self._lock:
            # If we still have a healthy proc and a known URL, reuse it.
            if self.proc and self.proc.poll() is None and self.base_url:
                return self.base_url

            binary = find_binary()
            if not binary:
                raise RuntimeError(
                    "webscout binary not found. Build with `cargo build --release` "
                    "or set WEBSCOUT_BIN."
                )

            # Bind to port 0 → OS picks a free one. We read the actual port
            # from the daemon's first stdout line (`WEBSCOUT_LISTEN=...`).
            cmd = [binary, "serve", "--host", "127.0.0.1", "--port", "0"]
            self.proc = subprocess.Popen(
                cmd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,  # logs noise; engine errors come back via JSON
                # Put the child in its own process group so a Ctrl-C in our terminal
                # doesn't accidentally take down a daemon someone else is using.
                start_new_session=True,
            )

            deadline = time.monotonic() + DAEMON_STARTUP_TIMEOUT
            listen_line: Optional[str] = None
            assert self.proc.stdout is not None
            # Read a single line — `webscout serve` writes exactly one
            # `WEBSCOUT_LISTEN=host:port` line then nothing else on stdout.
            while time.monotonic() < deadline:
                if self.proc.poll() is not None:
                    raise RuntimeError(
                        f"webscout serve exited early (code {self.proc.returncode})"
                    )
                line = self.proc.stdout.readline()
                if not line:
                    time.sleep(0.05)
                    continue
                line = line.decode("utf-8", "replace").strip()
                if line.startswith("WEBSCOUT_LISTEN="):
                    listen_line = line
                    break

            if not listen_line:
                self.stop()
                raise RuntimeError(
                    f"webscout serve did not announce a port within {DAEMON_STARTUP_TIMEOUT}s"
                )

            addr = listen_line.split("=", 1)[1].strip()
            self.base_url = f"http://{addr}"

            # Health probe to be sure the router is wired up.
            self._wait_healthy(self.base_url, deadline)
            return self.base_url

    def _wait_healthy(self, base_url: str, deadline: float) -> None:
        last_err = None
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(base_url + "/health", timeout=1.5) as resp:
                    if resp.status == 200:
                        return
            except (urllib.error.URLError, ConnectionError) as e:
                last_err = e
                time.sleep(0.1)
        raise RuntimeError(f"daemon not healthy: {last_err}")

    def stop(self) -> None:
        proc = self.proc
        self.proc = None
        self.base_url = None
        if not proc or proc.poll() is not None:
            return
        try:
            # Polite shutdown first, then escalate if it hangs.
            os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            return
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
            except ProcessLookupError:
                pass


DAEMON = Daemon()


# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------

class EngineError(Exception):
    pass


def _post_json(path: str, body: dict, want_bytes: bool = False):
    base = DAEMON.ensure_running()
    data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(
        base + path,
        data=data,
        method="POST",
        headers={"content-type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT) as resp:
            payload = resp.read()
            ctype = resp.headers.get("content-type", "")
    except urllib.error.HTTPError as e:
        # API returns JSON errors with shape {"error": "..."}.
        body_text = e.read().decode("utf-8", "replace")
        try:
            obj = json.loads(body_text)
            raise EngineError(obj.get("error") or body_text)
        except json.JSONDecodeError:
            raise EngineError(f"HTTP {e.code}: {_ANSI_RE.sub('', body_text)[:400]}")
    except urllib.error.URLError as e:
        raise EngineError(f"daemon unreachable: {e.reason}")

    if want_bytes:
        return payload, ctype
    if ctype.startswith("application/json"):
        return json.loads(payload.decode("utf-8")), ctype
    return payload.decode("utf-8"), ctype


def _with_session(body: dict, args: dict) -> dict:
    """Inject a session name if the caller didn't specify one. This is the
    knob that makes the MCP behave like one continuous browser."""
    sess = args.get("session") or DEFAULT_SESSION
    body["session"] = sess
    if "strategy" in args and args["strategy"]:
        body["strategy"] = args["strategy"]
    if args.get("mobile"):
        body["mobile"] = True
    return body


# ---------------------------------------------------------------------------
# Tool definitions
# ---------------------------------------------------------------------------

TOOLS = [
    {
        "name": "scrape",
        "description": (
            "Fetch a URL through the persistent WebScout daemon and return "
            "extracted content. The same browser identity + cookie jar are "
            "reused across calls (session 'mcp' by default), so a site sees "
            "one continuous visitor. Use format 'markdown' for reading text, "
            "'json' for structured data, 'raw' for full HTML. Set js=true "
            "for SPA / client-rendered pages. strategy: plain | js | stealth. "
            "Set mobile=true for a phone identity. Set render=true to ALSO get "
            "a rendered PNG of the page back alongside the text, so you can see "
            "what it looks like."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "format": {"type": "string", "enum": ["json", "markdown", "raw"], "default": "json"},
                "js": {"type": "boolean", "default": False},
                "js_timeout_ms": {"type": "integer", "default": 5000},
                "delay": {"type": "string", "description": "Random delay range ms, e.g. '100-500'."},
                "strategy": {"type": "string", "enum": ["plain", "js", "stealth"],
                              "description": "plain/js/stealth — stealth adds random delay + retry."},
                "session": {"type": "string",
                             "description": f"Named session (default '{DEFAULT_SESSION}'). Pass a different name to use a separate identity + cookie jar."},
                "mobile": {"type": "boolean", "default": False,
                            "description": "Use a mobile browser identity (phone UA + viewport). Applies when the session is first created."},
                "render": {"type": "boolean", "default": False,
                            "description": "Also render the page to a PNG and return it as an image, so you can see the page live."},
            },
            "required": ["url"],
        },
    },
    {
        "name": "screenshot",
        "description": (
            "Render a PNG of the page. Three engines: 'page' (default) is "
            "WebScout's Chrome-free DOM layout/paint engine — it renders the "
            "page's real structure (headings, paragraphs, lists, tables, "
            "links, images, forms) faithfully, fetching images with the "
            "session's cookies. No browser required. 'card' builds a curated "
            "article summary with the og:image hero + a few inline images "
            "(best for social-card previews). 'chrome' shells out to headless "
            "Chrome for a pixel-perfect browser screenshot (requires Chrome "
            "installed; doesn't inherit session cookies). Returns "
            "base64-encoded PNG. Use height to clip a long page to the top "
            "fold; omit it to capture the full page."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "engine": {"type": "string", "enum": ["page", "card", "chrome"], "default": "page",
                            "description": "Which renderer to use."},
                "width": {"type": "integer", "default": 1024},
                "height": {"type": "integer",
                            "description": "Max height in px. 0/omitted = full page (page/card) or 800 (chrome); a value clips to the top fold."},
                "js": {"type": "boolean", "default": False},
                "js_timeout_ms": {"type": "integer", "default": 5000},
                "strategy": {"type": "string", "enum": ["plain", "js", "stealth"]},
                "session": {"type": "string"},
                "mobile": {"type": "boolean", "default": False,
                            "description": "Render with a mobile identity at phone width."},
            },
            "required": ["url"],
        },
    },
    {
        "name": "extract_links",
        "description": "Return only the page's links (href / text / rel / is_external).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"}, "js": {"type": "boolean", "default": False},
                "strategy": {"type": "string"}, "session": {"type": "string"},
                "mobile": {"type": "boolean", "default": False},
            },
            "required": ["url"],
        },
    },
    {
        "name": "extract_forms",
        "description": "Return only the page's HTML forms + fields.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"}, "js": {"type": "boolean", "default": False},
                "strategy": {"type": "string"}, "session": {"type": "string"},
                "mobile": {"type": "boolean", "default": False},
            },
            "required": ["url"],
        },
    },
    {
        "name": "extract_metadata",
        "description": "Return only page metadata: title, OpenGraph, meta tags, JSON-LD.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"}, "js": {"type": "boolean", "default": False},
                "strategy": {"type": "string"}, "session": {"type": "string"},
                "mobile": {"type": "boolean", "default": False},
            },
            "required": ["url"],
        },
    },
    {
        "name": "session_info",
        "description": (
            "Show info about the current MCP session: identity (user-agent, "
            "platform), and whether cookies / identity files exist on disk."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {"session": {"type": "string"}},
        },
    },
    {
        "name": "import_cookies",
        "description": (
            "Hand off auth/captcha to the user's real browser. Inject cookies "
            "from a `Cookie:` header into the current session, so the next "
            "scrape sees the user as logged-in. Workflow: scrape gets blocked "
            "→ user opens the URL in their browser, solves the challenge, "
            "copies the Cookie request header from DevTools → call this tool "
            "with that header → re-run the scrape."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "URL the cookies belong to."},
                "header": {"type": "string", "description": "Cookie header value, e.g. 'a=1; b=2'."},
                "session": {"type": "string", "description": f"Session name (default '{DEFAULT_SESSION}')."},
            },
            "required": ["url", "header"],
        },
    },
    {
        "name": "session_clear",
        "description": (
            "Wipe a session — deletes identity + cookies for it. The next "
            "request using that session will get a fresh browser fingerprint."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {"session": {"type": "string"}},
        },
    },
]

# Tool handlers

def handle_scrape(args: dict):
    body = {
        "url": args["url"],
        "format": args.get("format", "json"),
        "js": bool(args.get("js", False)),
    }
    if "js_timeout_ms" in args:
        body["js_timeout_ms"] = int(args["js_timeout_ms"])
    if "delay" in args and args["delay"]:
        body["delay"] = args["delay"]
    _with_session(body, args)

    data, _ctype = _post_json("/scrape", body)
    fmt = body["format"]
    if isinstance(data, dict):
        if fmt == "json":
            text = json.dumps(data, indent=2)
        else:
            # Markdown/raw — unwrap envelope's `content` field.
            text = data.get("content", "") or json.dumps(data, indent=2)
    else:
        text = str(data)

    # render=true: also hand back a PNG of the page so the caller can see it.
    if args.get("render"):
        shot_body = {"url": args["url"], "engine": "page",
                     "js": bool(args.get("js", False))}
        _with_session(shot_body, args)
        try:
            payload, _ = _post_json("/screenshot", shot_body, want_bytes=True)
            b64 = base64.b64encode(payload).decode("ascii")
            return [
                {"type": "text", "text": text},
                {"type": "image", "data": b64, "mimeType": "image/png"},
                {"type": "text", "text": f"Rendered {len(payload)} byte PNG of {args['url']}."},
            ]
        except EngineError as exc:
            return [
                {"type": "text", "text": text},
                {"type": "text", "text": f"(render failed: {exc})"},
            ]

    return text


def handle_screenshot(args: dict) -> list:
    body = {
        "url": args["url"],
        "js": bool(args.get("js", False)),
        "width": int(args.get("width", 1024)),
    }
    if "height" in args:
        body["height"] = int(args["height"])
    if "engine" in args and args["engine"]:
        body["engine"] = args["engine"]
    if "js_timeout_ms" in args:
        body["js_timeout_ms"] = int(args["js_timeout_ms"])
    _with_session(body, args)

    payload, _ctype = _post_json("/screenshot", body, want_bytes=True)
    b64 = base64.b64encode(payload).decode("ascii")
    return [
        {"type": "image", "data": b64, "mimeType": "image/png"},
        {"type": "text", "text": f"Rendered {len(payload)} byte PNG of {args['url']}."},
    ]


def _extract(path: str, args: dict) -> str:
    body = {"url": args["url"], "js": bool(args.get("js", False))}
    _with_session(body, args)
    data, _ = _post_json(path, body)
    return json.dumps(data, indent=2)


def handle_extract_links(args: dict) -> str:    return _extract("/extract/links", args)
def handle_extract_forms(args: dict) -> str:    return _extract("/extract/forms", args)
def handle_extract_metadata(args: dict) -> str: return _extract("/extract/metadata", args)


def handle_session_info(args: dict) -> str:
    name = args.get("session") or DEFAULT_SESSION
    binary = find_binary()
    if not binary:
        raise EngineError("webscout binary not found")
    proc = subprocess.run(
        [binary, "session", "show", name],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10,
    )
    text = (proc.stdout + proc.stderr).decode("utf-8", "replace")
    return _ANSI_RE.sub("", text).strip()


def handle_import_cookies(args: dict) -> str:
    body = {
        "session": args.get("session") or DEFAULT_SESSION,
        "url":     args["url"],
        "header":  args["header"],
    }
    data, _ = _post_json("/cookies/import", body)
    return json.dumps(data, indent=2)


def handle_session_clear(args: dict) -> str:
    name = args.get("session") or DEFAULT_SESSION
    binary = find_binary()
    if not binary:
        raise EngineError("webscout binary not found")
    proc = subprocess.run(
        [binary, "session", "delete", name],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10,
    )
    if proc.returncode != 0:
        raise EngineError(_ANSI_RE.sub("", proc.stderr.decode("utf-8", "replace")))
    return f"session '{name}' cleared (next request will get fresh identity + cookies)"


HANDLERS = {
    "scrape":            handle_scrape,
    "screenshot":        handle_screenshot,
    "extract_links":     handle_extract_links,
    "extract_forms":     handle_extract_forms,
    "extract_metadata":  handle_extract_metadata,
    "session_info":      handle_session_info,
    "session_clear":     handle_session_clear,
    "import_cookies":    handle_import_cookies,
}

# JSON-RPC / MCP plumbing

def make_response(req_id: Any, result: Any) -> dict:
    return {"jsonrpc": "2.0", "id": req_id, "result": result}


def make_error(req_id: Any, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}


def handle_request(req: dict) -> Optional[dict]:
    method = req.get("method")
    req_id = req.get("id")
    params = req.get("params") or {}

    if req_id is None:
        return None  # notification

    if method == "initialize":
        return make_response(req_id, {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        })
    if method == "ping":
        return make_response(req_id, {})
    if method == "tools/list":
        return make_response(req_id, {"tools": TOOLS})
    if method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments") or {}
        handler = HANDLERS.get(name)
        if not handler:
            return make_error(req_id, -32602, f"Unknown tool: {name}")
        try:
            result = handler(arguments)
            content = result if isinstance(result, list) else [{"type": "text", "text": str(result)}]
            return make_response(req_id, {"content": content, "isError": False})
        except EngineError as exc:
            return make_response(req_id, {
                "content": [{"type": "text", "text": f"Error: {exc}"}],
                "isError": True,
            })
        except Exception as exc:  # noqa: BLE001
            return make_response(req_id, {
                "content": [{"type": "text", "text": f"Unexpected error: {exc}"}],
                "isError": True,
            })
    return make_error(req_id, -32601, f"Method not found: {method}")


def main() -> None:
    stdin = sys.stdin
    stdout = sys.stdout
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            stdout.write(json.dumps(make_error(None, -32700, "Parse error")) + "\n")
            stdout.flush()
            continue
        response = handle_request(req)
        if response is not None:
            stdout.write(json.dumps(response) + "\n")
            stdout.flush()


if __name__ == "__main__":
    main()
