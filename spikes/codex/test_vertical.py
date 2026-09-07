#!/usr/bin/env python3
"""Offline vertical-fixture checks: synthetic router and fake native executable."""
import http.server
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import tomllib
import unittest

SCRIPT = Path(__file__).with_name("vertical.py").resolve()
SPEC = importlib.util.spec_from_file_location("vertical", SCRIPT)
VERTICAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERTICAL)


class PureChecks(unittest.TestCase):
    def test_safe_default_does_not_require_grant_or_launch_codex(self):
        result = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0)
        self.assertIn(b"--live", result.stdout)

    def test_config_disables_retry_and_keeps_grant_out_of_shell(self):
        config = tomllib.loads(VERTICAL.config_text("http://127.0.0.1:1234", "binding-a", "synthetic-model", "low"))
        provider = config["model_providers"]["poolparty"]
        self.assertEqual(provider["base_url"], "http://127.0.0.1:1234/routes/binding-a/codex")
        self.assertFalse(provider["requires_openai_auth"])
        self.assertFalse(provider["supports_websockets"])
        self.assertEqual(provider["request_max_retries"], 0)
        self.assertEqual(provider["stream_max_retries"], 0)
        self.assertFalse(config["features"]["unbounded_connection_retries"])
        self.assertEqual(config["shell_environment_policy"]["inherit"], "none")

    def test_origins_require_https_or_literal_loopback_and_no_embedded_secrets(self):
        for origin in ["http://example.com", "http://localhost:8080", "https://token@example.com", "https://example.com/?token=value"]:
            with self.assertRaises(VERTICAL.CheckFailed):
                VERTICAL.router_url(origin)
        self.assertEqual(VERTICAL.router_url("https://example.com/"), "https://example.com")

    def test_native_trust_metadata_is_allowed_only_for_exact_isolated_workspace(self):
        expected = VERTICAL.config_text("http://127.0.0.1:1234", "binding-a", "synthetic-model", "low")
        workdir = Path("/synthetic/work")
        VERTICAL.verify_config(expected + "\n# native formatting change\n", expected, workdir)
        for trust in ("trusted", "untrusted"):
            actual = expected + '\n[projects."/synthetic/work"]\ntrust_level = ' + json.dumps(trust) + '\n'
            VERTICAL.verify_config(actual, expected, workdir)
        for extra in [
            '\n[projects."/synthetic/other"]\ntrust_level = "trusted"\n',
            '\n[projects."/synthetic/work"]\ntrust_level = "invalid"\n',
            '\n[projects."/synthetic/work"]\ntrust_level = "trusted"\nother = true\n',
            '\n[projects."/synthetic/work"]\ntrust_level = "trusted"\n[projects."/synthetic/other"]\ntrust_level = "trusted"\n',
        ]:
            with self.assertRaises(VERTICAL.CheckFailed):
                VERTICAL.verify_config(expected + extra, expected, workdir)

    def test_semantic_config_check_rejects_provider_retry_or_type_changes(self):
        expected = VERTICAL.config_text("http://127.0.0.1:1234", "binding-a", "synthetic-model", "low")
        for old, new in [
            ("binding-a/codex", "binding-b/codex"),
            ("request_max_retries = 0", "request_max_retries = 1"),
            ("stream_max_retries = 0", "stream_max_retries = 1"),
            ("supports_websockets = false", "supports_websockets = true"),
            ("supports_websockets = false", "supports_websockets = 0"),
            ('env_key = "POOLPARTY_GRANT"', 'env_key = "OTHER_GRANT"'),
            ("requires_openai_auth = false", "requires_openai_auth = true"),
        ]:
            with self.assertRaises(VERTICAL.CheckFailed):
                VERTICAL.verify_config(expected.replace(old, new), expected, Path("/synthetic/work"))

    def test_text_completion_without_tool_or_changed_thread_fails(self):
        events = [{"type": "thread.started", "thread_id": "thread-a"}, {"type": "turn.completed"}]
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.check_events(b"\n".join(json.dumps(v).encode() for v in events))
        events.insert(1, {"type": "item.completed", "item": {"type": "command_execution", "status": "completed", "exit_code": 0}})
        with self.assertRaises(VERTICAL.CheckFailed):
            VERTICAL.check_events(b"\n".join(json.dumps(v).encode() for v in events), "thread-b")


