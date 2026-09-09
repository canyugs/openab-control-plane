#!/usr/bin/env python3
"""Validate and execute generated reproduction controls inside disposable OCI.

The controller validates model-produced plans but never materializes generated
files on the host.  A trusted fixed runner writes the plan inside a fresh,
non-root, networkless container.  The executor reports mechanical observations
only; semantic claim assessment belongs to the independent controller stages.
"""

from __future__ import annotations

import base64
import hashlib
import json
import math
import os
import re
import selectors
import shutil
import signal
import subprocess
import tempfile
import textwrap
import time
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
DEFAULT_TMPFS_SIZE = "64m"
DEFAULT_IMAGE = "python@sha256:b64631e04e4920160c50fbe8d8df828f7f35f06f425cb44aa09bca53e708a35a"


class OCIError(RuntimeError):
    """A controller validation or OCI execution error."""


class OCIEnvironmentBlocked(OCIError):
    """The requested isolated execution environment is unavailable."""


class _BoundedBytes(bytes):
    """Bytes retained up to a limit with metadata about the full observation."""

    def __new__(
        cls,
        value: bytes,
        *,
        overflow: bool = False,
        digest: Optional[str] = None,
        observed_bytes: Optional[int] = None,
    ) -> "_BoundedBytes":
        result = super().__new__(cls, value)
        result.overflow = bool(overflow)
        result.digest = digest or sha256_bytes(value)
        result.observed_bytes = len(value) if observed_bytes is None else int(observed_bytes)
        return result


_SHELL_NAMES = {
    "sh",
    "bash",
    "dash",
    "zsh",
    "fish",
    "csh",
    "tcsh",
    "ksh",
    "ash",
    "cmd",
    "cmd.exe",
    "powershell",
    "powershell.exe",
    "pwsh",
    "pwsh.exe",
}
_SHELL_META = re.compile(r"[;&|$`()<>{}\[\]\n\r]")
_SHELL_FLAGS = {"-c", "--command", "-Command", "/c", "/C", "-e", "--eval", "--exec"}
_FORBIDDEN_ASSERT = re.compile(r"\bassert\s+(?:false|False|FALSE)\b|AssertionError")
_SHA256 = re.compile(r"^[0-9a-f]{64}$")


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
    if not isinstance(value, str) or not value or "\x00" in value:
        raise OCIError(f"{name} must be a bounded non-empty string")
    try:
        size = len(value.encode("utf-8"))
    except UnicodeEncodeError as exc:
        raise OCIError(f"{name} must be valid UTF-8") from exc
    if size > maximum:
        raise OCIError(f"{name} exceeds the size bound")
    return value


def _bounded_text(value: Any, name: str, maximum: int = 4096) -> str:
    if not isinstance(value, str) or "\x00" in value:
        raise OCIError(f"{name} must be bounded UTF-8 text")
    try:
        size = len(value.encode("utf-8"))
    except UnicodeEncodeError as exc:
        raise OCIError(f"{name} must be valid UTF-8") from exc
    if size > maximum:
        raise OCIError(f"{name} exceeds the size bound")
    return value


def _utf8_bytes(value: Any, name: str) -> bytes:
    if not isinstance(value, str) or "\x00" in value:
        raise OCIError(f"{name} must be NUL-free UTF-8 text")
    try:
        return value.encode("utf-8")
    except UnicodeEncodeError as exc:
        raise OCIError(f"{name} must be valid UTF-8") from exc


def _safe_generated_path(value: Any) -> str:
    path = _bounded_string(value, "generated file path", 512)
    if "\\" in path or path.startswith("/"):
        raise OCIError("generated paths must be relative POSIX paths")
    parsed = PurePosixPath(path)
    if not path.startswith("generated/") or parsed.is_absolute() or ".." in parsed.parts:
        raise OCIError("generated paths must remain below generated/")
    if str(parsed) != path or path.endswith("/") or "." in parsed.parts:
        raise OCIError("generated path is not normalized")
    return path


