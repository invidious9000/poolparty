#!/usr/bin/env python3
"""Bounded, buffered Messages -> bound Poolparty Codex Responses experiment.

No network without --live. This is a bound-session development fixture,
not a daemon adapter. Runtime state contains conversation data and stays private.
"""
import argparse
import copy
import fcntl
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import threading
import urllib.error
import urllib.request
import uuid

_SPEC = importlib.util.spec_from_file_location(
    "poolparty_bridge_common", Path(__file__).resolve().parents[1] / "codex/vertical.py")
COMMON = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(COMMON)
Failed = COMMON.CheckFailed
BODY_LIMIT = 2 * 1024 * 1024
STREAM_LIMIT = 4 * 1024 * 1024
STATE_LIMIT = 16 * 1024 * 1024
FRAME_LIMIT = 256 * 1024
STATE_NAME = "messages-responses.json"


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(encoded(value)).hexdigest()


def require(condition, code):
    if not condition:
        raise Failed(code)


def fields(value, allowed):
    require(isinstance(value, dict) and not set(value).difference(allowed), "unsupported_fields")


def string(value):
    require(isinstance(value, str), "expected_string")
    return value


def nonempty(value):
    require(bool(string(value)), "expected_nonempty_string")
    return value


def without_cache(value, allow_cache):
    """Only the explicitly selected automatic-cache profile drops native markers."""
    if isinstance(value, list):
        return [without_cache(item, allow_cache) for item in value]
    if isinstance(value, dict):
        require("cache_control" not in value or allow_cache, "native_cache_semantics_unsupported")
        # Do not recurse into arbitrary tool JSON: cache_control may be user data.
        return {key: item for key, item in value.items() if key != "cache_control"}
    return value


def text_blocks(value, allow_cache):
    if isinstance(value, str):
        return [value]
    require(isinstance(value, list), "unsupported_content")
    result = []
    for block in value:
        block = without_cache(block, allow_cache)
        fields(block, {"type", "text"})
        require(block.get("type") == "text", "only_text_content_supported")
        result.append(string(block.get("text")))
    return result


def normalized_history(messages, allow_cache):
    require(isinstance(messages, list) and messages, "messages_required")
    result = []
    for message in messages:
        message = without_cache(message, allow_cache)
        fields(message, {"role", "content"})
        role = message.get("role")
        require(role in ("user", "assistant", "system"), "unsupported_message_role")
        blocks = message.get("content")
        if isinstance(blocks, str):
            blocks = [{"type": "text", "text": blocks}]
        require(isinstance(blocks, list) and blocks, "content_required")
        for block in blocks:
            block = without_cache(block, allow_cache)
            require(isinstance(block, dict), "unsupported_content")
            kind = block.get("type")
            if kind == "text":
                fields(block, {"type", "text"})
                string(block.get("text"))
            elif kind == "tool_use" and role == "assistant":
                fields(block, {"type", "id", "name", "input"})
                nonempty(block.get("id"))
                nonempty(block.get("name"))
                require(isinstance(block.get("input"), dict), "tool_input_object_required")
            elif kind == "tool_result" and role == "user":
                fields(block, {"type", "tool_use_id", "content", "is_error"})
                nonempty(block.get("tool_use_id"))
                require(type(block.get("is_error", False)) is bool, "invalid_tool_result")
                # Preserve error status in a JSON result envelope, never as success.
                block = {"type": kind, "tool_use_id": block["tool_use_id"],
                         "content": text_blocks(block.get("content", ""), allow_cache),
                         "is_error": block.get("is_error", False)}
            elif kind == "thinking" and role == "assistant":
                fields(block, {"type", "thinking", "signature"})
                string(block.get("thinking"))
                nonempty(block.get("signature"))
            else:
                raise Failed("unsupported_content_block")
            result.append({"role": role, "block": block})
    return result


