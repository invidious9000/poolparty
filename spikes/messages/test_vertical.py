#!/usr/bin/env python3
"""Synthetic native executable and local router; no installed CLI or provider."""
import contextlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("vertical.py").resolve()
SPEC = importlib.util.spec_from_file_location("messages_vertical", SCRIPT)
VERTICAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERTICAL)
PROBE_SPEC = importlib.util.spec_from_file_location("messages_probe", SCRIPT.with_name("native_cli_probe.py"))
PROBE = importlib.util.module_from_spec(PROBE_SPEC)
PROBE_SPEC.loader.exec_module(PROBE)
GRANT = "synthetic-poolparty-grant-" * 2

FAKE_CLI = r'''
import json, os, pathlib, sys, urllib.request
args = sys.argv[1:]
if args == ["--version"]:
    print("2.1.263 (Claude Code)")
    sys.exit(0)
assert os.environ["CLAUDE_CODE_MAX_RETRIES"] == "0"
assert "ANTHROPIC_API_KEY" not in os.environ
assert "POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN" not in os.environ
assert "HTTPS_PROXY" not in os.environ
assert "--no-session-persistence" not in args
assert args[args.index("--tools") + 1] == "Read"
phase = "resume" if "--resume" in args else "start"
session = args[args.index("--resume" if phase == "resume" else "--session-id") + 1]
root = pathlib.Path(os.environ["CLAUDE_CONFIG_DIR"])
state = root / "history.json"
history = json.loads(state.read_text()) if phase == "resume" else []
history.append({"role": "user", "content": [{"type": "text", "text": phase}]})
def send():
    body = {"model": os.environ["ANTHROPIC_MODEL"], "stream": True,
            "max_tokens": 4096, "thinking": {"type": "enabled", "budget_tokens": 1024}, "messages": history}
    request = urllib.request.Request(os.environ["ANTHROPIC_BASE_URL"] + "/v1/messages?beta=true",
        data=json.dumps(body).encode(), headers={"Authorization": "Bearer " + os.environ["ANTHROPIC_AUTH_TOKEN"],
        "Content-Type": "application/json", "anthropic-version": "2023-06-01"})
    with urllib.request.urlopen(request, timeout=10) as response:
        wire = response.read()
    blocks = {}
    partial = {}
    for frame in wire.split(b"\n\n"):
        for line in frame.splitlines():
            if not line.startswith(b"data: "): continue
            event = json.loads(line[6:])
            if event["type"] == "content_block_start":
                blocks[event["index"]] = event["content_block"]
            elif event["type"] == "content_block_delta":
                index, delta = event["index"], event["delta"]
                if delta["type"] == "input_json_delta":
                    partial[index] = partial.get(index, "") + delta["partial_json"]
                else:
                    field = {"thinking_delta": "thinking", "signature_delta": "signature", "text_delta": "text"}[delta["type"]]
                    blocks[index][field] = blocks[index].get(field, "") + delta[field]
    for index, value in partial.items(): blocks[index]["input"] = json.loads(value)
    return list(blocks.values())
blocks = send()
history.append({"role": "assistant", "content": blocks})
tool = next(block for block in blocks if block["type"] == "tool_use")
value = pathlib.Path(tool["input"]["file_path"]).read_text()
history.append({"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool["id"], "content": value}]})
history.append({"role": "assistant", "content": send()})
state.write_text(json.dumps(history))
print(json.dumps({"type": "system", "subtype": "init", "session_id": session}))
print(json.dumps({"type": "assistant", "session_id": session, "message": {"model": os.environ["ANTHROPIC_MODEL"], "content": history[-1]["content"]}}))
print(json.dumps({"type": "result", "subtype": "success", "is_error": False, "session_id": session, "result": value}))
'''


class Router(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send_json(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def authorized(self):
        return self.headers.get("Authorization") == "Bearer " + GRANT

    def do_GET(self):
        if not self.authorized():
            self.send_json({}, 401)
        elif self.path.startswith("/api/v1/attempts/"):
            self.send_json({"state": "succeeded", "binding": "binding-a"})
        else:
            self.send_json(self.server.binding)

    def do_POST(self):
        if not self.authorized():
            self.send_json({}, 401)
            return
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.path == "/api/v1/sessions":
            self.server.binding = {"id": "binding-a", "principal": "principal-a", "account": "account-a",
                                   "intent": body, "created_at": 1, "closed_at": None}
            self.send_json(self.server.binding)
            return
        self.server.requests.append({"path": self.path, "body": body, "operation": self.headers.get("x-poolparty-operation-id")})
        if getattr(self.server, "fail", False):
            self.send_json({"error": "synthetic"}, 503)
            return
        number = len(self.server.requests)
        phase = "start" if number <= 2 else "resume"
        sample = str(self.server.work / (phase + ".txt")) if number % 2 else None
        events = list(PROBE.response_events(sample))
        events[0]["message"]["model"] = "model-a"
        wire = b"".join(map(PROBE.frame, events))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(wire)))
        self.send_header("x-poolparty-attempt-id", "attempt-" + str(number))
        self.end_headers()
        self.wfile.write(wire)


