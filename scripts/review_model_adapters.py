#!/usr/bin/env python3
"""Strict, process-bound adapters for the model-evaluation controller.

The controller owns packets and schemas.  These adapters only transport one
packet to a configured CLI and return the CLI's structured result plus
transport metadata.  They deliberately do not offer an HTTP or JSON-v1
fallback: a failed or unsupported adapter is visible to the caller.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import signal
import shutil
import subprocess
import tempfile
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence


MAX_PACKET_BYTES = 8 * 1024 * 1024
MAX_OUTPUT_BYTES = 8 * 1024 * 1024
DEFAULT_TIMEOUT_SECONDS = 900
SAFE_ENVIRONMENT_KEYS = frozenset(
    {
        "PATH",
        "HOME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "TERM",
        "USER",
        "NO_COLOR",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
    }
)


class AdapterError(RuntimeError):
    """A transport or structured-output failure."""

    def __init__(self, message: str, *, capture: Optional["ProcessCapture"] = None) -> None:
        super().__init__(message)
        self.capture = capture


class AdapterUnavailable(AdapterError):
    """The configured transport cannot safely run in this environment."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _bounded_text(value: Any, field_name: str, limit: int = 4096) -> str:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > limit:
        raise AdapterError(f"{field_name} must be a bounded non-empty string")
    if "\x00" in value:
        raise AdapterError(f"{field_name} contains NUL")
    return value


def safe_environment(source: Optional[Mapping[str, str]] = None) -> dict[str, str]:
    """Select the small environment required by the supported CLI transport.

    In particular, do not pass through arbitrary variables from a repository,
    shell, or test harness.  The OAuth/API-key variables are passed through as
    credentials for the CLI itself; the adapter never reads or copies them.
    """

    source = os.environ if source is None else source
    result: dict[str, str] = {}
    for key in sorted(SAFE_ENVIRONMENT_KEYS):
        if key not in source:
            continue
        value = str(source[key])
        if "\x00" in value:
            raise AdapterError(f"environment value contains NUL: {key}")
        result[key] = value
    return result


def validate_profile(profile: Mapping[str, Any], role: str) -> dict[str, Any]:
    if not isinstance(profile, Mapping):
        raise AdapterError(f"model profile for {role} must be an object")
    required = ("adapter", "model_id", "family", "strength")
    for key in required:
        if key not in profile:
            raise AdapterError(f"model profile for {role} lacks {key}")
    adapter = _bounded_text(profile["adapter"], f"{role}.adapter", 32)
    if adapter not in {"claude", "codex"}:
        raise AdapterError(f"unsupported adapter {adapter!r} for {role}")
    model_id = _bounded_text(profile["model_id"], f"{role}.model_id", 512)
    family = _bounded_text(profile["family"], f"{role}.family", 128)
    if profile["strength"] != "strong":
        raise AdapterError(f"{role} must use strength=strong")
    executable = profile.get("executable", adapter)
    executable = _bounded_text(executable, f"{role}.executable", 2048)
    transport = profile.get("transport", "oauth")
    transport = _bounded_text(transport, f"{role}.transport", 32)
    if adapter == "claude" and transport not in {"oauth", "bare"}:
        raise AdapterError(f"unsupported Claude transport {transport!r} for {role}")
    if adapter == "codex" and transport != "oauth":
        raise AdapterError(f"unsupported Codex transport {transport!r} for {role}")
    normalized = dict(profile)
    normalized.update(
        {
            "adapter": adapter,
            "model_id": model_id,
            "family": family,
            "executable": executable,
            "transport": transport,
        }
    )
    return normalized


@dataclass(frozen=True)
class ProcessCapture:
    argv: tuple[str, ...]
    returncode: Optional[int]
    stdout: bytes
    stderr: bytes
    timed_out: bool = False
    error: Optional[str] = None
    output_limited: bool = False
    stdout_complete: bool = True
    stderr_complete: bool = True
    output_limit: Optional[int] = None


