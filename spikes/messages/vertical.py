#!/usr/bin/env python3
"""Explicit native Messages start/resume through an already-running Poolparty.

No network or native process without --live. Provider credentials never enter
this fixture. A bounded loopback observer records only in-memory protocol evidence
and refuses repeated request bodies; this instrumentation is not native parity.
"""
import argparse
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import urllib.error
import urllib.request
import uuid

# Reuse the existing private-state, bounded-process and control-API safeguards.
_SPEC = importlib.util.spec_from_file_location("poolparty_native_common", Path(__file__).resolve().parents[1] / "codex/vertical.py")
COMMON = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(COMMON)
CheckFailed = COMMON.CheckFailed
EXPECTED_VERSION = "2.1.263 (Claude Code)"
MANIFEST = "messages-vertical-state.json"
BODY_LIMIT = 2 * 1024 * 1024
STREAM_LIMIT = 4 * 1024 * 1024
MAX_REQUESTS = 6
LAST_OBSERVER = None


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def thinking_fingerprint(block):
    # Cache placement is native metadata, separate from signed reasoning content.
    return fingerprint({key: block[key] for key in ("type", "thinking", "signature", "data") if key in block})


def history_blocks(body):
    messages = body.get("messages", [])
    if not isinstance(messages, list):
        raise CheckFailed("invalid_messages_history")
    return [block for message in messages if isinstance(message, dict)
            for block in message.get("content", []) if isinstance(block, dict)]


def stream_blocks(raw, expected_model=None):
    """Collect completed thinking/tool blocks without changing relayed bytes."""
    blocks = {}
    terminal = False
    started = False
    stopped = set()
    tools = {}
    try:
        for frame in raw.replace(b"\r\n", b"\n").split(b"\n\n"):
            data = b"\n".join(line[5:].lstrip(b" ") for line in frame.split(b"\n") if line.startswith(b"data:"))
            if not data:
                continue
            event = json.loads(data)
            if terminal:
                raise CheckFailed("observer_event_after_message_stop")
            kind = event.get("type")
            if kind == "message_start":
                if started or terminal or event.get("message", {}).get("content") != []:
                    raise CheckFailed("observer_message_sequence_invalid")
                started = True
            elif kind.startswith("content_block_") and (not started or terminal):
                raise CheckFailed("observer_message_sequence_invalid")
            if kind == "message_start" and expected_model is not None and event.get("message", {}).get("model") != expected_model:
                raise CheckFailed("observer_upstream_model_label_differs_from_pin")
            if kind == "error":
                raise CheckFailed("upstream_stream_error_no_automatic_retry")
            if kind == "content_block_start":
                index = event["index"]
                if index in blocks:
                    raise CheckFailed("observer_reused_content_block_index")
                blocks[index] = dict(event["content_block"])
                if blocks[index].get("type") == "tool_use":
                    if blocks[index].get("input") != {}:
                        raise CheckFailed("observer_streaming_tool_input_must_start_empty")
                    tools[index] = ""
            elif kind == "content_block_delta":
                index, delta = event["index"], event["delta"]
                if index not in blocks or index in stopped:
                    raise CheckFailed("observer_content_delta_outside_block")
                field = {"thinking_delta": "thinking", "signature_delta": "signature", "text_delta": "text"}.get(delta.get("type"))
                if field:
                    blocks[index][field] = blocks[index].get(field, "") + delta[field]
                elif delta.get("type") == "input_json_delta":
                    tools[index] = tools.get(index, "") + delta["partial_json"]
            elif kind == "content_block_stop":
                index = event["index"]
                if index not in blocks or index in stopped:
                    raise CheckFailed("observer_content_stop_outside_block")
                stopped.add(index)
            elif kind == "message_stop":
                if not started or terminal or set(blocks) != stopped:
                    raise CheckFailed("observer_message_sequence_invalid")
                terminal = True
        for index, value in tools.items():
            if value:
                blocks[index]["input"] = json.loads(value)
    except (ValueError, KeyError, TypeError, AttributeError):
        raise CheckFailed("observer_stream_shape_invalid") from None
    if not terminal:
        raise CheckFailed("observer_stream_incomplete_no_automatic_retry")
    return list(blocks.values())