def _validate_scalar_tree(value: Any, depth: int = 0) -> Any:
    if depth > 8:
        raise OCIError("observation is too deeply nested")
    if value is None or isinstance(value, (bool, int, str)):
        if isinstance(value, str) and len(_utf8_bytes(value, "observation string")) > 16 * 1024:
            raise OCIError("observation string is too large")
        return value
    if isinstance(value, float):
        if not math.isfinite(value):
            raise OCIError("observation numbers must be finite")
        return value
    if isinstance(value, list):
        if len(value) > 128:
            raise OCIError("observation array is too large")
        return [_validate_scalar_tree(item, depth + 1) for item in value]
    if isinstance(value, Mapping):
        if len(value) > 128:
            raise OCIError("observation object is too large")
        result: dict[str, Any] = {}
        for key, item in value.items():
            if not isinstance(key, str) or len(key) > 256 or "\x00" in key:
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
        if any(ord(character) < 32 for character in value) or _SHELL_META.search(value):
            raise OCIError("shell syntax is not allowed")
        if value in _SHELL_FLAGS or Path(value).name.lower() in _SHELL_NAMES:
            raise OCIError("shell interpreters and command strings are not allowed")
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
    claim_observed = _bounded_text(plan["claim_observed"], "claim_observed", 16 * 1024)

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
        data = _utf8_bytes(file["utf8"], "generated file")
        if len(data) > MAX_GENERATED_FILE_BYTES:
            raise OCIError("generated file exceeds the size bound")
        if _FORBIDDEN_ASSERT.search(file["utf8"]):
            raise OCIError("assert-false/crash harnesses are not demonstrations")
        total += len(data)
        if total > MAX_GENERATED_TOTAL_BYTES:
            raise OCIError("generated files exceed the total size bound")
        normalized_files.append(
            {"path": path, "utf8": file["utf8"], "sha256": sha256_bytes(data), "bytes": len(data)}
        )

    runs = plan["runs"]
    if not isinstance(runs, list) or len(runs) != MAX_RUNS:
        raise OCIError("exactly baseline and counterexample runs are required")
    normalized_runs: list[dict[str, Any]] = []
    seen_names: set[str] = set()
    seen_argv: set[str] = set()
    expected_claims: dict[str, bool] = {}
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
        if isinstance(expect["exit"], bool) or not isinstance(expect["exit"], int) or expect["exit"] != 0:
            raise OCIError("both control expectations must require exit 0")
        observation = _validate_scalar_tree(expect["observation"])
        if not isinstance(observation, Mapping) or not isinstance(observation.get("claim_present"), bool):
            raise OCIError("expected observation must contain a boolean claim_present")
        expected_claims[name] = observation["claim_present"]
        refs = run["evidence_ids"]
        if not isinstance(refs, list) or any(not isinstance(ref, str) or not ref for ref in refs):
            raise OCIError("run evidence_ids must be a string array")
        if not set(refs).issubset(evidence_ids):
            raise OCIError("run cites undeclared evidence")
        normalized_runs.append(
            {
                "name": name,
                "argv": argv,
                "cwd": "/work",
                "expect": {"exit": 0, "observation": observation},
                "evidence_ids": list(dict.fromkeys(refs)),
            }
        )
    if seen_names != {"baseline", "counterexample"}:
        raise OCIError("both baseline and counterexample are required")
    if expected_claims["baseline"] == expected_claims["counterexample"]:
        raise OCIError("baseline and counterexample claim_present expectations must differ")
    return {"item_id": item_id, "files": normalized_files, "runs": normalized_runs, "claim_observed": claim_observed}


def _safe_mount_path(path: Path, name: str) -> Path:
    path = Path(path)
    if not path.is_absolute() or path.is_symlink():
        raise OCIError(f"{name} mount must be an absolute non-symlink path")
    _reject_symlink_components(path)
    return path


def _reject_symlink_components(path: Path) -> None:
    path = Path(path)
    if not path.is_absolute():
        path = Path.cwd() / path
    existing = path
    while not existing.exists() and not existing.is_symlink() and existing.parent != existing:
        existing = existing.parent
    if existing.is_symlink():
        raise OCIError("path contains a symlink component")
    try:
        remainder = path.relative_to(existing).parts
    except ValueError as exc:
        raise OCIError("path has an invalid component") from exc
    current = existing
    for part in remainder:
        current /= part
        if current.is_symlink():
            raise OCIError("path contains a symlink component")


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
    source = _safe_mount_path(Path(source_dir), "OCI source")
    runner = _safe_mount_path(Path(runner_path), "fixed runner")
    if not source.is_dir() or not runner.is_file():
        raise OCIError("OCI source and fixed runner mounts must exist")
    _bounded_string(container_name, "container name", 128)
    _bounded_string(docker_executable, "docker executable", 4096)
    _bounded_string(memory, "memory limit", 64)
    _bounded_string(pids_limit, "pids limit", 64)
    _bounded_string(cpus, "CPU limit", 64)
    return [
        docker_executable,
        "run",
        "--rm",
        "--init",
        "--pull=never",
        "-i",
        "--name",
        container_name,
        "--network",
        "none",
        "--read-only",
        "--tmpfs",
        f"/work:rw,nosuid,nodev,size={DEFAULT_TMPFS_SIZE},mode=1777",
        "--tmpfs",
        f"/tmp:rw,nosuid,nodev,size={DEFAULT_TMPFS_SIZE},mode=1777",
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
        "--env",
        "LANG=C.UTF-8",
        "--env",
        "LC_ALL=C.UTF-8",
        image,
        "python3",
        "/runner.py",
    ]


def _kill_process_tree(process: subprocess.Popen[bytes]) -> None:
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
    except (OSError, ProcessLookupError):
        try:
            process.kill()
        except (OSError, ProcessLookupError):
            pass


