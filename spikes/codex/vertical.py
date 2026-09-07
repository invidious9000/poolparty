#!/usr/bin/env python3
"""Explicit native Codex -> running Poolparty -> provider vertical check.

No network or native inference without --live. Native auth/config/history live in
an explicitly supplied private directory outside git checkouts. Only a Poolparty
caller grant is passed to Codex. Raw child output is bounded and never printed.
"""
import argparse
import ipaddress
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import uuid


EXPECTED_VERSION = "codex-cli 0.153.4"
OUTPUT_LIMIT = 4 * 1024 * 1024
MANIFEST = "vertical-state.json"


class CheckFailed(Exception):
    """Messages must be fixed/sanitized, never provider or native stderr text."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        raise CheckFailed("router_redirect_refused")


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", value):
        raise CheckFailed("invalid_identifier")
    return value


def router_url(value):
    parsed = urllib.parse.urlsplit(value)
    try:
        local = ipaddress.ip_address(parsed.hostname or "").is_loopback
    except ValueError:
        local = False
    if (not parsed.hostname or parsed.username or parsed.password or parsed.query
            or parsed.fragment or parsed.path not in ("", "/")
            or (parsed.scheme != "https" and not (parsed.scheme == "http" and local))):
        raise CheckFailed("router_requires_https_or_literal_loopback_http_origin")
    return value.rstrip("/")


def api(origin, grant, method, path, body=None):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    request = urllib.request.Request(origin + path, method=method,
        headers={"Authorization": "Bearer " + grant, "Content-Type": "application/json"},
        data=None if body is None else json.dumps(body).encode())
    try:
        with opener.open(request, timeout=20) as response:
            data = response.read(1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        raise CheckFailed("router_http_" + str(error.code)) from None
    except (urllib.error.URLError, TimeoutError, OSError):
        raise CheckFailed("router_request_failed") from None
    if len(data) > 1024 * 1024:
        raise CheckFailed("router_response_too_large")
    try:
        value = json.loads(data)
    except (ValueError, UnicodeError):
        raise CheckFailed("router_response_invalid_json") from None
    if not isinstance(value, dict):
        raise CheckFailed("router_response_invalid_shape")
    return value


def private_directory(path, create=False):
    path = path.absolute()
    if path.is_symlink():
        raise CheckFailed("state_directory_symlink_refused")
    if create:
        path.mkdir(mode=0o700, parents=True, exist_ok=True)
    path = path.resolve(strict=True)
    if any((parent / ".git").exists() for parent in [path, *path.parents]):
        raise CheckFailed("native_state_must_be_outside_git_checkouts")
    info = path.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_mode & 0o077 or info.st_uid != os.getuid():
        raise CheckFailed("state_directory_requires_owner_only_permissions")
    return path


def write_private(path, data, replace=False):
    if isinstance(data, str):
        data = data.encode()
    # Adjacent replacement is atomic and does not silently follow a symlink.
    temporary = path.with_name(path.name + ".writing") if replace else path
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        if replace:
            if path.is_symlink():
                raise CheckFailed("state_file_symlink_refused")
            os.replace(temporary, path)
    except BaseException:
        if temporary.exists():
            temporary.unlink()
        raise


def read_private(path, limit=1024 * 1024):
    if path.is_symlink():
        raise CheckFailed("state_file_symlink_refused")
    info = path.stat()
    if info.st_mode & 0o077 or info.st_uid != os.getuid() or not stat.S_ISREG(info.st_mode):
        raise CheckFailed("state_file_requires_owner_only_permissions")
    with path.open("rb") as source:
        data = source.read(limit + 1)
    if len(data) > limit:
        raise CheckFailed("state_file_too_large")
    return data


def child_environment(root, binary, grant):
    directories = [str(binary.parent)]
    # The installed npm entrypoint uses /usr/bin/env node. Preserve its executable
    # directory, not the invoking shell's full environment or startup options.
    node = shutil.which("node")
    if node:
        directories.append(str(Path(node).parent))
    directories += ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
    return {
        "HOME": str(root / "home"), "CODEX_HOME": str(root / "codex"),
        "XDG_CONFIG_HOME": str(root / "home" / ".config"),
        "XDG_CACHE_HOME": str(root / "home" / ".cache"),
        "XDG_DATA_HOME": str(root / "home" / ".local" / "share"),
        "PATH": ":".join(dict.fromkeys(directories)), "TMPDIR": str(root / "tmp"),
        "POOLPARTY_GRANT": grant, "TERM": "dumb", "LANG": "C.UTF-8",
    }


def config_text(origin, binding, model, effort):
    quote = json.dumps
    return f'''model = {quote(model)}
model_provider = "poolparty"
model_reasoning_effort = {quote(effort)}
cli_auth_credentials_store = "file"
approval_policy = "never"
sandbox_mode = "workspace-write"
web_search = "disabled"
check_for_update_on_startup = false

[model_providers.poolparty]
name = "Poolparty"
base_url = {quote(origin + "/routes/" + binding + "/codex")}
env_key = "POOLPARTY_GRANT"
requires_openai_auth = false
wire_api = "responses"
supports_websockets = false
request_max_retries = 0
stream_max_retries = 0

[features]
unbounded_connection_retries = false

[sandbox_workspace_write]
network_access = false

[shell_environment_policy]
inherit = "none"
experimental_use_profile = false
[shell_environment_policy.set]
PATH = "/usr/bin:/bin:/usr/sbin:/sbin"
'''


def verify_config(actual_text, expected_text, workdir):
    """Allow only native trust metadata for this exact isolated workspace."""
    try:
        actual = tomllib.loads(actual_text)
        expected = tomllib.loads(expected_text)
    except tomllib.TOMLDecodeError:
        raise CheckFailed("native_provider_config_changed_before_resume") from None
    if "projects" in actual:
        projects = actual.pop("projects")
        expected_path = str(workdir.resolve())
        if (not isinstance(projects, dict) or set(projects) != {expected_path}
                or not isinstance(projects[expected_path], dict)
                or set(projects[expected_path]) != {"trust_level"}
                or projects[expected_path]["trust_level"] not in ("trusted", "untrusted")):
            raise CheckFailed("native_provider_config_changed_before_resume")

    def same(left, right):
        if type(left) is not type(right):
            return False
        if isinstance(left, dict):
            return left.keys() == right.keys() and all(same(left[key], right[key]) for key in left)
        if isinstance(left, list):
            return len(left) == len(right) and all(same(a, b) for a, b in zip(left, right))
        return left == right

    if not same(actual, expected):
        raise CheckFailed("native_provider_config_changed_before_resume")


def terminate(process):
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    process.wait(timeout=5)


def run_native(binary, root, environment, arguments, timeout, keep_output=False):
    process = subprocess.Popen([str(binary), *arguments], cwd=root / "work",
        env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, start_new_session=True)
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    output = {"stdout": bytearray(), "stderr": bytearray()}
    deadline = time.monotonic() + timeout
    try:
        while selector.get_map():
            if time.monotonic() >= deadline:
                raise CheckFailed("native_timeout_no_automatic_retry")
            for key, _ in selector.select(timeout=min(0.2, max(0, deadline - time.monotonic()))):
                chunk = os.read(key.fileobj.fileno(), 65536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                output[key.data].extend(chunk)
                if sum(map(len, output.values())) > OUTPUT_LIMIT:
                    raise CheckFailed("native_output_bound_exceeded")
        status = process.wait(timeout=max(0.1, deadline - time.monotonic()))
        if status:
            # A few local diagnostics are recognized without printing raw stderr.
            stderr = bytes(output["stderr"])
            if b"not recognized" in stderr or b"unknown field" in stderr:
                raise CheckFailed("native_config_rejected")
            raise CheckFailed("native_process_failed_no_automatic_retry")
        return bytes(output["stdout"])
    finally:
        selector.close()
        terminate(process)
        process.stdout.close()
        process.stderr.close()
        if keep_output and arguments[0] != "--version":
            phase = "resume" if "resume" in arguments else "start"
            for stream, data in output.items():
                target = root / ("native-" + phase + "." + stream)
                write_private(target, data, replace=target.exists())


def check_events(raw, expected_thread=None):
    try:
        events = [json.loads(line) for line in raw.splitlines() if line.strip()]
    except (ValueError, UnicodeError):
        raise CheckFailed("native_events_invalid_json") from None
    if not all(isinstance(event, dict) for event in events):
        raise CheckFailed("native_events_invalid_shape")
    if any(event.get("type") in ("turn.failed", "error") for event in events):
        raise CheckFailed("native_reported_failure_no_automatic_retry")
    threads = [event.get("thread_id") for event in events if event.get("type") == "thread.started"]
    if len(threads) != 1 or (expected_thread and threads[0] != expected_thread):
        raise CheckFailed("native_thread_affinity_failed")
    thread = identifier(threads[0])
    if sum(event.get("type") == "turn.completed" for event in events) != 1:
        raise CheckFailed("native_turn_did_not_complete_once")
    commands = [event["item"] for event in events if event.get("type") == "item.completed"
        and isinstance(event.get("item"), dict) and event["item"].get("type") == "command_execution"
        and event["item"].get("status") == "completed" and event["item"].get("exit_code") == 0]
    if not commands:
        raise CheckFailed("native_successful_shell_tool_event_missing")
    return thread, {"event_count": len(events), "successful_shell_commands": len(commands),
        "turn_completed": True}


def inspect_binding(origin, grant, binding, expected=None):
    value = api(origin, grant, "GET", "/api/v1/sessions/" + identifier(binding))
    if value.get("id") != binding or value.get("closed_at") is not None:
        raise CheckFailed("binding_missing_changed_or_closed")
    intent = value.get("intent", {})
    if not isinstance(intent, dict) or intent.get("product") != "codex_subscription":
        raise CheckFailed("binding_is_not_codex_subscription")
    identifier(value.get("account"))
    if expected and value != expected:
        raise CheckFailed("binding_changed_across_native_processes")
    return value


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--live", action="store_true", help="explicitly enable router/provider operations")
    parser.add_argument("--router", help="HTTPS or literal loopback HTTP router origin")
    parser.add_argument("--state-dir", type=Path, help="dedicated owner-only directory outside git checkouts")
    parser.add_argument("--phase", choices=("all", "start", "resume"), default="all")
    parser.add_argument("--binding", help="use an existing explicitly pinned binding")
    parser.add_argument("--pool", help="explicit pool when creating a new binding")
    parser.add_argument("--account", help="explicit account when creating a new binding")
    parser.add_argument("--model", help="explicit native model matching the binding")
    parser.add_argument("--effort", choices=("minimal", "low", "medium", "high", "xhigh"), default="low")
    parser.add_argument("--codex", default="codex", help="installed Codex executable or launcher")
    parser.add_argument("--timeout", type=int, default=180, help="per native process deadline in seconds")
    parser.add_argument("--keep-output", action="store_true", help="retain bounded raw diagnostics only in private state")
    args = parser.parse_args()
    if not args.live:
        parser.print_help()
        return 0
    if not args.router or not args.state_dir or not 1 <= args.timeout <= 600:
        raise CheckFailed("live_requires_router_private_state_and_bounded_timeout")
    origin = router_url(args.router)
    grant = os.environ.get("POOLPARTY_NATIVE_GRANT", "")
    if len(grant) < 32 or any(ord(c) < 33 or ord(c) > 126 for c in grant):
        raise CheckFailed("set_scoped_POOLPARTY_NATIVE_GRANT")
    executable = shutil.which(args.codex)
    if not executable:
        raise CheckFailed("installed_codex_not_found")
    binary = Path(executable).absolute()
    root = private_directory(args.state_dir, create=args.phase != "resume")
    manifest_path = root / MANIFEST
    if args.phase != "resume":
        if any(root.iterdir()):
            raise CheckFailed("start_requires_empty_private_state_directory")
        for name in ("home", "codex", "tmp", "work"):
            (root / name).mkdir(mode=0o700)
    else:
        for name in ("home", "codex", "tmp", "work"):
            private_directory(root / name)
    environment = child_environment(root, binary, grant)
    version = run_native(binary, root, environment, ["--version"], 10).decode().strip()
    if version != EXPECTED_VERSION:
        raise CheckFailed("native_version_not_qualified_update_fixture_first")
    summaries = []
    if args.phase != "resume":
        if not args.model or len(args.model) > 256 or any(ord(c) < 32 for c in args.model):
            raise CheckFailed("explicit_model_required")
        if args.binding:
            binding = inspect_binding(origin, grant, args.binding)
            if args.account and binding["account"] != args.account:
                raise CheckFailed("existing_binding_account_does_not_match")
        else:
            if not args.pool or not args.account:
                raise CheckFailed("new_binding_requires_explicit_pool_and_account")
            binding = api(origin, grant, "POST", "/api/v1/sessions", {
                "session": str(uuid.uuid4()), "pool": identifier(args.pool),
                "product": "codex_subscription", "model": args.model,
                "account": identifier(args.account), "effort": args.effort,
            })
            binding = inspect_binding(origin, grant, identifier(binding.get("id")), binding)
        if binding["intent"].get("model") != args.model or binding["intent"].get("effort") != args.effort:
            raise CheckFailed("native_model_or_effort_must_match_binding")
        config = config_text(origin, binding["id"], args.model, args.effort)
        write_private(root / "codex" / "config.toml", config)
        write_private(root / "work" / "seed.txt", uuid.uuid4().hex + "\n")
        manifest = {"schema": 1, "router": origin, "binding": binding, "model": args.model,
            "effort": args.effort, "phase": "starting", "version": version}
        write_private(manifest_path, json.dumps(manifest))
        raw = run_native(binary, root, environment, ["exec", "--strict-config", "--skip-git-repo-check", "--json",
            "Use the local shell tool to read seed.txt. Using that same tool, create proof.txt with "
            "the exact seed.txt contents followed by the line started and a newline. Read proof.txt "
            "with the shell tool to verify it. Do not access other directories, network, configuration "
            "or environment variables. Do not use apply_patch. Reply only POOLPARTY_START_OK."], args.timeout, args.keep_output)
        thread, summary = check_events(raw)
        proof = root / "work" / "proof.txt"
        if read_private(proof, 1024) != read_private(root / "work" / "seed.txt") + b"started\n":
            raise CheckFailed("start_artifact_verification_failed")
        if (root / "codex" / "auth.json").exists():
            raise CheckFailed("unexpected_native_auth_cache_refused")
        inspect_binding(origin, grant, binding["id"], binding)
        manifest.update({"phase": "started", "thread": thread})
        write_private(manifest_path, json.dumps(manifest), replace=True)
        summaries.append({"phase": "start", "artifact_verified": True, **summary})
    if args.phase != "start":
        try:
            manifest = json.loads(read_private(manifest_path))
        except (ValueError, UnicodeError):
            raise CheckFailed("native_state_manifest_invalid") from None
        if (manifest.get("schema") != 1 or manifest.get("phase") != "started"
                or manifest.get("router") != origin or manifest.get("version") != version):
            raise CheckFailed("resume_requires_matching_completed_start_state")
        binding = manifest["binding"]
        if ((args.binding and args.binding != binding["id"])
                or (args.model and args.model != manifest["model"])
                or (args.account and args.account != binding["account"])):
            raise CheckFailed("resume_arguments_conflict_with_original_binding")
        inspect_binding(origin, grant, binding["id"], binding)
        expected_config = config_text(origin, binding["id"], manifest["model"], manifest["effort"])
        verify_config(read_private(root / "codex" / "config.toml").decode(), expected_config, root / "work")
        if (root / "codex" / "auth.json").exists():
            raise CheckFailed("unexpected_native_auth_cache_refused")
        manifest["phase"] = "resuming"
        write_private(manifest_path, json.dumps(manifest), replace=True)
        raw = run_native(binary, root, environment, ["exec", "resume", "--strict-config", "--skip-git-repo-check", "--json",
            identifier(manifest["thread"]), "Resume the previous task. Use the local shell tool to read proof.txt, "
            "then append exactly the line resumed and a newline. Read proof.txt with the shell tool "
            "to verify both markers. Do not access other directories, network, configuration or "
            "environment variables. Do not use apply_patch. Reply only POOLPARTY_RESUME_OK."], args.timeout, args.keep_output)
        _, summary = check_events(raw, manifest["thread"])
        proof = root / "work" / "proof.txt"
        if read_private(proof, 1024) != read_private(root / "work" / "seed.txt") + b"started\nresumed\n":
            raise CheckFailed("resume_artifact_verification_failed")
        inspect_binding(origin, grant, binding["id"], binding)
        manifest["phase"] = "resumed"
        write_private(manifest_path, json.dumps(manifest), replace=True)
        summaries.append({"phase": "resume", "artifact_verified": True, "same_native_thread": True,
            "same_router_binding": True, **summary})
    print(json.dumps({"passed": True, "version": version, "checks": summaries}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except CheckFailed as error:
        print(json.dumps({"passed": False, "error": str(error)}))
        sys.exit(1)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print(json.dumps({"passed": False, "error": "local_fixture_operation_failed"}))
        sys.exit(1)
