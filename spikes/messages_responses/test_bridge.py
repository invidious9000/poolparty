"""Synthetic protocol and HTTP checks. No installed CLI, vault or live provider."""
import copy
import http.server
import io
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from contextlib import redirect_stdout

import bridge as B


GRANT = "synthetic-caller-grant-with-at-least-32-bytes"
BINDING = {"id": "binding-a", "account": "account-a", "closed_at": None,
           "intent": {"product": "codex_subscription", "model": "synthetic-model", "effort": "medium"}}


def body():
    return {"model": "synthetic-model", "stream": True, "max_tokens": 4096,
            "system": "Synthetic instructions.", "messages": [{"role": "user", "content": "Read a file."}],
            "tools": [{"name": "Read", "input_schema": {"type": "object", "properties": {
                "file_path": {"type": "string"}}}}]}


def response(tool=True):
    output = [{"type": "reasoning", "id": "rs-a", "encrypted_content": "synthetic-encrypted-content",
               "summary": [{"type": "summary_text", "text": "Synthetic summary λ."}]}]
    if tool:
        output.append({"type": "function_call", "id": "fc-a", "call_id": "call-a", "name": "Read",
                       "arguments": '{"file_path":"/synthetic/example.txt"}', "status": "completed"})
    else:
        output.append({"type": "message", "id": "msg-a", "role": "assistant", "status": "completed",
                       "phase": "final_answer", "content": [{"type": "output_text", "text": "Synthetic done."}]})
    return {"id": "resp-a", "status": "completed", "model": "synthetic-model", "output": output,
            "usage": {"input_tokens": 100, "output_tokens": 20, "input_tokens_details": {"cached_tokens": 60}}}


def wire(value):
    return b"event: response.completed\ndata: " + B.encoded({"type": "response.completed", "response": value}) + b"\n\n"


def append_assistant(request, assistant):
    result = copy.deepcopy(request)
    result["messages"].append({"role": "assistant", "content": [item["block"] for item in assistant]})
    return result


