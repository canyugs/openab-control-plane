#!/usr/bin/env python3
"""Offline, immutable model evaluation for one frozen repository revision.

The command in the module is intentionally a controller, not a model prompt
wrapper.  It snapshots exact source/evidence bytes, constructs role-isolated
packets, validates every model reference against that snapshot, and delegates
generated execution to the OCI executor.  Model judgments are always recorded
as ``model_assessment`` and never become product verdict authority.
"""

from __future__ import annotations

import argparse
import ast
import base64
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any, Mapping, Optional, Sequence


def _load_sibling(filename: str, name: str):
    try:
        return __import__(name)
    except ImportError:
        path = Path(__file__).with_name(filename)
        spec = importlib.util.spec_from_file_location(name, path)
        if spec is None or spec.loader is None:
            raise
        module = importlib.util.module_from_spec(spec)
        sys.modules[name] = module
        spec.loader.exec_module(module)
        return module


adapters = _load_sibling("review_model_adapters.py", "review_model_adapters")
oci = _load_sibling("review_model_oci_executor.py", "review_model_oci_executor")


CONTROLLER_VERSION = "review-model-evaluation/v1"
FINDINGS_VERSION = "review-model-findings/v1"
EVIDENCE_VERSION = "review-model-evidence/v1"
MAX_INPUT_BYTES = 64 * 1024 * 1024
MAX_PACKET_BYTES = 8 * 1024 * 1024
MAX_FINDINGS = 10_000
MAX_EVIDENCE = 20_000
MAX_CITATIONS = 32
MAX_ARTIFACTS = 200_000
FULL_SHA = re.compile(r"^[0-9a-fA-F]{40}$")
HEX_SHA256 = re.compile(r"^[0-9a-f]{64}$")


class EvaluationError(RuntimeError):
    """A visible input, packet, model, or restart conflict."""


