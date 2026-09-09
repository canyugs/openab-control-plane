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
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence


MAX_PACKET_BYTES = 8 * 1024 * 1024
MAX_OUTPUT_BYTES = 8 * 1024 * 1024
DEFAULT_TIMEOUT_SECONDS = 900


class AdapterError(RuntimeError):
    """A transport or structured-output failure."""


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
    normalized = dict(profile)
    normalized.update(
        {"adapter": adapter, "model_id": model_id, "family": family, "executable": executable}
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
                "known" if isinstance(self.actual_metadata.get("usage"), Mapping) else "unknown"
            ),
            "cost_status": (
                "known"
                if self.actual_metadata.get("cost") is not None
                or self.actual_metadata.get("cost_usd") is not None
                else "unknown"
            ),
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
    cwd = Path(cwd)
    if not cwd.is_dir() or any(cwd.iterdir()):
        raise AdapterError("model process cwd must be an existing empty directory")
    process: Optional[subprocess.Popen[bytes]] = None
    try:
        process = subprocess.Popen(
            list(argv),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(cwd),
            env=dict(env) if env is not None else None,
            shell=False,
            close_fds=True,
            start_new_session=True,
        )
        stdout, stderr = process.communicate(input=input_bytes, timeout=timeout)
        if len(stdout) > max_output_bytes or len(stderr) > max_output_bytes:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            return ProcessCapture(
                tuple(argv), process.returncode, stdout[:max_output_bytes], stderr[:max_output_bytes], error="output_limit"
            )
        return ProcessCapture(tuple(argv), process.returncode, stdout, stderr)
    except subprocess.TimeoutExpired as exc:
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            stdout, stderr = process.communicate()
        else:
            stdout, stderr = (exc.stdout or b""), (exc.stderr or b"")
        return ProcessCapture(tuple(argv), process.returncode if process else None, stdout, stderr, True, "timeout")
    except (OSError, ValueError) as exc:
        return ProcessCapture(tuple(argv), None, b"", str(exc).encode("utf-8", "replace"), error=type(exc).__name__)


def _default_runner(
    argv: Sequence[str], input_bytes: bytes, cwd: Path, env: Optional[Mapping[str, str]], timeout: float, max_output: int
) -> ProcessCapture:
    return run_direct(argv, input_bytes, cwd, env, timeout, max_output)


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
    ) -> None:
        self.executable = executable or self.adapter_name
        self.timeout_seconds = timeout_seconds
        self.max_output_bytes = max_output_bytes
        self.runner = runner or _default_runner
        self.environment = dict(environment) if environment is not None else None

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
    def _parse_json(stdout: bytes) -> Mapping[str, Any]:
        try:
            value = json.loads(stdout.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise AdapterError("CLI stdout is not UTF-8 JSON") from exc
        if not isinstance(value, Mapping):
            raise AdapterError("CLI JSON envelope must be an object")
        return value

    def _response_from_capture(
        self, capture: ProcessCapture, requested_model_id: str, *, require_structured_output: bool = True
    ) -> AdapterResponse:
        if capture.error or capture.timed_out:
            reason = capture.error or "timeout"
            raise AdapterError(f"{self.adapter_name} invocation failed: {reason}")
        if capture.returncode != 0:
            raise AdapterError(f"{self.adapter_name} exited with {capture.returncode}")
        envelope = self._parse_json(capture.stdout)
        if require_structured_output:
            if "structured_output" not in envelope:
                raise AdapterError("CLI JSON envelope lacks structured_output")
            structured = envelope["structured_output"]
        else:
            structured = envelope
        metadata_keys = {"type", "subtype", "is_error", "duration_ms", "usage", "cost", "cost_usd", "session_id", "model"}
        actual_metadata = {key: envelope[key] for key in metadata_keys if key in envelope}
        # A model-authored field is never copied into observed identity.  The
        # JSON CLI envelope is retained separately, but identity is unknown
        # unless a future trusted transport explicitly supplies it.
        actual_metadata["redacted_environment"] = _redacted_environment(self.environment)
        return AdapterResponse(
            structured_output=structured,
            requested_model_id=requested_model_id,
            observed_model_id="unavailable",
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
        "--bare",
        "--no-session-persistence",
        "--output-format",
        "--json-schema",
        "--tools",
        "--strict-mcp-config",
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
    ) -> list[str]:
        schema_text = canonical_json(schema)
        _bounded_text(model_id, "model_id", 512)
        _bounded_text(system_prompt, "system_prompt", 32 * 1024)
        return [
            executable,
            "--print",
            "--bare",
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
            "--system-prompt",
            system_prompt,
            "-",
        ]

    def invoke(
        self,
        packet: Mapping[str, Any],
        schema: Mapping[str, Any],
        model_id: str,
        *,
        system_prompt: str,
        session_dir: Path,
    ) -> AdapterResponse:
        argv = self.build_argv(self.executable, model_id, schema, system_prompt)
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
        base = super().probe(profile)
        proof = profile.get("confinement_proof") if isinstance(profile, Mapping) else None
        # A configuration claim alone is intentionally not accepted.  The
        # proof must be a controller-recognised, immutable external attestation
        # with an evidence digest; no current local path can establish it.
        if not isinstance(proof, Mapping) or proof.get("status") != "verified" or not proof.get("evidence_sha256"):
            return {
                "status": "unavailable",
                "adapter": "codex",
                "reason": "no_verified_no_tools_process_confinement",
                "cli_probe": base,
            }
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
    )


def response_to_artifact(response: AdapterResponse) -> dict[str, Any]:
    return {
        "structured_output": response.structured_output,
        "transport": response.artifact_metadata(),
        "argv": list(response.argv),
    }