class Observer:
    """One bounded request at a time, exact binding/model, zero upstream retries."""
    def __init__(self, origin, binding, model, grant, port=0, prior_thinking=(), allowed_read=None):
        self.origin, self.binding, self.model, self.grant = origin, binding, model, grant
        self.allowed_read = str(allowed_read) if allowed_read is not None else None
        self.path = "/routes/" + COMMON.identifier(binding) + "/v1/messages"
        self.lock = threading.Lock()
        self.seen = set()
        self.thinking = list(prior_thinking)
        self.echoed = []
        self.tools = set()
        self.returned_tools = set()
        self.requests = 0
        self.repeated_requests = 0
        self.error = None
        self.attempts = []
        self.signatures = 0
        self.started_with_prior = bool(prior_thinking)
        self.prior_preserved = not prior_thinking
        observer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def setup(self):
                self.request.settimeout(5)
                super().setup()

            def log_message(self, *_):
                pass

            def reject(self, reason, status=403):
                observer.error = observer.error or reason
                data = json.dumps({"type": "error", "error": {"type": "permission_error", "message": reason}}).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(data)
                self.close_connection = True

            def do_GET(self):
                self.reject("observer_unsupported_route")

            def do_POST(self):
                self.connection.settimeout(180)
                if not observer.lock.acquire(timeout=5):
                    self.reject("observer_concurrent_request_refused")
                    return
                headers_sent = False
                try:
                    if observer.error:
                        raise CheckFailed("observer_previous_failure_no_automatic_retry")
                    if (self.path.split("?")[0] != observer.path
                            or self.headers.get_all("Authorization") != ["Bearer " + observer.grant]
                            or self.headers.get("X-Api-Key") is not None
                            or self.headers.get("Transfer-Encoding") is not None):
                        raise CheckFailed("observer_route_or_caller_auth_changed")
                    length = int(self.headers.get("Content-Length", "0"))
                    if not 0 < length <= BODY_LIMIT:
                        raise CheckFailed("observer_request_bound_exceeded")
                    raw = self.rfile.read(length)
                    if len(raw) != length:
                        raise CheckFailed("observer_request_incomplete")
                    body = json.loads(raw)
                    if not isinstance(body, dict) or body.get("model") != observer.model or body.get("stream") is not True:
                        raise CheckFailed("observer_model_or_stream_pin_changed")
                    digest = fingerprint(body)
                    if digest in observer.seen:
                        observer.repeated_requests += 1
                        raise CheckFailed("observer_repeated_request_refused")
                    if observer.requests >= MAX_REQUESTS:
                        raise CheckFailed("observer_request_budget_exceeded")
                    blocks = history_blocks(body)
                    present = [thinking_fingerprint(block) for block in blocks if block.get("type") in ("thinking", "redacted_thinking")]
                    if present != observer.thinking:
                        raise CheckFailed("observer_thinking_history_changed")
                    observer.echoed = list(present)
                    if observer.requests == 0:
                        observer.prior_preserved = present == observer.thinking
                    observer.returned_tools.update(block.get("tool_use_id") for block in blocks
                        if block.get("type") == "tool_result" and not block.get("is_error"))
                    observer.seen.add(digest)
                    observer.requests += 1
                    headers = {"Authorization": "Bearer " + observer.grant, "Content-Type": "application/json",
                               "x-poolparty-operation-id": "native-" + uuid.uuid4().hex}
                    for name in ("anthropic-version", "anthropic-beta"):
                        if self.headers.get(name):
                            headers[name] = self.headers[name]
                    request = urllib.request.Request(observer.origin + observer.path, data=raw, headers=headers, method="POST")
                    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), COMMON.NoRedirect())
                    try:
                        upstream = opener.open(request, timeout=150)
                    except urllib.error.HTTPError as error:
                        code = error.code
                        error.close()
                        raise CheckFailed("router_http_" + str(code)) from None
                    with upstream:
                        if upstream.status != 200 or upstream.headers.get_content_type() != "text/event-stream":
                            raise CheckFailed("observer_upstream_stream_expected")
                        attempt = upstream.headers.get("x-poolparty-attempt-id")
                        if not attempt:
                            raise CheckFailed("observer_attempt_id_missing")
                        observer.attempts.append(COMMON.identifier(attempt))
                        # Bound and validate all tool paths before native execution.
                        # This observer intentionally does not qualify stream latency.
                        payload = bytearray()
                        while True:
                            chunk = upstream.read1(65536)
                            if not chunk:
                                break
                            payload.extend(chunk)
                            if len(payload) > STREAM_LIMIT:
                                raise CheckFailed("observer_stream_bound_exceeded")
                        completed_blocks = stream_blocks(bytes(payload), observer.model)
                        for block in completed_blocks:
                            if block.get("type") == "tool_use" and (
                                    block.get("name") != "Read" or not isinstance(block.get("input"), dict)
                                    or block["input"].get("file_path") != observer.allowed_read):
                                raise CheckFailed("observer_tool_outside_synthetic_read_scope")
                        for block in completed_blocks:
                            if block.get("type") in ("thinking", "redacted_thinking"):
                                observer.thinking.append(thinking_fingerprint(block))
                                observer.signatures += int(bool(block.get("signature")))
                            elif block.get("type") == "tool_use":
                                if block.get("name") != "Read":
                                    raise CheckFailed("observer_unexpected_tool")
                                observer.tools.add(block["id"])
                        self.send_response(200)
                        self.send_header("Content-Type", "text/event-stream")
                        self.send_header("Content-Length", str(len(payload)))
                        self.send_header("Connection", "close")
                        self.end_headers()
                        headers_sent = True
                        self.wfile.write(payload)
                        self.wfile.flush()
                except CheckFailed as error:
                    observer.error = observer.error or str(error)
                    if not headers_sent:
                        self.reject(str(error))
                except (OSError, ValueError, TypeError, KeyError, urllib.error.URLError):
                    observer.error = "observer_operation_failed_no_automatic_retry"
                    if not headers_sent:
                        self.reject(observer.error)
                finally:
                    self.close_connection = True
                    observer.lock.release()

        class Server(http.server.ThreadingHTTPServer):
            def handle_error(self, request, client_address):
                observer.error = observer.error or "observer_handler_failed"

        self.server = Server(("127.0.0.1", port), Handler)
        self.server.daemon_threads = False
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def port(self):
        return self.server.server_address[1]

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)

    def summary(self):
        if self.error:
            raise CheckFailed(self.error)
        if not self.tools or not self.tools <= self.returned_tools:
            raise CheckFailed("observer_successful_tool_roundtrip_missing")
        if not self.echoed:
            raise CheckFailed("observer_thinking_roundtrip_missing")
        if not self.prior_preserved:
            raise CheckFailed("observer_prior_thinking_changed_on_resume")
        return {"requests": self.requests, "successful_tool_roundtrips": len(self.tools),
                "thinking_blocks_echoed": len(self.echoed), "signature_blocks_observed": self.signatures,
                "prior_thinking_preserved": self.prior_preserved, "observed_repeated_requests": self.repeated_requests,
                "observer_replay_guard_enabled": True, "observer_response_buffering_enabled": True,
                "native_retry_parity_not_claimed": True}