def _bounded_process_bytes(
    value: bytes,
    maximum: int,
    *,
    overflow: Optional[bool] = None,
    digest: Optional[str] = None,
    observed_bytes: Optional[int] = None,
) -> _BoundedBytes:
    raw = bytes(value or b"")
    return _BoundedBytes(
        raw[:maximum],
        overflow=(len(raw) > maximum if overflow is None else overflow),
        digest=digest or sha256_bytes(raw),
        observed_bytes=len(raw) if observed_bytes is None else observed_bytes,
    )


def _default_process_runner(
    argv: Sequence[str], payload: bytes, cwd: Path, timeout: float, max_output: int
) -> tuple[int, bytes, bytes, bool]:
    """Run a boundary process with streaming, bounded output and tree cleanup."""

    maximum = max(1, int(max_output))
    environment = {
        "PATH": os.environ.get("PATH", ""),
        "LANG": "C",
        "LC_ALL": "C",
    }
    try:
        process = subprocess.Popen(
            list(argv),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=str(cwd),
            shell=False,
            env=environment,
            start_new_session=(os.name == "posix"),
        )
    except OSError as exc:
        error = str(exc).encode("utf-8", "replace")
        return -1, b"", _bounded_process_bytes(error, maximum), False

    try:
        if process.stdin is not None:
            try:
                process.stdin.write(bytes(payload))
                process.stdin.close()
            except (BrokenPipeError, OSError):
                try:
                    process.stdin.close()
                except OSError:
                    pass

        selector = selectors.DefaultSelector()
        streams: dict[int, tuple[str, Any]] = {}
        for name, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
            if stream is not None:
                selector.register(stream, selectors.EVENT_READ, name)
                streams[stream.fileno()] = (name, stream)

        buffers = {"stdout": bytearray(), "stderr": bytearray()}
        digests = {"stdout": hashlib.sha256(), "stderr": hashlib.sha256()}
        observed = {"stdout": 0, "stderr": 0}
        overflow = {"stdout": False, "stderr": False}
        timed_out = False
        killed = False
        drain_deadline: Optional[float] = None
        deadline = time.monotonic() + max(0.001, float(timeout))

        def terminate(reason: str) -> None:
            nonlocal killed, timed_out
            if reason == "timeout":
                timed_out = True
            if not killed:
                killed = True
                _kill_process_tree(process)

        while selector.get_map():
            now = time.monotonic()
            if not killed and now >= deadline:
                terminate("timeout")
                drain_deadline = now + 2.0
            if killed:
                remaining = max(0.0, (drain_deadline or now) - now)
                if remaining == 0.0:
                    for key in list(selector.get_map().values()):
                        selector.unregister(key.fileobj)
                    break
            else:
                remaining = max(0.0, deadline - now)
            events = selector.select(remaining)
            if not events:
                if not killed:
                    terminate("timeout")
                    drain_deadline = time.monotonic() + 2.0
                continue
            for key, _ in events:
                stream_name = key.data
                stream = key.fileobj
                try:
                    data = os.read(stream.fileno(), 64 * 1024)
                except (BlockingIOError, OSError):
                    data = b""
                if not data:
                    try:
                        selector.unregister(stream)
                    except KeyError:
                        pass
                    continue
                observed[stream_name] += len(data)
                digests[stream_name].update(data)
                available = maximum - len(buffers[stream_name])
                if available > 0:
                    buffers[stream_name].extend(data[:available])
                if observed[stream_name] > maximum:
                    overflow[stream_name] = True
                    if not killed:
                        terminate("output")
                        drain_deadline = time.monotonic() + 2.0

        try:
            process.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            _kill_process_tree(process)
            try:
                process.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                pass
        try:
            selector.close()
        except Exception:
            pass
        code = process.returncode if process.returncode is not None else -1
        stdout = _BoundedBytes(
            bytes(buffers["stdout"]),
            overflow=overflow["stdout"],
            digest=digests["stdout"].hexdigest(),
            observed_bytes=observed["stdout"],
        )
        stderr = _BoundedBytes(
            bytes(buffers["stderr"]),
            overflow=overflow["stderr"],
            digest=digests["stderr"].hexdigest(),
            observed_bytes=observed["stderr"],
        )
        return code, stdout, stderr, timed_out
    finally:
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None:
                try:
                    stream.close()
                except OSError:
                    pass


