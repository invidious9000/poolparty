#!/usr/bin/env python3
"""Process-level synthetic router acceptance. No external services or credentials."""
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/poolpartyd").resolve()
    token = secrets.token_hex(32)
    with tempfile.TemporaryDirectory(prefix="poolparty-smoke-") as temporary:
        state = Path(temporary).resolve() / "state"
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        # Ignore ambient proxy configuration for this isolated loopback check.
        http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        environment = os.environ | {
            "POOLPARTY_STATE_DIR": str(state),
            "POOLPARTY_LISTEN": f"127.0.0.1:{port}",
            "POOLPARTY_DEMO_TOKEN": token,
        }

        def request(path, body=None, operation=None, authenticated=True):
            headers = {"Content-Type": "application/json"}
            if authenticated:
                headers["Authorization"] = f"Bearer {token}"
            if operation:
                headers["x-poolparty-operation-id"] = operation
            data = json.dumps(body).encode() if body is not None else None
            req = urllib.request.Request(base + path, data=data, headers=headers)
            try:
                response = http.open(req, timeout=5)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                return response.status, response.headers, response.read()

        def start():
            process = subprocess.Popen([str(binary), "--demo"], env=environment,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError("synthetic daemon exited during startup")
                try:
                    if request("/healthz", authenticated=False)[0] == 200:
                        return process
                except (OSError, urllib.error.URLError):
                    pass
                time.sleep(0.05)
            stop(process)
            raise RuntimeError("synthetic daemon readiness timed out")

        def stop(process):
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)

        process = start()
        try:
            intent = {"session": "smoke-session", "pool": "demo", "product": "codex_subscription",
                      "model": "synthetic-model", "account": None, "effort": None}
            assert request("/api/v1/sessions", intent, authenticated=False)[0] == 401
            status, _, raw = request("/api/v1/sessions", intent)
            assert status == 201
            binding = json.loads(raw)
            route = f"/routes/{binding['id']}/codex/responses"
            payload = {"model": "synthetic-model", "input": "synthetic request", "stream": True}
            status, headers, raw = request(route, payload, "first-operation")
            assert status == 200 and b"response.completed" in raw
            attempt = headers["x-poolparty-attempt-id"]
            assert json.loads(request(f"/api/v1/attempts/{attempt}")[2])["state"] == "succeeded"
            status, _, raw = request(route, payload, "first-operation")
            error = json.loads(raw)["error"]
            assert status == 409 and error["code"] == "operation_already_exists"
            assert error["attempt_id"] == attempt
            stop(process)
            process = start()
            restored = json.loads(request(f"/api/v1/sessions/{binding['id']}")[2])
            assert restored["id"] == binding["id"] and restored["account"] == binding["account"]
            assert request(route, payload, "second-operation")[0] == 200
            assert request(f"/api/v1/sessions/{binding['id']}/close", {})[0] == 200
            assert request("/api/v1/sessions", intent)[0] == 409
            print("PASS: auth, create, stream, durable completion, duplicate rejection, restart, pinned resume, close")
        finally:
            stop(process)


if __name__ == "__main__":
    main()