def prepare(body, state):
    fields(body, {"model", "messages", "system", "tools", "tool_choice", "stream",
                  "max_tokens", "metadata", "thinking", "output_config", "context_management"})
    intent = state["binding"]["intent"]
    require(body.get("model") == intent["model"], "model_pin_conflict")
    require(body.get("stream") is True, "stream_required")
    require(type(body.get("max_tokens")) is int and body["max_tokens"] > 0, "max_tokens_required")
    # Codex's subscription endpoint is not the metered Responses API. A native
    # output ceiling cannot be claimed when the operator selects its default.
    require(state["codex_default_output_budget"], "native_output_budget_unsupported")
    if "thinking" in body:
        require(body["thinking"] == {"type": "adaptive"} and intent.get("effort"),
                "thinking_budget_or_disable_cannot_map_to_bound_effort")
    if "output_config" in body:
        require(body["output_config"] == {"effort": intent.get("effort")} and intent.get("effort"),
                "output_config_conflicts_with_bound_effort")
    if "context_management" in body:
        require(body["context_management"] == {"edits": [{"type": "clear_thinking_20251015", "keep": "all"}]},
                "context_editing_unsupported")
    if "metadata" in body:
        fields(body["metadata"], {"user_id"})
        string(body["metadata"].get("user_id"))
    cache = state["codex_automatic_cache"]
    instructions = "\n\n".join(text_blocks(body.get("system", ""), cache))
    if state["instructions"] is not None:
        require(instructions == state["instructions"], "system_changed_requires_new_experiment")
    history = normalized_history(body.get("messages"), cache)
    previous = state["history"]
    # Native resume regroups assistant blocks and user tool results. Verify text
    # and assistant sequences in order, and consumed results by unique call ID.
    # Reuse original Responses order, never the regrouped native order.
    users = [entry for entry in history if entry["role"] != "assistant"]
    prior_users = [entry for entry in previous if entry["role"] != "assistant"]
    assistants = [entry for entry in history if entry["role"] == "assistant"]
    prior_assistants = [entry for entry in previous if entry["role"] == "assistant"]
    texts = {role: [entry for entry in users if entry["role"] == role and entry["block"]["type"] == "text"]
             for role in ("user", "system")}
    prior_texts = {role: [entry for entry in prior_users if entry["role"] == role and entry["block"]["type"] == "text"]
                   for role in ("user", "system")}
    prior_results = {entry["block"]["tool_use_id"]: entry for entry in prior_users if entry["block"]["type"] == "tool_result"}
    require(all(texts[role][:len(prior_texts[role])] == prior_texts[role] for role in texts)
            and assistants == prior_assistants,
            "history_changed_or_resume_state_missing")
    new, results_seen, text_index = [], set(), {"user": 0, "system": 0}
    for entry in users:
        block = entry["block"]
        if block["type"] == "text":
            role = entry["role"]
            if text_index[role] >= len(prior_texts[role]):
                new.append(entry)
            text_index[role] += 1
        else:
            call_id = block["tool_use_id"]
            require(call_id not in results_seen, "duplicate_tool_result")
            results_seen.add(call_id)
            if call_id in prior_results:
                require(entry == prior_results[call_id], "historical_tool_result_changed")
            else:
                new.append(entry)
    require(set(prior_results).issubset(results_seen), "historical_tool_result_missing")
    require(any(entry["role"] == "user" for entry in new), "new_user_input_required_no_replay")
    inputs = copy.deepcopy(state["input"])
    pending_tools = set(state["pending_tools"])
    for entry in new:
        block = entry["block"]
        if block["type"] == "text":
            inputs.append({"role": entry["role"], "content": [{"type": "input_text", "text": block["text"]}]})
        elif block["type"] == "tool_result":
            call_id = block["tool_use_id"]
            require(call_id in pending_tools, "unknown_or_duplicate_tool_result")
            pending_tools.remove(call_id)
            output = encoded({"content": block["content"], "is_error": block["is_error"]}).decode()
            inputs.append({"type": "function_call_output", "call_id": call_id, "output": output})
        else:
            raise Failed("unsupported_new_input")
    require(not pending_tools, "missing_tool_results")
    tools = []
    names = set()
    require(isinstance(body.get("tools", []), list), "invalid_tools")
    for tool in body.get("tools", []):
        tool = without_cache(tool, cache)
        fields(tool, {"name", "description", "input_schema"})
        name = nonempty(tool.get("name"))
        require(name not in names and isinstance(tool.get("input_schema"), dict), "invalid_tool_schema")
        names.add(name)
        tools.append({"type": "function", "name": name,
                      "description": string(tool.get("description", "")),
                      "parameters": tool["input_schema"], "strict": False})
    choice = body.get("tool_choice", {"type": "auto"})
    fields(choice, {"type", "name", "disable_parallel_tool_use"})
    kind = choice.get("type")
    require(kind in ("auto", "any", "none", "tool"), "unsupported_tool_choice")
    require(type(choice.get("disable_parallel_tool_use", False)) is bool, "invalid_parallel_tool_choice")
    if kind == "tool":
        require(choice.get("name") in names, "unknown_named_tool")
        choice_value = {"type": "function", "name": choice["name"]}
    else:
        require("name" not in choice, "invalid_tool_choice")
        choice_value = {"auto": "auto", "any": "required", "none": "none"}[kind]
    require(bool(tools) or kind in ("auto", "none"), "tool_choice_requires_tools")
    outgoing = {"model": intent["model"], "instructions": instructions,
                "input": inputs, "tools": tools, "tool_choice": choice_value,
                "parallel_tool_calls": not choice.get("disable_parallel_tool_use", False),
                "store": False, "stream": True, "include": ["reasoning.encrypted_content"],
                "prompt_cache_key": state.get("cache_key", state["binding"]["id"])}
    if intent.get("effort"):
        outgoing["reasoning"] = {"effort": intent["effort"], "summary": "auto"}
    require(len(encoded(outgoing)) <= BODY_LIMIT, "translated_request_too_large")
    return outgoing, history, names