def fixed_runner_source() -> str:
    """Return the trusted runner; generated bytes enter only through stdin."""

    return textwrap.dedent(
        r'''
        import base64
        import hashlib
        import json
        import os
        import selectors
        import signal
        import subprocess
        import sys
        import time
        from pathlib import Path, PurePosixPath

        MAX_OUTPUT = 1024 * 1024
        RUN_TIMEOUT = 30.0
        plan = json.load(sys.stdin)
        work = Path("/work")
        Path("/tmp/home").mkdir(mode=0o700, exist_ok=True)

        def kill_tree(process):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (AttributeError, OSError, ProcessLookupError):
                try:
                    process.kill()
                except (OSError, ProcessLookupError):
                    pass

        def stream_process(process):
            selector = selectors.DefaultSelector()
            buffers = {"stdout": bytearray(), "stderr": bytearray()}
            digests = {"stdout": hashlib.sha256(), "stderr": hashlib.sha256()}
            sizes = {"stdout": 0, "stderr": 0}
            overflow = {"stdout": False, "stderr": False}
            for label, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
                selector.register(stream, selectors.EVENT_READ, label)
            killed = False
            timed_out = False
            deadline = time.monotonic() + RUN_TIMEOUT
            drain_deadline = None
            while selector.get_map():
                now = time.monotonic()
                if not killed and now >= deadline:
                    timed_out = True
                    killed = True
                    kill_tree(process)
                    drain_deadline = now + 2.0
                if killed:
                    remaining = max(0.0, (drain_deadline or now) - now)
                    if remaining == 0.0:
                        break
                else:
                    remaining = max(0.0, deadline - now)
                events = selector.select(remaining)
                if not events:
                    if not killed:
                        timed_out = True
                        killed = True
                        kill_tree(process)
                        drain_deadline = time.monotonic() + 2.0
                    continue
                for key, _ in events:
                    label = key.data
                    try:
                        data = os.read(key.fileobj.fileno(), 64 * 1024)
                    except (BlockingIOError, OSError):
                        data = b""
                    if not data:
                        try:
                            selector.unregister(key.fileobj)
                        except KeyError:
                            pass
                        continue
                    sizes[label] += len(data)
                    digests[label].update(data)
                    remaining_capacity = MAX_OUTPUT - len(buffers[label])
                    if remaining_capacity > 0:
                        buffers[label].extend(data[:remaining_capacity])
                    if sizes[label] > MAX_OUTPUT:
                        overflow[label] = True
                        if not killed:
                            killed = True
                            kill_tree(process)
                            drain_deadline = time.monotonic() + 2.0
            try:
                process.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                kill_tree(process)
                try:
                    process.wait(timeout=2.0)
                except subprocess.TimeoutExpired:
                    pass
            selector.close()
            return (
                bytes(buffers["stdout"]),
                bytes(buffers["stderr"]),
                digests["stdout"].hexdigest(),
                digests["stderr"].hexdigest(),
                sizes["stdout"],
                sizes["stderr"],
                overflow["stdout"] or overflow["stderr"],
                timed_out,
            )

        def run_one(spec):
            started = time.monotonic()
            try:
                process = subprocess.Popen(
                    spec["argv"],
                    cwd="/work",
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    shell=False,
                    env={"HOME": "/tmp/home", "PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"},
                    start_new_session=True,
                )
            except OSError as exc:
                error = str(exc).encode("utf-8", "replace")[:MAX_OUTPUT]
                return {
                    "name": spec["name"], "exit": None, "timeout": False, "output_limit": False,
                    "duration_ms": 0, "observation": {"runner_error": True},
                    "stdout": "", "stderr": error.decode("utf-8", "replace"),
                    "stdout_b64": "", "stderr_b64": base64.b64encode(error).decode("ascii"),
                    "stdout_sha256": hashlib.sha256(b"").hexdigest(),
                    "stderr_sha256": hashlib.sha256(error).hexdigest(),
                    "stdout_bytes": 0, "stderr_bytes": len(error),
                    "stdout_truncated": False, "stderr_truncated": False,
                }
            out, err, out_digest, err_digest, out_size, err_size, output_limit, timed_out = stream_process(process)
            if timed_out or output_limit:
                observation = {"claim_present": None, "timeout": timed_out, "output_limit": output_limit}
            else:
                try:
                    parsed = json.loads(out.decode("utf-8"))
                    observation = parsed if isinstance(parsed, dict) else {"json": parsed}
                except (UnicodeDecodeError, json.JSONDecodeError):
                    observation = {"structured_output": False}
            return {
                "name": spec["name"],
                "exit": None if timed_out or output_limit else process.returncode,
                "timeout": timed_out,
                "output_limit": output_limit,
                "duration_ms": int((time.monotonic() - started) * 1000),
                "observation": observation,
                "stdout": out.decode("utf-8", "replace"),
                "stderr": err.decode("utf-8", "replace"),
                "stdout_b64": base64.b64encode(out).decode("ascii"),
                "stderr_b64": base64.b64encode(err).decode("ascii"),
                "stdout_sha256": out_digest,
                "stderr_sha256": err_digest,
                "stdout_bytes": out_size,
                "stderr_bytes": err_size,
                "stdout_truncated": out_size > MAX_OUTPUT,
                "stderr_truncated": err_size > MAX_OUTPUT,
            }

        for item in plan["files"]:
            path = PurePosixPath(item["path"])
            if path.is_absolute() or ".." in path.parts or "." in path.parts or not str(path).startswith("generated/"):
                raise SystemExit("unsafe generated path")
            target = work / path
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.exists() or target.is_symlink():
                raise SystemExit("generated path collision")
            target.write_text(item["utf8"], encoding="utf-8", newline="")

        results = [run_one(spec) for spec in plan["runs"]]
        print(json.dumps({"runs": results}, ensure_ascii=False, sort_keys=True, separators=(",", ":")))
        ''').lstrip()