def settings_text(model):
    return json.dumps({"model": model, "alwaysThinkingEnabled": True, "permissions": {"defaultMode": "dontAsk"}}, sort_keys=True)


def child_environment(root, binary, grant, port, binding, model):
    environment = COMMON.child_environment(root, binary, grant)
    environment.pop("CODEX_HOME")
    environment.pop("POOLPARTY_GRANT")
    environment.update({"CLAUDE_CONFIG_DIR": str(root / "claude"),
        "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}/routes/{binding}",
        "ANTHROPIC_AUTH_TOKEN": grant, "ANTHROPIC_MODEL": model,
        "ANTHROPIC_DEFAULT_SONNET_MODEL": model, "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
        "ANTHROPIC_DEFAULT_HAIKU_MODEL": model,
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "DISABLE_AUTOUPDATER": "1",
        "CLAUDE_CODE_MAX_RETRIES": "0", "CLAUDE_CODE_MAX_OUTPUT_TOKENS": "4096",
        "MAX_THINKING_TOKENS": "1024"})
    return environment


def native_launch(binary):
    sandbox = Path("/usr/bin/sandbox-exec")
    if not sandbox.is_file():
        raise CheckFailed("native_fixture_requires_macos_loopback_sandbox")
    profile = '(version 1) (allow default) (deny network*) (allow network-outbound (remote ip "localhost:*"))'
    return sandbox, ["-p", profile, str(binary)]


