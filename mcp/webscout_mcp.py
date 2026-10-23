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


def _post_json(path: str, body, want_bytes: bool = False):
    """POST `body` as JSON (GET when body is None) to the daemon."""
    base = DAEMON.ensure_running()
    req = urllib.request.Request(
        base + path,
        data=None if body is None else json.dumps(body).encode("utf-8"),
        method="GET" if body is None else "POST",
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


def _get_json(path: str):
    return _post_json(path, None)


def _with_session(body: dict, args: dict) -> dict:
    """Inject a session name if the caller didn't specify one. This is the
    knob that makes the MCP behave like one continuous browser."""
    sess = args.get("session") or DEFAULT_SESSION
    body["session"] = sess
    if "strategy" in args and args["strategy"]:
        body["strategy"] = args["strategy"]
    # Only used when this call creates the session; the daemon refuses a mismatch.
    for k in ("identity", "proxy", "forwarded_for"):
        if args.get(k):
            body[k] = args[k]
    if args.get("mobile") and not args.get("identity"):
        body["identity"] = "phone"
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
            "actions drive the page like a user (implies js) and follow the "
            "navigations they cause: [{\"type\": sel, \"text\": \"...\"}, "
            "{\"click\": sel}, {\"select\": sel, \"value\": v}, {\"check\": sel}, "
            "{\"uncheck\": sel}, {\"submit\": sel}, {\"press\": \"Enter\"}, "
            "{\"wait\": ms}, {\"wait_for\": sel, \"timeout\": ms}, {\"goto\": url}, "
            "{\"eval\": \"js expression\"}]. Selectors are CSS or 'text=Sign in'. "
            "Results come back per action in js.actions. If the page is a bot "
            "wall/captcha (state Blocked/Challenge), use the handoff tool."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "format": {"type": "string", "enum": ["json", "markdown", "raw"], "default": "json"},
                "js": {"type": "boolean", "default": False},
                "js_timeout_ms": {"type": "integer", "default": 5000},
                "actions": {"type": "array", "items": {"type": "object"},
                             "description": "Interactions to run in order; see the tool description."},
                "delay": {"type": "string", "description": "Random delay range ms, e.g. '100-500'."},
                "strategy": {"type": "string", "enum": ["plain", "js", "stealth"],
                              "description": "plain/js/stealth — stealth adds random delay + retry."},
                "session": {"type": "string",
                             "description": f"Named session (default '{DEFAULT_SESSION}'). Each session is one browser: identity + cookie jar."},
                "identity": {"type": "string",
                              "description": "Who to be when this call creates the session: words matched against ~/.webscout/identities/*.toml, e.g. 'phone samsung', 'iphone', 'tablet android', 'windows edge', 'linux firefox', 'iqoo-13'. Ignored-with-error if the session already exists as something else."},
                "proxy": {"type": "string", "description": "New sessions only: http(s):// or socks5:// proxy, the real way to change the IP sites see."},
                "forwarded_for": {"type": "string", "description": "New sessions only: send X-Forwarded-For/X-Real-IP with this IP ('random' ok). Fools only naive servers; bot walls see it as a bot signal."},
                "mobile": {"type": "boolean", "default": False,
                            "description": "Shorthand for identity='phone' (new sessions only)."},
            },
            "required": ["url"],
        },
    },
    {
        "name": "handoff",
        "description": (
            "Let a human through a page webscout can't pass (captcha, bot wall, "
            "login): opens it in a real Chrome. On a desktop a Chrome window "
            "appears for the user; on a headless machine the user opens the "
            "returned view_url in any browser to drive it remotely. When they're "
            "through, call handoff_done (or pass until_cookie / until_url to "
            "finish automatically). The session then takes over Chrome's cookies "
            "AND its exact fingerprint, so later scrapes look like the same "
            "browser that passed. Tell the user what to do and give them view_url."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "session": {"type": "string", "description": f"Session to hand the result to (default '{DEFAULT_SESSION}')."},
                "headless": {"type": "boolean", "description": "Default: only when there's no display."},
                "until_cookie": {"type": "string", "description": "Finish once Chrome holds this cookie (e.g. cf_clearance, datadome, _px3)."},
                "until_url": {"type": "string", "description": "Finish once the page URL contains this."},
            },
            "required": ["url"],
        },
    },
    {
        "name": "handoff_status",
        "description": "Where a handoff is: status (waiting/done/closed/cancelled), current URL and title, plus a screenshot of the remote page while waiting.",
        "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]},
    },
    {
        "name": "handoff_done",
        "description": "Finish a handoff now: take Chrome's cookies + fingerprint into the session and close Chrome.",
        "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]},
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
    if args.get("actions"):
        body["actions"] = args["actions"]
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

    return text


def handle_handoff(args: dict) -> str:
    body = {"url": args["url"], "session": args.get("session") or DEFAULT_SESSION}
    for k in ("headless", "until_cookie", "until_url"):
        if k in args and args[k] not in (None, ""):
            body[k] = args[k]
    data, _ = _post_json("/handoff", body)
    how = ("open view_url in any browser and get through the page"
           if data.get("headless") else "a Chrome window opened on this machine; get through the page there")
    return json.dumps(data, indent=2) + f"\n\nUser: {how}, then I'll call handoff_done (id {data.get('id')})."


def handle_handoff_status(args: dict):
    data, _ = _get_json(f"/handoff/{args['id']}")
    out = [{"type": "text", "text": json.dumps(data, indent=2)}]
    if data.get("status") == "waiting":
        try:
            frame, _ = _get_json(f"/handoff/{args['id']}/frame")
            if frame.get("data"):
                out.append({"type": "image", "data": frame["data"], "mimeType": "image/jpeg"})
        except EngineError:
            pass
    return out


def handle_handoff_done(args: dict) -> str:
    data, _ = _post_json(f"/handoff/{args['id']}/done", {})
    return json.dumps(data, indent=2)


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
    "handoff":           handle_handoff,
    "handoff_status":    handle_handoff_status,
    "handoff_done":      handle_handoff_done,
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