def _write_fixed_runner(directory: Path) -> Path:
    path = Path(directory) / "fixed-runner.py"
    path.write_text(fixed_runner_source(), encoding="utf-8", newline="")
    path.chmod(0o444)
    return path


def _is_default_runner(runner: Callable[..., Any]) -> bool:
    return runner is _default_process_runner


def _decode_capture(value: Any, maximum: int) -> tuple[bytes, bool, Optional[str], Optional[int]]:
    raw = bytes(value or b"") if not isinstance(value, str) else value.encode("utf-8", "replace")
    return (
        raw[:maximum],
        bool(getattr(value, "overflow", False)) or len(raw) > maximum,
        getattr(value, "digest", None) or sha256_bytes(raw),
        getattr(value, "observed_bytes", None) or len(raw),
    )


def _capture_fields(stdout: Any, stderr: Any, maximum: int, *, timed_out: bool = False) -> dict[str, Any]:
    out, out_overflow, out_digest, out_observed = _decode_capture(stdout, maximum)
    err, err_overflow, err_digest, err_observed = _decode_capture(stderr, maximum)
    return {
        "stdout": out.decode("utf-8", "replace"),
        "stderr": err.decode("utf-8", "replace"),
        "stdout_b64": base64.b64encode(out).decode("ascii"),
        "stderr_b64": base64.b64encode(err).decode("ascii"),
        "stdout_sha256": out_digest or sha256_bytes(out),
        "stderr_sha256": err_digest or sha256_bytes(err),
        "stdout_bytes": len(out) if out_observed is None else int(out_observed),
        "stderr_bytes": len(err) if err_observed is None else int(err_observed),
        "stdout_truncated": out_overflow,
        "stderr_truncated": err_overflow,
        "timeout": bool(timed_out),
    }


def _observation_matches(expected: Mapping[str, Any], actual: Any) -> bool:
    return isinstance(actual, Mapping) and _is_subset(expected, actual)


def _expected_run_record(expected: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "name": expected["name"],
        "expect": expected["expect"],
        "valid": False,
        "actual": None,
        "stdout": "",
        "stderr": "",
        "stdout_b64": "",
        "stderr_b64": "",
        "stdout_sha256": sha256_bytes(b""),
        "stderr_sha256": sha256_bytes(b""),
        "stdout_bytes": 0,
        "stderr_bytes": 0,
        "stdout_truncated": False,
        "stderr_truncated": False,
        "timeout": False,
        "output_limit": False,
    }


def _actual_bytes(value: Mapping[str, Any], key: str, maximum: int) -> tuple[bytes, bool]:
    encoded_key = f"{key}_b64"
    if isinstance(value.get(encoded_key), str):
        try:
            raw = base64.b64decode(value[encoded_key], validate=True)
        except (ValueError, TypeError):
            raise OCIError(f"runner {encoded_key} is invalid")
    elif isinstance(value.get(key), str):
        raw = value[key].encode("utf-8", "replace")
    else:
        raw = b""
    return raw[:maximum], len(raw) > maximum