@dataclass(frozen=True)
class AdapterResponse:
    """A structured model result with transport metadata kept separate."""

    structured_output: Any
    requested_model_id: str
    observed_model_id: str
    actual_metadata: dict[str, Any]
    raw_stdout: bytes
    raw_stderr: bytes
    argv: tuple[str, ...]
    returncode: int
    attempts: int = 1
    transport_status: str = "success"

    def artifact_metadata(self) -> dict[str, Any]:
        return {
            "requested_model_id": self.requested_model_id,
            "observed_model_id": self.observed_model_id,
            "actual_metadata": self.actual_metadata,
            "returncode": self.returncode,
            "attempts": self.attempts,
            "transport_status": self.transport_status,
            "stdout_sha256": sha256_bytes(self.raw_stdout),
            "stderr_sha256": sha256_bytes(self.raw_stderr),
            "usage_status": (
                "known"
                if isinstance(self.actual_metadata.get("usage"), Mapping)
                or isinstance(self.actual_metadata.get("model_usage"), Mapping)
                else "unknown"
            ),
            "cost_status": (
                "estimate"
                if self.actual_metadata.get("estimated_cost_usd") is not None
                else "unknown"
            ),
            "estimated_cost_usd": self.actual_metadata.get("estimated_cost_usd"),
            "actual_cost_usd": self.actual_metadata.get("actual_cost_usd", "unknown"),
        }


Runner = Callable[[Sequence[str], bytes, Path, Optional[Mapping[str, str]], float, int], ProcessCapture]


def _redacted_environment(env: Optional[Mapping[str, str]]) -> dict[str, str]:
    """Return only safe metadata; never put inherited secrets in artifacts."""

    if not env:
        return {}
    sensitive = ("KEY", "TOKEN", "SECRET", "PASSWORD", "COOKIE", "AUTH", "CREDENTIAL")
    result: dict[str, str] = {}
    for key in sorted(env):
        upper = key.upper()
        if any(part in upper for part in sensitive):
            result[key] = "<redacted>"
        elif key in {"PATH", "HOME", "TMPDIR", "LANG", "LC_ALL"}:
            result[key] = str(env[key])[:512]
    return result