@contextlib.contextmanager
def local_router():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Router)
    server.requests = []
    server.work = Path("/synthetic")
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server, "http://127.0.0.1:" + str(server.server_address[1])
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def observed_request(observer, messages):
    body = {"model": "model-a", "stream": True, "messages": messages}
    request = urllib.request.Request("http://127.0.0.1:" + str(observer.port) + observer.path,
        data=json.dumps(body).encode(), headers={"Authorization": "Bearer " + GRANT})
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        with error:
            return error.code, error.read()


class Checks(unittest.TestCase):
    def test_safe_default_launches_nothing(self):
        with patch.object(VERTICAL, "native_launch", side_effect=AssertionError("native")), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(VERTICAL.main([]), 0)

    def test_environment_contains_only_caller_grant_and_pinned_provider(self):
        with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "synthetic-provider-key", "HTTPS_PROXY": "https://example.com"}):
            env = VERTICAL.child_environment(Path("/synthetic"), Path("/bin/claude"), GRANT, 1234, "binding-a", "model-a")
        self.assertNotIn("ANTHROPIC_API_KEY", env)
        self.assertNotIn("HTTPS_PROXY", env)
        self.assertNotIn("POOLPARTY_OP_SERVICE_ACCOUNT_TOKEN", env)
        self.assertEqual(env["ANTHROPIC_AUTH_TOKEN"], GRANT)
        self.assertEqual(env["CLAUDE_CODE_MAX_RETRIES"], "0")
        self.assertEqual(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:1234/routes/binding-a")

    def test_stream_thinking_signature_and_tools_preserved(self):
        blocks = VERTICAL.stream_blocks(b"".join(map(PROBE.frame, PROBE.response_events("/synthetic/file"))))
        self.assertEqual(blocks[0], {"type": "thinking", "thinking": "Synthetic reasoning.", "signature": "synthetic-signature"})
        self.assertEqual(blocks[1]["input"], {"file_path": "/synthetic/file"})
        self.assertEqual(VERTICAL.thinking_fingerprint(blocks[0]), VERTICAL.thinking_fingerprint({**blocks[0], "cache_control": {"type": "ephemeral"}}))
        self.assertNotEqual(VERTICAL.thinking_fingerprint(blocks[0]), VERTICAL.thinking_fingerprint({**blocks[0], "signature": "changed"}))

    def test_stopped_unsafe_tool_cannot_be_overwritten_by_later_safe_delta(self):
        events = [{"type": "message_start", "message": {"model": "model-a", "content": []}},
                  {"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "name": "Read", "id": "tool-a", "input": {}}},
                  {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": '{"file_path":"/outside/private"}'}},
                  {"type": "content_block_stop", "index": 0},
                  {"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": '{"file_path":"/synthetic/start.txt"}'}},
                  {"type": "message_stop"}]
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.stream_blocks(b"".join(map(PROBE.frame, events)), "model-a")
        events[1]["content_block"]["input"] = {"file_path": "/outside/private"}
        events = [events[0], events[1], events[4], events[3], events[-1]]
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.stream_blocks(b"".join(map(PROBE.frame, events)), "model-a")

    def test_embedded_start_content_and_events_after_terminal_are_refused(self):
        events = list(PROBE.response_events("/synthetic/file"))
        events[0]["message"]["content"] = [{"type": "tool_use", "name": "Read", "input": {"file_path": "/outside/private"}}]
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.stream_blocks(b"".join(map(PROBE.frame, events)))
        events[0]["message"]["content"] = []
        events.append({"type": "message_delta", "delta": {"stop_reason": "tool_use"}})
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.stream_blocks(b"".join(map(PROBE.frame, events)))

    def test_incomplete_or_error_stream_cannot_complete(self):
        for wire in (PROBE.frame({"type": "message_start"}), PROBE.frame({"type": "error"})):
            with self.assertRaises(VERTICAL.CheckFailed):
                VERTICAL.stream_blocks(wire)

    def test_upstream_and_native_model_aliases_are_not_silently_accepted(self):
        wire = b"".join(map(PROBE.frame, PROBE.response_events("/synthetic/file")))
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.stream_blocks(wire, "different-model")
        events = [{"type": "assistant", "session_id": "session-a", "message": {"model": "different-model"}},
                  {"type": "result", "subtype": "success", "is_error": False, "session_id": "session-a", "result": "nonce"}]
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.check_events(b"\n".join(json.dumps(event).encode() for event in events), "session-a", "nonce", "model-a")

    def test_session_or_result_mismatch_fails(self):
        for event in ({"type": "result", "subtype": "success", "session_id": "other", "result": "nonce"},
                      {"type": "result", "subtype": "error", "session_id": "session-a", "is_error": True},
                      {"type": "result", "subtype": "success", "session_id": "session-a", "result": "wrong"}):
            with self.assertRaises(VERTICAL.CheckFailed):
                VERTICAL.check_events(json.dumps(event).encode(), "session-a", "nonce")

    def test_failure_latches_before_different_body_can_dispatch(self):
        with local_router() as (server, origin), VERTICAL.Observer(origin, "binding-a", "model-a", GRANT,
                allowed_read="/synthetic/start.txt") as observer:
            server.fail = True
            self.assertEqual(observed_request(observer, [{"role": "user", "content": "first"}])[0], 403)
            server.fail = False
            self.assertEqual(observed_request(observer, [{"role": "user", "content": "changed"}])[0], 403)
            self.assertEqual(len(server.requests), 1)

    def test_duplicate_native_request_is_counted_and_never_redispatched(self):
        with local_router() as (server, origin), VERTICAL.Observer(origin, "binding-a", "model-a", GRANT,
                allowed_read="/synthetic/start.txt") as observer:
            messages = [{"role": "user", "content": "synthetic"}]
            self.assertEqual(observed_request(observer, messages)[0], 200)
            self.assertEqual(observed_request(observer, messages)[0], 403)
            self.assertEqual(observer.repeated_requests, 1)
            self.assertEqual(len(server.requests), 1)

    def test_thinking_history_requires_order_multiplicity_and_complete_content(self):
        first = {"type": "thinking", "thinking": "first", "signature": "one"}
        second = {"type": "thinking", "thinking": "second", "signature": "two"}
        for changed in ([first], [second, first], [first, second, second], [first, {**second, "signature": "changed"}]):
            with self.subTest(changed=changed), local_router() as (server, origin), VERTICAL.Observer(
                    origin, "binding-a", "model-a", GRANT,
                    prior_thinking=list(map(VERTICAL.thinking_fingerprint, [first, second])),
                    allowed_read="/synthetic/start.txt") as observer:
                self.assertEqual(observed_request(observer, [{"role": "assistant", "content": changed}])[0], 403)
                self.assertEqual(len(server.requests), 0)

    def test_out_of_scope_read_is_refused_before_native_receives_tool_payload(self):
        with local_router() as (server, origin), VERTICAL.Observer(origin, "binding-a", "model-a", GRANT,
                allowed_read="/synthetic/allowed.txt") as observer:
            status, body = observed_request(observer, [{"role": "user", "content": "synthetic"}])
            self.assertEqual(status, 403)
            self.assertNotIn(b"content_block_start", body)
            self.assertNotIn(b"/synthetic/start.txt", body)
            self.assertEqual(len(server.requests), 1)

    def test_cross_process_resume_restores_binding_route_model_and_thinking(self):
        with tempfile.TemporaryDirectory(prefix="messages-offline-") as temporary:
            root = Path(temporary).resolve()
            state = root / "state"
            binary = root / "fake-claude"
            binary.write_text("#!" + sys.executable + "\n" + FAKE_CLI)
            binary.chmod(0o700)
            server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Router)
            server.binding = None
            server.requests = []
            server.work = state / "work"
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            origin = "http://127.0.0.1:" + str(server.server_address[1])
            base = ["--live", "--router", origin, "--state-dir", str(state), "--claude", str(binary)]
            try:
                with patch.dict(os.environ, {"POOLPARTY_NATIVE_GRANT": GRANT, "ANTHROPIC_API_KEY": "synthetic-provider-key"}), \
                        patch.object(VERTICAL, "native_launch", side_effect=lambda executable: (executable, [])):
                    first = io.StringIO()
                    with contextlib.redirect_stdout(first):
                        self.assertEqual(VERTICAL.main(base + ["--phase", "start", "--pool", "pool-a", "--account", "account-a", "--model", "model-a"]), 0)
                    saved = json.loads((state / VERTICAL.MANIFEST).read_text())
                    second = io.StringIO()
                    with contextlib.redirect_stdout(second):
                        self.assertEqual(VERTICAL.main(base + ["--phase", "resume"]), 0)
                    resumed = json.loads((state / VERTICAL.MANIFEST).read_text())
                    self.assertEqual((saved["binding"], saved["session"], saved["port"]), (resumed["binding"], resumed["session"], resumed["port"]))
                    self.assertEqual(len(server.requests), 4)
                    self.assertTrue(all(r["path"] == "/routes/binding-a/v1/messages" and r["body"]["model"] == "model-a" for r in server.requests))
                    self.assertEqual(len({r["operation"] for r in server.requests}), 4)
                    self.assertTrue(json.loads(second.getvalue())["checks"][0]["prior_thinking_preserved"])
                    self.assertNotIn(GRANT, first.getvalue() + second.getvalue())
                    with self.assertRaises(VERTICAL.CheckFailed):
                        VERTICAL.main(base + ["--phase", "resume"])
            finally:
                server.shutdown()
                server.server_close()
                thread.join()


if __name__ == "__main__":
    unittest.main()