def _normalize_actual(actual: Any, expected_name: str, maximum: int) -> dict[str, Any]:
    if not isinstance(actual, Mapping):
        raise OCIError("runner control result is not an object")
    if actual.get("name") != expected_name:
        raise OCIError("runner control result name does not match plan")
    exit_code = actual.get("exit")
    if isinstance(exit_code, bool) or (exit_code is not None and not isinstance(exit_code, int)):
        raise OCIError("runner control exit is invalid")
    observation = actual.get("observation")
    observation = _validate_scalar_tree(observation)
    if not isinstance(observation, Mapping) or not isinstance(observation.get("claim_present"), bool):
        raise OCIError("runner control observation lacks boolean claim_present")
    out, out_overflow = _actual_bytes(actual, "stdout", maximum)
    err, err_overflow = _actual_bytes(actual, "stderr", maximum)
    for key, raw in (("stdout_bytes", out), ("stderr_bytes", err)):
        observed_bytes = actual.get(key, len(raw))
        if isinstance(observed_bytes, bool) or not isinstance(observed_bytes, int) or observed_bytes < len(raw):
            raise OCIError(f"runner {key} is invalid")
    for key in ("stdout_sha256", "stderr_sha256"):
        if key in actual and (not isinstance(actual[key], str) or not _SHA256.fullmatch(actual[key])):
            raise OCIError(f"runner {key} is invalid")
    normalized = dict(actual)
    normalized["name"] = expected_name
    normalized["observation"] = observation
    normalized["stdout"] = out.decode("utf-8", "replace")
    normalized["stderr"] = err.decode("utf-8", "replace")
    normalized["stdout_b64"] = base64.b64encode(out).decode("ascii")
    normalized["stderr_b64"] = base64.b64encode(err).decode("ascii")
    normalized["stdout_sha256"] = (
        actual.get("stdout_sha256") if isinstance(actual.get("stdout_sha256"), str) else sha256_bytes(out)
    )
    normalized["stderr_sha256"] = (
        actual.get("stderr_sha256") if isinstance(actual.get("stderr_sha256"), str) else sha256_bytes(err)
    )
    normalized["stdout_bytes"] = actual.get("stdout_bytes", len(out))
    normalized["stderr_bytes"] = actual.get("stderr_bytes", len(err))
    normalized["stdout_truncated"] = bool(actual.get("stdout_truncated", False)) or out_overflow
    normalized["stderr_truncated"] = bool(actual.get("stderr_truncated", False)) or err_overflow
    normalized["timeout"] = bool(actual.get("timeout", False))
    normalized["output_limit"] = bool(actual.get("output_limit", False)) or out_overflow or err_overflow
    return normalized


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
        """Remove only the exact container owned by this executor attempt."""

        if not container_name.startswith("openab-eval-"):
            return
        if not _is_default_runner(self.process_runner):
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
        if _is_default_runner(self.process_runner) and shutil.which(self.docker_executable) is None:
            return {"status": "environment_blocked", "reason": "docker_executable_not_found", "image": image}
        if not self.probe_daemon:
            return {"status": "ready", "image": image, "daemon_probe": "disabled"}
        try:
            code, stdout, stderr, timed_out = self.process_runner(
                [self.docker_executable, "version", "--format", "{{.Server.Version}}"],
                b"",
                Path(tempfile.gettempdir()),
                min(self.timeout_seconds, 10),
                64 * 1024,
            )
        except Exception as exc:
            return {"status": "environment_blocked", "reason": "docker_probe_failed", "image": image, "error": type(exc).__name__}
        capture = _capture_fields(stdout, stderr, 64 * 1024, timed_out=timed_out)
        if timed_out or code != 0:
            return {
                "status": "environment_blocked",
                "reason": "docker_daemon_unavailable",
                "image": image,
                "stderr_sha256": capture["stderr_sha256"],
                "stderr": capture["stderr"],
            }
        return {
            "status": "ready",
            "image": image,
            "daemon_probe": "ok",
            "server_version": capture["stdout"].strip()[:128],
        }

    def _blocked_result(self, normalized: Mapping[str, Any], preflight: Mapping[str, Any]) -> dict[str, Any]:
        runs = [_expected_run_record(run) for run in normalized["runs"]]
        for run in runs:
            run["reason"] = preflight.get("reason", "environment_blocked")
        return {
            "status": "environment_blocked",
            "classification": "environment_blocked",
            "controls_passed": False,
            "claim_present": None,
            "reason": preflight.get("reason", "environment_blocked"),
            "preflight": dict(preflight),
            "plan": dict(normalized),
            "runs": runs,
            "operator_checks": "not_run",
        }

    def _control_payload(self, normalized: Mapping[str, Any], expected: Mapping[str, Any]) -> bytes:
        return canonical_json(
            {
                "item_id": normalized["item_id"],
                "files": normalized["files"],
                "runs": [expected],
                "claim_observed": normalized["claim_observed"],
            }
        ).encode("utf-8")

    def _run_one_control(
        self,
        normalized: Mapping[str, Any],
        expected: Mapping[str, Any],
        source_dir: Path,
        runner_path: Path,
        process_cwd: Path,
    ) -> dict[str, Any]:
        record = _expected_run_record(expected)
        container_name = "openab-eval-" + uuid.uuid4().hex[:20]
        try:
            argv = build_docker_run_argv(
                self.image,
                source_dir,
                runner_path,
                container_name,
                docker_executable=self.docker_executable,
            )
        except OCIError as exc:
            record["reason"] = type(exc).__name__
            return record

        record["container_name"] = container_name
        record["argv"] = argv
        payload = self._control_payload(normalized, expected)
        process_error: Optional[Exception] = None
        code = -1
        stdout: bytes = b""
        stderr: bytes = b""
        timed_out = False
        try:
            returned = self.process_runner(argv, payload, process_cwd, self.timeout_seconds, self.max_output_bytes)
            try:
                code, stdout, stderr, timed_out = returned
            except (TypeError, ValueError) as exc:
                raise OCIError("process runner returned an invalid result") from exc
        except Exception as exc:
            process_error = exc

        captures = _capture_fields(stdout, stderr, max(1, int(self.max_output_bytes)), timed_out=timed_out)
        record.update(captures)
        output_limit = captures["stdout_truncated"] or captures["stderr_truncated"]
        record["output_limit"] = output_limit
        record["timeout"] = bool(timed_out)

        succeeded = False
        try:
            if process_error is not None:
                record["reason"] = "process_runner_failed"
            elif timed_out:
                record["reason"] = "container_timeout"
            elif output_limit:
                record["reason"] = "output_limit"
            elif code != 0:
                lowered_stderr = record["stderr"].lower()
                if any(
                    marker in lowered_stderr
                    for marker in (
                        "cannot connect to the docker daemon",
                        "is the docker daemon running",
                        "no such image",
                        "permission denied",
                    )
                ):
                    record["reason"] = "environment_blocked"
                else:
                    record["reason"] = "container_failed"
            else:
                parsed = json.loads(bytes(stdout).decode("utf-8"))
                observed_runs = parsed.get("runs") if isinstance(parsed, Mapping) else None
                if not isinstance(observed_runs, list):
                    raise OCIError("runner output has no runs array")
                matching = [item for item in observed_runs if isinstance(item, Mapping) and item.get("name") == expected["name"]]
                if len(matching) != 1:
                    raise OCIError("runner output has no unique requested control")
                actual = _normalize_actual(matching[0], expected["name"], MAX_OUTPUT_BYTES)
                record["actual"] = actual
                record["valid"] = (
                    actual.get("exit") == expected["expect"]["exit"]
                    and actual.get("exit") == 0
                    and not actual.get("timeout", False)
                    and not actual.get("output_limit", False)
                    and not actual.get("stdout_truncated", False)
                    and not actual.get("stderr_truncated", False)
                    and _observation_matches(expected["expect"]["observation"], actual.get("observation"))
                    and actual["observation"].get("claim_present") == expected["expect"]["observation"]["claim_present"]
                )
                if not record["valid"]:
                    record["reason"] = "control_expectation_mismatch"
                else:
                    succeeded = True
        except (UnicodeDecodeError, json.JSONDecodeError, OCIError, TypeError, ValueError) as exc:
            record["reason"] = "invalid_runner_output" if not isinstance(exc, OCIError) else str(exc)
        finally:
            if not succeeded:
                self._cleanup_container(container_name)
        return record

    def _execute_normalized(
        self,
        normalized: Mapping[str, Any],
        source_dir: Path,
        preflight: Mapping[str, Any],
        runner_path: Path,
        process_cwd: Path,
    ) -> dict[str, Any]:
        records = [
            self._run_one_control(normalized, expected, source_dir, runner_path, process_cwd)
            for expected in normalized["runs"]
        ]
        baseline = next(record for record in records if record["name"] == "baseline")
        counterexample = next(record for record in records if record["name"] == "counterexample")
        baseline_claim = None
        for record in records:
            actual = record.get("actual")
            if record["name"] == "baseline" and isinstance(actual, Mapping):
                observation = actual.get("observation")
                if isinstance(observation, Mapping) and isinstance(observation.get("claim_present"), bool):
                    baseline_claim = observation["claim_present"]
        controls_passed = bool(
            baseline["valid"]
            and counterexample["valid"]
            and isinstance(baseline.get("actual"), Mapping)
            and isinstance(counterexample.get("actual"), Mapping)
            and baseline["actual"]["observation"].get("claim_present")
            != counterexample["actual"]["observation"].get("claim_present")
        )
        reasons = [record.get("reason") for record in records if record.get("reason")]
        reason = "controls_failed" if not controls_passed else None
        if any(record.get("reason") == "environment_blocked" for record in records):
            reason = "environment_blocked"
        for preferred in ("output_limit", "container_timeout", "environment_blocked", "container_failed", "process_runner_failed"):
            if preferred in reasons:
                reason = preferred
                break
        status = "environment_blocked" if reason == "environment_blocked" else ("success" if controls_passed else "unproven")
        return {
            "status": status,
            "classification": "environment_blocked" if status == "environment_blocked" else "unproven",
            "controls_passed": controls_passed,
            "claim_present": baseline_claim,
            "reason": reason,
            "preflight": dict(preflight),
            "plan": dict(normalized),
            "generated_file_digests": [
                {"path": item["path"], "sha256": item["sha256"], "bytes": item["bytes"]}
                for item in normalized["files"]
            ],
            "runs": records,
            "operator_checks": "not_run",
        }

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
        source_dir = Path(source_dir)
        if source_dir.is_symlink() or not source_dir.is_dir():
            raise OCIError("source directory must be an existing non-symlink directory")
        _reject_symlink_components(source_dir)
        self._reject_symlinks_under(source_dir)
        preflight = self.preflight()
        if preflight.get("status") != "ready":
            return self._blocked_result(normalized, preflight)

        runner_parent = Path(tempfile.mkdtemp(prefix="openab-eval-runner-"))
        process_cwd = Path(tempfile.mkdtemp(prefix="openab-eval-process-"))
        try:
            runner_path = _write_fixed_runner(runner_parent)
            result = self._execute_normalized(normalized, source_dir, preflight, runner_path, process_cwd)
            if operator_checks:
                if result["controls_passed"]:
                    operator_results = []
                    for check in operator_checks:
                        try:
                            check_normalized = validate_generated_plan(check, evidence_ids)
                            operator_results.append(
                                self._execute_normalized(check_normalized, source_dir, preflight, runner_path, process_cwd)
                            )
                        except OCIError as exc:
                            operator_results.append(
                                {
                                    "status": "unproven",
                                    "classification": "unproven",
                                    "controls_passed": False,
                                    "claim_present": None,
                                    "reason": type(exc).__name__,
                                }
                            )
                    result["operator_checks"] = operator_results
                else:
                    result["operator_checks"] = "deferred_until_identical_validation"
            return result
        finally:
            shutil.rmtree(runner_parent, ignore_errors=True)
            shutil.rmtree(process_cwd, ignore_errors=True)

    @staticmethod
    def _reject_symlinks_under(root: Path) -> None:
        for current, directories, files in os.walk(root, followlinks=False):
            for name in list(directories) + list(files):
                path = Path(current) / name
                if path.is_symlink():
                    raise OCIError("source directory contains a symlink")