def run_direct(
    argv: Sequence[str],
    input_bytes: bytes,
    cwd: Path,
    env: Optional[Mapping[str, str]],
    timeout: float,
    max_output_bytes: int,
) -> ProcessCapture:
    if not argv or any(not isinstance(part, str) or "\x00" in part for part in argv):
        raise AdapterError("argv must be a non-empty NUL-free string list")
    if len(input_bytes) > MAX_PACKET_BYTES:
        raise AdapterError("model packet exceeds the transport bound")
    if timeout <= 0 or max_output_bytes <= 0:
        raise AdapterError("model transport bounds must be positive")
    cwd = Path(cwd)
    if not cwd.is_dir() or any(cwd.iterdir()):
        raise AdapterError("model process cwd must be an existing empty directory")
    process: Optional[subprocess.Popen[bytes]] = None
    stdout_buffer = bytearray()
    stderr_buffer = bytearray()
    output_limited = threading.Event()
    stdout_limited = threading.Event()
    stderr_limited = threading.Event()
    reader_errors: list[str] = []

    def read_stream(stream: Any, buffer: bytearray, label: str) -> None:
        try:
            while True:
                chunk = stream.read(64 * 1024)
                if not chunk:
                    return
                remaining = max_output_bytes - len(buffer)
                if len(chunk) > remaining:
                    if remaining > 0:
                        buffer.extend(chunk[:remaining])
                    (stdout_limited if label == "stdout" else stderr_limited).set()
                    output_limited.set()
                    return
                buffer.extend(chunk)
        except (OSError, ValueError) as exc:
            reader_errors.append(f"{label}:{type(exc).__name__}")

    def kill_process_group(child: subprocess.Popen[bytes]) -> None:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            child.kill()
        except ProcessLookupError:
            pass

    try:
        process = subprocess.Popen(
            list(argv),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(cwd),
            env=safe_environment(env),
            shell=False,
            close_fds=True,
            start_new_session=True,
        )
        stdout_thread = threading.Thread(target=read_stream, args=(process.stdout, stdout_buffer, "stdout"), daemon=True)
        stderr_thread = threading.Thread(target=read_stream, args=(process.stderr, stderr_buffer, "stderr"), daemon=True)
        stdout_thread.start()
        stderr_thread.start()

        def write_stdin() -> None:
            try:
                if process is not None and process.stdin is not None:
                    process.stdin.write(input_bytes)
                    process.stdin.close()
            except (BrokenPipeError, OSError, ValueError):
                pass

        stdin_thread = threading.Thread(target=write_stdin, daemon=True)
        stdin_thread.start()
        deadline = time.monotonic() + max(0.0, timeout)
        timed_out = False
        while process.poll() is None:
            if output_limited.is_set():
                kill_process_group(process)
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                kill_process_group(process)
                break
            try:
                process.wait(timeout=min(remaining, 0.05))
            except subprocess.TimeoutExpired:
                continue
        if output_limited.is_set() and process.poll() is None:
            kill_process_group(process)
        if process.poll() is None:
            process.wait()
        if process.stdin is not None:
            try:
                process.stdin.close()
            except (OSError, ValueError):
                pass
        stdout_thread.join(timeout=1.0)
        stderr_thread.join(timeout=1.0)
        stdin_thread.join(timeout=1.0)
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except (OSError, ValueError):
                    pass
        error = "timeout" if timed_out else ("output_limit" if output_limited.is_set() else None)
        if reader_errors and error is None:
            error = "capture_failed"
        return ProcessCapture(
            tuple(argv),
            process.returncode,
            bytes(stdout_buffer),
            bytes(stderr_buffer),
            timed_out,
            error,
            output_limited.is_set(),
            not stdout_limited.is_set(),
            not stderr_limited.is_set(),
            max_output_bytes,
        )
    except (OSError, ValueError) as exc:
        return ProcessCapture(tuple(argv), None, b"", str(exc).encode("utf-8", "replace"), error=type(exc).__name__)


def _default_runner(
    argv: Sequence[str], input_bytes: bytes, cwd: Path, env: Optional[Mapping[str, str]], timeout: float, max_output: int
) -> ProcessCapture:
    return run_direct(argv, input_bytes, cwd, safe_environment(env), timeout, max_output)


def _executable_exists(executable: str) -> bool:
    if os.path.sep in executable:
        return Path(executable).is_file() and os.access(executable, os.X_OK)
    return shutil.which(executable) is not None