class FakeRouter(http.server.BaseHTTPRequestHandler):
    binding = None
    creates = 0
    inspections = 0

    def log_message(self, *_):
        pass

    def authorized(self):
        return self.headers.get("Authorization") == "Bearer " + "synthetic-grant-" * 3

    def send_json(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        if not self.authorized() or self.path != "/api/v1/sessions":
            return self.send_json({}, 403)
        intent = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        type(self).creates += 1
        type(self).binding = {"id": "binding-a", "principal": "caller-a", "account": intent["account"],
            "intent": intent, "created_at": 0, "closed_at": None}
        self.send_json(type(self).binding)

    def do_GET(self):
        if not self.authorized() or self.path != "/api/v1/sessions/binding-a":
            return self.send_json({}, 403)
        type(self).inspections += 1
        self.send_json(type(self).binding)


class OfflineLifecycle(unittest.TestCase):
    def test_separate_native_processes_preserve_binding_and_refuse_replay(self):
        # This --live invocation reaches only the local fixture and fake executable.
        # No installed Codex binary or provider endpoint participates.
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeRouter)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        FakeRouter.creates = 0
        FakeRouter.inspections = 0
        try:
            with tempfile.TemporaryDirectory(prefix="poolparty-native-offline-") as temporary:
                root = Path(temporary).resolve()
                native = root / "fake-codex"
                native.write_text("#!" + sys.executable + "\n" + '''
import json, os, pathlib, sys
assert "OPENAI_API_KEY" not in os.environ
assert "ANTHROPIC_AUTH_TOKEN" not in os.environ
assert "OP_SERVICE_ACCOUNT_TOKEN" not in os.environ
assert os.environ["POOLPARTY_GRANT"] == "synthetic-grant-" * 3
assert pathlib.Path(os.environ["CODEX_HOME"]).parent == pathlib.Path.cwd().parent
if sys.argv[1:] == ["--version"]:
    print("codex-cli 0.153.4")
    sys.exit(0)
assert "--json" in sys.argv
assert "--strict-config" in sys.argv
if "resume" in sys.argv:
    assert "thread-a" in sys.argv
    with open("proof.txt", "a") as output:
        output.write("resumed\\n")
else:
    pathlib.Path("proof.txt").write_bytes(pathlib.Path("seed.txt").read_bytes() + b"started\\n")
for event in [
    {"type":"thread.started","thread_id":"thread-a"},
    {"type":"item.completed","item":{"type":"command_execution","status":"completed","exit_code":0}},
    {"type":"turn.completed"}
]:
    print(json.dumps(event))
''')
                native.chmod(0o700)
                state = root / "state"
                base = [sys.executable, str(SCRIPT), "--live", "--router", f"http://127.0.0.1:{server.server_port}",
                    "--state-dir", str(state), "--codex", str(native)]
                environment = dict(os.environ, POOLPARTY_NATIVE_GRANT="synthetic-grant-" * 3,
                    OPENAI_API_KEY="synthetic-must-not-inherit", ANTHROPIC_AUTH_TOKEN="synthetic-must-not-inherit")
                first = subprocess.run(base + ["--phase", "start", "--pool", "pool-a", "--account", "account-a", "--model", "synthetic-model"],
                    env=environment, capture_output=True, timeout=20)
                self.assertEqual(first.returncode, 0, first.stdout)
                second = subprocess.run(base + ["--phase", "resume"], env=environment, capture_output=True, timeout=20)
                self.assertEqual(second.returncode, 0, second.stdout)
                report = json.loads(second.stdout)
                self.assertTrue(report["checks"][0]["same_native_thread"])
                self.assertTrue(report["checks"][0]["same_router_binding"])
                self.assertEqual(FakeRouter.creates, 1)
                self.assertGreaterEqual(FakeRouter.inspections, 4)
                self.assertNotIn(str(state).encode(), second.stdout)
                self.assertNotIn(b"synthetic-grant", second.stdout)
                again = subprocess.run(base + ["--phase", "resume"], env=environment, capture_output=True, timeout=20)
                self.assertNotEqual(again.returncode, 0)
        finally:
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    unittest.main()