def completed_response(raw):
    require(len(raw) <= STREAM_LIMIT, "upstream_stream_too_large")
    result = None
    added, done = {}, {}
    # HTTP bytes are buffered but SSE framing and terminal evidence are required.
    frames = raw.replace(b"\r\n", b"\n").split(b"\n\n")
    require(not frames[-1].strip(), "unterminated_sse_frame")
    for frame in frames[:-1]:
        require(len(frame) <= FRAME_LIMIT, "upstream_frame_too_large")
        lines = frame.split(b"\n")
        data = b"\n".join(line[5:].removeprefix(b" ") for line in lines if line.startswith(b"data:"))
        if not data:
            continue
        require(result is None, "events_after_terminal")
        event = json.loads(data)
        require(isinstance(event, dict), "invalid_upstream_event")
        kind = event.get("type")
        names = [line[6:].strip().decode() for line in lines if line.startswith(b"event:")]
        require(not names or names == [kind], "sse_event_type_mismatch")
        require(kind not in ("error", "response.failed", "response.incomplete"), "upstream_did_not_complete")
        if kind in ("response.output_item.added", "response.output_item.done"):
            index, item = event.get("output_index"), event.get("item")
            require(type(index) is int and index >= 0 and isinstance(item, dict), "invalid_output_item_event")
            nonempty(item.get("id"))
            if kind == "response.output_item.added":
                require(index not in added and index not in done, "duplicate_output_item")
                added[index] = item["id"]
            else:
                require(index not in done and added.get(index) == item["id"], "output_item_done_without_matching_start")
                done[index] = item
        if kind == "response.completed":
            result = event.get("response")
            require(isinstance(result, dict) and result.get("status") == "completed", "invalid_terminal_response")
    require(result is not None, "upstream_eof_without_completion")
    require(isinstance(result.get("output"), list), "upstream_output_required")
    if added:
        require(set(added) == set(done) == set(range(len(added))), "incomplete_output_items")
        items = [done[index] for index in range(len(done))]
        # The subscription backend can carry complete output only in item.done.
        # A populated terminal array must agree with those authoritative items.
        require(not result["output"] or result["output"] == items, "terminal_output_conflicts_with_items")
        result = dict(result, output=items)
    return result