def _safe_source_path(value: Any) -> str:
    path = _bounded_string(value, "source path", 1024)
    if "\\" in path or path.startswith("/"):
        raise OCIError("source path is unsafe")
    parsed = PurePosixPath(path)
    if ".." in parsed.parts or "." in parsed.parts or str(parsed) != path or path.endswith("/"):
        raise OCIError("source path traversal or normalization")
    return path


def _ensure_directory_without_symlinks(path: Path) -> None:
    path = Path(path)
    if path.is_symlink():
        raise OCIError("source destination is a symlink")
    try:
        path.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        raise OCIError("source destination is not a directory") from exc
    if path.is_symlink() or not path.is_dir():
        raise OCIError("source destination is not a directory")


def _ensure_parent_without_symlinks(root: Path, relative_path: str) -> Path:
    current = root
    parts = PurePosixPath(relative_path).parts[:-1]
    for part in parts:
        current = current / part
        if current.exists() or current.is_symlink():
            if current.is_symlink() or not current.is_dir():
                raise OCIError("source path contains a symlink or non-directory")
        else:
            current.mkdir()
    return current


def materialize_source_tree(source_packet: Mapping[str, Any], destination: Path) -> None:
    """Materialize verified controller-owned bytes without following links."""

    if not isinstance(source_packet, Mapping) or not isinstance(source_packet.get("files"), list):
        raise OCIError("source packet files are invalid")
    destination = Path(destination)
    _reject_symlink_components(destination)
    _ensure_directory_without_symlinks(destination)
    entries: list[tuple[str, bytes, str]] = []
    seen: set[str] = set()
    for item in source_packet["files"]:
        if not isinstance(item, Mapping):
            raise OCIError("source packet file entry is invalid")
        path = _safe_source_path(item.get("path"))
        if path in seen:
            raise OCIError("source packet path has a digest conflict")
        seen.add(path)
        has_utf8 = "utf8" in item
        has_base64 = "base64" in item
        if has_utf8 == has_base64:
            raise OCIError("source packet file must contain exactly one byte encoding")
        if has_utf8:
            data = _utf8_bytes(item["utf8"], "source packet UTF-8 bytes")
        else:
            if not isinstance(item["base64"], str):
                raise OCIError("source packet base64 bytes are invalid")
            try:
                data = base64.b64decode(item["base64"], validate=True)
            except (ValueError, TypeError) as exc:
                raise OCIError("source packet base64 bytes are invalid") from exc
        digest = item.get("sha256")
        if not isinstance(digest, str) or not _SHA256.fullmatch(digest) or sha256_bytes(data) != digest:
            raise OCIError("source packet file digest mismatch")
        entries.append((path, data, digest))

    for path, data, _ in entries:
        parent = _ensure_parent_without_symlinks(destination, path)
        target = parent / PurePosixPath(path).name
        if target.exists() or target.is_symlink():
            raise OCIError("source packet path already exists")
        try:
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
            nofollow = getattr(os, "O_NOFOLLOW", 0)
            descriptor = os.open(str(target), flags | nofollow, 0o444)
            with os.fdopen(descriptor, "wb") as handle:
                handle.write(data)
        except FileExistsError as exc:
            raise OCIError("source packet path already exists") from exc
        target.chmod(0o444)
