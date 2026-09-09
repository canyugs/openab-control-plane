#!/usr/bin/env python3
"""Validate and execute generated reproduction controls inside disposable OCI.

The host controller validates plans and retains their bytes for audit.  It
never writes model-generated files into the repository or invokes generated
shell text.  A fixed runner is mounted read-only into a non-root, networkless
container and receives the validated plan on stdin.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import textwrap
import uuid
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Mapping, Optional, Sequence


MAX_GENERATED_FILE_BYTES = 512 * 1024
MAX_GENERATED_TOTAL_BYTES = 2 * 1024 * 1024
MAX_RUNS = 2
MAX_OUTPUT_BYTES = 1024 * 1024
DEFAULT_TIMEOUT_SECONDS = 30
DEFAULT_MEMORY = "512m"
DEFAULT_PIDS_LIMIT = "128"
DEFAULT_CPUS = "1.0"
DEFAULT_IMAGE = "python:3.12-slim@sha256:" + "0" * 64


class OCIError(RuntimeError):
    """A controller validation or OCI execution error."""


class OCIEnvironmentBlocked(OCIError):
    """The requested isolated execution environment is unavailable."""


_SHELL_NAMES = {
    "sh",
    "bash",
    "dash",
    "zsh",
    "fish",
    "csh",
    "tcsh",
    "ksh",
    " ash",
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
}
_SHELL_META = re.compile(r"[;&|$`()<>\n\r]")
_FORBIDDEN_ASSERT = re.compile(r"\bassert\s+(?:false|False|FALSE)\b|AssertionError")


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def validate_pinned_image(image: str) -> str:
    if not isinstance(image, str) or not image or "\x00" in image:
        raise OCIError("OCI image must be a non-empty string")
    if not re.search(r"@sha256:[0-9a-f]{64}$", image):
        raise OCIError("OCI execution requires a digest-pinned image")
    return image


def _bounded_string(value: Any, name: str, maximum: int = 4096) -> str:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > maximum or "\x00" in value:
        raise OCIError(f"{name} must be a bounded non-empty string")
    return value


def _safe_generated_path(value: Any) -> str:
    path = _bounded_string(value, "generated file path", 512)
    if "\\" in path or path.startswith("/"):
        raise OCIError("generated paths must be relative POSIX paths")
    parsed = PurePosixPath(path)
    if not path.startswith("generated/") or parsed.is_absolute() or ".." in parsed.parts:
        raise OCIError("generated paths must remain below generated/")
    if str(parsed) != path or path.endswith("/"):
        raise OCIError("generated path is not normalized")
    return path


def _validate_scalar_tree(value: Any, depth: int = 0) -> Any:
    if depth > 8:
        raise OCIError("observation is too deeply nested")
    if value is None or isinstance(value, (bool, int, float, str)):
        if isinstance(value, str) and len(value.encode("utf-8")) > 16 * 1024:
            raise OCIError("observation string is too large")
        return value
    if isinstance(value, list):
        if len(value) > 128:
            raise OCIError("observation array is too large")
        return [_validate_scalar_tree(item, depth + 1) for item in value]
    if isinstance(value, Mapping):
        if len(value) > 128:
            raise OCIError("observation object is too large")
        result = {}
        for key, item in value.items():
            if not isinstance(key, str) or len(key) > 256:
                raise OCIError("observation keys must be bounded strings")
            result[key] = _validate_scalar_tree(item, depth + 1)
        return result
    raise OCIError("observation must be JSON structured data")


def _validate_literal_argv(argv: Any) -> list[str]:
    if not isinstance(argv, list) or not argv or len(argv) > 64:
        raise OCIError("run argv must be a bounded non-empty array")
    result: list[str] = []
    for part in argv:
        value = _bounded_string(part, "run argv element", 2048)
        if _SHELL_META.search(value) or value in {"-c", "--command", "-Command", "/c", "/C"}:
            raise OCIError("shell syntax and command interpreters are not allowed")
        if Path(value).name.lower() in _SHELL_NAMES:
            raise OCIError("shell interpreters are not allowed")
        result.append(value)
    return result


def _is_subset(expected: Any, actual: Any) -> bool:
    if isinstance(expected, Mapping):
        if not isinstance(actual, Mapping):
            return False
        return all(key in actual and _is_subset(value, actual[key]) for key, value in expected.items())
    if isinstance(expected, list):
        return expected == actual
    return expected == actual


def validate_generated_plan(plan: Mapping[str, Any], evidence_ids: set[str]) -> dict[str, Any]:
    if not isinstance(plan, Mapping):
        raise OCIError("generated validation plan must be an object")
    required = {"item_id", "files", "runs", "claim_observed"}
    if set(plan) != required:
        raise OCIError("generated validation plan has an unexpected or missing field")
    item_id = _bounded_string(plan["item_id"], "item_id", 512)
    if not isinstance(plan["claim_observed"], str) or len(plan["claim_observed"].encode("utf-8")) > 16 * 1024:
        raise OCIError("claim_observed must be bounded UTF-8 text")
    files = plan["files"]
    if not isinstance(files, list) or len(files) > 128:
        raise OCIError("files must be a bounded array")
    normalized_files: list[dict[str, Any]] = []
    seen_paths: set[str] = set()
    total = 0
    for file in files:
        if not isinstance(file, Mapping) or set(file) != {"path", "utf8"}:
            raise OCIError("generated files require only path and utf8")
        path = _safe_generated_path(file["path"])
        if path in seen_paths:
            raise OCIError("duplicate generated file path")
        seen_paths.add(path)
        content = file["utf8"]
        if not isinstance(content, str) or "\x00" in content:
            raise OCIError("generated file must be NUL-free UTF-8")
        data = content.encode("utf-8")
        if len(data) > MAX_GENERATED_FILE_BYTES:
            raise OCIError("generated file exceeds the size bound")
        if _FORBIDDEN_ASSERT.search(content):
            raise OCIError("assert-false/crash harnesses are not demonstrations")
        total += len(data)
        if total > MAX_GENERATED_TOTAL_BYTES:
            raise OCIError("generated files exceed the total size bound")
        normalized_files.append({"path": path, "utf8": content, "sha256": sha256_bytes(data), "bytes": len(data)})
    runs = plan["runs"]
    if not isinstance(runs, list) or len(runs) != MAX_RUNS:
        raise OCIError("exactly baseline and counterexample runs are required")
    normalized_runs: list[dict[str, Any]] = []
    seen_names: set[str] = set()
    seen_argv: set[str] = set()
    for run in runs:
        if not isinstance(run, Mapping) or set(run) != {"name", "argv", "cwd", "expect", "evidence_ids"}:
            raise OCIError("runs have an unsupported schema")
        name = run["name"]
        if name not in {"baseline", "counterexample"} or name in seen_names:
            raise OCIError("runs must contain one baseline and one counterexample")
        seen_names.add(name)
        if run["cwd"] != "/work":
            raise OCIError("runs must use the isolated /work cwd")
        argv = _validate_literal_argv(run["argv"])
        argv_key = canonical_json(argv)
        if argv_key in seen_argv:
            raise OCIError("baseline and counterexample argv must differ")
        seen_argv.add(argv_key)
        expect = run["expect"]
        if not isinstance(expect, Mapping) or set(expect) != {"exit", "observation"}:
            raise OCIError("run expectation must contain exit and structured observation")
        if isinstance(expect["exit"], bool) or not isinstance(expect["exit"], int):
            raise OCIError("expected exit must be an integer")
        observation = _validate_scalar_tree(expect["observation"])
        if not isinstance(observation, Mapping):
            raise OCIError("expected observation must be an object")
        refs = run["evidence_ids"]
        if not isinstance(refs, list) or any(not isinstance(ref, str) for ref in refs):
            raise OCIError("run evidence_ids must be a string array")
        if not set(refs).issubset(evidence_ids):
            raise OCIError("run cites undeclared evidence")
        normalized_runs.append(
            {
                "name": name,
                "argv": argv,
                "cwd": "/work",
                "expect": {"exit": expect["exit"], "observation": observation},
                "evidence_ids": list(dict.fromkeys(refs)),
            }
        )
    if seen_names != {"baseline", "counterexample"}:
        raise OCIError("both baseline and counterexample are required")
    return {"item_id": item_id, "files": normalized_files, "runs": normalized_runs, "claim_observed": plan["claim_observed"]}


def build_docker_run_argv(
    image: str,
    source_dir: Path,
    runner_path: Path,
    container_name: str,
    *,
    docker_executable: str = "docker",
    memory: str = DEFAULT_MEMORY,
    pids_limit: str = DEFAULT_PIDS_LIMIT,
    cpus: str = DEFAULT_CPUS,
) -> list[str]:
    validate_pinned_image(image)
    source = Path(source_dir).resolve()
    runner = Path(runner_path).resolve()
    if not source.is_dir() or not runner.is_file():
        raise OCIError("OCI source and fixed runner mounts must exist")
    _bounded_string(container_name, "container name", 128)
    return [
        docker_executable,
        "run",
        "--rm",
        "--init",
        "--pull=never",
        "--name",
        container_name,
        "--network",
        "none",
        "--read-only",
        "--tmpfs",
        "/work:rw,nosuid,nodev",
        "--tmpfs",
        "/tmp:rw,nosuid,nodev",
        "--mount",
        f"type=bind,src={source},dst=/source,readonly",
        "--mount",
        f"type=bind,src={runner},dst=/runner.py,readonly",
        "--user",
        "65532:65532",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        pids_limit,
        "--memory",
        memory,
        "--cpus",
        cpus,
        "--env",
        "HOME=/tmp/home",
        "--env",
        "PATH=/usr/local/bin:/usr/bin:/bin",
        image,
        "python3",
        "/runner.py",
    ]


def fixed_runner_source() -> str:
    """Return the only code allowed to write generated content in the OCI."""

    return textwrap.dedent(
        r'''
        import hashlib
        import json
        import os
        import subprocess
        import sys
        import time
        from pathlib import Path

        MAX_OUTPUT = 1024 * 1024
        plan = json.load(sys.stdin)
        work = Path("/work")
        for item in plan["files"]:
            path = Path(item["path"])
            if path.is_absolute() or ".." in path.parts or not str(path).startswith("generated/"):
                raise SystemExit("unsafe generated path")
            target = work / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(item["utf8"], encoding="utf-8", newline="")

        def run_one(spec):
            started = time.monotonic()
            proc = subprocess.Popen(
                spec["argv"], cwd="/work", stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, shell=False,
                env={"HOME": "/tmp/home", "PATH": "/usr/local/bin:/usr/bin:/bin"},
            )
            try:
                out, err = proc.communicate(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
                out, err = proc.communicate()
                return {"name": spec["name"], "exit": None, "timeout": True,
                        "stdout_sha256": hashlib.sha256(out[:MAX_OUTPUT]).hexdigest(),
                        "stderr_sha256": hashlib.sha256(err[:MAX_OUTPUT]).hexdigest(),
                        "observation": {"timeout": True}}
            out = out[:MAX_OUTPUT]
            err = err[:MAX_OUTPUT]
            try:
                parsed = json.loads(out.decode("utf-8"))
                observation = parsed if isinstance(parsed, dict) else {"json": parsed}
            except Exception:
                observation = {
                    "stdout_sha256": hashlib.sha256(out).hexdigest(),
                    "stderr_sha256": hashlib.sha256(err).hexdigest(),
                    "stdout": out.decode("utf-8", "replace")[:8192],
                    "stderr": err.decode("utf-8", "replace")[:8192],
                }
            return {"name": spec["name"], "exit": proc.returncode,
                    "timeout": False, "duration_ms": int((time.monotonic() - started) * 1000),
                    "stdout_sha256": hashlib.sha256(out).hexdigest(),
                    "stderr_sha256": hashlib.sha256(err).hexdigest(),
                    "observation": observation}

        results = [run_one(spec) for spec in plan["runs"]]
        print(json.dumps({"runs": results}, sort_keys=True, separators=(",", ":")))
        ''').lstrip()


def _write_fixed_runner(directory: Path) -> Path:
    path = directory / "fixed-runner.py"
    path.write_text(fixed_runner_source(), encoding="utf-8", newline="")
    path.chmod(0o444)
    return path


def _default_process_runner(
    argv: Sequence[str], payload: bytes, cwd: Path, timeout: float, max_output: int
) -> tuple[int, bytes, bytes, bool]:
    try:
        completed = subprocess.run(
            list(argv),
            input=payload,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(cwd),
            timeout=timeout,
            check=False,
            shell=False,
            env={"PATH": os.environ.get("PATH", "")},
        )
        return completed.returncode, completed.stdout[:max_output], completed.stderr[:max_output], False
    except subprocess.TimeoutExpired as exc:
        return -1, (exc.stdout or b"")[:max_output], (exc.stderr or b"")[:max_output], True
    except OSError as exc:
        return -1, b"", str(exc).encode("utf-8", "replace"), False


def _observation_matches(expected: Mapping[str, Any], actual: Any) -> bool:
    return isinstance(actual, Mapping) and _is_subset(expected, actual)


class OCIExecutor:
    def __init__(
        self,
        image: Optional[str] = None,
        *,
        docker_executable: str = "docker",
        timeout_seconds: float = 120,
        max_output_bytes: int = MAX_OUTPUT_BYTES,
        process_runner: Optional[Callable[[Sequence[str], bytes, Path, float, int], tuple[int, bytes, bytes, bool]]] = None,
        probe_daemon: bool = True,
    ) -> None:
        self.image = image or DEFAULT_IMAGE
        self.docker_executable = docker_executable
        self.timeout_seconds = timeout_seconds
        self.max_output_bytes = max_output_bytes
        self.process_runner = process_runner or _default_process_runner
        self.probe_daemon = probe_daemon

    def _cleanup_container(self, container_name: str) -> None:
        if self.process_runner is not _default_process_runner:
            return
        if shutil.which(self.docker_executable) is None:
            return
        try:
            subprocess.run(
                [self.docker_executable, "rm", "-f", container_name],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env={"PATH": os.environ.get("PATH", "")},
                shell=False,
                check=False,
                timeout=10,
            )
        except (OSError, subprocess.TimeoutExpired):
            pass

    def preflight(self) -> dict[str, Any]:
        try:
            image = validate_pinned_image(self.image)
        except OCIError as exc:
            return {"status": "environment_blocked", "reason": str(exc)}
        if self.process_runner is _default_process_runner and shutil.which(self.docker_executable) is None:
            return {"status": "environment_blocked", "reason": "docker_executable_not_found", "image": image}
        if not self.probe_daemon:
            return {"status": "ready", "image": image, "daemon_probe": "disabled"}
        code, stdout, stderr, timed_out = self.process_runner(
            [self.docker_executable, "version", "--format", "{{.Server.Version}}"],
            b"",
            Path(tempfile.gettempdir()),
            min(self.timeout_seconds, 10),
            64 * 1024,
        )
        if timed_out or code != 0:
            return {
                "status": "environment_blocked",
                "reason": "docker_daemon_unavailable",
                "image": image,
                "stderr_sha256": sha256_bytes(stderr),
            }
        return {"status": "ready", "image": image, "daemon_probe": "ok", "server_version": stdout.decode("utf-8", "replace").strip()[:128]}

    def execute(
        self,
        plan: Mapping[str, Any],
        source_dir: Path,
        *,
        evidence_ids: set[str],
        item_dir: Optional[Path] = None,
        operator_checks: Optional[Sequence[Mapping[str, Any]]] = None,
    ) -> dict[str, Any]:
        normalized = validate_generated_plan(plan, evidence_ids)
        preflight = self.preflight()
        if preflight.get("status") != "ready":
            return {
                "status": "environment_blocked",
                "classification": "environment_blocked",
                "preflight": preflight,
                "plan": normalized,
            }
        source_dir = Path(source_dir).resolve()
        if not source_dir.is_dir():
            raise OCIError("source directory does not exist")
        work_parent = Path(item_dir) if item_dir is not None else Path(tempfile.mkdtemp(prefix="openab-eval-oci-"))
        work_parent.mkdir(parents=True, exist_ok=True)
        runner_parent = Path(tempfile.mkdtemp(prefix="openab-eval-runner-"))
        container_name = "openab-eval-" + uuid.uuid4().hex[:20]
        try:
            runner_path = _write_fixed_runner(runner_parent)
            argv = build_docker_run_argv(
                self.image,
                source_dir,
                runner_path,
                container_name,
                docker_executable=self.docker_executable,
            )
            payload = canonical_json(normalized).encode("utf-8")
            code, stdout, stderr, timed_out = self.process_runner(
                argv, payload, work_parent, self.timeout_seconds, self.max_output_bytes
            )
            if timed_out:
                self._cleanup_container(container_name)
                return {"status": "unproven", "classification": "unproven", "reason": "container_timeout", "argv": argv}
            if code != 0:
                self._cleanup_container(container_name)
                # Docker daemon/image/permission failures are environment
                # blockers; a process that did start but failed is unproven.
                text = stderr.decode("utf-8", "replace").lower()
                blocked_markers = ("cannot connect to the docker daemon", "is the docker daemon running", "no such image", "permission denied")
                classification = "environment_blocked" if any(marker in text for marker in blocked_markers) else "unproven"
                return {
                    "status": classification,
                    "classification": classification,
                    "reason": "container_failed",
                    "stderr_sha256": sha256_bytes(stderr),
                    "argv": argv,
                }
            try:
                observed = json.loads(stdout.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                self._cleanup_container(container_name)
                return {"status": "unproven", "classification": "unproven", "reason": "invalid_runner_output", "argv": argv}
            actual_runs = observed.get("runs") if isinstance(observed, Mapping) else None
            if not isinstance(actual_runs, list) or len(actual_runs) != 2:
                return {"status": "unproven", "classification": "unproven", "reason": "incomplete_runner_output", "argv": argv}
            by_name = {item.get("name"): item for item in actual_runs if isinstance(item, Mapping)}
            if set(by_name) != {"baseline", "counterexample"}:
                return {"status": "unproven", "classification": "unproven", "reason": "missing_control_result", "argv": argv}
            checks: list[dict[str, Any]] = []
            for expected in normalized["runs"]:
                actual = by_name[expected["name"]]
                valid = (
                    actual.get("exit") == expected["expect"]["exit"]
                    and actual.get("exit") == 0
                    and not actual.get("timeout", False)
                    and _observation_matches(expected["expect"]["observation"], actual.get("observation"))
                )
                checks.append({"name": expected["name"], "valid": valid, "actual": dict(actual)})
            all_valid = all(check["valid"] for check in checks)
            distinct = canonical_json(by_name["baseline"].get("observation")) != canonical_json(by_name["counterexample"].get("observation"))
            if not all_valid or not distinct:
                classification = "unproven"
            else:
                baseline_obs = by_name["baseline"].get("observation")
                if isinstance(baseline_obs, Mapping) and baseline_obs.get("claim_present") is False:
                    classification = "executed_refuted"
                else:
                    classification = "executed_reproduced"
            result = {
                "status": "success" if classification.startswith("executed_") else "unproven",
                "classification": classification,
                "argv": argv,
                "preflight": preflight,
                "plan": normalized,
                "generated_file_digests": [{"path": item["path"], "sha256": item["sha256"], "bytes": item["bytes"]} for item in normalized["files"]],
                "runs": checks,
                "stdout_sha256": sha256_bytes(stdout),
                "stderr_sha256": sha256_bytes(stderr),
                "operator_checks": "not_run" if not operator_checks else "deferred_until_identical_validation",
            }
            if operator_checks and classification.startswith("executed_"):
                additional = []
                for index, check in enumerate(operator_checks):
                    check_dir = work_parent / f"operator-check-{index + 1}"
                    try:
                        additional.append(
                            self.execute(
                                check,
                                source_dir,
                                evidence_ids=evidence_ids,
                                item_dir=check_dir,
                                operator_checks=None,
                            )
                        )
                    except OCIError as exc:
                        additional.append({"status": "unproven", "classification": "unproven", "reason": type(exc).__name__})
                result["operator_checks"] = additional
            return result
        finally:
            # --rm is the normal cleanup.  The host work directory contains
            # only the fixed runner and is removed by its TemporaryDirectory
            # owner when the caller did not provide an audit directory.
            if item_dir is None:
                shutil.rmtree(work_parent, ignore_errors=True)
            shutil.rmtree(runner_parent, ignore_errors=True)


def materialize_source_tree(source_packet: Mapping[str, Any], destination: Path) -> None:
    """Materialize controller-owned source bytes for a read-only OCI mount."""

    destination = Path(destination)
    destination.mkdir(parents=True, exist_ok=True)
    for item in source_packet.get("files", []):
        if not isinstance(item, Mapping):
            raise OCIError("source packet file entry is invalid")
        path = _safe_source_path(item.get("path"))
        target = destination / path
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.exists() or target.is_symlink():
            raise OCIError("duplicate source packet path")
        if "utf8" in item:
            data = item["utf8"].encode("utf-8")
        elif "base64" in item:
            import base64

            data = base64.b64decode(item["base64"], validate=True)
        else:
            raise OCIError("source packet file has no bytes")
        if sha256_bytes(data) != item.get("sha256"):
            raise OCIError("source packet file digest mismatch")
        target.write_bytes(data)
        target.chmod(0o444)


def _safe_source_path(value: Any) -> str:
    path = _bounded_string(value, "source path", 1024)
    if "\\" in path or path.startswith("/"):
        raise OCIError("source path is unsafe")
    parsed = PurePosixPath(path)
    if ".." in parsed.parts or str(parsed) != path:
        raise OCIError("source path traversal")
    return path