def message_events(response, model, names):
    require(response.get("model") == model, "upstream_model_pin_conflict")
    nonempty(response.get("id"))
    require(isinstance(response.get("output"), list), "upstream_output_required")
    blocks, calls = [], set()
    for item in response["output"]:
        require(isinstance(item, dict), "unsupported_upstream_item")
        kind = item.get("type")
        if kind == "reasoning":
            nonempty(item.get("encrypted_content"))
            require(isinstance(item.get("summary"), list), "reasoning_summary_required")
            summary = []
            for part in item["summary"]:
                fields(part, {"type", "text"})
                require(part.get("type") == "summary_text", "unsupported_reasoning_summary")
                summary.append(string(part.get("text")))
            blocks.append({"type": "thinking", "thinking": "\n\n".join(summary),
                           "signature": "poolparty-responses-v1:" + digest(item)})
        elif kind == "message":
            require(item.get("role") == "assistant" and item.get("status") == "completed", "invalid_assistant_output")
            require(isinstance(item.get("content"), list), "invalid_assistant_content")
            for part in item["content"]:
                fields(part, {"type", "text", "annotations", "logprobs"})
                require(part.get("type") == "output_text" and not part.get("annotations") and not part.get("logprobs"),
                        "unsupported_output_content_or_annotations")
                blocks.append({"type": "text", "text": string(part.get("text"))})
        elif kind == "function_call":
            call_id = nonempty(item.get("call_id"))
            require(item.get("status") == "completed" and call_id not in calls and item.get("name") in names,
                    "invalid_upstream_tool_call")
            arguments = json.loads(string(item.get("arguments")))
            require(isinstance(arguments, dict), "tool_arguments_object_required")
            calls.add(call_id)
            blocks.append({"type": "tool_use", "id": call_id, "name": item["name"], "input": arguments})
        else:
            raise Failed("unsupported_upstream_item")
    require(bool(blocks), "empty_upstream_output")
    usage = response.get("usage")
    require(isinstance(usage, dict), "upstream_usage_required")
    for key in ("input_tokens", "output_tokens"):
        require(type(usage.get(key)) is int and usage[key] >= 0, "invalid_upstream_usage")
    details = usage.get("input_tokens_details")
    require(isinstance(details, dict), "invalid_upstream_usage")
    cached = details.get("cached_tokens")
    require(type(cached) is int and 0 <= cached <= usage["input_tokens"], "invalid_cached_usage")
    events = [{"type": "message_start", "message": {
        "id": response["id"], "type": "message", "role": "assistant", "model": model,
        "content": [], "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": usage["input_tokens"] - cached, "output_tokens": 0,
                  "cache_read_input_tokens": cached}}}]
    for index, block in enumerate(blocks):
        kind = block["type"]
        start = {"type": kind}
        if kind == "tool_use":
            start.update(id=block["id"], name=block["name"], input={})
            deltas = [{"type": "input_json_delta", "partial_json": encoded(block["input"]).decode()}]
        elif kind == "thinking":
            start.update(thinking="", signature="")
            deltas = [{"type": "thinking_delta", "thinking": block["thinking"]},
                      {"type": "signature_delta", "signature": block["signature"]}]
        else:
            start["text"] = ""
            deltas = [{"type": "text_delta", "text": block["text"]}]
        events.append({"type": "content_block_start", "index": index, "content_block": start})
        events.extend({"type": "content_block_delta", "index": index, "delta": delta} for delta in deltas)
        events.append({"type": "content_block_stop", "index": index})
    events += [{"type": "message_delta", "delta": {"stop_reason": "tool_use" if calls else "end_turn",
                "stop_sequence": None}, "usage": {"output_tokens": usage["output_tokens"]}}, {"type": "message_stop"}]
    wire = b"".join(b"event: " + event["type"].encode() + b"\ndata: " + encoded(event) + b"\n\n" for event in events)
    require(len(wire) <= STREAM_LIMIT, "translated_stream_too_large")
    return wire, [{"role": "assistant", "block": block} for block in blocks], sorted(calls)


def initial_state(origin, binding, output_budget, automatic_cache):
    require(binding.get("closed_at") is None and binding.get("intent", {}).get("product") == "codex_subscription",
            "open_codex_binding_required")
    COMMON.identifier(binding.get("id"))
    COMMON.identifier(binding.get("account"))
    nonempty(binding["intent"].get("model"))
    return {"version": 1, "origin": origin, "binding": binding,
            "codex_default_output_budget": output_budget, "codex_automatic_cache": automatic_cache,
            "instructions": None, "history": [], "input": [], "pending_tools": [],
            "pending": None, "attempts": [], "port": None}


class Journal:
    def __init__(self, root, state=None):
        self.path = root / STATE_NAME
        self.state = state if state is not None else json.loads(COMMON.read_private(self.path, STATE_LIMIT))
        require(isinstance(self.state, dict) and self.state.get("version") == 1, "unsupported_bridge_state")
        require(self.state.get("pending") is None, "previous_attempt_requires_inspection_no_replay")
        if state is not None:
            self.save()

    def save(self):
        data = encoded(self.state)
        require(len(data) <= STATE_LIMIT, "bridge_state_too_large")
        COMMON.write_private(self.path, data, replace=self.path.exists())
        directory = os.open(self.path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)