def check_events(raw, session, nonce, model=None):
    try:
        events = [json.loads(line) for line in raw.splitlines() if line.strip()]
    except (ValueError, UnicodeError):
        raise CheckFailed("native_events_invalid_json") from None
    if not all(isinstance(event, dict) for event in events):
        raise CheckFailed("native_events_invalid_shape")
    sessions = {event.get("session_id") for event in events if event.get("session_id")}
    results = [event for event in events if event.get("type") == "result"]
    if sessions != {session}:
        raise CheckFailed("native_session_affinity_failed")
    if len(results) != 1 or results[0].get("is_error") or results[0].get("subtype") != "success":
        raise CheckFailed("native_result_failed_no_automatic_retry")
    if model is not None:
        assistants = [event.get("message", {}) for event in events if event.get("type") == "assistant"]
        if not assistants or any(message.get("model") != model for message in assistants):
            raise CheckFailed("native_output_model_label_differs_from_pin")
    if nonce not in results[0].get("result", ""):
        raise CheckFailed("native_read_artifact_result_missing")
    return {"native_session_preserved": True, "synthetic_artifact_verified": True, "event_count": len(events)}


def inspect_binding(origin, grant, binding_id, expected=None):
    value = COMMON.api(origin, grant, "GET", "/api/v1/sessions/" + COMMON.identifier(binding_id))
    intent = value.get("intent", {})
    if (value.get("id") != binding_id or value.get("closed_at") is not None or not isinstance(intent, dict)
            or intent.get("product") != "glm_coding" or intent.get("effort") is not None):
        raise CheckFailed("binding_must_be_open_glm_coding_without_effort_mapping")
    COMMON.identifier(value.get("account"))
    if expected is not None and value != expected:
        raise CheckFailed("binding_changed_across_native_processes")
    return value


def run_phase(binary, root, environment, manifest, phase, timeout, keep_output):
    launch, prefix = native_launch(binary)
    sample = root / "work" / (phase + ".txt")
    nonce = COMMON.read_private(sample, 1024).decode().strip()
    args = ["--bare", "--print", "--output-format", "stream-json", "--verbose", "--include-partial-messages",
            "--model", manifest["model"], "--tools", "Read", "--allowedTools", "Read",
            "--permission-mode", "dontAsk", "--strict-mcp-config", "--setting-sources", "",
            "--settings", str(root / "settings.json"), "--max-turns", "4",
            "--system-prompt", "Read only the exact synthetic file requested. Do not read other files or environment. Return its contents."]
    args += ["--resume" if phase == "resume" else "--session-id", manifest["session"]]
    args += ["Use the Read tool on " + str(sample) + ". Reply with its exact contents. "
             "This is a synthetic tool and thinking preservation check."]
    raw = COMMON.run_native(launch, root, environment, prefix + args, timeout)
    if keep_output:
        COMMON.write_private(root / ("native-" + phase + ".stdout"), raw)
    return check_events(raw, manifest["session"], nonce, manifest["model"])