class BaseAdapter:
    adapter_name = "base"
    required_help_flags: tuple[str, ...] = ()

    def __init__(
        self,
        executable: Optional[str] = None,
        *,
        timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
        max_output_bytes: int = MAX_OUTPUT_BYTES,
        runner: Optional[Runner] = None,
        environment: Optional[Mapping[str, str]] = None,
        transport: str = "oauth",
    ) -> None:
        self.executable = executable or self.adapter_name
        self.timeout_seconds = timeout_seconds
        self.max_output_bytes = max_output_bytes
        self.runner = runner or _default_runner
        self.environment = safe_environment(environment)
        self.transport = transport

    def _run(self, argv: Sequence[str], payload: bytes, cwd: Path) -> ProcessCapture:
        return self.runner(argv, payload, cwd, self.environment, self.timeout_seconds, self.max_output_bytes)

    def probe(self, profile: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
        if not _executable_exists(self.executable) and self.runner is _default_runner:
            return {"status": "unavailable", "reason": "executable_not_found", "adapter": self.adapter_name}
        with tempfile.TemporaryDirectory(prefix=f"openab-eval-{self.adapter_name}-probe-") as temp:
            cwd = Path(temp)
            version_capture = self._run([self.executable, "--version"], b"", cwd)
        with tempfile.TemporaryDirectory(prefix=f"openab-eval-{self.adapter_name}-probe-") as temp:
            cwd = Path(temp)
            help_capture = self._run([self.executable, "--help"], b"", cwd)
        if version_capture.returncode != 0 or help_capture.returncode != 0:
            return {
                "status": "unavailable",
                "reason": "feature_probe_failed",
                "adapter": self.adapter_name,
                "version_status": version_capture.returncode,
                "help_status": help_capture.returncode,
            }
        help_text = help_capture.stdout.decode("utf-8", "replace")
        missing = [flag for flag in self.required_help_flags if flag not in help_text]
        result: dict[str, Any] = {
            "status": "ready" if not missing else "unsupported",
            "adapter": self.adapter_name,
            "version": version_capture.stdout.decode("utf-8", "replace").strip()[:512],
            "help_sha256": sha256_bytes(help_capture.stdout),
            "missing_flags": missing,
        }
        if missing:
            result["reason"] = "required_cli_flag_not_advertised"
        return result

    @staticmethod
    def _packet_bytes(packet: Mapping[str, Any]) -> bytes:
        payload = canonical_json(packet).encode("utf-8")
        if len(payload) > MAX_PACKET_BYTES:
            raise AdapterError("model packet exceeds the transport bound")
        return payload

    @staticmethod
    def _parse_json(stdout: bytes) -> Any:
        try:
            value = json.loads(stdout.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise AdapterError("CLI stdout is not UTF-8 JSON") from exc
        return value

    @staticmethod
    def _reject_unexpected_tools(events: Sequence[Mapping[str, Any]]) -> None:
        """Accept only one ID-bound, non-executable StructuredOutput exchange."""

        forbidden_event_types = {
            "tool",
            "tool_call",
            "tool_use",
            "tool_result",
            "function_call",
            "function_result",
            "file_write",
            "file_edit",
            "mcp_tool_call",
            "mcp_tool_use",
            "mcp_tool_result",
            "mcp",
            "shell_command",
            "command_execution",
            "file_read",
        }
        structured_output_declared = False
        structured_tool_use_ids: list[str] = []
        structured_tool_result_ids: list[str] = []
        structured_tool_use_positions: list[int] = []
        structured_tool_result_positions: list[int] = []
        result_positions = [index for index, event in enumerate(events) if event.get("type") == "result"]
        for index, event in enumerate(events):
            event_type = event.get("type")
            if event_type in forbidden_event_types:
                raise AdapterError("CLI emitted an unexpected executable or MCP tool event")
            if event_type == "system" and event.get("subtype") == "init":
                structured_output_declared = event.get("tools") == ["StructuredOutput"]
            for key in ("plugins", "skills", "mcp_servers", "mcpServers"):
                if key in event and event[key] not in (None, [], {}):
                    raise AdapterError(f"CLI emitted unexpected {key} metadata")
            if "tools" in event:
                tools = event["tools"]
                if (
                    not isinstance(tools, list)
                    or any(tool != "StructuredOutput" for tool in tools)
                    or len(tools) != len(set(tools))
                ):
                    raise AdapterError("CLI emitted an unexpected model tool")
            message = event.get("message")
            if isinstance(message, Mapping):
                content = message.get("content")
                if isinstance(content, list):
                    for block in content:
                        if not isinstance(block, Mapping):
                            continue
                        block_type = block.get("type")
                        if block_type == "tool_use":
                            if (
                                event_type != "assistant"
                                or message.get("role") not in (None, "assistant")
                                or block.get("name") != "StructuredOutput"
                            ):
                                raise AdapterError("CLI emitted an unexpected executable or MCP tool block")
                            tool_use_id = _bounded_text(block.get("id"), "StructuredOutput tool_use id", 512)
                            if not isinstance(block.get("input"), Mapping):
                                raise AdapterError("StructuredOutput tool_use input must be an object")
                            if structured_tool_use_ids:
                                raise AdapterError("CLI emitted duplicate StructuredOutput tool_use blocks")
                            structured_tool_use_ids.append(tool_use_id)
                            structured_tool_use_positions.append(index)
                        elif block_type == "tool_result":
                            if event_type != "user" or message.get("role") not in (None, "user"):
                                raise AdapterError("CLI emitted an unexpected executable or MCP tool block")
                            tool_use_id = _bounded_text(block.get("tool_use_id"), "StructuredOutput tool_result id", 512)
                            if block.get("content") != "Structured output provided successfully":
                                raise AdapterError("CLI emitted an unexpected StructuredOutput completion")
                            if block.get("is_error") is True or block.get("isError") is True:
                                raise AdapterError("CLI emitted a failed StructuredOutput completion")
                            if structured_tool_result_ids:
                                raise AdapterError("CLI emitted duplicate StructuredOutput tool_result blocks")
                            structured_tool_result_ids.append(tool_use_id)
                            structured_tool_result_positions.append(index)
                        elif block_type in forbidden_event_types:
                            raise AdapterError("CLI emitted an unexpected executable or MCP tool block")
            if event.get("tool_use_result") not in (None, "Structured output provided successfully"):
                raise AdapterError("CLI emitted an unexpected StructuredOutput completion")
        if structured_tool_use_ids or structured_tool_result_ids:
            if not structured_output_declared:
                raise AdapterError("CLI StructuredOutput call was not declared by init")
            if structured_tool_use_ids != structured_tool_result_ids:
                raise AdapterError("CLI StructuredOutput completion ID does not match its call")
            if structured_tool_use_positions[0] >= structured_tool_result_positions[0]:
                raise AdapterError("CLI StructuredOutput completion precedes its call")
            if result_positions and structured_tool_result_positions[0] >= result_positions[-1]:
                raise AdapterError("CLI StructuredOutput completion follows the result envelope")

    @classmethod
    def _parse_envelope(
        cls, stdout: bytes, requested_model_id: str
    ) -> tuple[Any, dict[str, Any], str]:
        value = cls._parse_json(stdout)
        if isinstance(value, list):
            if not value or any(not isinstance(event, Mapping) for event in value):
                raise AdapterError("CLI JSON event envelope is invalid")
            events = list(value)
            cls._reject_unexpected_tools(events)
            result_events = [event for event in events if event.get("type") == "result"]
            if not result_events:
                raise AdapterError("CLI JSON event array lacks a result envelope")
            envelope = result_events[-1]
        elif isinstance(value, Mapping):
            events = [value]
            cls._reject_unexpected_tools(events)
            envelope = value
        else:
            raise AdapterError("CLI JSON envelope must be an object or event array")
        if envelope.get("is_error") is True or envelope.get("isError") is True:
            raise AdapterError("CLI result envelope reports an error")
        if envelope.get("subtype") in {"error", "failure"}:
            raise AdapterError("CLI result envelope reports a failure")
        if "structured_output" not in envelope:
            raise AdapterError("CLI JSON envelope lacks structured_output")

        observed_models = []
        for event in events:
            if event.get("type") != "assistant":
                continue
            message = event.get("message")
            if isinstance(message, Mapping) and isinstance(message.get("model"), str) and message["model"]:
                observed_models.append(message["model"])
        unique_models = list(dict.fromkeys(observed_models))
        observed_model_id = unique_models[-1] if unique_models else "unavailable"
        metadata_keys = {
            "type",
            "subtype",
            "is_error",
            "isError",
            "duration_ms",
            "durationMs",
            "usage",
            "cost",
            "cost_usd",
            "total_cost_usd",
            "session_id",
            "sessionId",
            "model",
            "modelUsage",
            "model_usage",
        }
        metadata = {key: envelope[key] for key in metadata_keys if key in envelope}
        if unique_models:
            metadata["transport_observed_model_ids"] = unique_models
        init_events = [event for event in events if event.get("type") == "system" and event.get("subtype") == "init"]
        if init_events:
            init = init_events[-1]
            metadata["init"] = {
                key: init[key]
                for key in ("subtype", "tools", "plugins", "skills", "mcp_servers", "mcpServers")
                if key in init
            }
        usage = envelope.get("modelUsage", envelope.get("model_usage"))
        if isinstance(usage, Mapping):
            metadata["model_usage"] = dict(usage)
            auxiliary = {str(key): value for key, value in usage.items() if key != requested_model_id}
            metadata["auxiliary_model_usage"] = auxiliary
            estimates = []
            bases = []
            for entry in usage.values():
                if not isinstance(entry, Mapping):
                    continue
                for cost_key in ("costUSD", "cost_usd"):
                    cost = entry.get(cost_key)
                    if isinstance(cost, (int, float)) and not isinstance(cost, bool):
                        estimates.append(float(cost))
                        break
                basis = entry.get("costBasis", entry.get("cost_basis"))
                if basis is not None:
                    bases.append(str(basis))
            if estimates:
                metadata["estimated_cost_usd"] = sum(estimates)
            if bases:
                metadata["cost_basis"] = list(dict.fromkeys(bases))
        elif any(key in metadata for key in ("total_cost_usd", "cost_usd", "cost")):
            cost = metadata.get("total_cost_usd", metadata.get("cost_usd", metadata.get("cost")))
            if isinstance(cost, (int, float)) and not isinstance(cost, bool):
                metadata["estimated_cost_usd"] = float(cost)
                metadata["cost_basis"] = ["cli_reported_estimate"]
        metadata["actual_cost_usd"] = "unknown"
        return envelope["structured_output"], metadata, observed_model_id

    def _response_from_capture(
        self, capture: ProcessCapture, requested_model_id: str, *, require_structured_output: bool = True
    ) -> AdapterResponse:
        if capture.error or capture.timed_out:
            reason = capture.error or "timeout"
            raise AdapterError(f"{self.adapter_name} invocation failed: {reason}", capture=capture)
        if capture.returncode != 0:
            raise AdapterError(f"{self.adapter_name} exited with {capture.returncode}", capture=capture)
        try:
            if require_structured_output:
                structured, actual_metadata, observed_model_id = self._parse_envelope(capture.stdout, requested_model_id)
            else:
                structured = self._parse_json(capture.stdout)
                if not isinstance(structured, Mapping):
                    raise AdapterError("CLI JSON envelope must be an object")
                actual_metadata = {}
                observed_model_id = "unavailable"
        except AdapterError as exc:
            if exc.capture is None:
                exc.capture = capture
            raise
        # A model-authored field is never copied into observed identity.  The
        # JSON CLI envelope is retained separately, but identity is unknown
        # unless a future trusted transport explicitly supplies it.
        actual_metadata["redacted_environment"] = _redacted_environment(self.environment)
        return AdapterResponse(
            structured_output=structured,
            requested_model_id=requested_model_id,
            observed_model_id=observed_model_id,
            actual_metadata=actual_metadata,
            raw_stdout=capture.stdout,
            raw_stderr=capture.stderr,
            argv=capture.argv,
            returncode=int(capture.returncode or 0),
        )


class ClaudeAdapter(BaseAdapter):
    """The supported text-only Claude transport."""

    adapter_name = "claude"
    required_help_flags = (
        "--safe-mode",
        "--restricted",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--output-format",
        "--json-schema",
        "--model",
        "--tools",
        "--strict-mcp-config",
        "--setting-sources",
        "--permission-mode",
        "--permission-prompts",
        "--system-prompt",
    )

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        if self.transport == "bare":
            self.required_help_flags = (
                "--bare",
                "--no-session-persistence",
                "--output-format",
                "--json-schema",
                "--model",
                "--tools",
                "--strict-mcp-config",
                "--setting-sources",
                "--permission-mode",
                "--permission-prompts",
                "--system-prompt",
            )

    @staticmethod
    def build_argv(
        executable: str,
        model_id: str,
        schema: Mapping[str, Any],
        system_prompt: str,
        *,
        transport: str = "oauth",
    ) -> list[str]:
        schema_text = canonical_json(schema)
        _bounded_text(model_id, "model_id", 512)
        _bounded_text(system_prompt, "system_prompt", 32 * 1024)
        if transport not in {"oauth", "bare"}:
            raise AdapterError(f"unsupported Claude transport {transport!r}")
        argv = [executable, "--print"]
        if transport == "oauth":
            argv.extend(["--safe-mode", "--restricted", "--disable-slash-commands"])
        else:
            argv.append("--bare")
        argv.extend(
            [
            "--no-session-persistence",
            "--output-format",
            "json",
            "--json-schema",
            schema_text,
            "--model",
            model_id,
            "--permission-mode",
            "dontAsk",
            "--permission-prompts",
            "none",
            "--tools",
            "",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--system-prompt",
            system_prompt,
            "-",
            ]
        )
        return argv

    def invoke(
        self,
        packet: Mapping[str, Any],
        schema: Mapping[str, Any],
        model_id: str,
        *,
        system_prompt: str,
        session_dir: Path,
    ) -> AdapterResponse:
        argv = self.build_argv(self.executable, model_id, schema, system_prompt, transport=self.transport)
        capture = self._run(argv, self._packet_bytes(packet), session_dir)
        return self._response_from_capture(capture, model_id)


class CodexAdapter(BaseAdapter):
    """A deliberately gated Codex transport.

    Codex's read-only sandbox is not enough to establish the required
    no-model-tools/no-host-command boundary.  Until an external process
    confinement proof is supplied and independently checked, this adapter is
    explicitly unavailable; it never silently substitutes Claude.
    """

    adapter_name = "codex"
    required_help_flags = ("--ephemeral", "--ignore-user-config", "--ignore-rules", "--sandbox", "--output-schema")

    @staticmethod
    def build_argv(
        executable: str,
        model_id: str,
        schema_path: Path,
        session_dir: Path,
    ) -> list[str]:
        return [
            executable,
            "exec",
            "--ephemeral",
            "--ignore-user-config",
            "--ignore-rules",
            "--sandbox",
            "read-only",
            "--cd",
            str(session_dir),
            "--model",
            model_id,
            "--output-schema",
            str(schema_path),
            "--output-last-message",
            str(session_dir / "final.json"),
            "-",
        ]

    def probe(self, profile: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
        proof = profile.get("confinement_proof") if isinstance(profile, Mapping) else None
        if not isinstance(proof, Mapping) or proof.get("status") != "verified" or not proof.get("evidence_sha256"):
            return {
                "status": "unavailable",
                "adapter": "codex",
                "reason": "no_verified_no_tools_process_confinement",
            }
        base = super().probe(profile)
        # A configuration claim alone is intentionally not accepted.  The
        # proof must be a controller-recognised, immutable external attestation
        # with an evidence digest; no current local path can establish it.
        return {
            "status": "unavailable",
            "adapter": "codex",
            "reason": "confinement_attestation_requires_external_verification",
            "cli_probe": base,
        }

    def invoke(self, *args: Any, **kwargs: Any) -> AdapterResponse:
        raise AdapterUnavailable("Codex is unavailable until no-tools/process confinement is proven")


def adapter_for_profile(
    profile: Mapping[str, Any],
    *,
    runner: Optional[Runner] = None,
    environment: Optional[Mapping[str, str]] = None,
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
) -> BaseAdapter:
    normalized = validate_profile(profile, "profile")
    cls = ClaudeAdapter if normalized["adapter"] == "claude" else CodexAdapter
    return cls(
        normalized["executable"],
        timeout_seconds=timeout_seconds,
        runner=runner,
        environment=environment,
        transport=normalized.get("transport", "oauth"),
    )


def response_to_artifact(response: AdapterResponse) -> dict[str, Any]:
    return {
        "structured_output": response.structured_output,
        "transport": response.artifact_metadata(),
        "argv": list(response.argv),
    }
