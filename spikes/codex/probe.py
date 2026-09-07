#!/usr/bin/env python3
"""Native Codex carrier probe. Synthetic localhost server; no provider credentials.

Usage: python3 spikes/codex/probe.py /path/to/codex-0.153.4
Requires macOS sandbox-exec to prohibit non-loopback outbound traffic.
This is a compatibility fixture, not a Poolparty implementation.
"""
import argparse
import base64
import hashlib
import http.server
import json
from pathlib import Path
import queue
import socket
import struct
import subprocess
import tempfile
import threading

RECORDS = []


def events():
    return [
        {"type": "response.created", "response": {"id": "resp_synthetic"}},
        {"type": "response.output_item.done", "item": {
            "type": "message", "role": "assistant", "id": "msg_synthetic",
            "content": [{"type": "output_text", "text": "fixture ok"}]}},
        {"type": "response.completed", "response": {"id": "resp_synthetic",
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}},
    ]


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def record(self, body, transport):
        RECORDS.append({"path": self.path, "transport": transport,
            "authorized": self.headers.get("Authorization") == "Bearer synthetic-grant",
            "session": self.headers.get("session-id"),
            "thread": self.headers.get("thread-id"),
            "cache_key": body.get("prompt_cache_key"),
            "metadata": body.get("client_metadata", {})})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        self.record(body, "http")
        if "/binding-exhausted/" in self.path or "/binding-unauthorized/" in self.path:
            exhausted = "/binding-exhausted/" in self.path
            payload = json.dumps({"error": {"type": "usage_limit_reached" if exhausted else "invalid_api_key",
                "message": "Synthetic admission refusal; binding preserved."}}).encode()
            self.send_response(429 if exhausted else 401)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            self.wfile.flush()
            return
        selected = events()[:-1] if "/binding-partial/" in self.path else events()
        payload = b"".join(("data: " + json.dumps(e) + "\n\n").encode() for e in selected)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
        self.wfile.flush()

    def do_GET(self):
        if self.headers.get("Upgrade", "").lower() != "websocket":
            self.send_error(404)
            return
        accept = base64.b64encode(hashlib.sha1((self.headers["Sec-WebSocket-Key"] +
            "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        self.send_response(101)
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.connection.settimeout(15)
        try:
            while True:
                head = self.rfile.read(2)
                if len(head) != 2 or head[0] & 15 == 8:
                    break
                length = head[1] & 127
                if length == 126:
                    length = struct.unpack("!H", self.rfile.read(2))[0]
                elif length == 127:
                    length = struct.unpack("!Q", self.rfile.read(8))[0]
                mask = self.rfile.read(4) if head[1] & 128 else None
                data = self.rfile.read(length)
                if mask:
                    data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
                if head[0] & 15 != 1:
                    continue
                self.record(json.loads(data), "websocket")
                if "/binding-interrupted/" in self.path:
                    # Simulate an ambiguous request after receipt, before completion.
                    self.connection.shutdown(socket.SHUT_RDWR)
                    break
                for event in events():
                    payload = json.dumps(event).encode()
                    prefix = bytes([129, len(payload)]) if len(payload) < 126 else b"\x81\x7e" + struct.pack("!H", len(payload))
                    self.wfile.write(prefix + payload)
                self.wfile.flush()
        except (TimeoutError, ConnectionError, socket.timeout):
            pass
        self.close_connection = True


class Rpc:
    def __init__(self, command, env, cwd):
        self.p = subprocess.Popen(command + ["app-server"], env=env, cwd=cwd,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.q = queue.Queue()
        self.messages = []
        threading.Thread(target=lambda: [self.q.put(json.loads(line)) for line in self.p.stdout], daemon=True).start()
        self.n = 0
        self.call("initialize", {"clientInfo": {"name": "poolparty_fixture", "version": "1"},
            "capabilities": {"experimentalApi": True}})
        self.send({"method": "initialized", "params": {}})

    def send(self, value):
        self.p.stdin.write(json.dumps(value) + "\n")
        self.p.stdin.flush()

    def until(self, predicate):
        while True:
            message = self.q.get(timeout=30)
            self.messages.append(message)
            if predicate(message):
                return message

    def call(self, method, params):
        self.n += 1
        self.send({"id": self.n, "method": method, "params": params})
        result = self.until(lambda m: m.get("id") == self.n)
        if "error" in result:
            raise RuntimeError({"method": method, "error": result["error"]})
        return result["result"]

    def turn(self, thread, status="completed"):
        self.call("turn/start", {"threadId": thread,
            "input": [{"type": "text", "text": "Reply with fixture ok."}]})
        result = self.until(lambda m: m.get("method") == "turn/completed")
        assert result["params"]["turn"]["status"] == status, result

    def close(self):
        self.p.terminate()
        self.p.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("codex", type=Path)
    args = parser.parse_args()
    binary = str(args.codex.resolve())
    version = subprocess.check_output([binary, "--version"], text=True).strip()
    assert version == "codex-cli 0.153.4", version
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}/routes/binding-a/codex"
    results = {"version": version, "checks": []}
    with tempfile.TemporaryDirectory(prefix="poolparty-codex-fixture-") as tmp:
        root = Path(tmp).resolve()
        home = root / "home"
        home.mkdir()
        codex_home = root / "codex"
        codex_home.mkdir()
        (codex_home / "config.toml").write_text(f'''
model = "gpt-5.4"
model_provider = "poolparty"
cli_auth_credentials_store = "file"
web_search = "disabled"
check_for_update_on_startup = false
[model_providers.poolparty]
name = "Poolparty"
base_url = "{base}"
env_key = "POOLPARTY_GRANT"
requires_openai_auth = false
wire_api = "responses"
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0
[features]
unbounded_connection_retries = false
''')
        env = {"HOME": str(home), "CODEX_HOME": str(codex_home), "PATH": "/usr/bin:/bin",
            "TMPDIR": str(root), "POOLPARTY_GRANT": "synthetic-grant"}
        profile = '(version 1)(allow default)(deny network-outbound)(allow network-outbound (remote ip "localhost:*"))'
        command = ["/usr/bin/sandbox-exec", "-p", profile, binary]

        def cli(extra):
            p = subprocess.run(command + extra, cwd=root, env=env,
                capture_output=True, text=True, timeout=45)
            if p.returncode:
                raise RuntimeError(p.stderr[-2000:])
            return [json.loads(line) for line in p.stdout.splitlines() if line.startswith("{")]

        output = cli(["exec", "--skip-git-repo-check", "--json", "Reply with fixture ok."])
        thread = next(e["thread_id"] for e in output if e.get("type") == "thread.started")
        cli(["exec", "resume", "--skip-git-repo-check", "--json", thread, "Reply again."])
        assert len(RECORDS) == 2, len(RECORDS)
        assert all(r["thread"] == thread and r["session"] == thread for r in RECORDS)
        results["checks"].append("CLI HTTP grant and bound URL; resume preserves session/thread across processes")
        rpc = Rpc(command, env, root)
        try:
            a = rpc.call("thread/start", {})["thread"]["id"]
            rpc.turn(a)
            other_base = base.replace("binding-a", "binding-b")
            b = rpc.call("thread/start", {"config": {
                "model_providers.poolparty.base_url": other_base}})["thread"]["id"]
            rpc.turn(b)
            rpc.turn(a)
            assert a != b
            assert RECORDS[-3]["path"] == RECORDS[-1]["path"]
            assert "/binding-b/" in RECORDS[-2]["path"]
            results["checks"].append("app-server per-thread bound URL overrides remain isolated across alternating turns")
            for label in ("exhausted", "unauthorized", "partial"):
                refused = rpc.call("thread/start", {"config": {
                    "model_providers.poolparty.base_url": base.replace("binding-a", "binding-" + label)}})["thread"]["id"]
                count = len(RECORDS)
                rpc.turn(refused, "failed")
                assert len(RECORDS) == count + 1
                detail = "partial HTTP SSE response" if label == "partial" else label + " HTTP admission"
                results["checks"].append(detail + " fails after one request with env bearer and retry counts zero")
        finally:
            rpc.close()
        rpc = Rpc(command, env, root)
        try:
            rpc.call("thread/resume", {"threadId": b, "config": {
                "model_providers.poolparty.base_url": other_base,
                "model_providers.poolparty.supports_websockets": True}})
            rpc.turn(b)
            assert RECORDS[-1]["transport"] == "websocket"
            assert RECORDS[-1]["thread"] == b and RECORDS[-1]["session"] == b
            assert "/binding-b/" in RECORDS[-1]["path"]
            results["checks"].append("app-server restart/resume with explicitly restored binding uses authenticated WebSocket")
            interrupted = rpc.call("thread/start", {"config": {
                "model_providers.poolparty.base_url": base.replace("binding-a", "binding-interrupted"),
                "model_providers.poolparty.supports_websockets": True}})["thread"]["id"]
            count = len(RECORDS)
            rpc.turn(interrupted)
            attempts = [r for r in RECORDS[count:] if r["metadata"].get("turn_id")]
            assert [r["transport"] for r in attempts] == ["websocket", "http"], attempts
            results["checks"].append("COUNTEREXAMPLE: interrupted WebSocket resubmits via HTTP despite both retry counts zero")
        finally:
            rpc.close()
        assert all(r["authorized"] and r["path"].endswith("/responses") for r in RECORDS)
        results["request_count"] = len(RECORDS)
        results["transports"] = sorted({r["transport"] for r in RECORDS})
    server.shutdown()
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