def main(argv=None):
    global LAST_OBSERVER
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--router")
    parser.add_argument("--state-dir", type=Path)
    parser.add_argument("--phase", choices=("all", "start", "resume"), default="all")
    parser.add_argument("--binding")
    parser.add_argument("--pool")
    parser.add_argument("--account")
    parser.add_argument("--model")
    parser.add_argument("--claude", default="claude")
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--keep-output", action="store_true")
    args = parser.parse_args(argv)
    if not args.live:
        parser.print_help()
        return 0
    if not args.router or not args.state_dir or not 1 <= args.timeout <= 600:
        raise CheckFailed("live_requires_router_private_state_and_bounded_timeout")
    origin = COMMON.router_url(args.router)
    grant = os.environ.get("POOLPARTY_NATIVE_GRANT", "")
    if len(grant) < 32 or any(ord(c) < 33 or ord(c) > 126 for c in grant):
        raise CheckFailed("set_scoped_POOLPARTY_NATIVE_GRANT")
    executable = shutil.which(args.claude)
    if not executable:
        raise CheckFailed("installed_claude_not_found")
    binary = Path(executable).absolute()
    launch, prefix = native_launch(binary)
    root = COMMON.private_directory(args.state_dir, create=args.phase != "resume")
    manifest_path = root / MANIFEST
    if args.phase != "resume":
        if any(root.iterdir()):
            raise CheckFailed("start_requires_empty_private_state_directory")
        for name in ("home", "claude", "tmp", "work"):
            (root / name).mkdir(mode=0o700)
        if not args.model or len(args.model) > 128 or any(ord(c) < 33 for c in args.model):
            raise CheckFailed("explicit_model_required")
        if args.binding:
            binding = inspect_binding(origin, grant, args.binding)
        else:
            if not args.account or not args.pool:
                raise CheckFailed("new_binding_requires_explicit_pool_and_account")
            binding = COMMON.api(origin, grant, "POST", "/api/v1/sessions", {
                "session": str(uuid.uuid4()), "pool": COMMON.identifier(args.pool), "product": "glm_coding",
                "model": args.model, "account": COMMON.identifier(args.account), "effort": None})
            binding = inspect_binding(origin, grant, COMMON.identifier(binding.get("id")), binding)
        if (binding["intent"].get("model") != args.model or (args.account and args.account != binding["account"])
                or (args.pool and args.pool != binding["intent"]["pool"])):
            raise CheckFailed("native_model_or_account_must_match_binding")
        manifest = {"schema": 1, "router": origin, "binding": binding, "model": args.model,
                    "session": str(uuid.uuid4()), "phase": "prepared", "version": EXPECTED_VERSION, "port": 0, "thinking": []}
        COMMON.write_private(root / "settings.json", settings_text(args.model))
        for phase in ("start", "resume"):
            COMMON.write_private(root / "work" / (phase + ".txt"), "synthetic-" + uuid.uuid4().hex + "\n")
    else:
        manifest = json.loads(COMMON.read_private(manifest_path))
        if (manifest.get("schema") != 1 or manifest.get("phase") != "started" or manifest.get("router") != origin
                or manifest.get("version") != EXPECTED_VERSION):
            raise CheckFailed("resume_requires_matching_completed_start_state")
        for name in ("home", "claude", "tmp", "work"):
            COMMON.private_directory(root / name)
        binding = manifest["binding"]
        if ((args.binding and args.binding != binding["id"]) or (args.model and args.model != manifest["model"])
                or (args.account and args.account != binding["account"]) or (args.pool and args.pool != binding["intent"]["pool"])):
            raise CheckFailed("resume_arguments_conflict_with_original_binding")
        if COMMON.read_private(root / "settings.json").decode() != settings_text(manifest["model"]):
            raise CheckFailed("native_settings_changed_before_resume")
        inspect_binding(origin, grant, binding["id"], binding)
    summaries = []
    phases = ("start", "resume") if args.phase == "all" else (args.phase,)
    for phase in phases:
        with Observer(origin, binding["id"], manifest["model"], grant, manifest["port"], manifest["thinking"],
                      root / "work" / (phase + ".txt")) as observer:
            LAST_OBSERVER = observer
            manifest["port"] = observer.port
            environment = child_environment(root, binary, grant, observer.port, binding["id"], manifest["model"])
            version = COMMON.run_native(launch, root, environment, prefix + ["--version"], 10).decode().strip()
            if version != EXPECTED_VERSION:
                raise CheckFailed("native_version_not_qualified_update_fixture_first")
            manifest["phase"] = "starting" if phase == "start" else "resuming"
            COMMON.write_private(manifest_path, json.dumps(manifest), replace=manifest_path.exists())
            native = run_phase(binary, root, environment, manifest, phase, args.timeout, args.keep_output)
        observed = observer.summary()
        inspect_binding(origin, grant, binding["id"], binding)
        for attempt in observer.attempts:
            record = COMMON.api(origin, grant, "GET", "/api/v1/attempts/" + attempt)
            if record.get("state") != "succeeded" or record.get("binding") != binding["id"]:
                raise CheckFailed("router_attempt_not_settled_successfully")
        manifest.update(phase="started" if phase == "start" else "resumed", thinking=list(observer.thinking))
        COMMON.write_private(manifest_path, json.dumps(manifest), replace=True)
        summaries.append({"phase": phase, "same_router_binding": True, **native, **observed})
    print(json.dumps({"passed": True, "version": EXPECTED_VERSION, "checks": summaries}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except CheckFailed as error:
        print(json.dumps({"passed": False, "error": str(error), "automatic_retry": False,
                          "observed_repeated_requests": LAST_OBSERVER.repeated_requests if LAST_OBSERVER else 0,
                          "observer_replay_guard_enabled": True, "observer_response_buffering_enabled": True}))
        sys.exit(1)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print(json.dumps({"passed": False, "error": "local_fixture_operation_failed", "automatic_retry": False}))
        sys.exit(1)
