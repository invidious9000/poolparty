#!/usr/bin/env python3
"""Explicit installed-CLI subagent probe against synthetic loopback Responses."""
import argparse
import copy
import http.server
import json
import os
from pathlib import Path
import tempfile
import threading
import uuid

import native_probe as P

B = P.B


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', required=True, type=Path)
    parser.add_argument('--background', action='store_true')
    args = parser.parse_args()
    binary = args.cli.absolute()
    launch, prefix = P.NATIVE.native_launch(binary)
    with tempfile.TemporaryDirectory(prefix='poolparty-native-agents-') as temporary:
        root = Path(temporary).resolve()
        for name in ('home', 'claude', 'tmp', 'work', 'bridge'):
            (root / name).mkdir(mode=0o700)
        sample = root / 'work/sample.txt'
        B.COMMON.write_private(sample, 'synthetic-child-result\n')
        requests, replies, counts = [], [], {}

        class Upstream(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                B.require(self.path == '/routes/binding-a/codex/responses', 'route_changed')
                B.require(self.headers.get('Authorization') == 'Bearer ' + P.GRANT, 'grant_changed')
                requests.append(request)
                key = request['prompt_cache_key']
                counts[key] = counts.get(key, 0) + 1
                B.require(len(requests) <= 10, 'synthetic_attempt_limit')
                complete = P.response(False)
                if key == 'binding-a' and counts[key] == 1:
                    complete['output'] = [{
                        'type': 'function_call', 'id': 'fc-' + suffix, 'call_id': 'call-' + suffix,
                        'name': 'Agent', 'status': 'completed', 'arguments': json.dumps({
                            'description': 'Read synthetic fixture', 'subagent_type': 'synthetic-worker',
                            'prompt': 'Read ' + str(sample) + ' and return its exact contents.',
                            'run_in_background': args.background})} for suffix in ('one', 'two')]
                elif key != 'binding-a' and counts[key] == 1:
                    complete = P.response(True)
                    complete['output'][1]['arguments'] = json.dumps({'file_path': str(sample)})
                else:
                    complete['output'][1]['content'][0]['text'] = 'synthetic-child-result'
                complete['id'] = 'response-' + str(len(requests))
                for index, item in enumerate(complete['output']):
                    item['id'] = 'item-' + str(len(requests)) + '-' + str(index)
                replies.append(copy.deepcopy(complete))
                data = P.wire(complete)
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Content-Length', str(len(data)))
                self.send_header('x-poolparty-attempt-id', 'attempt-' + str(len(requests)))
                self.end_headers()
                self.wfile.write(data)

        upstream = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Upstream)
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        origin = 'http://127.0.0.1:' + str(upstream.server_address[1])
        journal = B.Journal(root / 'bridge', B.initial_state(origin, copy.deepcopy(P.BINDING), True, True))
        session = str(uuid.uuid4())
        try:
            with B.Bridge(journal, P.GRANT) as bridge:
                environment = P.NATIVE.child_environment(root, binary, P.GRANT, bridge.port, 'binding-a', 'synthetic-model')
                environment.pop('MAX_THINKING_TOKENS')
                environment['CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS'] = '1'
                environment['ENABLE_TOOL_SEARCH'] = 'false'
                version = B.COMMON.run_native(launch, root, environment, prefix + ['--version'], 10).decode().strip()
                B.require(version == P.EXPECTED_VERSION, 'native_version_not_qualified')
                arguments = ['-p', '--output-format', 'stream-json', '--verbose',
                    '--model', 'synthetic-model', '--effort', 'medium', '--tools', 'Agent,Read',
                    '--allowedTools', 'Agent,Read', '--permission-mode', 'dontAsk',
                    '--strict-mcp-config', '--setting-sources', '', '--max-turns', '6',
                    '--agents', json.dumps({'synthetic-worker': {'description': 'Reads the synthetic fixture',
                        'prompt': 'Read only the specified synthetic fixture. Return its exact text.', 'tools': ['Read']}}),
                    '--system-prompt', 'Synthetic parent. Delegate the two reads and report the result.',
                    '--session-id', session, 'Delegate two identical reads of ' + str(sample) + '.']
                raw = B.COMMON.run_native(launch, root, environment, prefix + arguments, 60)
                B.require(bridge.error is None, bridge.error or 'native_bridge_failed')
                events = [json.loads(line) for line in raw.splitlines() if line.strip()]
                results = [event for event in events if event.get('type') == 'result']
                B.require({event['session_id'] for event in events if event.get('session_id')} == {session},
                          'native_session_changed')
                B.require(bool(results) and all(event.get('subtype') == 'success' and not event.get('is_error')
                    and 'synthetic-child-result' in event.get('result', '') for event in results), 'native_result_failed')
                B.require(all(event['message']['model'] == 'synthetic-model' for event in events
                    if event.get('type') == 'assistant'), 'native_model_changed')
            loaded = B.Journal(root / 'bridge')
            children = loaded.state.get('agents', {})
            B.require(len(children) == 2, 'two_agent_roundtrips_required')
            B.require(all(count == 2 for key, count in counts.items() if key != 'binding-a'),
                      'child_request_counts_changed')
            B.require(counts['binding-a'] in ((3, 4) if args.background else (2,)), 'parent_request_count_changed')
            B.require(len({child['instructions'] for child in children.values()}) == 1, 'identical_agent_prompts_required')
            for key in counts:
                if key == 'binding-a':
                    continue
                indices = [index for index, request in enumerate(requests) if request['prompt_cache_key'] == key]
                first, second = indices
                original = requests[first]['input'] + replies[first]['output']
                B.require(requests[second]['input'][:len(original)] == original, 'child_original_items_changed')
            for child in children.values():
                results = [item for item in child['input'] if item.get('type') == 'function_call_output']
                B.require(len(results) == 1 and not json.loads(results[0]['output'])['is_error'], 'child_read_failed')
                B.require('synthetic-child-result' in results[0]['output'], 'child_result_missing')
                B.require(not child['pending_tools'], 'child_tools_unsettled')
            print(json.dumps({'cli_version': version, 'agents': len(children),
                'synthetic_upstream_requests': len(requests), 'child_read_roundtrips': 2,
                'background': args.background, 'same_binding': True,
                'independent_histories': True, 'pending': loaded.state['pending']}))
        finally:
            upstream.shutdown()
            upstream.server_close()
            thread.join()


if __name__ == '__main__':
    os.umask(0o077)
    try:
        main()
    except B.COMMON.CheckFailed as error:
        raise SystemExit(str(error)) from None