class EvaluationConflict(EvaluationError):
    """Existing immutable output bytes conflict with this run."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def pretty_json_bytes(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _bounded_string(value: Any, name: str, maximum: int = 4096, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value) or len(value.encode("utf-8")) > maximum:
        raise EvaluationError(f"{name} must be bounded UTF-8 text")
    if "\x00" in value:
        raise EvaluationError(f"{name} contains NUL")
    return value


def safe_relative_path(value: Any, name: str = "path") -> str:
    path = _bounded_string(value, name, 1024)
    if "\\" in path or path.startswith("/"):
        raise EvaluationError(f"{name} must be a relative POSIX path")
    parsed = PurePosixPath(path)
    if parsed.is_absolute() or ".." in parsed.parts or str(parsed) != path or path.endswith("/"):
        raise EvaluationError(f"{name} is unsafe")
    return path


def _json_no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise EvaluationError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def read_json(path: Path, *, maximum: int = MAX_INPUT_BYTES) -> tuple[Any, bytes]:
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise EvaluationError(f"input is not a regular file: {path}")
    raw = path.read_bytes()
    if len(raw) > maximum:
        raise EvaluationError(f"input exceeds the bound: {path}")
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_json_no_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise EvaluationError(f"invalid JSON input: {path}") from exc
    return value, raw


def write_immutable(path: Path, data: bytes) -> None:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        if path.is_symlink() or not path.is_file() or path.read_bytes() != data:
            raise EvaluationConflict(f"conflicting immutable artifact: {path}")
        return
    path.write_bytes(data)


def write_json_immutable(path: Path, value: Any) -> None:
    write_immutable(path, pretty_json_bytes(value))


def _under(path: Path, parent: Path) -> bool:
    try:
        path.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def _validate_output_boundary(output: Path, repo: Path, inputs: Sequence[Path]) -> None:
    output = output.resolve()
    repo = repo.resolve()
    if _under(output, repo) or _under(repo, output):
        raise EvaluationError("evaluation output must not overlap the repository")
    for item in inputs:
        resolved = item.resolve()
        if _under(output, resolved) or _under(resolved, output):
            raise EvaluationError("evaluation output must not overlap an input")


_REPOSITORY_EXECUTION_CONFIG = re.compile(
    r"^(?:"
    r"core\.(?:fsmonitor|fsmonitorhookpath|hookspath|pager|sshcommand|gitproxy)|"
    r"credential\.helper|"
    r"diff\.external|"
    r"interactive\.difffilter|"
    r"(?:diff\..+\.(?:command|textconv)|"
    r"filter\..+\.(?:clean|smudge|process|required)|"
    r"include(?:if)?\..+|"
    r"pager\..+|"
    r"remote\..+\.(?:uploadpack|receivepack)|"
    r"submodule\..+\.update|"
    r"tar\..+\.command)"
    r")$",
    re.IGNORECASE,
)


def _repository_config_keys(repo: Path) -> set[str]:
    env = {
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_ATTR_NOSYSTEM": "1",
        "LC_ALL": "C",
    }
    keys: set[str] = set()
    for scope in ("--local", "--worktree"):
        try:
            proc = subprocess.run(
                [
                    "git",
                    "--no-pager",
                    "--no-replace-objects",
                    "-C",
                    str(repo),
                    "config",
                    scope,
                    "--no-includes",
                    "--name-only",
                    "--null",
                    "--list",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=env,
                shell=False,
                check=False,
            )
        except OSError as exc:
            raise EvaluationError(f"git unavailable: {exc}") from exc
        if proc.returncode != 0 or len(proc.stdout) > 4 * 1024 * 1024:
            raise EvaluationError("repository Git configuration could not be inspected")
        try:
            keys.update(key.decode("ascii").lower() for key in proc.stdout.split(b"\0") if key)
        except UnicodeDecodeError as exc:
            raise EvaluationError("repository Git configuration could not be inspected") from exc
    return keys


def _validate_repository_git_config(repo: Path) -> None:
    if any(_REPOSITORY_EXECUTION_CONFIG.fullmatch(key) for key in _repository_config_keys(repo)):
        raise EvaluationError("repository contains executable Git configuration")


def _git(repo: Path, args: Sequence[str], *, max_output: int = MAX_INPUT_BYTES) -> bytes:
    _validate_repository_git_config(repo)
    env = {
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_ATTR_NOSYSTEM": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_TERMINAL_PROMPT": "0",
        "LC_ALL": "C",
    }
    command = [
        "git",
        "--no-pager",
        "--no-replace-objects",
        "-c",
        "core.fsmonitor=false",
        "-c",
        f"core.hooksPath={os.devnull}",
        "-c",
        "credential.helper=",
        "-c",
        "diff.external=",
        "-c",
        "submodule.recurse=false",
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "protocol.file.allow=never",
        "-C",
        str(repo),
    ]
    command_args = list(args)
    if command_args and command_args[0] == "diff":
        command_args[1:1] = ["--no-ext-diff", "--no-textconv"]
    command.extend(command_args)
    try:
        proc = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            shell=False,
            check=False,
        )
    except OSError as exc:
        raise EvaluationError(f"git unavailable: {exc}") from exc
    if proc.returncode != 0:
        raise EvaluationError(f"git operation failed: {args[0] if args else 'unknown'}")
    if len(proc.stdout) > max_output:
        raise EvaluationError("git output exceeds the bound")
    return proc.stdout


def _resolve_revision(repo: Path, requested: str) -> str:
    if not FULL_SHA.fullmatch(requested):
        raise EvaluationError("revision and base must be full 40-hex SHA values")
    resolved = _git(repo, ["rev-parse", "--verify", f"{requested}^{{commit}}"], max_output=128).decode().strip()
    if not FULL_SHA.fullmatch(resolved) or resolved.lower() != requested.lower():
        raise EvaluationError("revision did not resolve to the requested full SHA")
    return resolved.lower()


def _gitlink_paths(repo: Path, args: Sequence[str]) -> list[str]:
    raw = _git(repo, args, max_output=4 * 1024 * 1024)
    paths: list[str] = []
    for record in raw.split(b"\0"):
        if not record:
            continue
        try:
            meta, path_bytes = record.split(b"\t", 1)
            mode = meta.split(b" ", 1)[0].decode("ascii")
            path = path_bytes.decode("utf-8")
        except (ValueError, UnicodeDecodeError) as exc:
            raise EvaluationError("tracked tree contains an unreadable path") from exc
        if mode == "160000":
            paths.append(safe_relative_path(path, "submodule path"))
    return paths


def _validate_clean_repo(repo: Path) -> None:
    if not repo.is_dir():
        raise EvaluationError("repository path is not a directory")
    submodules = _gitlink_paths(repo, ["ls-files", "--stage", "-z"])
    if submodules:
        raise EvaluationError("repository contains submodule gitlinks")
    status = _git(repo, ["status", "--porcelain=v1", "--untracked-files=all"], max_output=4 * 1024 * 1024)
    if status:
        raise EvaluationError("repository must have a clean tracked and untracked tree")


def _archive_files(repo: Path, revision: str, limits: Mapping[str, int]) -> tuple[dict[str, Any], list[str], bytes, bytes]:
    # The base diff is supplied by the caller because a revision/base pair is
    # part of the frozen identity.  Keeping archive parsing here makes the
    # regular-file completeness check independent of the host checkout.
    expected_raw = _git(repo, ["ls-tree", "-r", "-z", "--full-tree", revision], max_output=MAX_INPUT_BYTES)
    expected: list[str] = []
    submodules: list[str] = []
    for record in expected_raw.split(b"\0"):
        if not record:
            continue
        try:
            meta, path_bytes = record.split(b"\t", 1)
            mode = meta.split(b" ", 1)[0].decode("ascii")
            path = path_bytes.decode("utf-8")
        except (ValueError, UnicodeDecodeError) as exc:
            raise EvaluationError("tracked tree contains an unreadable path") from exc
        if mode in {"100644", "100755"}:
            expected.append(safe_relative_path(path, "tracked path"))
        elif mode == "160000":
            submodules.append(safe_relative_path(path, "submodule path"))
    if submodules:
        raise EvaluationError("repository revision contains submodule gitlinks")
    archive_bytes = _git(repo, ["archive", "--format=tar", revision], max_output=MAX_INPUT_BYTES)
    expected.sort()
    max_files = int(limits.get("max_files", 10000))
    max_file = int(limits.get("max_file_bytes", 2 * 1024 * 1024))
    max_total = int(limits.get("max_total_bytes", 32 * 1024 * 1024))
    if max_files <= 0 or max_file <= 0 or max_total <= 0:
        raise EvaluationError("source limits must be positive")
    files: dict[str, Any] = {}
    omissions: list[str] = []
    total = 0
    members: dict[str, tarfile.TarInfo] = {}
    try:
        with tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:") as archive:
            for member in archive.getmembers():
                if member.isfile():
                    members[member.name] = member
            for index, path in enumerate(expected):
                if index >= max_files:
                    break
                member = members.get(path)
                if member is None:
                    omissions.append(f"missing:{path}")
                    continue
                if member.size > max_file or total + member.size > max_total:
                    omissions.append(f"over_limit:{path}")
                    continue
                extracted = archive.extractfile(member)
                if extracted is None:
                    omissions.append(f"unreadable:{path}")
                    continue
                data = extracted.read(max_file + 1)
                if len(data) != member.size or len(data) > max_file:
                    omissions.append(f"unreadable:{path}")
                    continue
                digest = sha256_bytes(data)
                try:
                    utf8 = data.decode("utf-8")
                    entry = {"path": path, "sha256": digest, "bytes": len(data), "utf8": utf8}
                except UnicodeDecodeError:
                    entry = {"path": path, "sha256": digest, "bytes": len(data), "base64": base64.b64encode(data).decode("ascii")}
                files[path] = entry
                total += len(data)
    except (tarfile.TarError, OSError) as exc:
        raise EvaluationError("cannot read frozen git archive") from exc
    if len(expected) > max_files:
        omissions.append(f"file_count>{max_files}")
    packet = {
        "schema_version": "model-source-packet/v1",
        "revision": revision,
        "files": [files[path] for path in sorted(files)],
        "tracked_regular_file_count": len(expected),
        "included_regular_file_count": len(files),
        "omissions": sorted(omissions),
        "complete": not omissions and len(expected) <= max_files,
        "archive_sha256": sha256_bytes(archive_bytes),
        "diff_sha256": None,
    }
    return packet, omissions, archive_bytes, b""


def build_source_packet(repo: Path, revision: str, base: str, limits: Mapping[str, int]) -> dict[str, Any]:
    packet, omissions, archive_bytes, _ = _archive_files(repo, revision, limits)
    diff_limit = int(limits.get("max_diff_bytes", 8 * 1024 * 1024))
    diff = _git(repo, ["diff", "--binary", base, revision], max_output=MAX_INPUT_BYTES)
    packet["diff_sha256"] = sha256_bytes(diff)
    if len(diff) > diff_limit:
        packet["diff"] = {"encoding": "omitted", "bytes": len(diff), "sha256": sha256_bytes(diff)}
        packet["complete"] = False
        packet["omissions"] = sorted([*omissions, "diff_over_limit"])
    else:
        try:
            packet["diff"] = {"encoding": "utf8", "bytes": len(diff), "sha256": sha256_bytes(diff), "utf8": diff.decode("utf-8")}
        except UnicodeDecodeError:
            packet["diff"] = {"encoding": "base64", "bytes": len(diff), "sha256": sha256_bytes(diff), "base64": base64.b64encode(diff).decode("ascii")}
    without_digest = dict(packet)
    packet["packet_sha256"] = sha256_bytes(canonical_json(without_digest).encode("utf-8"))
    return packet


def _validate_sha256(value: Any, name: str) -> str:
    if not isinstance(value, str) or not HEX_SHA256.fullmatch(value):
        raise EvaluationError(f"{name} is not a lowercase SHA-256")
    return value


def _line_count(entry: Mapping[str, Any]) -> int:
    if "utf8" not in entry:
        return 0
    return max(1, str(entry["utf8"]).count("\n") + (0 if str(entry["utf8"]).endswith("\n") else 1))


def load_findings(path: Path) -> tuple[list[dict[str, Any]], bytes]:
    value, raw = read_json(path)
    if isinstance(value, list):
        rows = value
    elif isinstance(value, Mapping) and value.get("schema_version", FINDINGS_VERSION) == FINDINGS_VERSION and isinstance(value.get("findings"), list):
        rows = value["findings"]
    else:
        raise EvaluationError("findings input must be model-review-findings/v1")
    if len(rows) > MAX_FINDINGS:
        raise EvaluationError("too many findings")
    result: list[dict[str, Any]] = []
    seen: set[str] = set()
    for row in rows:
        if not isinstance(row, Mapping):
            raise EvaluationError("finding must be an object")
        finding_id = row.get("finding_id", row.get("id"))
        finding_id = _bounded_string(finding_id, "finding_id", 512)
        if finding_id in seen:
            raise EvaluationError("duplicate finding_id")
        seen.add(finding_id)
        claim = _bounded_string(row.get("claim"), f"{finding_id}.claim", 16 * 1024)
        title = _bounded_string(row.get("title", finding_id), f"{finding_id}.title", 4096)
        severity = _bounded_string(row.get("severity", "unknown"), f"{finding_id}.severity", 128)
        location = row.get("location")
        if location is None:
            location = {key: row.get(key) for key in ("path", "start_line", "end_line")}
        if not isinstance(location, Mapping):
            raise EvaluationError(f"{finding_id}.location must be an object")
        path_value = safe_relative_path(location.get("path"), f"{finding_id}.location.path")
        start = location.get("start_line", location.get("start"))
        end = location.get("end_line", location.get("end"))
        if isinstance(start, bool) or not isinstance(start, int) or isinstance(end, bool) or not isinstance(end, int) or start < 1 or end < start:
            raise EvaluationError(f"{finding_id}.location has invalid lines")
        refs = row.get("evidence_ids", [])
        if not isinstance(refs, list) or not refs or any(not isinstance(ref, str) or not ref for ref in refs):
            raise EvaluationError(f"{finding_id}.evidence_ids must be a non-empty string array")
        result.append(
            {
                "finding_id": finding_id,
                "title": title,
                "severity": severity,
                "claim": claim,
                "location": {"path": path_value, "start_line": start, "end_line": end},
                "evidence_ids": list(dict.fromkeys(refs)),
                # The finding payload is untrusted input; any model-authored
                # identity field is intentionally not carried into role packets.
                "author_model_identity": "unknown",
            }
        )
    return result, raw


def load_evidence(directory: Path) -> tuple[dict[str, dict[str, Any]], bytes]:
    directory = Path(directory)
    if not directory.is_dir():
        raise EvaluationError("evidence path must be a directory")
    manifest_path = directory / "manifest.json"
    if not manifest_path.is_file():
        raise EvaluationError("evidence/manifest.json is required")
    manifest, raw = read_json(manifest_path)
    if not isinstance(manifest, Mapping) or manifest.get("schema_version", EVIDENCE_VERSION) != EVIDENCE_VERSION:
        raise EvaluationError("evidence manifest has an unsupported schema version")
    rows = manifest.get("entries", manifest.get("evidence"))
    if not isinstance(rows, list) or len(rows) > MAX_EVIDENCE:
        raise EvaluationError("evidence manifest entries are required")
    result: dict[str, dict[str, Any]] = {}
    for row in rows:
        if not isinstance(row, Mapping):
            raise EvaluationError("evidence entry must be an object")
        evidence_id = _bounded_string(row.get("evidence_id", row.get("id")), "evidence_id", 512)
        if evidence_id in result:
            raise EvaluationError("duplicate evidence_id")
        if "utf8" in row:
            text = row["utf8"]
            if not isinstance(text, str) or "\x00" in text:
                raise EvaluationError("evidence utf8 must be NUL-free text")
            data = text.encode("utf-8")
        elif "file" in row:
            relative = safe_relative_path(row["file"], "evidence file")
            file_path = (directory / relative).resolve()
            if not _under(file_path, directory.resolve()) or file_path.is_symlink() or not file_path.is_file():
                raise EvaluationError("evidence file is outside the evidence directory")
            data = file_path.read_bytes()
            try:
                text = data.decode("utf-8")
            except UnicodeDecodeError as exc:
                raise EvaluationError("evidence bytes must be UTF-8") from exc
        else:
            raise EvaluationError("evidence entry needs utf8 or file")
        if len(data) > 2 * 1024 * 1024:
            raise EvaluationError("evidence entry exceeds the size bound")
        digest = _validate_sha256(row.get("sha256"), f"evidence {evidence_id}.sha256")
        if sha256_bytes(data) != digest:
            raise EvaluationError(f"evidence digest mismatch for {evidence_id}")
        ranges = row.get("allowed_ranges", [])
        if not isinstance(ranges, list):
            raise EvaluationError("evidence allowed_ranges must be an array")
        normalized_ranges = []
        for allowed in ranges:
            if not isinstance(allowed, Mapping):
                raise EvaluationError("evidence range must be an object")
            allowed_path = safe_relative_path(allowed.get("path"), "evidence allowed path")
            start = allowed.get("start", allowed.get("start_line"))
            end = allowed.get("end", allowed.get("end_line"))
            if isinstance(start, bool) or not isinstance(start, int) or isinstance(end, bool) or not isinstance(end, int) or start < 1 or end < start:
                raise EvaluationError("evidence allowed range has invalid lines")
            normalized_ranges.append({"path": allowed_path, "start": start, "end": end})
        result[evidence_id] = {"evidence_id": evidence_id, "utf8": text, "sha256": digest, "allowed_ranges": normalized_ranges}
    return result, raw


def _citation_allowed(citation: Mapping[str, Any], finding: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]], source_lines: Mapping[str, int]) -> dict[str, Any]:
    if not isinstance(citation, Mapping) or set(citation) != {"path", "start", "end", "evidence_id"}:
        raise EvaluationError("citation schema is invalid")
    path = safe_relative_path(citation["path"], "citation.path")
    start, end = citation["start"], citation["end"]
    if isinstance(start, bool) or not isinstance(start, int) or isinstance(end, bool) or not isinstance(end, int) or start < 1 or end < start:
        raise EvaluationError("citation line range is invalid")
    if path not in source_lines or source_lines[path] == 0 or end > source_lines[path]:
        raise EvaluationError("citation is outside the frozen source packet")
    evidence_id = citation["evidence_id"]
    if evidence_id not in set(finding.get("evidence_ids", [])) or evidence_id not in evidence:
        raise EvaluationError("citation uses undeclared finding evidence")
    allowed = evidence[evidence_id].get("allowed_ranges", [])
    if not any(item["path"] == path and start >= item["start"] and end <= item["end"] for item in allowed):
        raise EvaluationError("citation is outside the evidence allowed range")
    return {"path": path, "start": start, "end": end, "evidence_id": evidence_id}


def validate_judge_result(result: Mapping[str, Any], finding: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]], source_lines: Mapping[str, int]) -> dict[str, Any]:
    if not isinstance(result, Mapping):
        raise EvaluationError("judge result must be an object")
    required = {"finding_id", "verdict", "severity", "usefulness", "citations", "counterexample", "validation_verdict"}
    if set(result) != required:
        raise EvaluationError("judge result schema is invalid")
    if result["finding_id"] != finding["finding_id"]:
        raise EvaluationError("judge returned the wrong finding id")
    if result["verdict"] not in {"support", "refute", "insufficient_evidence"}:
        raise EvaluationError("judge verdict is invalid")
    severity = _bounded_string(result["severity"], "judge severity", 128)
    if result["usefulness"] not in {"useful", "not_useful", "unknown"}:
        raise EvaluationError("judge usefulness is invalid")
    if result["validation_verdict"] not in {"valid", "invalid", "unproven"}:
        raise EvaluationError("judge validation verdict is invalid")
    citations = result["citations"]
    if not isinstance(citations, list) or len(citations) > MAX_CITATIONS:
        raise EvaluationError("judge citations are invalid")
    if result["verdict"] != "insufficient_evidence" and not citations:
        raise EvaluationError("support/refute assessments require at least one citation")
    normalized = [_citation_allowed(citation, finding, evidence, source_lines) for citation in citations]
    counterexample = result["counterexample"]
    if not isinstance(counterexample, str) or len(counterexample.encode("utf-8")) > 16 * 1024:
        raise EvaluationError("judge counterexample is invalid")
    return {
        "finding_id": finding["finding_id"],
        "verdict": result["verdict"],
        "severity": severity,
        "usefulness": result["usefulness"],
        "citations": normalized,
        "counterexample": counterexample,
        "validation_verdict": result["validation_verdict"],
        "provenance": "model_assessment",
    }


def build_discovery_packet(
    invocation_id: str,
    source_packet: Mapping[str, Any],
    evidence_catalog: Mapping[str, Mapping[str, Any]],
    original_findings: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    """Build a blind packet without serializing any original finding data."""

    del original_findings
    packet = {
        "invocation_id": _bounded_string(invocation_id, "invocation_id", 256),
        "role": "discovery",
        "source_packet": source_packet,
        "evidence_catalog": {key: evidence_catalog[key] for key in sorted(evidence_catalog)},
        "allowed_references": "source packet paths and evidence manifest only",
        "author_model_identity": "withheld",
        "schema": DISCOVERY_SCHEMA,
        "rules": [
            "find new, concrete, source-cited issues",
            "each candidate is exactly {claim,path,start,end,evidence_ids}; use a supplied relative POSIX source path and inclusive positive range",
            "each evidence_id must be supplied; at least one declared evidence range must cover the candidate range, and additional evidence may cover related contract or call-path material",
            "do not infer recall or confirm any original finding, and return no finding identifier",
            "report only actionable defects directly proven by supplied source under a supplied contract or actual call paths",
            "do not report style, documentation, type, or other convention assumptions, or speculate about missing callers or unshown behavior",
            "if no concrete defect is proven, return an empty candidates array",
            "return JSON only; no citation strings, generated-file references, or /source paths",
        ],
    }
    # A null marker is safe and makes the blindness auditable without leaking
    # a candidate's original input.  Do not put the discarded arguments in it.
    return packet


def build_judge_packet(
    invocation_id: str,
    source_packet: Mapping[str, Any],
    finding: Mapping[str, Any],
    evidence: Mapping[str, Mapping[str, Any]],
    role: str,
    validation_evidence: Optional[Mapping[str, Any]] = None,
) -> dict[str, Any]:
    refs = {key: evidence[key] for key in finding["evidence_ids"] if key in evidence}
    if len(refs) != len(set(finding["evidence_ids"])):
        raise EvaluationError("finding references missing evidence")
    packet = {
        "invocation_id": invocation_id,
        "role": role,
        "source_packet": source_packet,
        "finding": finding,
        "evidence": refs,
        "schema": JUDGE_SCHEMA,
        "rules": [
            "return exactly the schema fields; finding_id must match the supplied finding",
            "each citation is exactly {path,start,end,evidence_id} and cites only a supplied frozen-source range covered by that evidence_id",
            "support/refute assessments require at least one citation; insufficient_evidence may return an empty citations array",
            "never cite generated files or /source paths, and never emit citation strings or quote/note fields",
            "assess whether executed baseline and counterexample controls meaningfully distinguish the claim from its negation",
            "do not execute commands, use tools, or rely on a manual test prerequisite",
            "return JSON only",
        ],
    }
    if validation_evidence is not None:
        packet["validation"] = validation_evidence
    return packet


def build_synthesis_packet(
    invocation_id: str,
    source_packet: Mapping[str, Any],
    finding: Mapping[str, Any],
    evidence: Mapping[str, Mapping[str, Any]],
    executed_evidence: Mapping[str, Any],
    judgments: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    if len(judgments) != 2:
        raise EvaluationError("synthesis requires two valid judge assessments")
    anonymous = []
    for index, judgment in enumerate(judgments, 1):
        anonymous.append({"label": f"judge_{index}", "assessment": judgment})
    return {
        "invocation_id": invocation_id,
        "role": "synthesis",
        "source_packet": source_packet,
        "finding": finding,
        "evidence": {key: evidence[key] for key in finding["evidence_ids"]},
        "executed_evidence": executed_evidence,
        "judgments": anonymous,
        "schema": SYNTHESIS_SCHEMA,
        "rules": [
            "return exactly the schema fields and preserve the supplied item_id",
            "preserve disagreement; unknown is allowed; do not invent evidence or promote failed controls",
            "each citation is exactly {path,start,end,evidence_id} and cites only a supplied frozen-source range covered by that evidence_id",
            "supported/refuted results require at least one citation; unknown may return an empty citations array",
            "never cite generated files or /source paths, and return no citation strings",
        ],
    }


def build_validation_packet(invocation_id: str, finding: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]], source_packet: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "invocation_id": invocation_id,
        "role": "validation",
        "source_packet": source_packet,
        "finding": finding,
        "evidence": {key: evidence[key] for key in finding["evidence_ids"]},
        "source_references": {"revision": source_packet.get("revision"), "paths": [finding["location"]["path"]]},
        "schema": VALIDATION_SCHEMA,
        "rules": [
            "return exactly {item_id,files,runs,claim_observed}; item_id must match the supplied finding",
            "generated files require exactly path and utf8, with paths below generated/; they materialize below /work/generated",
            "use exactly one baseline and one counterexample with distinct literal argv arrays and cwd /work",
            "invoke python3 directly; the Python3 OCI image exposes read-only source at /source",
            "each generated control must read or import the supplied source and print actual JSON stdout with boolean claim_present",
            "expect exactly exit 0 and opposing claim_present values; cite only supplied evidence IDs",
            "no shell, -c, host paths, crash/assertion-false harnesses, or manual test prerequisite",
            "the decoded utf8 field is raw source text with actual newline characters; JSON escaping is applied once by the transport",
            "do not provide doubly escaped newline sequences or unescape or rewrite model-generated bytes; malformed or doubly escaped source remains unproven",
            "return JSON only",
        ],
    }


def _validate_discovery_result(result: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]], source_lines: Mapping[str, int]) -> list[dict[str, Any]]:
    if not isinstance(result, Mapping) or set(result) != {"candidates"} or not isinstance(result["candidates"], list):
        raise EvaluationError("discovery result schema is invalid")
    candidates = []
    for candidate in result["candidates"]:
        if not isinstance(candidate, Mapping) or set(candidate) != {"claim", "path", "start", "end", "evidence_ids"}:
            raise EvaluationError("discovery candidate schema is invalid")
        claim = _bounded_string(candidate["claim"], "candidate claim", 16 * 1024)
        path = safe_relative_path(candidate["path"], "candidate path")
        start, end = candidate["start"], candidate["end"]
        if path not in source_lines or source_lines[path] == 0 or not isinstance(start, int) or not isinstance(end, int) or start < 1 or end < start or end > source_lines[path]:
            raise EvaluationError("candidate source range is invalid")
        refs = candidate["evidence_ids"]
        if not isinstance(refs, list) or not refs or any(not isinstance(ref, str) or not ref or ref not in evidence for ref in refs):
            raise EvaluationError("candidate evidence reference is invalid")
        if not any(
            any(item["path"] == path and start >= item["start"] and end <= item["end"] for item in evidence[ref].get("allowed_ranges", []))
            for ref in refs
        ):
            raise EvaluationError("candidate evidence range is invalid")
        candidates.append({"claim": claim, "path": path, "start": start, "end": end, "evidence_ids": list(dict.fromkeys(refs))})
    return candidates


def _validate_synthesis_result(result: Mapping[str, Any], item_id: str, finding: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]], source_lines: Mapping[str, int]) -> dict[str, Any]:
    if not isinstance(result, Mapping) or set(result) != {"item_id", "verdict", "citations", "disagreement", "reason"}:
        raise EvaluationError("synthesis result schema is invalid")
    if result["item_id"] != item_id or result["verdict"] not in {"supported", "refuted", "unknown"}:
        raise EvaluationError("synthesis item or verdict is invalid")
    citations = result["citations"]
    if not isinstance(citations, list) or len(citations) > MAX_CITATIONS:
        raise EvaluationError("synthesis citations are invalid")
    if result["verdict"] != "unknown" and not citations:
        raise EvaluationError("supported/refuted synthesis requires at least one citation")
    normalized = [_citation_allowed(item, finding, evidence, source_lines) for item in citations]
    disagreement = _bounded_string(result["disagreement"], "synthesis disagreement", 16 * 1024, allow_empty=True)
    reason = _bounded_string(result["reason"], "synthesis reason", 16 * 1024, allow_empty=True)
    return {"item_id": item_id, "verdict": result["verdict"], "citations": normalized, "disagreement": disagreement, "reason": reason, "provenance": "model_assessment"}


def _python_source_binding_signal(content: str, source_path: str) -> bool:
    """Find a structural source-read signal, never prove semantics by text."""

    try:
        tree = ast.parse(content)
    except SyntaxError:
        return False

    source_relative = PurePosixPath(source_path)
    cited_source_literals = {source_path, str(PurePosixPath("/source") / source_relative)}
    source_mount_literals = {"/source", str(PurePosixPath("/source") / source_relative)}
    module_parts = list(source_relative.parts)
    if module_parts and module_parts[-1].endswith(".py"):
        module_parts[-1] = module_parts[-1][:-3]
    source_module = ".".join(module_parts)
    source_modules = {source_module}
    if source_modules and source_modules != {"__init__"} and source_module.endswith(".__init__"):
        source_modules.add(source_module[: -len(".__init__")])

    def target_names(target: ast.AST) -> set[str]:
        if isinstance(target, ast.Name):
            return {target.id}
        if isinstance(target, (ast.List, ast.Tuple)):
            names: set[str] = set()
            for element in target.elts:
                names.update(target_names(element))
            return names
        return set()

    def is_source_literal(node: ast.AST) -> bool:
        return isinstance(node, ast.Constant) and isinstance(node.value, str) and node.value in cited_source_literals

    source_bound_names: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Assign) and is_source_literal(node.value):
            for target in node.targets:
                source_bound_names.update(target_names(target))
        elif isinstance(node, ast.AnnAssign) and node.value is not None and is_source_literal(node.value):
            source_bound_names.update(target_names(node.target))
        elif isinstance(node, ast.NamedExpr) and is_source_literal(node.value):
            source_bound_names.update(target_names(node.target))

    def source_reference(node: ast.AST) -> bool:
        return any(
            (isinstance(child, ast.Constant) and isinstance(child.value, str) and child.value in cited_source_literals)
            or (isinstance(child, ast.Name) and child.id in source_bound_names)
            for child in ast.walk(node)
        )

    def literal_contains_source_mount(node: ast.AST) -> bool:
        return any(
            isinstance(child, ast.Constant)
            and isinstance(child.value, str)
            and child.value in source_mount_literals
            for child in ast.walk(node)
        )

    source_path_setup = any(
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr in {"insert", "append", "extend"}
        and isinstance(node.func.value, ast.Attribute)
        and isinstance(node.func.value.value, ast.Name)
        and node.func.value.value.id == "sys"
        and node.func.value.attr == "path"
        and literal_contains_source_mount(node)
        for node in ast.walk(tree)
    )
    source_imported = any(
        isinstance(node, ast.Import)
        and any(alias.name in source_modules for alias in node.names)
        or isinstance(node, ast.ImportFrom)
        and node.level == 0
        and node.module in source_modules
        for node in ast.walk(tree)
    )
    if source_path_setup and source_imported:
        return True

    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        function = node.func
        if isinstance(function, ast.Name) and function.id in {"open", "read_file", "read_source"}:
            if source_reference(node):
                return True
        if isinstance(function, ast.Attribute) and function.attr in {"read_text", "read_bytes", "readline", "readlines"}:
            if source_reference(function.value) or source_reference(node):
                return True
        if (
            isinstance(function, ast.Attribute)
            and function.attr == "spec_from_file_location"
            and isinstance(function.value, ast.Attribute)
            and function.value.attr == "util"
            and isinstance(function.value.value, ast.Name)
            and function.value.value.id == "importlib"
        ):
            location = node.args[1] if len(node.args) > 1 else next(
                (keyword.value for keyword in node.keywords if keyword.arg == "location"),
                None,
            )
            if location is not None and source_reference(location):
                return True
    return False


def _plan_source_binding_signal(plan: Mapping[str, Any], finding: Mapping[str, Any]) -> bool:
    location = finding.get("location", {})
    source_path = location.get("path") if isinstance(location, Mapping) else ""
    if not isinstance(source_path, str):
        return False
    return any(
        isinstance(item, Mapping)
        and isinstance(item.get("utf8"), str)
        and _python_source_binding_signal(item["utf8"], source_path)
        for item in plan.get("files", [])
    )


def _plan_mentions_frozen_source(plan: Mapping[str, Any], finding: Mapping[str, Any]) -> bool:
    """Compatibility alias for the structural, non-semantic binding signal."""

    return _plan_source_binding_signal(plan, finding)


_SOURCE_PATH_PATTERN = r"^(?!/)(?!.*\\)(?!.*//)(?!.*(?:^|/)(?:\.|\.\.)(?:/|$))(?!.*\/$).+$"


JUDGE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["finding_id", "verdict", "severity", "usefulness", "citations", "counterexample", "validation_verdict"],
    "properties": {
        "finding_id": {
            "type": "string",
            "minLength": 1,
            "maxLength": 512,
            "description": "Return the supplied finding_id exactly; do not invent or rename it.",
        },
        "verdict": {"type": "string", "enum": ["support", "refute", "insufficient_evidence"]},
        "severity": {"type": "string", "minLength": 1, "maxLength": 128},
        "usefulness": {"type": "string", "enum": ["useful", "not_useful", "unknown"]},
        "citations": {
            "type": "array",
            "maxItems": MAX_CITATIONS,
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "start", "end", "evidence_id"],
                "properties": {
                    "path": {"type": "string", "minLength": 1, "maxLength": 1024, "pattern": _SOURCE_PATH_PATTERN},
                    "start": {"type": "integer", "minimum": 1},
                    "end": {"type": "integer", "minimum": 1},
                    "evidence_id": {"type": "string", "minLength": 1, "maxLength": 512},
                },
            },
            "description": "Each citation must use an exact supplied source path/range and supplied evidence_id; never cite generated files or /source paths. The transport may return an empty array for insufficient_evidence; the controller requires at least one citation for support or refute.",
        },
        "counterexample": {"type": "string", "maxLength": 16 * 1024},
        "validation_verdict": {"type": "string", "enum": ["valid", "invalid", "unproven"]},
    },
}
DISCOVERY_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["candidates"],
    "properties": {
        "candidates": {
            "type": "array",
            "maxItems": 128,
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["claim", "path", "start", "end", "evidence_ids"],
                "properties": {
                    "claim": {"type": "string", "minLength": 1, "maxLength": 16 * 1024},
                    "path": {"type": "string", "minLength": 1, "maxLength": 1024, "pattern": _SOURCE_PATH_PATTERN},
                    "start": {"type": "integer", "minimum": 1},
                    "end": {"type": "integer", "minimum": 1},
                    "evidence_ids": {
                        "type": "array",
                        "minItems": 1,
                        "items": {"type": "string", "minLength": 1, "maxLength": 512},
                    },
                },
            },
            "description": "Return only new source-cited candidates. Each candidate path/range must be in the frozen source and each evidence_id must be supplied.",
        }
    },
}
SYNTHESIS_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["item_id", "verdict", "citations", "disagreement", "reason"],
    "properties": {
        "item_id": {"type": "string", "minLength": 1, "maxLength": 512},
        "verdict": {"type": "string", "enum": ["supported", "refuted", "unknown"]},
        "citations": {
            "type": "array",
            "maxItems": MAX_CITATIONS,
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "start", "end", "evidence_id"],
                "properties": {
                    "path": {"type": "string", "minLength": 1, "maxLength": 1024, "pattern": _SOURCE_PATH_PATTERN},
                    "start": {"type": "integer", "minimum": 1},
                    "end": {"type": "integer", "minimum": 1},
                    "evidence_id": {"type": "string", "minLength": 1, "maxLength": 512},
                },
            },
            "description": "Cite only exact supplied source paths/ranges and supplied evidence_id values; generated files are not citation sources. The transport may return an empty array for unknown; the controller requires at least one citation for supported or refuted.",
        },
        "disagreement": {"type": "string", "maxLength": 16 * 1024},
        "reason": {"type": "string", "maxLength": 16 * 1024},
    },
}
VALIDATION_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["item_id", "files", "runs", "claim_observed"],
    "properties": {
        "item_id": {"type": "string", "minLength": 1, "maxLength": 512},
        "files": {
            "type": "array",
            "maxItems": 128,
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["path", "utf8"],
                "properties": {
                    "path": {"type": "string", "minLength": 1, "maxLength": 512, "pattern": r"^generated/(?!.*\\)(?!.*//)(?!.*\/$)(?!.*(?:^|/)(?:\.|\.\.)(?:/|$)).+$"},
                    "utf8": {"type": "string", "maxLength": 512 * 1024},
                },
            },
            "description": "Files are UTF-8 generated test files; paths are relative and materialized below /work/generated in OCI.",
        },
        "runs": {
            "type": "array",
            "minItems": 2,
            "maxItems": 2,
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["name", "argv", "cwd", "expect", "evidence_ids"],
                "properties": {
                    "name": {"type": "string", "enum": ["baseline", "counterexample"]},
                    "argv": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 64,
                        "items": {"type": "string", "minLength": 1, "maxLength": 2048},
                        "description": "Literal argv only; invoke Python3 directly, never a shell or -c command string.",
                    },
                    "cwd": {"type": "string", "const": "/work"},
                    "expect": {
                        "type": "object",
                        "additionalProperties": False,
                        "required": ["exit", "observation"],
                        "properties": {
                            "exit": {"type": "integer", "const": 0},
                            "observation": {
                                "type": "object",
                                "additionalProperties": False,
                                "required": ["claim_present"],
                                "properties": {"claim_present": {"type": "boolean"}},
                            },
                        },
                    },
                    "evidence_ids": {
                        "type": "array",
                        "items": {"type": "string", "minLength": 1, "maxLength": 512},
                        "description": "Only supplied evidence IDs; the controller checks membership.",
                    },
                },
            },
            "description": "Provide exactly one baseline and one counterexample with distinct literal argv and opposite claim_present expectations.",
        },
        "claim_observed": {"type": "string", "maxLength": 16 * 1024},
    },
}


ROLE_NAMES = ("judge_a", "judge_b", "synthesis", "discovery", "validation")
ROLE_PROMPTS = {
    "judge_a": (
        "You are fresh judge A. Assess only the supplied finding, supplied source packet, supplied evidence, "
        "and executed validation observations. Judge whether the executed baseline and counterexample meaningfully "
        "distinguish the claim from its negation; a print-only, comment-only, hard-coded, argv-echo, crashing, or "
        "otherwise non-source-bound harness is invalid or unproven. Judge B's output is not available to you. "
        "Return exactly the requested JSON. Each citation must be exactly {path,start,end,evidence_id}, using a "
        "supplied frozen-source path/range and supplied evidence_id; never cite generated files or /source paths. "
        "Set validation_verdict to valid only for a meaningful executed distinction, invalid when the controls do "
        "not test the source or do not distinguish the claim, and unproven when execution evidence is absent."
    ),
    "judge_b": (
        "You are fresh judge B. Independently assess only the supplied finding, supplied source packet, supplied "
        "evidence, and executed validation observations. Judge whether the executed baseline and counterexample "
        "meaningfully distinguish the claim from its negation; a print-only, comment-only, hard-coded, argv-echo, "
        "crashing, or otherwise non-source-bound harness is invalid or unproven. Judge A's output is not available "
        "to you. Return exactly the requested JSON. Each citation must be exactly {path,start,end,evidence_id}, "
        "using a supplied frozen-source path/range and supplied evidence_id; never cite generated files or /source "
        "paths. Set validation_verdict to valid only for a meaningful executed distinction, invalid when the controls "
        "do not test the source or do not distinguish the claim, and unproven when execution evidence is absent."
    ),
    "synthesis": (
        "You are a fresh synthesis judge. Compare only the two anonymized assessments, supplied source/evidence, "
        "and executed observations. Preserve disagreement and return unknown when proof is incomplete; do not "
        "invent evidence or promote failed controls. Return the exact item_id supplied. Every citation must be "
        "exactly {path,start,end,evidence_id} and must cite a supplied frozen-source range covered by that evidence; "
        "generated files and /source paths are not citation sources. Do not use peer identity or hidden context."
    ),
    "discovery": (
        "You are a blind discovery judge. Review only the supplied frozen source/evidence and report only actionable "
        "defects directly proven by the source under a supplied contract or an actual call path. No original findings, "
        "peer assessments, or author identity are available. Do not report style, documentation, type, or other "
        "convention assumptions, and do not speculate about missing callers or unshown behavior. If no concrete "
        "defect is proven, return an empty candidates array. Return candidates with exactly "
        "{claim,path,start,end,evidence_ids}: path is a supplied relative POSIX source path, start/end are inclusive "
        "positive line numbers, and every evidence_id must be supplied; at least one declared evidence range must "
        "cover the candidate range, while additional evidence may cover related contract or call-path material. Return no "
        "finding_id, citation strings, generated-file references, or /source paths. Do not use tools."
    ),
    "validation": (
        "You are a validation-plan author. Return exactly one bounded plan for the supplied item_id and claim. "
        "The controller executes it in a digest-pinned Python3 OCI image: source is read-only at /source, generated "
        "files are materialized only below /work/generated, and every run uses literal argv with cwd exactly /work. "
        "Use two distinct runs named baseline and counterexample, invoke python3 directly on the generated file, "
        "and make the generated program read/import the supplied source rather than print a path, comment, constant, "
        "or argv-derived answer. Each program must print actual JSON stdout containing boolean claim_present matching "
        "its expectation; do not use a shell, -c, host paths, assertion-false/crash harnesses, or manual test/operator "
        "prerequisites. The decoded utf8 field is raw source text and must contain actual newline characters; JSON "
        "escaping is applied once by the transport. Do not provide doubly escaped newline sequences or unescape or "
        "rewrite model-generated bytes. Malformed or doubly escaped source remains unproven. Use only supplied "
        "evidence IDs. Return JSON only; prose cannot establish execution."
    ),
}


def load_models(path: Path) -> tuple[dict[str, dict[str, Any]], bytes, list[str]]:
    value, raw = read_json(path)
    if not isinstance(value, Mapping):
        raise EvaluationError("models input must be an object")
    roles = value.get("roles", value)
    if not isinstance(roles, Mapping):
        raise EvaluationError("models.roles must be an object")
    if set(roles) != set(ROLE_NAMES):
        raise EvaluationError("models must configure exactly judge_a, judge_b, synthesis, discovery, validation")
    normalized: dict[str, dict[str, Any]] = {}
    for role in ROLE_NAMES:
        normalized[role] = adapters.validate_profile(roles[role], role)
    if normalized["judge_a"]["model_id"] == normalized["judge_b"]["model_id"]:
        raise EvaluationError("judge_a and judge_b must use distinct configured model IDs")
    warnings: list[str] = []
    if normalized["judge_a"]["family"] == normalized["judge_b"]["family"]:
        warnings.append("judge_a and judge_b share a model family; correlation remains possible")
    return normalized, raw, warnings


def load_environment(path: Optional[Path]) -> tuple[dict[str, Any], bytes]:
    if path is None:
        return {}, b"{}"
    value, raw = read_json(path)
    if not isinstance(value, Mapping):
        raise EvaluationError("environment input must be an object")
    normalized = dict(value)
    limits = normalized.get("source_limits", {})
    if not isinstance(limits, Mapping):
        raise EvaluationError("environment.source_limits must be an object")
    try:
        normalized["source_limits"] = {str(key): int(value) for key, value in limits.items()}
    except (TypeError, ValueError) as exc:
        raise EvaluationError("environment source limits must be integers") from exc
    oci_config = normalized.get("oci", {})
    if oci_config is not None and not isinstance(oci_config, Mapping):
        raise EvaluationError("environment.oci must be an object")
    return normalized, raw


def _source_lines(source_packet: Mapping[str, Any]) -> dict[str, int]:
    return {entry["path"]: _line_count(entry) for entry in source_packet.get("files", []) if isinstance(entry, Mapping)}


def _validate_source_packet(packet: Mapping[str, Any]) -> None:
    if not isinstance(packet, Mapping) or packet.get("schema_version") != "model-source-packet/v1":
        raise EvaluationError("source packet schema is invalid")
    expected = dict(packet)
    digest = expected.pop("packet_sha256", None)
    if not isinstance(digest, str) or sha256_bytes(canonical_json(expected).encode("utf-8")) != digest:
        raise EvaluationError("source packet digest is invalid")
    seen = set()
    for entry in packet.get("files", []):
        if not isinstance(entry, Mapping):
            raise EvaluationError("source packet file is invalid")
        path = safe_relative_path(entry.get("path"), "source packet path")
        if path in seen:
            raise EvaluationError("duplicate source packet path")
        seen.add(path)
        if "utf8" in entry:
            data = entry["utf8"].encode("utf-8")
        elif "base64" in entry:
            try:
                data = base64.b64decode(entry["base64"], validate=True)
            except (ValueError, TypeError) as exc:
                raise EvaluationError("source packet base64 is invalid") from exc
        else:
            raise EvaluationError("source packet file has no bytes")
        if sha256_bytes(data) != entry.get("sha256") or len(data) != entry.get("bytes"):
            raise EvaluationError("source packet file digest or size mismatch")


def _output_file(root: Path, relative: str) -> Path:
    safe = safe_relative_path(relative, "artifact path")
    path = root / safe
    if not _under(path, root) or path.is_symlink() or not path.is_file():
        raise EvaluationConflict(f"artifact is not a regular file: {relative}")
    current = root
    for part in PurePosixPath(safe).parts[:-1]:
        current = current / part
        if current.is_symlink() or not current.is_dir():
            raise EvaluationConflict(f"artifact parent is unsafe: {relative}")
    return path


def _output_files(root: Path) -> set[str]:
    files: set[str] = set()
    for current, directories, names in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        kept_directories = []
        for name in directories:
            path = current_path / name
            if path.is_symlink():
                raise EvaluationConflict(f"output contains a symlink: {path}")
            if not path.is_dir():
                raise EvaluationConflict(f"output contains a non-directory: {path}")
            kept_directories.append(name)
        directories[:] = kept_directories
        for name in names:
            path = current_path / name
            if path.is_symlink() or not path.is_file():
                raise EvaluationConflict(f"output contains an unsafe file: {path}")
            relative = str(path.relative_to(root)).replace(os.sep, "/")
            safe_relative_path(relative, "artifact path")
            files.add(relative)
    return files


def _verify_invocation_artifact(root: Path, directory: Path) -> None:
    relative_directory = str(directory.relative_to(root)).replace(os.sep, "/")
    packet_path = _output_file(root, f"{relative_directory}/packet.json")
    argv_path = _output_file(root, f"{relative_directory}/argv.json")
    stdout_path = _output_file(root, f"{relative_directory}/raw.stdout")
    stderr_path = _output_file(root, f"{relative_directory}/raw.stderr")
    result_path = _output_file(root, f"{relative_directory}/result.json")
    packet_record, _ = read_json(packet_path)
    result, _ = read_json(result_path)
    if not isinstance(packet_record, Mapping) or not isinstance(packet_record.get("packet"), Mapping):
        raise EvaluationConflict(f"invocation packet is invalid: {relative_directory}")
    if not isinstance(result, Mapping) or result.get("status") not in {"success", "failed"}:
        raise EvaluationConflict(f"invocation result is invalid: {relative_directory}")
    packet_hash = sha256_bytes(canonical_json(packet_record["packet"]).encode("utf-8"))
    if result.get("packet_sha256") != packet_hash:
        raise EvaluationConflict(f"invocation packet digest conflicts: {relative_directory}")
    expected_packet_artifact = result.get("packet_artifact_sha256")
    if expected_packet_artifact != sha256_bytes(packet_path.read_bytes()):
        raise EvaluationConflict(f"invocation packet bytes conflict: {relative_directory}")
    argv_value, _ = read_json(argv_path)
    if not isinstance(argv_value, list) or any(not isinstance(value, str) for value in argv_value):
        raise EvaluationConflict(f"invocation argv is invalid: {relative_directory}")
    if result.get("argv_sha256") != sha256_bytes(argv_path.read_bytes()):
        raise EvaluationConflict(f"invocation argv bytes conflict: {relative_directory}")
    if result.get("stdout_sha256") != sha256_bytes(stdout_path.read_bytes()) or result.get("stderr_sha256") != sha256_bytes(stderr_path.read_bytes()):
        raise EvaluationConflict(f"invocation raw capture conflicts: {relative_directory}")
    if result.get("status") == "success":
        final_path = _output_file(root, f"{relative_directory}/final.json")
        if result.get("final_sha256") != sha256_bytes(final_path.read_bytes()):
            raise EvaluationConflict(f"invocation final bytes conflict: {relative_directory}")
        read_json(final_path)


def _verify_validation_artifact(root: Path, directory: Path) -> None:
    relative_directory = str(directory.relative_to(root)).replace(os.sep, "/")
    result_path = _output_file(root, f"{relative_directory}/result.json")
    result, _ = read_json(result_path)
    if not isinstance(result, Mapping) or result.get("status") not in {
        "success",
        "unproven",
        "environment_blocked",
        "failed",
    }:
        raise EvaluationConflict(f"validation result is invalid: {relative_directory}")
    has_plan = result.get("plan_sha256") is not None
    has_digests = result.get("generated_file_digests_sha256") is not None
    if has_plan != has_digests:
        raise EvaluationConflict(f"validation plan digest metadata is incomplete: {relative_directory}")
    if result.get("status") == "success" and not has_plan:
        raise EvaluationConflict(f"successful validation lacks a retained plan: {relative_directory}")
    if has_plan:
        plan_path = _output_file(root, f"{relative_directory}/plan.json")
        if result.get("plan_sha256") != sha256_bytes(plan_path.read_bytes()):
            raise EvaluationConflict(f"validation plan bytes conflict: {relative_directory}")
        digest_path = _output_file(root, f"{relative_directory}/generated-file-digests.json")
        if result.get("generated_file_digests_sha256") != sha256_bytes(digest_path.read_bytes()):
            raise EvaluationConflict(f"validation digest bytes conflict: {relative_directory}")


def verify_evaluation_artifacts(root: Path) -> dict[str, Any]:
    """Validate a completed evaluation ledger before exposing its summary.

    ``run.json`` is the mutable ledger and is deliberately excluded from its
    own hash map.  Every other regular file must be represented and every
    represented byte must match; output symlinks and path aliases are errors.
    """

    supplied_root = Path(root)
    if supplied_root.is_symlink() or not supplied_root.is_dir():
        raise EvaluationConflict("evaluation artifact root must be a directory")
    root = supplied_root.resolve()
    snapshot, _ = read_json(_output_file(root, "snapshot.json"))
    run, _ = read_json(_output_file(root, "run.json"))
    summary, _ = read_json(_output_file(root, "summary.json"))
    if not isinstance(snapshot, Mapping) or snapshot.get("schema_version") != CONTROLLER_VERSION:
        raise EvaluationConflict("evaluation snapshot schema is invalid")
    if not isinstance(run, Mapping) or run.get("schema_version") != CONTROLLER_VERSION:
        raise EvaluationConflict("evaluation run schema is invalid")
    if snapshot.get("input_identity") != run.get("input_identity") or not isinstance(run.get("input_identity"), str):
        raise EvaluationConflict("evaluation input identity conflicts")
    preflight = run.get("preflight")
    if not isinstance(preflight, Mapping) or set(preflight) != {*ROLE_NAMES, "oci"}:
        raise EvaluationConflict("evaluation preflight ledger is invalid")
    if run.get("preflight_sha256") != sha256_bytes(canonical_json(preflight).encode("utf-8")):
        raise EvaluationConflict("evaluation preflight identity conflicts")
    profiles = snapshot.get("model_profiles")
    identity_status = run.get("identity_status")
    if not isinstance(profiles, Mapping) or not isinstance(identity_status, Mapping):
        raise EvaluationConflict("evaluation model identity ledger is missing")
    if set(profiles) != set(ROLE_NAMES) or set(identity_status) != set(ROLE_NAMES):
        raise EvaluationConflict("evaluation model identity roles conflict")
    for role in ROLE_NAMES:
        profile = profiles[role]
        status = identity_status[role]
        if not isinstance(profile, Mapping) or not isinstance(status, Mapping):
            raise EvaluationConflict("evaluation model identity ledger is invalid")
        if any(not isinstance(profile.get(field), str) or not profile[field] for field in ("adapter", "model_id", "family", "transport")):
            raise EvaluationConflict("evaluation model profile is invalid")
        if status.get("requested_model_id") != profile.get("model_id"):
            raise EvaluationConflict(f"requested model identity conflicts: {role}")
        for field in ("adapter", "family"):
            if field in status and status.get(field) != profile.get(field):
                raise EvaluationConflict(f"model identity conflicts: {role}")
    if run.get("state") not in {"complete", "partial", "failed"}:
        raise EvaluationConflict("evaluation is not in a terminal state")
    if not isinstance(summary, Mapping) or summary.get("schema_version") != CONTROLLER_VERSION:
        raise EvaluationConflict("evaluation summary schema is invalid")
    if summary.get("state") != run.get("state") or summary.get("provenance") != "model_assessment":
        raise EvaluationConflict("evaluation summary state or provenance conflicts")
    if run.get("summary_sha256") != sha256_bytes(pretty_json_bytes(summary)):
        raise EvaluationConflict("evaluation summary digest conflicts")

    source_packet, _ = read_json(_output_file(root, "source-packet.json"))
    _validate_source_packet(source_packet)
    if source_packet.get("packet_sha256") != snapshot.get("source_packet_sha256"):
        raise EvaluationConflict("source packet identity conflicts")
    if not isinstance(summary.get("source"), Mapping) or summary["source"].get("revision") != snapshot.get("revision") or summary["source"].get("base") != snapshot.get("base"):
        raise EvaluationConflict("summary source identity conflicts")
    findings, _ = read_json(_output_file(root, "findings.json"))
    omissions, _ = read_json(_output_file(root, "omissions.json"))
    if not isinstance(findings, Mapping) or findings.get("schema_version") != FINDINGS_VERSION or findings.get("input_sha256") != snapshot.get("findings_sha256") or not isinstance(findings.get("findings"), list):
        raise EvaluationConflict("findings artifact is not a full result record")
    if not isinstance(omissions, Mapping) or omissions.get("schema_version") != CONTROLLER_VERSION or omissions.get("input_sha256") != snapshot.get("findings_sha256") or not isinstance(omissions.get("candidates"), list):
        raise EvaluationConflict("omissions artifact is invalid")

    artifact_hashes = run.get("artifact_hashes")
    if not isinstance(artifact_hashes, Mapping) or len(artifact_hashes) > MAX_ARTIFACTS:
        raise EvaluationConflict("run artifact hash ledger is invalid")
    expected: dict[str, str] = {}
    for relative, digest in artifact_hashes.items():
        if not isinstance(relative, str) or relative == "run.json" or relative in expected:
            raise EvaluationConflict("run artifact hash path is invalid")
        safe = safe_relative_path(relative, "artifact hash path")
        if safe != relative or not isinstance(digest, str) or not HEX_SHA256.fullmatch(digest):
            raise EvaluationConflict("run artifact hash entry is invalid")
        path = _output_file(root, relative)
        if sha256_bytes(path.read_bytes()) != digest:
            raise EvaluationConflict(f"artifact bytes conflict: {relative}")
        expected[relative] = digest
    actual = _output_files(root)
    actual.discard("run.json")
    if actual != set(expected):
        missing = sorted(set(expected) - actual)
        extra = sorted(actual - set(expected))
        raise EvaluationConflict(f"artifact ledger coverage conflict: missing={missing[:3]} extra={extra[:3]}")

    invocation_root = root / "invocations"
    if invocation_root.exists():
        if invocation_root.is_symlink() or not invocation_root.is_dir():
            raise EvaluationConflict("invocations root is unsafe")
        for directory in sorted(invocation_root.iterdir()):
            if directory.is_symlink() or not directory.is_dir():
                raise EvaluationConflict("invocation directory is unsafe")
            _verify_invocation_artifact(root, directory)
    validation_root = root / "validation"
    if validation_root.exists():
        if validation_root.is_symlink() or not validation_root.is_dir():
            raise EvaluationConflict("validation root is unsafe")
        for directory in sorted(validation_root.iterdir()):
            if directory.is_symlink() or not directory.is_dir():
                raise EvaluationConflict("validation directory is unsafe")
            _verify_validation_artifact(root, directory)
    return dict(summary)


def _profile_identity_status(profile: Mapping[str, Any], probe: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "requested_model_id": profile["model_id"],
        "family": profile["family"],
        "adapter": profile["adapter"],
        "strength": profile["strength"],
        "capability": probe.get("status", "unknown"),
        "observed_model_id": "unavailable",
        "actual_cost": "unknown",
        "usage": "unknown",
    }


class EvaluationController:
    def __init__(
        self,
        *,
        repo: Path,
        revision: str,
        base: str,
        findings_path: Path,
        evidence_dir: Path,
        models_path: Path,
        environment_path: Optional[Path],
        output: Path,
        adapter_runner: Optional[Any] = None,
        oci_executor: Optional[Any] = None,
    ) -> None:
        self.repo = Path(repo).resolve()
        self.requested_revision = revision
        self.requested_base = base
        self.findings_path = Path(findings_path).resolve()
        self.evidence_dir = Path(evidence_dir).resolve()
        self.models_path = Path(models_path).resolve()
        self.environment_path = Path(environment_path).resolve() if environment_path else None
        self.output = Path(output).absolute()
        self.adapter_runner = adapter_runner
        self.injected_oci_executor = oci_executor
        self.findings: list[dict[str, Any]] = []
        self.evidence: dict[str, dict[str, Any]] = {}
        self.models: dict[str, dict[str, Any]] = {}
        self.environment: dict[str, Any] = {}
        self.source_packet: dict[str, Any] = {}
        self.snapshot: dict[str, Any] = {}
        self.preflight: dict[str, Any] = {}
        self.item_records: list[dict[str, Any]] = []
        self.candidates: list[dict[str, Any]] = []
        self.discovery_complete = False
        self.discovery_status: dict[str, Any] = {"status": "not_started"}
        self._source_temp: Optional[tempfile.TemporaryDirectory[str]] = None
        self._resume_summary: Optional[dict[str, Any]] = None
        self._role_records: dict[str, list[dict[str, Any]]] = {role: [] for role in ROLE_NAMES}

    def _output_is_resumable(self) -> bool:
        if not self.output.exists():
            self.output.mkdir(parents=True)
            return False
        if self.output.is_symlink() or not self.output.is_dir():
            raise EvaluationError("output must be a directory")
        entries = list(self.output.iterdir())
        if not entries:
            return False
        if not (self.output / "snapshot.json").is_file():
            raise EvaluationError("non-empty output lacks an immutable snapshot")
        return True

    def _input_identity(self, resolved_revision: str, resolved_base: str, findings_raw: bytes, evidence_raw: bytes, models_raw: bytes, environment_raw: bytes) -> str:
        return sha256_bytes(
            canonical_json(
                {
                    "controller_version": CONTROLLER_VERSION,
                    "revision": resolved_revision,
                    "base": resolved_base,
                    "findings_sha256": sha256_bytes(findings_raw),
                    "evidence_manifest_sha256": sha256_bytes(evidence_raw),
                    "models_sha256": sha256_bytes(models_raw),
                    "environment_sha256": sha256_bytes(environment_raw),
                }
            ).encode("utf-8")
        )

    def prepare(self) -> None:
        _validate_output_boundary(
            self.output,
            self.repo,
            [self.findings_path, self.evidence_dir, self.models_path, *( [self.environment_path] if self.environment_path else [])],
        )
        resumable = self._output_is_resumable()
        _validate_clean_repo(self.repo)
        resolved_revision = _resolve_revision(self.repo, self.requested_revision)
        resolved_base = _resolve_revision(self.repo, self.requested_base)
        self.findings, findings_raw = load_findings(self.findings_path)
        self.evidence, evidence_raw = load_evidence(self.evidence_dir)
        self.models, models_raw, family_warnings = load_models(self.models_path)
        self.environment, environment_raw = load_environment(self.environment_path)
        source_limits = self.environment.get("source_limits", {})
        self.source_packet = build_source_packet(self.repo, resolved_revision, resolved_base, source_limits)
        _validate_source_packet(self.source_packet)
        identity = self._input_identity(resolved_revision, resolved_base, findings_raw, evidence_raw, models_raw, environment_raw)
        self.snapshot = {
            "schema_version": CONTROLLER_VERSION,
            "input_identity": identity,
            "revision": resolved_revision,
            "base": resolved_base,
            "archive_sha256": self.source_packet["archive_sha256"],
            "diff_sha256": self.source_packet["diff_sha256"],
            "source_packet_sha256": self.source_packet["packet_sha256"],
            "findings_sha256": sha256_bytes(findings_raw),
            "evidence_manifest_sha256": sha256_bytes(evidence_raw),
            "models_sha256": sha256_bytes(models_raw),
            "environment_sha256": sha256_bytes(environment_raw),
            "source_complete": bool(self.source_packet.get("complete")),
            "source_omissions": list(self.source_packet.get("omissions", [])),
            "model_family_warnings": family_warnings,
            "model_profiles": {
                role: {
                    "adapter": self.models[role]["adapter"],
                    "model_id": self.models[role]["model_id"],
                    "family": self.models[role]["family"],
                    "transport": self.models[role].get("transport", "oauth"),
                }
                for role in ROLE_NAMES
            },
        }
        if resumable:
            existing, _ = read_json(self.output / "snapshot.json")
            if existing != self.snapshot:
                raise EvaluationConflict("snapshot identity or source bytes conflict on restart")
        else:
            write_json_immutable(self.output / "snapshot.json", self.snapshot)
            write_json_immutable(self.output / "source-packet.json", self.source_packet)
        source_packet_path = self.output / "source-packet.json"
        if not source_packet_path.is_file():
            raise EvaluationConflict("source packet is missing on restart")
        existing_packet, _ = read_json(source_packet_path)
        if existing_packet != self.source_packet:
            raise EvaluationConflict("source packet conflicts on restart")

        existing_run: Optional[dict[str, Any]] = None
        run_path = self.output / "run.json"
        if run_path.is_file():
            value, _ = read_json(run_path)
            if not isinstance(value, Mapping) or value.get("input_identity") != identity:
                raise EvaluationConflict("run identity conflicts on restart")
            existing_run = dict(value)
            existing_preflight = existing_run.get("preflight")
            if not isinstance(existing_preflight, Mapping) or set(existing_preflight) != {*ROLE_NAMES, "oci"}:
                raise EvaluationConflict("preflight ledger conflicts on restart")
            if existing_run.get("preflight_sha256") != sha256_bytes(canonical_json(existing_preflight).encode("utf-8")):
                raise EvaluationConflict("preflight identity conflicts on restart")
            if existing_run.get("state") == "complete":
                # A completed result is exposed only after the frozen seam has
                # checked every retained byte.  This path intentionally does
                # not probe a model CLI or the OCI daemon again.
                self._resume_summary = verify_evaluation_artifacts(self.output)
                self.preflight = dict(existing_run.get("preflight", {}))
                return
            self._verify_completed_artifacts()

        if existing_run is not None and isinstance(existing_run.get("preflight"), Mapping) and all(
            role in existing_run["preflight"] for role in (*ROLE_NAMES, "oci")
        ):
            self.preflight = dict(existing_run["preflight"])
            self._configure_oci_executor()
        else:
            self._preflight_models()
        if existing_run is None:
            initial_run = {
                "schema_version": CONTROLLER_VERSION,
                "input_identity": identity,
                "state": "running",
                "source_complete": self.snapshot["source_complete"],
                "preflight": self.preflight,
                "preflight_sha256": sha256_bytes(canonical_json(self.preflight).encode("utf-8")),
                "completed_invocations": [],
                "failed_invocations": [],
                "identity_status": {role: _profile_identity_status(self.models[role], self.preflight[role]) for role in ROLE_NAMES},
                "actual_cost_status": "unknown",
                "artifact_hashes": {},
            }
            self._write_run(initial_run)
            self._refresh_artifact_hashes()
        self._source_temp = tempfile.TemporaryDirectory(prefix="openab-eval-source-")
        oci.materialize_source_tree(self.source_packet, Path(self._source_temp.name))

    def _preflight_models(self) -> None:
        statuses: dict[str, Any] = {}
        for role in ROLE_NAMES:
            profile = self.models[role]
            adapter = adapters.adapter_for_profile(profile, runner=self.adapter_runner)
            statuses[role] = adapter.probe(profile)
        oci_config = self.environment.get("oci", {}) or {}
        image = oci_config.get("image", self.environment.get("oci_image", oci.DEFAULT_IMAGE)) if isinstance(oci_config, Mapping) else oci.DEFAULT_IMAGE
        self._configure_oci_executor(image=image, oci_config=oci_config)
        statuses["oci"] = self.oci_executor.preflight()
        self.preflight = statuses

    def _configure_oci_executor(self, *, image: Optional[str] = None, oci_config: Optional[Mapping[str, Any]] = None) -> None:
        if self.injected_oci_executor is not None:
            self.oci_executor = self.injected_oci_executor
            return
        config = oci_config if oci_config is not None else (self.environment.get("oci", {}) or {})
        if not isinstance(config, Mapping):
            config = {}
        selected_image = image or config.get("image", self.environment.get("oci_image", oci.DEFAULT_IMAGE))
        self.oci_executor = oci.OCIExecutor(
            selected_image,
            docker_executable=config.get("docker_executable", "docker"),
            probe_daemon=bool(config.get("probe_daemon", True)),
        )

    @staticmethod
    def _component(value: str) -> str:
        value = re.sub(r"[^A-Za-z0-9_.-]+", "_", value)
        return value[:80] or "item"

    def _write_run(self, value: Mapping[str, Any]) -> None:
        path = self.output / "run.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.is_symlink() or (path.exists() and not path.is_file()):
            raise EvaluationConflict("run ledger is not a regular file")
        temporary = path.with_name(f".{path.name}.tmp")
        if temporary.is_symlink() or (temporary.exists() and not temporary.is_file()):
            raise EvaluationConflict("run ledger temporary path is unsafe")
        temporary.write_bytes(pretty_json_bytes(value))
        os.replace(temporary, path)

    def _read_run(self) -> dict[str, Any]:
        value, _ = read_json(self.output / "run.json")
        if not isinstance(value, Mapping) or value.get("input_identity") != self.snapshot["input_identity"]:
            raise EvaluationConflict("run artifact identity is invalid")
        return dict(value)

    def _verify_completed_artifacts(self) -> None:
        run = self._read_run()
        hashes = run.get("artifact_hashes", {})
        if not isinstance(hashes, Mapping):
            raise EvaluationConflict("run contains an invalid artifact hash ledger")
        for relative, digest in hashes.items():
            if not isinstance(relative, str) or relative == "run.json" or not isinstance(digest, str) or not HEX_SHA256.fullmatch(digest):
                raise EvaluationConflict("run contains an invalid artifact hash")
            path = _output_file(self.output, relative)
            if sha256_bytes(path.read_bytes()) != digest:
                raise EvaluationConflict(f"artifact bytes conflict: {relative}")
        _output_files(self.output)
        invocation_root = self.output / "invocations"
        if invocation_root.is_dir():
            for directory in invocation_root.iterdir():
                if directory.is_dir() and (directory / "result.json").is_file():
                    _verify_invocation_artifact(self.output, directory)
        validation_root = self.output / "validation"
        if validation_root.is_dir():
            for directory in validation_root.iterdir():
                if directory.is_dir() and (directory / "result.json").is_file():
                    _verify_validation_artifact(self.output, directory)

    def _refresh_artifact_hashes(self) -> None:
        run = self._read_run()
        hashes: dict[str, str] = {}
        for relative in sorted(_output_files(self.output)):
            if relative == "run.json" or relative.endswith(".tmp"):
                continue
            path = _output_file(self.output, relative)
            hashes[relative] = sha256_bytes(path.read_bytes())
        run["artifact_hashes"] = hashes
        self._write_run(run)

    def _invocation_paths(self, invocation_id: str) -> Path:
        return self.output / "invocations" / self._component(invocation_id)

    def _invocation_attempt_paths(self, invocation_id: str) -> list[Path]:
        base = self._invocation_paths(invocation_id)
        root = base.parent
        prefix = base.name + "-attempt-"
        attempts = [path for path in root.glob(prefix + "*") if path.is_dir()]
        attempts.sort(key=lambda path: path.name)
        return [base, *attempts]

    def _next_invocation_path(self, invocation_id: str) -> Path:
        paths = self._invocation_attempt_paths(invocation_id)
        base = paths[0]
        if not (base / "result.json").is_file():
            return base
        index = len(paths)
        while True:
            candidate = base.parent / f"{base.name}-attempt-{index}"
            if not candidate.exists():
                return candidate
            index += 1

    def _completed_invocation(self, invocation_id: str, packet: Mapping[str, Any]) -> Optional[dict[str, Any]]:
        directory = self._invocation_paths(invocation_id)
        result_path = directory / "result.json"
        if not result_path.is_file():
            if (directory / "packet.json").is_file():
                old_packet, _ = read_json(directory / "packet.json")
                if old_packet.get("packet_sha256") != sha256_bytes(canonical_json(packet).encode("utf-8")):
                    raise EvaluationConflict(f"partial invocation packet conflict: {invocation_id}")
            return None
        result, _ = read_json(result_path)
        if not isinstance(result, Mapping) or result.get("status") not in {"success", "failed"}:
            raise EvaluationConflict(f"invocation result is invalid: {invocation_id}")
        packet_hash = sha256_bytes(canonical_json(packet).encode("utf-8"))
        if result.get("packet_sha256") != packet_hash:
            raise EvaluationConflict(f"completed invocation packet conflict: {invocation_id}")
        _verify_invocation_artifact(self.output, directory)
        if result.get("status") == "success":
            final, _ = read_json(directory / "final.json")
            result = dict(result)
            result["structured_output"] = final
            return dict(result)
        # A failed attempt is auditable but is deliberately eligible for a
        # later retry under a new attempt directory.
        return None

    def _completed_invocation_for_packet(self, invocation_id: str, packet: Mapping[str, Any]) -> Optional[dict[str, Any]]:
        for directory in reversed(self._invocation_attempt_paths(invocation_id)):
            candidate_id = directory.name
            result_path = directory / "result.json"
            if not result_path.is_file():
                continue
            result = self._completed_invocation(candidate_id, packet)
            if result is not None:
                return result
        return None

    def _invoke(self, role: str, invocation_id: str, packet: Mapping[str, Any], schema: Mapping[str, Any]) -> dict[str, Any]:
        completed = self._completed_invocation_for_packet(invocation_id, packet)
        if completed is not None:
            self._remember_invocation(role, invocation_id, completed)
            return completed
        directory = self._next_invocation_path(invocation_id)
        directory.mkdir(parents=True, exist_ok=True)
        packet_hash = sha256_bytes(canonical_json(packet).encode("utf-8"))
        artifact_invocation_id = directory.name
        profile = self.models[role]
        adapter = adapters.adapter_for_profile(profile, runner=self.adapter_runner)
        if role == "validation":
            output_schema = oci_plan_schema()
        else:
            output_schema = schema
        packet_record = {"packet_sha256": packet_hash, "packet": packet, "schema": output_schema}
        write_json_immutable(directory / "packet.json", packet_record)
        if isinstance(adapter, adapters.ClaudeAdapter):
            argv = adapter.build_argv(
                profile["executable"],
                profile["model_id"],
                output_schema,
                ROLE_PROMPTS[role],
                transport=profile.get("transport", "oauth"),
            )
        elif isinstance(adapter, adapters.CodexAdapter):
            argv = []
        else:
            argv = []
        argv_bytes = pretty_json_bytes(argv)
        write_immutable(directory / "argv.json", argv_bytes)
        argv_hash = sha256_bytes(argv_bytes)
        probe = self.preflight[role]
        if probe.get("status") != "ready":
            failure_class = "environment_blocked" if probe.get("status") in {"unavailable", "unsupported"} else "failed"
            failure = {
                "status": "failed",
                "invocation_id": invocation_id,
                "artifact_invocation_id": artifact_invocation_id,
                "failure_class": failure_class,
                "reason": probe.get("reason", "adapter_not_ready"),
                "packet_sha256": packet_hash,
                "packet_artifact_sha256": sha256_bytes(pretty_json_bytes(packet_record)),
                "argv_sha256": argv_hash,
                "stdout_sha256": sha256_bytes(b""),
                "stderr_sha256": sha256_bytes(b""),
                "requested_model_id": profile["model_id"],
                "observed_model_id": "unavailable",
                "attempts": 0,
                "capture_status": "not_started",
            }
            write_immutable(directory / "raw.stdout", b"")
            write_immutable(directory / "raw.stderr", b"")
            write_json_immutable(directory / "result.json", failure)
            self._remember_invocation(role, invocation_id, failure)
            return failure
        try:
            with tempfile.TemporaryDirectory(prefix=f"openab-eval-{self._component(invocation_id)}-") as session:
                if isinstance(adapter, adapters.ClaudeAdapter):
                    response = adapter.invoke(packet, output_schema, profile["model_id"], system_prompt=ROLE_PROMPTS[role], session_dir=Path(session))
                else:
                    response = adapter.invoke(packet, output_schema, profile["model_id"], session_dir=Path(session))
        except Exception as exc:
            capture = getattr(exc, "capture", None)
            if capture is None:
                capture = adapters.ProcessCapture(tuple(argv), None, b"", b"", error=type(exc).__name__)
            failure = {
                "status": "failed",
                "invocation_id": invocation_id,
                "artifact_invocation_id": artifact_invocation_id,
                "failure_class": "invocation_failed",
                "reason": type(exc).__name__,
                "transport_error": str(exc)[:4096],
                "packet_sha256": packet_hash,
                "packet_artifact_sha256": sha256_bytes(pretty_json_bytes(packet_record)),
                "argv_sha256": argv_hash,
                "stdout_sha256": sha256_bytes(capture.stdout),
                "stderr_sha256": sha256_bytes(capture.stderr),
                "requested_model_id": profile["model_id"],
                "observed_model_id": "unavailable",
                "returncode": capture.returncode,
                "attempts": 1,
                "capture_status": capture.error or ("timeout" if capture.timed_out else "failed"),
                "timed_out": capture.timed_out,
                "output_limited": capture.output_limited,
                "stdout_complete": capture.stdout_complete,
                "stderr_complete": capture.stderr_complete,
                "output_limit": capture.output_limit,
            }
            write_immutable(directory / "raw.stdout", capture.stdout)
            write_immutable(directory / "raw.stderr", capture.stderr)
            write_json_immutable(directory / "result.json", failure)
            self._remember_invocation(role, invocation_id, failure)
            return failure
        write_immutable(directory / "raw.stdout", response.raw_stdout)
        write_immutable(directory / "raw.stderr", response.raw_stderr)
        write_json_immutable(directory / "final.json", response.structured_output)
        result = {
            "status": "success",
            "invocation_id": invocation_id,
            "artifact_invocation_id": artifact_invocation_id,
            "packet_sha256": packet_hash,
            "packet_artifact_sha256": sha256_bytes(pretty_json_bytes(packet_record)),
            "argv_sha256": argv_hash,
            "stdout_sha256": sha256_bytes(response.raw_stdout),
            "stderr_sha256": sha256_bytes(response.raw_stderr),
            "final_sha256": sha256_bytes(pretty_json_bytes(response.structured_output)),
            "requested_model_id": response.requested_model_id,
            "observed_model_id": response.observed_model_id,
            "actual_metadata": response.actual_metadata,
            "attempts": response.attempts,
            "failure_class": None,
            "capture_status": response.transport_status,
            "actual_cost_usd": response.actual_metadata.get("actual_cost_usd", "unknown"),
        }
        write_json_immutable(directory / "result.json", result)
        result["structured_output"] = response.structured_output
        self._remember_invocation(role, invocation_id, result)
        return result

    def _remember_invocation(self, role: str, invocation_id: str, record: Mapping[str, Any]) -> None:
        remembered = dict(record)
        remembered.pop("structured_output", None)
        entry = {"id": invocation_id, **remembered}
        self._role_records.setdefault(role, [])
        self._role_records[role] = [item for item in self._role_records[role] if item.get("id") != invocation_id]
        self._role_records[role].append(entry)

    def _update_run_invocation(self, invocation_id: str, record: Mapping[str, Any], role: Optional[str] = None) -> None:
        run = self._read_run()
        completed = list(run.get("completed_invocations", []))
        failed = list(run.get("failed_invocations", []))
        artifact_invocation_id = record.get("artifact_invocation_id", invocation_id)
        completed = [item for item in completed if item.get("id") != artifact_invocation_id]
        failed = [item for item in failed if item.get("id") != artifact_invocation_id]
        entry = {
            "id": artifact_invocation_id,
            "status": record.get("status"),
            "failure_class": record.get("failure_class"),
            "requested_model_id": record.get("requested_model_id"),
            "observed_model_id": record.get("observed_model_id", "unavailable"),
            "attempts": record.get("attempts", 0),
        }
        target = completed if record.get("status") == "success" else failed
        target.append(entry)
        run["completed_invocations"] = sorted(completed, key=lambda item: item["id"])
        run["failed_invocations"] = sorted(failed, key=lambda item: item["id"])
        if role in ROLE_NAMES:
            identity_status = run.setdefault("identity_status", {}).setdefault(role, {})
            identity_status["requested_model_id"] = record.get("requested_model_id", self.models[role]["model_id"])
            identity_status["observed_model_id"] = record.get("observed_model_id", "unavailable")
            metadata = record.get("actual_metadata")
            if isinstance(metadata, Mapping):
                identity_status["usage"] = metadata.get("model_usage", metadata.get("usage", "unknown"))
                identity_status["estimated_cost_usd"] = metadata.get("estimated_cost_usd", "unknown")
                identity_status["actual_cost_usd"] = metadata.get("actual_cost_usd", "unknown")
        self._write_run(run)
        self._refresh_artifact_hashes()

    def _source_lines_for(self) -> dict[str, int]:
        return _source_lines(self.source_packet)

    def _validation_directory(self, item_id: str) -> Path:
        return self.output / "validation" / self._component(item_id)

    def _validation_attempt_paths(self, item_id: str) -> list[Path]:
        base = self._validation_directory(item_id)
        attempts = [path for path in base.parent.glob(base.name + "-attempt-*") if path.is_dir()]
        attempts.sort(key=lambda path: path.name)
        return [base, *attempts]

    def _next_validation_path(self, item_id: str) -> Path:
        paths = self._validation_attempt_paths(item_id)
        if not (paths[0] / "result.json").is_file():
            return paths[0]
        index = len(paths)
        while True:
            candidate = paths[0].parent / f"{paths[0].name}-attempt-{index}"
            if not candidate.exists():
                return candidate
            index += 1

    def _completed_validation(
        self, item_id: str, finding: Mapping[str, Any], packet: Mapping[str, Any]
    ) -> Optional[dict[str, Any]]:
        packet_hash = sha256_bytes(canonical_json(packet).encode("utf-8"))
        finding_hash = sha256_bytes(canonical_json(finding).encode("utf-8"))
        for directory in reversed(self._validation_attempt_paths(item_id)):
            result_path = directory / "result.json"
            if not result_path.is_file():
                continue
            value, _ = read_json(result_path)
            if not isinstance(value, Mapping) or value.get("status") not in {"success", "unproven", "environment_blocked", "failed"}:
                raise EvaluationConflict(f"validation result is invalid: {item_id}")
            if value.get("validation_packet_sha256") != packet_hash or value.get("source_packet_sha256") != self.source_packet.get("packet_sha256"):
                raise EvaluationConflict(f"validation packet conflicts: {item_id}")
            if value.get("finding_sha256") != finding_hash:
                raise EvaluationConflict(f"validation finding conflicts: {item_id}")
            _verify_validation_artifact(self.output, directory)
            if value.get("status") == "failed":
                continue
            return dict(value)
        return None

    def _write_validation_record(
        self,
        item_id: str,
        finding: Mapping[str, Any],
        packet: Mapping[str, Any],
        record: Mapping[str, Any],
        plan: Optional[Mapping[str, Any]] = None,
        directory: Optional[Path] = None,
    ) -> dict[str, Any]:
        directory = directory or self._validation_directory(item_id)
        directory.mkdir(parents=True, exist_ok=True)
        if plan is not None:
            write_json_immutable(directory / "plan.json", plan)
            write_json_immutable(
                directory / "generated-file-digests.json",
                [
                    {"path": item["path"], "sha256": item["sha256"], "bytes": item["bytes"]}
                    for item in plan.get("files", [])
                ],
            )
        execution = record.get("execution")
        if isinstance(execution, Mapping):
            for observed in execution.get("runs", []):
                if isinstance(observed, Mapping) and isinstance(observed.get("name"), str):
                    write_json_immutable(directory / "runs" / f"{self._component(observed['name'])}.json", observed)
        result = dict(record)
        result["item_id"] = item_id
        result["validation_packet_sha256"] = sha256_bytes(canonical_json(packet).encode("utf-8"))
        result["source_packet_sha256"] = self.source_packet.get("packet_sha256")
        result["finding_sha256"] = sha256_bytes(canonical_json(finding).encode("utf-8"))
        if plan is not None:
            result["plan_sha256"] = sha256_bytes(pretty_json_bytes(plan))
            result["generated_file_digests_sha256"] = sha256_bytes(
                pretty_json_bytes(
                    [
                        {"path": item["path"], "sha256": item["sha256"], "bytes": item["bytes"]}
                        for item in plan.get("files", [])
                    ]
                )
            )
        write_json_immutable(directory / "result.json", result)
        self._refresh_artifact_hashes()
        return result

    @staticmethod
    def _observed_run(run: Mapping[str, Any]) -> Mapping[str, Any]:
        actual = run.get("actual")
        return actual if isinstance(actual, Mapping) else run

    @classmethod
    def _baseline_claim_present(cls, execution: Mapping[str, Any]) -> Optional[bool]:
        declared = execution.get("claim_present") if isinstance(execution.get("claim_present"), bool) else None
        observed: Optional[bool] = None
        runs = execution.get("runs")
        if isinstance(runs, list):
            for run in runs:
                if not isinstance(run, Mapping) or run.get("name") != "baseline":
                    continue
                observation = cls._observed_run(run).get("observation")
                if isinstance(observation, Mapping) and isinstance(observation.get("claim_present"), bool):
                    observed = observation["claim_present"]
                break
        if declared is not None and observed is not None and declared != observed:
            return None
        return observed if observed is not None else declared

    @staticmethod
    def _subset(expected: Any, actual: Any) -> bool:
        if isinstance(expected, Mapping):
            return isinstance(actual, Mapping) and all(
                key in actual and EvaluationController._subset(value, actual[key]) for key, value in expected.items()
            )
        if isinstance(expected, list):
            return expected == actual
        return expected == actual

    def _controls_passed(self, plan: Mapping[str, Any], execution: Mapping[str, Any]) -> bool:
        if execution.get("controls_passed") is not True:
            return False
        observed = execution.get("runs")
        if not isinstance(observed, list) or len(observed) != 2:
            return False
        by_name = {
            run.get("name"): self._observed_run(run)
            for run in observed
            if isinstance(run, Mapping) and isinstance(run.get("name"), str)
        }
        if set(by_name) != {"baseline", "counterexample"}:
            return False
        for expected in plan.get("runs", []):
            actual = by_name.get(expected.get("name"))
            if not isinstance(actual, Mapping) or actual.get("exit") != 0 or actual.get("timeout", False):
                return False
            observation = actual.get("observation")
            if not self._subset(expected.get("expect", {}).get("observation"), observation):
                return False
        baseline = by_name["baseline"].get("observation")
        counterexample = by_name["counterexample"].get("observation")
        return (
            isinstance(baseline, Mapping)
            and isinstance(counterexample, Mapping)
            and isinstance(baseline.get("claim_present"), bool)
            and isinstance(counterexample.get("claim_present"), bool)
            and baseline["claim_present"] != counterexample["claim_present"]
        )

    def _execution_classification(self, validation: Mapping[str, Any]) -> Optional[str]:
        """Derive an execution direction without trusting OCI semantics."""

        if validation.get("status") != "success" or validation.get("controls_passed") is not True:
            return None
        if validation.get("controller_source_binding") is not True:
            return None
        execution = validation.get("execution")
        claim_present = validation.get("claim_present")
        if isinstance(execution, Mapping):
            claim_present = self._baseline_claim_present(execution)
        if not isinstance(claim_present, bool):
            return None
        return "executed_reproduced" if claim_present else "executed_refuted"

    def _run_judges(
        self, item_id: str, finding: Mapping[str, Any], validation: Mapping[str, Any]
    ) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
        valid: list[dict[str, Any]] = []
        raw_records: list[dict[str, Any]] = []
        for role in ("judge_a", "judge_b"):
            invocation_id = f"{role}-{item_id}"
            packet = build_judge_packet(invocation_id, self.source_packet, finding, self.evidence, role, validation)
            record = self._invoke(role, invocation_id, packet, JUDGE_SCHEMA)
            self._update_run_invocation(invocation_id, record, role)
            raw_records.append(
                {
                    "role": role,
                    "invocation_id": invocation_id,
                    "status": record.get("status"),
                    "failure_class": record.get("failure_class"),
                    "requested_model_id": record.get("requested_model_id"),
                    "observed_model_id": record.get("observed_model_id", "unavailable"),
                }
            )
            if record.get("status") == "success":
                try:
                    valid.append(validate_judge_result(record["structured_output"], finding, self.evidence, self._source_lines_for()))
                    raw_records[-1]["assessment"] = valid[-1]
                except EvaluationError as exc:
                    raw_records[-1]["validation"] = "invalid"
                    raw_records[-1]["validation_reason"] = type(exc).__name__
        return valid, raw_records

    def _run_validation(self, item_id: str, finding: Mapping[str, Any]) -> dict[str, Any]:
        invocation_id = f"validation-{item_id}"
        packet = build_validation_packet(invocation_id, finding, self.evidence, self.source_packet)
        cached = self._completed_validation(item_id, finding, packet)
        if cached is not None:
            return cached
        validation_directory = self._next_validation_path(item_id)
        record = self._invoke("validation", invocation_id, packet, VALIDATION_SCHEMA)
        self._update_run_invocation(invocation_id, record, "validation")
        if record.get("status") != "success":
            return self._write_validation_record(
                item_id,
                finding,
                packet,
                {
                    "status": "failed",
                    "classification": "unproven",
                    "reason": record.get("failure_class", "validation_invocation_failed"),
                    "invocation_id": invocation_id,
                    "model_status": record.get("status"),
                },
                directory=validation_directory,
            )
        try:
            plan = oci.validate_generated_plan(record["structured_output"], set(self.evidence))
            if plan.get("item_id") != item_id:
                raise EvaluationError("validation plan returned the wrong item id")
        except Exception as exc:
            return self._write_validation_record(
                item_id,
                finding,
                packet,
                {
                    "status": "unproven",
                    "classification": "unproven",
                    "reason": type(exc).__name__,
                    "invocation_id": invocation_id,
                    "model_status": "success",
                },
                directory=validation_directory,
            )
        if self._source_temp is None:
            raise EvaluationError("source tree was not prepared")
        oci_config = self.environment.get("oci", {}) if isinstance(self.environment.get("oci", {}), Mapping) else {}
        operator_checks = self.environment.get("operator_checks", oci_config.get("operator_checks", []))
        if operator_checks is not None and not isinstance(operator_checks, list):
            operator_checks = []
        try:
            execution = self.oci_executor.execute(
                plan,
                Path(self._source_temp.name),
                evidence_ids=set(self.evidence),
                item_dir=validation_directory,
                operator_checks=operator_checks,
            )
        except Exception as exc:
            execution = {"status": "failed", "classification": "unproven", "reason": type(exc).__name__, "controls_passed": False, "runs": []}
        binding_signal = _plan_source_binding_signal(plan, finding)
        controls_passed = self._controls_passed(plan, execution) if isinstance(execution, Mapping) else False
        status = execution.get("status") if isinstance(execution, Mapping) else "failed"
        raw_classification = execution.get("classification", "unproven") if isinstance(execution, Mapping) else "unproven"
        classification = "unproven"
        if status == "environment_blocked" or raw_classification == "environment_blocked":
            classification = "environment_blocked"
            status = "environment_blocked"
        elif not controls_passed or not binding_signal or status != "success":
            classification = "unproven"
            status = "unproven"
        claim_present = None
        if isinstance(execution, Mapping):
            claim_present = self._baseline_claim_present(execution)
        return self._write_validation_record(
            item_id,
            finding,
            packet,
            {
                "status": status,
                "classification": classification,
                "execution_classification": raw_classification,
                "invocation_id": invocation_id,
                "execution": dict(execution) if isinstance(execution, Mapping) else {"status": "failed"},
                "controls_passed": controls_passed,
                "claim_present": claim_present,
                "controller_source_binding": binding_signal,
                "model_status": "success",
            },
            plan,
            validation_directory,
        )

    def _run_synthesis(self, item_id: str, finding: Mapping[str, Any], judgments: Sequence[Mapping[str, Any]], executed: Mapping[str, Any]) -> Optional[dict[str, Any]]:
        if len(judgments) != 2:
            return None
        invocation_id = f"synthesis-{item_id}"
        packet = build_synthesis_packet(invocation_id, self.source_packet, finding, self.evidence, executed, judgments)
        record = self._invoke("synthesis", invocation_id, packet, SYNTHESIS_SCHEMA)
        self._update_run_invocation(invocation_id, record, "synthesis")
        if record.get("status") != "success":
            return None
        try:
            synthesis = _validate_synthesis_result(record["structured_output"], item_id, finding, self.evidence, self._source_lines_for())
            executed_classification = self._execution_classification(executed)
            if executed_classification in {"executed_reproduced", "executed_refuted"}:
                expected = "supported" if executed_classification == "executed_reproduced" else "refuted"
                if synthesis.get("verdict") != expected:
                    synthesis["execution_conflict"] = True
            return synthesis
        except EvaluationError:
            return None

    def _item(self, item_id: str, finding: Mapping[str, Any], kind: str, *, relation: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
        validation = self._run_validation(item_id, finding)
        judgments, raw_judges = self._run_judges(item_id, finding, validation)
        synthesis = self._run_synthesis(item_id, finding, judgments, validation)
        execution_classification = self._execution_classification(validation)
        expected_judge_verdict = {
            "executed_reproduced": "support",
            "executed_refuted": "refute",
        }.get(execution_classification)
        judges_qualify_execution = bool(
            execution_classification
            and expected_judge_verdict
            and len(judgments) == 2
            and all(judgment.get("validation_verdict") == "valid" for judgment in judgments)
            and all(judgment.get("verdict") == expected_judge_verdict for judgment in judgments)
        )
        synthesis_qualifies_execution = bool(
            judges_qualify_execution
            and isinstance(synthesis, Mapping)
            and not synthesis.get("execution_conflict")
            and synthesis.get("verdict") == ("supported" if execution_classification == "executed_reproduced" else "refuted")
        )
        if synthesis_qualifies_execution:
            classification = execution_classification
        elif validation.get("status") == "environment_blocked":
            classification = "environment_blocked"
        else:
            classification = "unproven"
        record: dict[str, Any] = {
            "item_id": item_id,
            "kind": kind,
            "finding": finding,
            "judges": raw_judges,
            "valid_judge_count": len(judgments),
            "judge_disagreement": len({item.get("verdict") for item in judgments}) > 1,
            "synthesis": synthesis,
            "validation": validation,
            "classification": classification,
            "provenance": "model_assessment",
            "scoreable": bool(
                self.snapshot.get("source_complete")
                and self.discovery_complete
                and judges_qualify_execution
                and synthesis_qualifies_execution
                and classification in {"executed_reproduced", "executed_refuted"}
            ),
        }
        if relation:
            record["relation"] = dict(relation)
        return record

    @staticmethod
    def _candidate_id(candidate: Mapping[str, Any], index: int) -> str:
        digest = sha256_bytes(canonical_json(candidate).encode("utf-8"))[:16]
        return f"omission-{index + 1}-{digest}"

    def _overlap_relation(self, candidate: Mapping[str, Any]) -> dict[str, Any]:
        start, end = candidate["start"], candidate["end"]
        duplicate_of = []
        normalized_claim = re.sub(r"\s+", " ", candidate["claim"].strip().lower())
        for finding in self.findings:
            location = finding["location"]
            other_claim = re.sub(r"\s+", " ", finding["claim"].strip().lower())
            overlap = candidate["path"] == location["path"] and not (end < location["start_line"] or start > location["end_line"])
            if overlap or (normalized_claim and normalized_claim == other_claim):
                duplicate_of.append(finding["finding_id"])
        return {"normalized_claim": normalized_claim, "duplicate_of": sorted(duplicate_of), "is_new_candidate": not duplicate_of}

    def _run_discovery(self) -> list[dict[str, Any]]:
        self.discovery_complete = False
        invocation_id = "discovery"
        packet = build_discovery_packet(invocation_id, self.source_packet, self.evidence, self.findings)
        record = self._invoke("discovery", invocation_id, packet, DISCOVERY_SCHEMA)
        self._update_run_invocation(invocation_id, record, "discovery")
        if record.get("status") != "success":
            self.discovery_status = {"status": "failed", "failure_class": record.get("failure_class", "discovery_failed"), "invocation_id": invocation_id}
            return []
        try:
            raw_candidates = _validate_discovery_result(record["structured_output"], self.evidence, self._source_lines_for())
        except EvaluationError as exc:
            self.discovery_status = {"status": "failed", "failure_class": "invalid_discovery_result", "reason": type(exc).__name__, "invocation_id": invocation_id}
            return []
        self.discovery_complete = True
        self.discovery_status = {"status": "success", "candidate_count": len(raw_candidates), "invocation_id": invocation_id}
        candidates: list[dict[str, Any]] = []
        for index, candidate in enumerate(raw_candidates):
            relation = self._overlap_relation(candidate)
            item_id = self._candidate_id(candidate, index)
            finding = {
                "finding_id": item_id,
                "title": "candidate",
                "severity": "unknown",
                "claim": candidate["claim"],
                "location": {"path": candidate["path"], "start_line": candidate["start"], "end_line": candidate["end"]},
                "evidence_ids": candidate["evidence_ids"],
                "author_model_identity": "unknown",
            }
            item = self._item(item_id, finding, "omission_candidate", relation=relation)
            item["discovery_candidate"] = candidate
            candidates.append(item)
        return candidates

    def _summary(self, state: str) -> dict[str, Any]:
        originals = [item for item in self.item_records if item["kind"] == "original"]
        omissions = [item for item in self.item_records if item["kind"] == "omission_candidate"]
        all_items = [*originals, *omissions]
        original_synth = [item["synthesis"] for item in originals if isinstance(item.get("synthesis"), Mapping)]
        usable_original_synth = [item for item in original_synth if not item.get("execution_conflict")]
        verdicts = {key: sum(1 for item in usable_original_synth if item.get("verdict") == key) for key in ("supported", "refuted", "unknown")}
        execution_conflicts = sum(
            1 for item in all_items if isinstance(item.get("synthesis"), Mapping) and item["synthesis"].get("execution_conflict")
        )
        usefulness = {
            key: sum(1 for item in originals for judge in item.get("judges", []) if isinstance(judge.get("assessment"), Mapping) and judge["assessment"].get("usefulness") == key)
            for key in ("useful", "not_useful", "unknown")
        }
        judge_valid = sum(item.get("valid_judge_count", 0) for item in all_items)
        classes = {key: sum(1 for item in all_items if item.get("classification") == key) for key in ("static_evidence", "executed_reproduced", "executed_refuted", "environment_blocked", "unproven")}
        automatic = sum(1 for item in omissions if self.discovery_complete and item.get("scoreable") and isinstance(item.get("synthesis"), Mapping) and item["synthesis"].get("verdict") == "supported" and item.get("relation", {}).get("is_new_candidate"))
        disagreements = sum(1 for item in all_items if item.get("judge_disagreement") or (isinstance(item.get("synthesis"), Mapping) and item["synthesis"].get("disagreement")))
        identity: dict[str, Any] = {}
        for role in ROLE_NAMES:
            records = self._role_records.get(role, [])
            observed = [
                record.get("observed_model_id")
                for record in records
                if isinstance(record.get("observed_model_id"), str) and record.get("observed_model_id") != "unavailable"
            ]
            metadata = [record.get("actual_metadata") for record in records if isinstance(record.get("actual_metadata"), Mapping)]
            estimates = [
                float(item["estimated_cost_usd"])
                for item in metadata
                if isinstance(item.get("estimated_cost_usd"), (int, float)) and not isinstance(item.get("estimated_cost_usd"), bool)
            ]
            auxiliary: dict[str, Any] = {}
            for item in metadata:
                values = item.get("auxiliary_model_usage")
                if isinstance(values, Mapping):
                    auxiliary.update(values)
            identity[role] = {
                "requested_model_id": self.models[role]["model_id"],
                "observed_model_id": observed[-1] if observed else "unavailable",
                "observed_model_ids": list(dict.fromkeys(observed)),
                "adapter": self.models[role]["adapter"],
                "family": self.models[role]["family"],
                "capability": self.preflight.get(role, {}).get("status", "unknown"),
                "estimated_cost_usd": sum(estimates) if estimates else "unknown",
                "actual_cost_usd": "unknown",
                "cost_basis": sorted({basis for item in metadata for basis in item.get("cost_basis", [])}) or "unknown",
                "auxiliary_model_usage": auxiliary,
            }
        return {
            "schema_version": CONTROLLER_VERSION,
            "state": state,
            "provenance": "model_assessment",
            "source": {
                "revision": self.snapshot["revision"],
                "base": self.snapshot["base"],
                "complete": self.snapshot["source_complete"],
                "omissions": self.snapshot["source_omissions"],
            },
            "model_assessment": {
                "original_findings": len(originals),
                "supported": verdicts["supported"],
                "refuted": verdicts["refuted"],
                "unresolved": verdicts["unknown"] + sum(1 for item in originals if not item.get("synthesis")) + execution_conflicts,
                "valid_judgments": judge_valid,
                "disagreement_items": disagreements,
                "usefulness": usefulness,
                "scoreable_items": sum(1 for item in all_items if self.discovery_complete and item.get("scoreable")),
                "estimated_cost_usd": sum(
                    item["estimated_cost_usd"]
                    for item in identity.values()
                    if isinstance(item.get("estimated_cost_usd"), (int, float))
                )
                or "unknown",
                "actual_cost_usd": "unknown",
                "execution_conflicts": execution_conflicts,
            },
            "candidate_model_assessment": {
                "assessed": len(omissions),
                "supported": sum(1 for item in omissions if isinstance(item.get("synthesis"), Mapping) and not item["synthesis"].get("execution_conflict") and item["synthesis"].get("verdict") == "supported"),
                "refuted": sum(1 for item in omissions if isinstance(item.get("synthesis"), Mapping) and not item["synthesis"].get("execution_conflict") and item["synthesis"].get("verdict") == "refuted"),
                "unresolved": sum(1 for item in omissions if not isinstance(item.get("synthesis"), Mapping) or item["synthesis"].get("execution_conflict") or item["synthesis"].get("verdict") == "unknown"),
            },
            "validation_coverage": classes,
            "omissions": {
                "candidate_count": len(omissions),
                "automatically_supported_omission": automatic,
                "human_confirmed_escape": 0,
                "human_confirmation_status": "not_collected",
                "recall_claim": "not_available",
                "unknown": len(omissions) - automatic,
            },
            "identity": identity,
            "stages": {
                "discovery": self.discovery_status,
                "source": {"status": "complete" if self.snapshot["source_complete"] else "incomplete", "omissions": self.snapshot["source_omissions"]},
                "items": {
                    item["item_id"]: {
                        "validation": item.get("validation", {}).get("status", "unknown"),
                        "classification": item.get("classification", "unproven"),
                        "judge_count": item.get("valid_judge_count", 0),
                        "synthesis": "complete" if item.get("synthesis") is not None else "incomplete",
                    }
                    for item in all_items
                },
            },
            "delivery_metrics": {"status": "not_collected; use the offline weekly report"},
        }

    @staticmethod
    def _summary_markdown(summary: Mapping[str, Any]) -> str:
        model = summary["model_assessment"]
        validation = summary["validation_coverage"]
        lines = [
            "# Model evaluation summary",
            "",
            f"- State: `{summary['state']}`",
            f"- Provenance: `{summary['provenance']}`",
            f"- Revision: `{summary['source']['revision']}`",
            f"- Base: `{summary['source']['base']}`",
            f"- Complete source scope: `{summary['source']['complete']}`",
            "",
            "## Model assessment",
            "",
            f"- Original findings: {model['original_findings']}",
            f"- Supported: {model['supported']}; refuted: {model['refuted']}; unresolved: {model['unresolved']}",
            f"- Valid judge assessments: {model['valid_judgments']}; disagreement items: {model['disagreement_items']}",
            f"- Scoreable items: {model['scoreable_items']}",
            f"- Estimated CLI cost (list-price basis): {model.get('estimated_cost_usd', 'unknown')}; actual provider cost: unknown",
            "",
            "## Validation coverage",
            "",
        ]
        for key in sorted(validation):
            lines.append(f"- {key}: {validation[key]}")
        lines += [
            "",
            "## Omissions",
            "",
            f"- Candidates: {summary['omissions']['candidate_count']}",
            f"- Automatically supported omission candidates: {summary['omissions']['automatically_supported_omission']}",
            "- Human-confirmed escapes: 0 (not collected)",
            "- No recall claim is made.",
            "",
            "## Limitations",
            "",
            "Model agreement is not correctness; provider identity and actual cost remain unknown when the CLI does not emit trusted metadata. Product delivery metrics are not collected by this command.",
            "",
        ]
        return "\n".join(lines)

    def _write_derived_json(self, path: Path, value: Any) -> None:
        """Write a derived artifact, allowing a terminal partial run to resume."""

        if path.is_file():
            existing_run = self._read_run()
            if existing_run.get("state") == "complete":
                write_json_immutable(path, value)
                return
            path.write_bytes(pretty_json_bytes(value))
            return
        write_json_immutable(path, value)

    @staticmethod
    def _flatten_item_result(item: Mapping[str, Any]) -> dict[str, Any]:
        finding = item.get("finding")
        result = dict(item)
        if isinstance(finding, Mapping):
            result = {**finding, **result}
        return result

    def _write_result_artifacts(self) -> None:
        originals = [self._flatten_item_result(item) for item in self.item_records if item.get("kind") == "original"]
        self._write_derived_json(
            self.output / "findings.json",
            {
                "schema_version": FINDINGS_VERSION,
                "input_sha256": self.snapshot["findings_sha256"],
                "findings": originals,
            },
        )
        self._write_derived_json(
            self.output / "omissions.json",
            {
                "schema_version": CONTROLLER_VERSION,
                "input_sha256": self.snapshot["findings_sha256"],
                "candidates": [self._flatten_item_result(item) for item in self.candidates],
            },
        )

    def run(self) -> dict[str, Any]:
        self.prepare()
        if self._resume_summary is not None:
            summary = self._resume_summary
            self._resume_summary = None
            return summary
        run = self._read_run()
        try:
            self.item_records = []
            self.candidates = self._run_discovery()
            for finding in self.findings:
                self.item_records.append(self._item(finding["finding_id"], finding, "original"))
            self.item_records.extend(self.candidates)
            self._write_result_artifacts()
            successful_stages = sum(1 for item in self.item_records if item.get("synthesis") is not None)
            required = len(self.item_records) * 4 + 1
            attempted = sum(
                len(item.get("judges", [])) + (1 if item.get("validation") else 0) + (1 if item.get("synthesis") is not None else 0)
                for item in self.item_records
            ) + 1
            all_validations_complete = all(
                item.get("validation", {}).get("status") == "success"
                and item.get("validation", {}).get("controls_passed") is True
                and item.get("validation", {}).get("controller_source_binding") is True
                for item in self.item_records
            )
            if not self.discovery_complete:
                state = "partial" if successful_stages or any(item.get("validation", {}).get("status") in {"success", "environment_blocked", "unproven"} for item in self.item_records) else "failed"
            elif attempted >= required and successful_stages == len(self.item_records) and all_validations_complete and self.snapshot["source_complete"]:
                state = "complete"
            elif successful_stages or any(item.get("classification") == "executed_reproduced" for item in self.item_records):
                state = "partial"
            else:
                state = "failed"
            summary = self._summary(state)
            self._write_derived_json(self.output / "summary.json", summary)
            summary_markdown = self._summary_markdown(summary).encode("utf-8")
            if (self.output / "summary.md").is_file() and self._read_run().get("state") != "complete":
                (self.output / "summary.md").write_bytes(summary_markdown)
            else:
                write_immutable(self.output / "summary.md", summary_markdown)
            run = self._read_run()
            run["state"] = state
            run["summary_sha256"] = sha256_bytes(pretty_json_bytes(summary))
            run["scope_complete"] = self.snapshot["source_complete"]
            run["actual_cost_status"] = "unknown"
            run["identity_status"] = summary.get("identity", run.get("identity_status", {}))
            self._write_run(run)
            self._refresh_artifact_hashes()
            return verify_evaluation_artifacts(self.output)
        finally:
            if self._source_temp is not None:
                self._source_temp.cleanup()
                self._source_temp = None


def oci_plan_schema() -> dict[str, Any]:
    return VALIDATION_SCHEMA


def run_evaluation(**kwargs: Any) -> dict[str, Any]:
    return EvaluationController(**kwargs).run()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Run immutable offline model evaluation")
    subparsers = parser.add_subparsers(dest="command", required=True)
    run = subparsers.add_parser("run")
    run.add_argument("--repo", required=True, type=Path)
    run.add_argument("--revision", required=True)
    run.add_argument("--base", required=True)
    run.add_argument("--findings", required=True, type=Path)
    run.add_argument("--evidence", required=True, type=Path)
    run.add_argument("--models", required=True, type=Path)
    run.add_argument("--environment", type=Path)
    run.add_argument("--output", required=True, type=Path)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        summary = run_evaluation(
            repo=args.repo,
            revision=args.revision,
            base=args.base,
            findings_path=args.findings,
            evidence_dir=args.evidence,
            models_path=args.models,
            environment_path=args.environment,
            output=args.output,
        )
    except EvaluationError as exc:
        print(f"evaluation failed: {type(exc).__name__}", file=sys.stderr)
        return 2
    print(json.dumps({"state": summary["state"], "summary": str(args.output / "summary.json")}, sort_keys=True))
    return 0 if summary["state"] in {"complete", "partial"} else 1


if __name__ == "__main__":
    raise SystemExit(main())
