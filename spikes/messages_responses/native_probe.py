#!/usr/bin/env python3
"""Explicit installed-Claude probe of the bridge against synthetic Responses.

macOS loopback sandbox, isolated native home, synthetic Read tools only. No live
provider or operator auth access. Not part of automated unittest discovery.
"""
import argparse
import copy
import http.server
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import uuid

import bridge as B
from test_bridge import BINDING, GRANT, response, wire

_SPEC = importlib.util.spec_from_file_location(
    "poolparty_messages_native", Path(__file__).resolve().parents[1] / "messages/vertical.py")
NATIVE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(NATIVE)
EXPECTED_VERSION = "2.1.267 (Claude Code)"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", required=True, type=Path)
    args = parser.parse_args()
    binary = args.cli.absolute()
    launch, prefix = NATIVE.native_launch(binary)
    with tempfile.TemporaryDirectory(prefix="poolparty-bridge-native-") as temporary:
        root = Path(temporary).resolve()
        for name in ("home", "claude", "tmp", "work", "bridge"):
            (root / name).mkdir(mode=0o700)
        for phase in ("start", "resume"):
            B.COMMON.write_private(root / "work" / (phase + ".txt"), "synthetic-" + phase + "\n")
        model = BINDING["intent"]["model"]
        B.COMMON.write_private(root / "settings.json", json.dumps({"model": model,
            "permissions": {"defaultMode": "dontAsk"}}))
        requests = []
        state = {"phase": "start", "calls": 0}

        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                B.require(self.path == "/routes/binding-a/codex/responses", "synthetic_route_changed")
                B.require(self.headers.get("Authorization") == "Bearer " + GRANT, "synthetic_grant_changed")
                state["calls"] += 1
                B.require(state["calls"] <= 2, "synthetic_phase_request_limit")
                complete = response(state["calls"] == 1)
                complete["output"][0]["id"] = "rs-" + state["phase"] + str(state["calls"])
                complete["output"][0]["encrypted_content"] += "-" + state["phase"] + str(state["calls"])
                if state["calls"] == 1:
                    complete["output"][1]["call_id"] = "call-" + state["phase"]
                    complete["output"][1]["arguments"] = json.dumps({"file_path": str(root / "work" / (state["phase"] + ".txt"))})
                else:
                    complete["output"][1]["content"][0]["text"] = "synthetic-" + state["phase"]
                data = wire(complete)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("x-poolparty-attempt-id", "attempt-" + str(len(requests)))
                self.end_headers()
                self.wfile.write(data)

        upstream = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        origin = "http://127.0.0.1:" + str(upstream.server_address[1])
        journal = B.Journal(root / "bridge", B.initial_state(origin, copy.deepcopy(BINDING), True, True))
        session, port = str(uuid.uuid4()), 0
        summaries = []
        try:
            for phase in ("start", "resume"):
                state.update(phase=phase, calls=0)
                if phase == "resume":
                    journal = B.Journal(root / "bridge")
                with B.Bridge(journal, GRANT, port) as bridge:
                    port = bridge.port
                    environment = NATIVE.child_environment(root, binary, GRANT, port, "binding-a", model)
                    environment.pop("MAX_THINKING_TOKENS")
                    version = B.COMMON.run_native(launch, root, environment, prefix + ["--version"], 10).decode().strip()
                    B.require(version == EXPECTED_VERSION, "native_version_not_qualified")
                    arguments = ["--bare", "--print", "--output-format", "stream-json", "--verbose",
                        "--include-partial-messages", "--model", model, "--tools", "Read", "--allowedTools", "Read",
                        "--effort", BINDING["intent"]["effort"],
                        "--permission-mode", "dontAsk", "--strict-mcp-config", "--setting-sources", "",
                        "--settings", str(root / "settings.json"), "--max-turns", "4",
                        "--system-prompt", "Synthetic local protocol check. Read only the requested file.",
                        "--resume" if phase == "resume" else "--session-id", session,
                        "Read " + str(root / "work" / (phase + ".txt")) + " and return its contents."]
                    try:
                        raw = B.COMMON.run_native(launch, root, environment, prefix + arguments, 45)
                    except B.COMMON.CheckFailed:
                        raise B.Failed(bridge.error or "native_probe_failed") from None
                    summaries.append(NATIVE.check_events(raw, session, "synthetic-" + phase, model))
                    B.require(state["calls"] == 2, "native_tool_roundtrip_missing")
            B.require(len(requests) == 4, "native_request_count_changed")
            reasoning = [item for item in requests[-1]["input"] if item.get("type") == "reasoning"]
            results = [item for item in requests[-1]["input"] if item.get("type") == "function_call_output"]
            B.require(len(reasoning) == 3 and len(results) == 2, "native_resume_lost_continuation")
            B.require(all(not json.loads(item["output"])["is_error"] for item in results), "native_read_failed")
            print(json.dumps({"cli_version": version, "synthetic_upstream_requests": len(requests),
                "native_tool_roundtrips": len(results), "reasoning_items_preserved": len(reasoning),
                "bridge_restart_and_native_resume": True, "phases": summaries}, indent=2))
        finally:
            upstream.shutdown()
            upstream.server_close()
            thread.join()


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except B.COMMON.CheckFailed as error:
        raise SystemExit(str(error)) from None
