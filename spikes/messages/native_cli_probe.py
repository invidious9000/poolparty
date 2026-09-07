"""Run an installed Messages CLI against a synthetic loopback origin on macOS.

No vendor traffic, credentials, or production configuration. macOS sandbox-exec
enforces loopback-only outbound networking. Raw CLI requests/output stay in memory.
This is a native client probe, not a Poolparty daemon or provider integration test.
"""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


GRANT = "synthetic-poolparty-grant"
BINDING_PATH = "/routes/binding_synthetic/v1/messages"


def frame(event):
    return ("event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n").encode()


def response_events(tool_path=None):
    yield {"type": "message_start", "message": {
        "id": "synthetic-message", "type": "message", "role": "assistant",
        "model": "kimi-for-coding", "content": [], "stop_reason": None,
        "stop_sequence": None, "usage": {"input_tokens": 10, "output_tokens": 0}}}
    if tool_path:
        yield {"type": "content_block_start", "index": 0,
               "content_block": {"type": "thinking", "thinking": "", "signature": ""}}
        yield {"type": "content_block_delta", "index": 0,
               "delta": {"type": "thinking_delta", "thinking": "Synthetic reasoning."}}
        yield {"type": "content_block_delta", "index": 0,
               "delta": {"type": "signature_delta", "signature": "synthetic-signature"}}
        yield {"type": "content_block_stop", "index": 0}
        yield {"type": "content_block_start", "index": 1,
               "content_block": {"type": "tool_use", "id": "synthetic-tool",
                                 "name": "Read", "input": {}}}
        argument = json.dumps({"file_path": tool_path})
        for part in (argument[:12], argument[12:]):
            yield {"type": "content_block_delta", "index": 1,
                   "delta": {"type": "input_json_delta", "partial_json": part}}
        yield {"type": "content_block_stop", "index": 1}
    else:
        yield {"type": "content_block_start", "index": 0,
               "content_block": {"type": "text", "text": ""}}
        yield {"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": "Synthetic continuation complete."}}
        yield {"type": "content_block_stop", "index": 0}
    yield {"type": "message_delta", "delta": {
        "stop_reason": "tool_use" if tool_path else "end_turn", "stop_sequence": None},
        "usage": {"output_tokens": 8}}
    yield {"type": "message_stop"}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli", required=True, help="Absolute path of installed claude binary")
    parser.add_argument("--expected-version", default="2.1.263 (Claude Code)")
    args = parser.parse_args()
    cli = Path(args.cli).resolve(strict=True)
    sandbox = Path("/usr/bin/sandbox-exec")
    if not sandbox.is_file():
        raise SystemExit("Requires macOS sandbox-exec; no unrestricted fallback.")

    with tempfile.TemporaryDirectory(prefix="poolparty-messages-native-") as tmp:
        root = Path(tmp).resolve()
        work = root / "work"
        home = root / "home"
        work.mkdir()
        home.mkdir()
        sample = work / "synthetic.txt"
        sample.write_text("synthetic fixture value\n")
        state = {"mode": "success", "requests": []}

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *unused):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
                state["requests"].append({"path": self.path, "body": body,
                    "bearer": self.headers.get("Authorization") == "Bearer " + GRANT,
                    "api_key": self.headers.get("X-Api-Key") == GRANT})
                if not state["requests"][-1]["bearer"]:
                    self.send_error(401)
                    return
                if self.path.split("?")[0] != BINDING_PATH:
                    self.send_error(404)
                    return
                if state["mode"] == "exhausted":
                    data = json.dumps({"type": "error", "error": {
                        "type": "permission_error", "message": "Synthetic bound quota exhausted"}}).encode()
                    self.send_response(403)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                    return
                events = response_events(str(sample) if len(state["requests"]) == 1 else None)
                data = b"".join(map(frame, events))
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        port = server.server_address[1]
        profile = '(version 1) (allow default) (deny network*) ' \
                  '(allow network-outbound (remote ip "localhost:*"))'
        env = {"HOME": str(home), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
               "TMPDIR": str(root), "CLAUDE_CONFIG_DIR": str(home / ".claude"),
               "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}/routes/binding_synthetic",
               "ANTHROPIC_AUTH_TOKEN": GRANT,
               "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_AUTOUPDATER": "1"}
        version = subprocess.run([str(sandbox), "-p", profile, str(cli), "--version"],
                                 cwd=work, env=env, capture_output=True,
                                 text=True, check=True, timeout=10).stdout.strip()
        if version != args.expected_version:
            server.shutdown()
            server.server_close()
            thread.join()
            raise SystemExit("Installed CLI version differs from expected fixture version.")
        command = [str(sandbox), "-p", profile, str(cli), "--bare", "--print",
                   "--output-format", "stream-json", "--verbose", "--include-partial-messages",
                   "--model", "kimi-for-coding", "--tools", "Read", "--allowedTools", "Read",
                   "--permission-mode", "dontAsk", "--strict-mcp-config",
                   "--setting-sources", "", "--no-session-persistence",
                   "--system-prompt", "Synthetic local protocol test.",
                   "Read the synthetic file requested by the tool and finish."]
        receipts = []
        try:
            for mode in ("success", "exhausted"):
                state["mode"] = mode
                state["requests"] = []
                try:
                    result = subprocess.run(command, cwd=work, env=env, capture_output=True,
                                            timeout=35, text=True)
                    output = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
                    exit_code = result.returncode
                except subprocess.TimeoutExpired:
                    output = []
                    exit_code = "timeout"
                requests = state["requests"]
                history = [block for req in requests[1:] for msg in req["body"].get("messages", [])
                           for block in msg.get("content", []) if isinstance(block, dict)]
                receipts.append({"scenario": mode, "exit_code": exit_code,
                    "requests": len(requests),
                    "binding_path_preserved": bool(requests) and all(
                        r["path"].split("?")[0] == BINDING_PATH for r in requests),
                    "bearer_grant_observed": bool(requests) and all(r["bearer"] for r in requests),
                    "api_key_grant_observed": bool(requests) and all(r["api_key"] for r in requests),
                    "model_pin_preserved": bool(requests) and all(
                        r["body"].get("model") == "kimi-for-coding" for r in requests),
                    "thinking_returned": any(b.get("thinking") == "Synthetic reasoning." for b in history),
                    "signature_returned": any(b.get("signature") == "synthetic-signature" for b in history),
                    "tool_result_returned": any(b.get("type") == "tool_result" and
                        b.get("tool_use_id") == "synthetic-tool" and not b.get("is_error") for b in history),
                    "result_error": any(e.get("type") == "result" and e.get("is_error") for e in output),
                    "result_success": any(e.get("type") == "result" and not e.get("is_error") for e in output)})
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
        print(json.dumps({"cli_version": version, "network": "sandbox enforced loopback only",
                          "receipts": receipts}, indent=2))
        success, exhausted = receipts
        required = ("binding_path_preserved", "bearer_grant_observed", "model_pin_preserved")
        assert all(all(r[key] for key in required) for r in receipts)
        assert not any(r["api_key_grant_observed"] for r in receipts)
        assert success["exit_code"] == 0 and success["requests"] == 2
        assert all(success[key] for key in ("thinking_returned", "signature_returned",
                                          "tool_result_returned", "result_success"))
        assert not success["result_error"]
        assert exhausted["exit_code"] == 1 and exhausted["requests"] == 1
        assert exhausted["result_error"] and not exhausted["result_success"]


if __name__ == "__main__":
    main()