class TranslationTests(unittest.TestCase):
    def setUp(self):
        self.state = B.initial_state("http://127.0.0.1:1", copy.deepcopy(BINDING), True, False)

    def test_no_live_means_no_network(self):
        with redirect_stdout(io.StringIO()):
            self.assertEqual(B.main([]), 0)

    def test_request_and_completed_tool_roundtrip(self):
        outgoing, history, names = B.prepare(body(), self.state)
        self.assertEqual(outgoing["model"], "synthetic-model")
        self.assertEqual(outgoing["reasoning"]["effort"], "medium")
        self.assertFalse(outgoing["store"])
        self.assertFalse(outgoing["tools"][0]["strict"])
        self.assertNotIn("max_output_tokens", outgoing)
        complete = B.completed_response(wire(response()))
        native, assistant, calls = B.message_events(complete, "synthetic-model", names)
        self.assertIn(b'"stop_reason":"tool_use"', native)
        self.assertIn(b'"input_tokens":40', native)
        self.assertIn(b'"cache_read_input_tokens":60', native)
        self.state.update(history=history + assistant, input=outgoing["input"] + complete["output"], pending_tools=calls)
        request = append_assistant(body(), assistant)
        request["messages"].append({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call-a",
            "is_error": True, "content": "Synthetic file missing."}]})
        continued, _, _ = B.prepare(request, self.state)
        self.assertEqual(continued["input"][1:3], complete["output"])
        self.assertEqual(continued["input"][-1]["call_id"], "call-a")
        self.assertTrue(json.loads(continued["input"][-1]["output"])["is_error"])

    def test_unknown_fields_and_unsupported_capabilities_refused(self):
        for key, value in (("temperature", 1), ("stop_sequences", ["stop"]), ("output_config", {"effort": "low"}),
                           ("thinking", {"type": "enabled", "budget_tokens": 1024}),
                           ("thinking", {"type": "disabled"}), ("stream", False), ("model", "different-model")):
            with self.subTest(key=key, value=value):
                request = body()
                request[key] = value
                with self.assertRaises(B.Failed):
                    B.prepare(request, self.state)
        request = body()
        request["messages"][0]["content"] = [{"type": "image", "source": {"type": "url", "url": "https://example.com/a"}}]
        with self.assertRaises(B.Failed):
            B.prepare(request, self.state)

    def test_explicit_budget_and_cache_policy(self):
        self.state["codex_default_output_budget"] = False
        with self.assertRaises(B.Failed):
            B.prepare(body(), self.state)
        self.state["codex_default_output_budget"] = True
        request = body()
        request["system"] = [{"type": "text", "text": "Synthetic instructions.", "cache_control": {"type": "ephemeral"}}]
        with self.assertRaises(B.Failed):
            B.prepare(request, self.state)
        self.state["codex_automatic_cache"] = True
        self.assertEqual(B.prepare(request, self.state)[0]["instructions"], "Synthetic instructions.")

    def test_tool_schema_and_argument_keys_are_opaque(self):
        self.state["codex_automatic_cache"] = True
        request = body()
        request["tools"][0]["input_schema"]["properties"]["cache_control"] = {"type": "string"}
        self.assertIn("cache_control", B.prepare(request, self.state)[0]["tools"][0]["parameters"]["properties"])

    def test_conversation_system_text_preserves_role_and_order(self):
        request = body()
        request["messages"].append({"role": "system", "content": "Synthetic tool guidance."})
        outgoing, history, _ = B.prepare(request, self.state)
        self.assertEqual([item["role"] for item in outgoing["input"]], ["user", "system"])
        self.assertEqual(outgoing["input"][-1]["content"][0]["text"], "Synthetic tool guidance.")
        self.state.update(history=history, input=outgoing["input"])
        system_only = copy.deepcopy(request)
        system_only["messages"].append({"role": "system", "content": "Additional guidance."})
        with self.assertRaises(B.Failed):
            B.prepare(system_only, self.state)
        request["messages"].append({"role": "user", "content": "Continue."})
        self.assertEqual(len(B.prepare(request, self.state)[0]["input"]), 3)
        request["messages"][1]["content"] = "Changed guidance."
        with self.assertRaises(B.Failed):
            B.prepare(request, self.state)

    def test_tool_choice_and_no_schema_rewriting(self):
        request = body()
        request["tool_choice"] = {"type": "tool", "name": "Read", "disable_parallel_tool_use": True}
        outgoing, _, _ = B.prepare(request, self.state)
        self.assertEqual(outgoing["tool_choice"], {"type": "function", "name": "Read"})
        self.assertFalse(outgoing["parallel_tool_calls"])
        self.assertEqual(outgoing["tools"][0]["parameters"], request["tools"][0]["input_schema"])

    def test_native_adaptive_effort_and_keep_all_context(self):
        request = body()
        request.update(thinking={"type": "adaptive"}, output_config={"effort": "medium"},
                       context_management={"edits": [{"type": "clear_thinking_20251015", "keep": "all"}]})
        self.assertEqual(B.prepare(request, self.state)[0]["reasoning"]["effort"], "medium")
        request["context_management"]["edits"][0]["keep"] = "none"
        with self.assertRaises(B.Failed):
            B.prepare(request, self.state)

    def test_missing_reasoning_and_unknown_output_refused(self):
        complete = response()
        del complete["output"][0]["encrypted_content"]
        with self.assertRaises(B.Failed):
            B.message_events(complete, "synthetic-model", {"Read"})
        for item in ({"type": "web_search_call"}, {"type": "message", "role": "assistant", "status": "completed",
                     "content": [{"type": "refusal", "refusal": "Synthetic refusal."}]}):
            complete = response()
            complete["output"].append(item)
            with self.assertRaises(B.Failed):
                B.message_events(complete, "synthetic-model", {"Read"})

    def test_terminal_evidence_and_framing(self):
        valid = wire(response())
        self.assertEqual(B.completed_response(valid.replace(b"\n", b"\r\n")), response())
        for raw in (valid[:-2], b"data: {}\n\n", valid + valid, valid.replace(b"response.completed", b"response.incomplete"),
                    b"event: wrong\ndata: " + B.encoded({"type": "response.completed", "response": response()}) + b"\n\n"):
            with self.subTest(raw=raw[:40]), self.assertRaises(B.Failed):
                B.completed_response(raw)

    def test_missing_cache_evidence_is_not_reported_as_zero(self):
        complete = response()
        del complete["usage"]["input_tokens_details"]
        with self.assertRaises(B.Failed):
            B.message_events(complete, "synthetic-model", {"Read"})

    def test_completed_items_survive_empty_terminal_output(self):
        complete = response()
        frames = []
        for index, item in enumerate(complete["output"]):
            for phase in ("added", "done"):
                event = {"type": "response.output_item." + phase, "output_index": index, "item": item}
                frames.append(b"data: " + B.encoded(event) + b"\n\n")
        terminal = dict(complete, output=[])
        raw = b"".join(frames) + wire(terminal)
        self.assertEqual(B.completed_response(raw), complete)
        self.assertEqual(B.completed_response(b"".join(frames) + wire(complete)), complete)
        for invalid in (b"".join(frames[:-1]) + wire(terminal), b"".join(frames + [frames[-1]]) + wire(terminal),
                        b"".join(frames) + wire(dict(complete, output=[complete["output"][0]]))):
            with self.assertRaises(B.Failed):
                B.completed_response(invalid)

    def test_input_history_changes_and_foreign_tool_results_refused(self):
        outgoing, history, names = B.prepare(body(), self.state)
        _, assistant, calls = B.message_events(response(), "synthetic-model", names)
        self.state.update(history=history + assistant, input=outgoing["input"] + response()["output"], pending_tools=calls)
        request = append_assistant(body(), assistant)
        request["messages"].append({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call-a", "content": "ok"}]})
        for alteration in ("thinking", "tool", "missing", "duplicate", "prior", "replay"):
            changed = copy.deepcopy(request)
            if alteration == "thinking":
                changed["messages"][1]["content"][0]["signature"] = "foreign-signature"
            elif alteration == "tool":
                changed["messages"][-1]["content"][0]["tool_use_id"] = "unknown-call"
            elif alteration == "missing":
                changed["messages"][-1]["content"] = "Next turn."
            elif alteration == "duplicate":
                changed["messages"][-1]["content"] *= 2
            elif alteration == "prior":
                changed["messages"][0]["content"] = "Changed history."
            else:
                changed = body()
            with self.subTest(alteration=alteration), self.assertRaises(B.Failed):
                B.prepare(changed, self.state)

    def test_native_resume_regrouping_preserves_original_upstream_order(self):
        outgoing, history, names = B.prepare(body(), self.state)
        _, assistant, calls = B.message_events(response(), "synthetic-model", names)
        result = {"role": "user", "block": {"type": "tool_result", "tool_use_id": "call-a",
                  "content": ["ok"], "is_error": False}}
        _, final, _ = B.message_events(response(False), "synthetic-model", names)
        original = outgoing["input"] + response()["output"] + [
            {"type": "function_call_output", "call_id": "call-a", "output": "ok"}] + response(False)["output"]
        self.state.update(history=history + assistant + [result] + final, input=original)
        request = append_assistant(body(), assistant + final)
        native_result = dict(result["block"], content="ok")
        request["messages"].append({"role": "user", "content": [native_result, {"type": "text", "text": "Resume."}]})
        translated, _, _ = B.prepare(request, self.state)
        self.assertEqual(translated["input"][:-1], original)

        # The next native request can move its new tool result before the user
        # text from the previous request, without changing either one's content.
        prior_text = {"role": "user", "block": {"type": "text", "text": "Resume."}}
        self.state["history"].append(prior_text)
        self.state["input"] = translated["input"]
        self.state["pending_tools"] = ["call-b"]
        request["messages"][-1]["content"].insert(1, {"type": "tool_result", "tool_use_id": "call-b", "content": "second result"})
        translated_again, _, _ = B.prepare(request, self.state)
        self.assertEqual(translated_again["input"][:-1], translated["input"])
        self.assertEqual(translated_again["input"][-1]["call_id"], "call-b")

        request["messages"][-1]["content"][0]["content"] = "changed prior result"
        with self.assertRaises(B.Failed):
            B.prepare(request, self.state)


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="poolparty-bridge-test-")
        self.root = Path(self.tmp.name).resolve()
        os.chmod(self.root, 0o700)
        self.requests = []
        self.mode = "success"
        test = self

        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                test.requests.append({"path": self.path, "auth": self.headers.get("Authorization"),
                    "operation": self.headers.get("x-poolparty-operation-id"),
                    "body": json.loads(self.rfile.read(int(self.headers["Content-Length"])))})
                data = wire(response(len(test.requests) == 1))
                if test.mode == "truncated":
                    data = b"event: response.created\ndata: {}\n\n"
                elif test.mode == "redirect":
                    self.send_response(307)
                    self.send_header("Location", "http://127.0.0.1:1/private")
                    self.end_headers()
                    return
                self.send_response(429 if test.mode == "exhausted" else 200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("x-poolparty-attempt-id", "attempt-" + str(len(test.requests)))
                self.end_headers()
                self.wfile.write(data)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        origin = "http://127.0.0.1:" + str(self.server.server_address[1])
        self.journal = B.Journal(self.root, B.initial_state(origin, copy.deepcopy(BINDING), True, False))

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.tmp.cleanup()

    def post(self, bridge, request=None, grant=GRANT, path="/routes/binding-a/v1/messages", headers=None):
        req = urllib.request.Request("http://127.0.0.1:" + str(bridge.port) + path,
            data=B.encoded(request or body()), headers={"Authorization": "Bearer " + grant, **(headers or {})})
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(req, timeout=5) as result:
                return result.status, result.read()
        except urllib.error.HTTPError as error:
            with error:
                return error.code, error.read()

    def test_http_tool_roundtrip_resume_and_exact_upstream_items(self):
        with B.Bridge(self.journal, GRANT) as bridge:
            self.assertEqual(self.post(bridge)[0], 200)
        loaded = B.Journal(self.root)
        self.assertEqual(loaded.state["binding"], BINDING)
        request = append_assistant(body(), loaded.state["history"][1:])
        request["messages"].append({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call-a", "content": "Synthetic result."}]})
        with B.Bridge(loaded, GRANT) as bridge:
            status, raw = self.post(bridge, request)
            self.assertEqual(status, 200)
            self.assertIn(b'"stop_reason":"end_turn"', raw)
            self.assertEqual(self.post(bridge, request)[0], 400)
        self.assertEqual(len(self.requests), 2)
        self.assertEqual(self.requests[1]["body"]["input"][1:3], response()["output"])
        self.assertEqual(loaded.state["input"][-1]["phase"], "final_answer")
        self.assertTrue(all(req["path"] == "/routes/binding-a/codex/responses" for req in self.requests))
        self.assertTrue(all(req["auth"] == "Bearer " + GRANT for req in self.requests))
        self.assertNotEqual(self.requests[0]["operation"], self.requests[1]["operation"])
        self.assertEqual((self.root / B.STATE_NAME).stat().st_mode & 0o777, 0o600)

    def test_auth_and_route_rejected_before_dispatch(self):
        with B.Bridge(self.journal, GRANT) as bridge:
            self.assertEqual(self.post(bridge, grant="foreign-grant")[0], 401)
            self.assertEqual(self.post(bridge, path="/routes/foreign/v1/messages")[0], 404)
            bad = body()
            bad["model"] = "different-model"
            self.assertEqual(self.post(bridge, bad)[0], 400)
        self.assertEqual(self.requests, [])

    def test_incomplete_upstream_fences_restart_and_further_dispatch(self):
        self.mode = "truncated"
        with B.Bridge(self.journal, GRANT) as bridge:
            status, data = self.post(bridge)
            self.assertNotEqual(status, 200)
            self.assertNotIn(b"message_stop", data)
            self.assertNotEqual(self.post(bridge)[0], 200)
        self.assertEqual(len(self.requests), 1)
        with self.assertRaises(B.Failed):
            B.Journal(self.root)

    def test_quota_status_preserved_binding_unchanged_and_no_retry(self):
        self.mode = "exhausted"
        with B.Bridge(self.journal, GRANT) as bridge:
            status, raw = self.post(bridge)
            self.assertEqual(status, 429)
            self.assertEqual(json.loads(raw)["error"]["type"], "rate_limit_error")
            self.assertNotEqual(self.post(bridge)[0], 200)
        self.assertEqual(len(self.requests), 1)
        self.assertEqual(self.journal.state["binding"], BINDING)
        self.assertEqual(self.journal.state["attempts"], ["attempt-1"])

    def test_redirect_is_never_followed(self):
        self.mode = "redirect"
        with B.Bridge(self.journal, GRANT) as bridge:
            self.assertNotEqual(self.post(bridge)[0], 200)
            self.assertNotEqual(self.post(bridge)[0], 200)
        self.assertEqual(len(self.requests), 1)

    def test_agents_have_independent_durable_histories_on_one_binding(self):
        parent_headers = {"x-claude-code-session-id": "session-a"}
        child_headers = {**parent_headers, "x-claude-code-agent-id": "agent-a"}
        child = body()
        child["system"] = "Synthetic child instructions."
        with B.Bridge(self.journal, GRANT) as bridge:
            self.assertEqual(self.post(bridge, headers=parent_headers)[0], 200)
            parent = copy.deepcopy(self.journal.state["history"])
            self.assertEqual(self.post(bridge, child, headers=child_headers)[0], 200)
            self.assertEqual(self.journal.state["history"], parent)
            self.assertEqual(self.post(bridge, child, headers={**child_headers,
                "x-claude-code-agent-id": "agent-b"})[0], 200)
            self.assertEqual(self.post(bridge, child, headers=child_headers)[0], 400)
            self.assertEqual(self.post(bridge, child, headers={**child_headers,
                "x-claude-code-session-id": "session-b"})[0], 400)
            self.assertEqual(self.post(bridge, child, headers=parent_headers)[0], 400)
        loaded = B.Journal(self.root)
        continued = append_assistant(child, loaded.state["agents"]["agent-a"]["history"][1:])
        continued["messages"].append({"role": "user", "content": "Synthetic follow-up."})
        with B.Bridge(loaded, GRANT) as bridge:
            changed = copy.deepcopy(continued)
            changed["system"] = "Changed child instructions."
            self.assertEqual(self.post(bridge, changed, headers=child_headers)[0], 400)
            self.assertEqual(self.post(bridge, continued, headers={**child_headers,
                "x-claude-code-agent-id": "missing-agent"})[0], 400)
            self.assertEqual(self.post(bridge, continued, headers=child_headers)[0], 200)
        self.assertEqual(len(self.requests), 4)
        self.assertEqual(loaded.state["history"], parent)
        self.assertEqual(loaded.state["binding"], BINDING)
        self.assertEqual(self.requests[1]["body"]["input"], self.requests[2]["body"]["input"])
        keys = [request["body"]["prompt_cache_key"] for request in self.requests]
        self.assertEqual(len(set(keys[:3])), 3)
        self.assertEqual(keys[1], keys[3])
        self.assertIsNone(loaded.state["pending"])

    def test_legacy_journal_resumes_and_establishes_native_identity(self):
        with B.Bridge(self.journal, GRANT) as bridge:
            self.assertEqual(self.post(bridge)[0], 200)
        loaded = B.Journal(self.root)
        self.assertNotIn("native_session", loaded.state)
        continued = append_assistant(body(), loaded.state["history"][1:])
        continued["messages"].append({"role": "user", "content": [{"type": "tool_result",
            "tool_use_id": "call-a", "content": "Synthetic result."}]})
        headers = {"x-claude-code-session-id": "session-a"}
        with B.Bridge(loaded, GRANT) as bridge:
            self.assertEqual(self.post(bridge, continued, headers=headers)[0], 200)
            self.assertEqual(self.post(bridge, headers={**headers, "x-claude-code-agent-id": "agent-a"})[0], 200)
        self.assertEqual(len(self.requests), 3)
        self.assertEqual(loaded.state["native_session"], "session-a")
        self.assertEqual(loaded.state["binding"], BINDING)

    def test_child_failure_fences_whole_binding(self):
        headers = {"x-claude-code-session-id": "session-a"}
        with B.Bridge(self.journal, GRANT) as bridge:
            self.assertEqual(self.post(bridge, headers=headers)[0], 200)
            self.mode = "truncated"
            self.assertEqual(self.post(bridge, headers={**headers, "x-claude-code-agent-id": "agent-a"})[0], 400)
            self.assertEqual(self.post(bridge, headers={**headers, "x-claude-code-agent-id": "agent-b"})[0], 400)
        self.assertEqual(len(self.requests), 2)
        with self.assertRaises(B.Failed):
            B.Journal(self.root)

    def test_concurrent_agents_queue_without_mixing_histories(self):
        headers = {"x-claude-code-session-id": "session-a"}
        with B.Bridge(self.journal, GRANT) as bridge:
            self.assertEqual(self.post(bridge, headers=headers)[0], 200)
            results = []
            bridge.lock.acquire()
            threads = [threading.Thread(target=lambda agent=agent: results.append(self.post(
                bridge, headers={**headers, "x-claude-code-agent-id": agent})[0]))
                for agent in ("agent-a", "agent-b")]
            try:
                for thread in threads:
                    thread.start()
            finally:
                bridge.lock.release()
            for thread in threads:
                thread.join(timeout=10)
                self.assertFalse(thread.is_alive())
            self.assertEqual(results, [200, 200])
        self.assertEqual(len(self.requests), 3)
        self.assertEqual(set(self.journal.state["agents"]), {"agent-a", "agent-b"})


if __name__ == "__main__":
    unittest.main()