class Bridge:
    def __init__(self, journal, grant, port=0):
        self.journal, self.grant = journal, grant
        self.lock = threading.Lock()
        self.slots = threading.BoundedSemaphore(16)
        self.error = None
        bridge = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def setup(self):
                self.request.settimeout(180)
                super().setup()

            def log_message(self, *_):
                pass

            def reply(self, status, payload, mime="application/json", attempt=None):
                self.send_response(status)
                self.send_header("Content-Type", mime)
                self.send_header("Content-Length", str(len(payload)))
                self.send_header("Connection", "close")
                if attempt:
                    self.send_header("x-poolparty-attempt-id", attempt)
                self.end_headers()
                self.wfile.write(payload)
                self.wfile.flush()
                self.close_connection = True

            def reject(self, code, status=400):
                bridge.error = code
                error_type = {400: "invalid_request_error", 401: "authentication_error",
                              403: "permission_error", 404: "not_found_error", 429: "rate_limit_error"}.get(status, "api_error")
                self.reply(status, encoded({"type": "error", "error": {
                    "type": error_type, "message": code}}))

            def do_GET(self):
                self.reject("unsupported_route", 404)

            def do_POST(self):
                state = journal.state
                path = "/routes/" + state["binding"]["id"] + "/v1/messages"
                if self.headers.get_all("Authorization") != ["Bearer " + grant] or self.headers.get("X-Api-Key"):
                    self.reject("caller_auth_required", 401)
                    return
                if self.path not in (path, path + "?beta=true"):
                    self.reject("unsupported_route", 404)
                    return
                if not bridge.slots.acquire(blocking=False):
                    self.reject("binding_request_queue_full", 429)
                    return
                if not bridge.lock.acquire(timeout=180):
                    bridge.slots.release()
                    self.reject("binding_request_queue_timeout", 409)
                    return
                sent = False
                try:
                    require(state["pending"] is None, "previous_attempt_requires_inspection_no_replay")
                    require(len(state["attempts"]) < 64, "experiment_attempt_limit")
                    lengths = self.headers.get_all("Content-Length") or []
                    require(len(lengths) == 1 and self.headers.get("Transfer-Encoding") is None, "bounded_body_required")
                    length = int(lengths[0])
                    require(0 < length <= BODY_LIMIT, "request_too_large")
                    raw = self.rfile.read(length)
                    require(len(raw) == length, "incomplete_request")
                    body = json.loads(raw)
                    session_headers = self.headers.get_all("x-claude-code-session-id") or []
                    agent_headers = self.headers.get_all("x-claude-code-agent-id") or []
                    require(len(session_headers) <= 1 and len(agent_headers) <= 1, "duplicate_conversation_identity")
                    session = COMMON.identifier(session_headers[0]) if session_headers else None
                    expected_session = state.get("native_session")
                    require(expected_session is None or session == expected_session, "native_session_changed_or_missing")
                    agent = COMMON.identifier(agent_headers[0]) if agent_headers else None
                    conversation = state
                    if agent is not None:
                        require(session is not None and session == expected_session, "agent_requires_established_native_session")
                        children = state.get("agents", {})
                        require(agent in children or len(children) < 64, "experiment_agent_limit")
                        conversation = children.get(agent, {
                            "instructions": None, "history": [], "input": [], "pending_tools": []})
                    # Children share custody, admission and the uncertainty fence,
                    # but never the parent's prompt, tools or original output items.
                    view = {**state, **conversation}
                    if agent is not None:
                        view["cache_key"] = digest([state["binding"]["id"], agent])
                    outgoing, history, names = prepare(body, view)
                    if agent is not None:
                        state.setdefault("agents", {})[agent] = conversation
                    if session is not None:
                        state["native_session"] = session
                    operation = "bridge-" + str(uuid.uuid4())
                    state["pending"] = operation
                    journal.save()
                    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), COMMON.NoRedirect())
                    request = urllib.request.Request(state["origin"] + "/routes/" + state["binding"]["id"] + "/codex/responses",
                        data=encoded(outgoing), headers={"Authorization": "Bearer " + grant,
                        "Content-Type": "application/json", "x-poolparty-operation-id": operation})
                    try:
                        upstream = opener.open(request, timeout=180)
                    except urllib.error.HTTPError as error:
                        # Preserve the status, redact the foreign protocol body.
                        status = error.code
                        attempt = error.headers.get("x-poolparty-attempt-id")
                        error.close()
                        if attempt:
                            state["attempts"].append(COMMON.identifier(attempt))
                            journal.save()
                        self.reject("router_rejected_attempt_inspect_binding", status)
                        return
                    with upstream:
                        require(upstream.status == 200 and upstream.headers.get_content_type() == "text/event-stream",
                                "router_stream_required")
                        attempt = COMMON.identifier(upstream.headers.get("x-poolparty-attempt-id"))
                        state["attempts"].append(attempt)
                        journal.save()
                        response = completed_response(upstream.read(STREAM_LIMIT + 1))
                    wire, assistant, calls = message_events(response, state["binding"]["intent"]["model"], names)
                    prior_calls = {item["call_id"] for item in conversation["input"] if item.get("type") == "function_call"}
                    require(prior_calls.isdisjoint(calls), "upstream_reused_tool_call_id")
                    conversation["instructions"] = outgoing["instructions"]
                    conversation["history"] = history + assistant
                    conversation["input"] = outgoing["input"] + response["output"]
                    conversation["pending_tools"] = calls
                    # Keep a durable fence until delivery. Missing delivery state
                    # cannot authorize replay even when the router completed.
                    journal.save()
                    sent = True
                    self.reply(200, wire, "text/event-stream", attempt)
                    state["pending"] = None
                    try:
                        journal.save()
                    except BaseException:
                        state["pending"] = operation
                        raise
                except Failed as error:
                    if not sent:
                        self.reject(str(error))
                except (OSError, ValueError, TypeError, KeyError, urllib.error.URLError):
                    if not sent:
                        self.reject("bridge_failed_inspect_state_no_automatic_retry", 502)
                finally:
                    self.close_connection = True
                    bridge.lock.release()
                    bridge.slots.release()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
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
        self.thread.join()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true")
    parser.add_argument("--router")
    parser.add_argument("--binding")
    parser.add_argument("--state-dir", type=Path)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--codex-default-output-budget", action="store_true",
                        help="explicitly use the backend output budget instead of native max_tokens")
    parser.add_argument("--codex-automatic-cache", action="store_true",
                        help="explicitly replace Anthropic cache markers with Codex automatic caching")
    args = parser.parse_args(argv)
    if not args.live:
        parser.print_help()
        return 0
    require(args.router and args.binding and args.state_dir, "router_binding_private_state_required")
    require(0 <= args.port <= 65535, "invalid_port")
    grant = os.environ.get("POOLPARTY_NATIVE_GRANT", "")
    require(len(grant) >= 32 and all(33 <= ord(char) <= 126 for char in grant), "set_scoped_POOLPARTY_NATIVE_GRANT")
    origin, binding_id = COMMON.router_url(args.router), COMMON.identifier(args.binding)
    root = COMMON.private_directory(args.state_dir, create=not args.resume)
    lock_path = root / "bridge.lock"
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        binding = COMMON.api(origin, grant, "GET", "/api/v1/sessions/" + binding_id)
        require(binding.get("id") == binding_id, "binding_identity_mismatch")
        if args.resume:
            journal = Journal(root)
            require(journal.state["origin"] == origin and journal.state["binding"] == binding,
                    "binding_changed_on_resume")
            port = journal.state["port"]
            require(type(port) is int and port > 0 and args.port in (0, port), "resume_port_changed")
        else:
            require(set(root.iterdir()) == {lock_path}, "new_requires_empty_private_state")
            require(args.codex_default_output_budget, "explicit_codex_output_budget_policy_required")
            journal = Journal(root, initial_state(origin, binding, args.codex_default_output_budget, args.codex_automatic_cache))
            port = args.port
        with Bridge(journal, grant, port) as bridge:
            journal.state["port"] = bridge.port
            journal.save()
            print(json.dumps({"listening_on_loopback_port": bridge.port, "buffered_experiment": True}), flush=True)
            try:
                threading.Event().wait()
            except KeyboardInterrupt:
                pass
    return 0


if __name__ == "__main__":
    os.umask(0o077)
    try:
        raise SystemExit(main())
    except (Failed, OSError, ValueError, KeyError, TypeError):
        raise SystemExit("Bridge refused startup or failed; inspect private state. No automatic retry.") from None
