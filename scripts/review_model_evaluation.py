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


def _git(repo: Path, args: Sequence[str], *, max_output: int = MAX_INPUT_BYTES) -> bytes:
    env = {
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_ATTR_NOSYSTEM": "1",
        "LC_ALL": "C",
    }
    try:
        proc = subprocess.run(
            ["git", "--no-replace-objects", "-C", str(repo), *args],
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


def _validate_clean_repo(repo: Path) -> None:
    if not repo.is_dir():
        raise EvaluationError("repository path is not a directory")
    status = _git(repo, ["status", "--porcelain=v1", "--untracked-files=all"], max_output=4 * 1024 * 1024)
    if status:
        raise EvaluationError("repository must have a clean tracked and untracked tree")


def _archive_files(repo: Path, revision: str, limits: Mapping[str, int]) -> tuple[dict[str, Any], list[str], bytes, bytes]:
    archive_bytes = _git(repo, ["archive", "--format=tar", revision], max_output=MAX_INPUT_BYTES)
    # The base diff is supplied by the caller because a revision/base pair is
    # part of the frozen identity.  Keeping archive parsing here makes the
    # regular-file completeness check independent of the host checkout.
    expected_raw = _git(repo, ["ls-tree", "-r", "-z", "--full-tree", revision], max_output=MAX_INPUT_BYTES)
    expected: list[str] = []
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
                "author_model_identity": row.get("author_model_identity", "unknown"),
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
    required = {"finding_id", "verdict", "severity", "usefulness", "citations", "counterexample"}
    if set(result) != required:
        raise EvaluationError("judge result schema is invalid")
    if result["finding_id"] != finding["finding_id"]:
        raise EvaluationError("judge returned the wrong finding id")
    if result["verdict"] not in {"support", "refute", "insufficient_evidence"}:
        raise EvaluationError("judge verdict is invalid")
    severity = _bounded_string(result["severity"], "judge severity", 128)
    if result["usefulness"] not in {"useful", "not_useful", "unknown"}:
        raise EvaluationError("judge usefulness is invalid")
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
        "rules": [
            "find new, concrete, source-cited issues",
            "do not infer recall or confirm any original finding",
            "return no finding identifier",
        ],
    }
    # A null marker is safe and makes the blindness auditable without leaking
    # a candidate's original input.  Do not put the discarded arguments in it.
    return packet


def build_judge_packet(invocation_id: str, source_packet: Mapping[str, Any], finding: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]], role: str) -> dict[str, Any]:
    refs = {key: evidence[key] for key in finding["evidence_ids"] if key in evidence}
    if len(refs) != len(set(finding["evidence_ids"])):
        raise EvaluationError("finding references missing evidence")
    return {
        "invocation_id": invocation_id,
        "role": role,
        "source_packet": source_packet,
        "finding": finding,
        "evidence": refs,
        "schema": JUDGE_SCHEMA,
        "rules": ["cite only allowed source ranges and evidence ranges", "do not execute commands", "return JSON only"],
    }


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
        "rules": ["preserve disagreement", "unknown is allowed", "do not invent evidence or promote failed controls"],
    }


def build_validation_packet(invocation_id: str, finding: Mapping[str, Any], evidence: Mapping[str, Mapping[str, Any]], source_packet: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "invocation_id": invocation_id,
        "role": "validation",
        "finding": finding,
        "evidence": {key: evidence[key] for key in finding["evidence_ids"]},
        "source_references": {"revision": source_packet.get("revision"), "paths": [finding["location"]["path"]]},
        "schema": VALIDATION_SCHEMA,
        "rules": ["return generated files only under generated/", "use two distinct literal controls", "no shell or host paths", "return JSON only"],
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
        if not isinstance(refs, list) or any(ref not in evidence for ref in refs):
            raise EvaluationError("candidate evidence reference is invalid")
        for ref in refs:
            if not any(item["path"] == path and start >= item["start"] and end <= item["end"] for item in evidence[ref].get("allowed_ranges", [])):
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


def _plan_mentions_frozen_source(plan: Mapping[str, Any], finding: Mapping[str, Any]) -> bool:
    """Reject a generated script that merely prints a claimed JSON answer.

    The OCI observation is real process output, but it is not evidence about
    the frozen revision unless the literal control reaches the read-only
    source mount (or names the cited source path).  This controller check is
    intentionally conservative; an incomplete binding remains unproven.
    """

    location = finding.get("location", {})
    source_path = location.get("path") if isinstance(location, Mapping) else None
    haystack = canonical_json({"files": plan.get("files", []), "runs": plan.get("runs", [])})
    return "/source" in haystack or (isinstance(source_path, str) and source_path in haystack)


JUDGE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["finding_id", "verdict", "severity", "usefulness", "citations", "counterexample"],
    "properties": {
        "finding_id": {"type": "string"},
        "verdict": {"enum": ["support", "refute", "insufficient_evidence"]},
        "severity": {"type": "string"},
        "usefulness": {"enum": ["useful", "not_useful", "unknown"]},
        "citations": {"type": "array"},
        "counterexample": {"type": "string"},
    },
}
DISCOVERY_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["candidates"],
    "properties": {"candidates": {"type": "array", "maxItems": 128}},
}
SYNTHESIS_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["item_id", "verdict", "citations", "disagreement", "reason"],
    "properties": {"item_id": {"type": "string"}, "verdict": {"enum": ["supported", "refuted", "unknown"]}, "citations": {"type": "array"}, "disagreement": {"type": "string"}, "reason": {"type": "string"}},
}
VALIDATION_SCHEMA: dict[str, Any] = {
    "type": "object",
    "additionalProperties": False,
    "required": ["item_id", "files", "runs", "claim_observed"],
    "properties": {"item_id": {"type": "string"}, "files": {"type": "array"}, "runs": {"type": "array", "minItems": 2, "maxItems": 2}, "claim_observed": {"type": "string"}},
}


ROLE_NAMES = ("judge_a", "judge_b", "synthesis", "discovery", "validation")
ROLE_PROMPTS = {
    "judge_a": "You are judge A. Assess only the supplied finding against the supplied frozen packet. Do not use tools. Return the requested JSON.",
    "judge_b": "You are judge B. Independently assess only the supplied finding against the supplied frozen packet. Do not use tools or infer judge A. Return the requested JSON.",
    "synthesis": "You are a fresh synthesis judge. Compare the two anonymized assessments and executed evidence. Preserve disagreement and return unknown when proof is incomplete. Do not use tools.",
    "discovery": "You are a blind discovery judge. Find additional concrete issues in the frozen packet. No original findings or assessments are available. Do not use tools.",
    "validation": "You are a validation-plan author. Return only a bounded generated reproduction plan. The controller executes it in OCI; you cannot execute commands or claim that prose is proof.",
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
        self.output = Path(output).resolve()
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
        self._source_temp: Optional[tempfile.TemporaryDirectory[str]] = None

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
        }
        if resumable:
            existing, _ = read_json(self.output / "snapshot.json")
            if existing != self.snapshot:
                raise EvaluationConflict("snapshot identity or source bytes conflict on restart")
        else:
            write_json_immutable(self.output / "snapshot.json", self.snapshot)
            write_json_immutable(self.output / "source-packet.json", self.source_packet)
            write_json_immutable(self.output / "findings.json", {"schema_version": FINDINGS_VERSION, "findings": self.findings})
        if (self.output / "source-packet.json").is_file():
            existing_packet, _ = read_json(self.output / "source-packet.json")
            if existing_packet != self.source_packet:
                raise EvaluationConflict("source packet conflicts on restart")
        self._preflight_models()
        initial_run = {
            "schema_version": CONTROLLER_VERSION,
            "input_identity": identity,
            "state": "running",
            "source_complete": self.snapshot["source_complete"],
            "preflight": self.preflight,
            "completed_invocations": [],
            "failed_invocations": [],
            "identity_status": {role: _profile_identity_status(self.models[role], self.preflight[role]) for role in ROLE_NAMES},
            "actual_cost_status": "unknown",
        }
        if (self.output / "run.json").is_file():
            existing_run, _ = read_json(self.output / "run.json")
            if existing_run.get("input_identity") != identity:
                raise EvaluationConflict("run identity conflicts on restart")
            self._verify_completed_artifacts()
        else:
            self._write_run(initial_run)
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
        if self.injected_oci_executor is None:
            self.oci_executor = oci.OCIExecutor(
                image,
                docker_executable=oci_config.get("docker_executable", "docker") if isinstance(oci_config, Mapping) else "docker",
                probe_daemon=bool(oci_config.get("probe_daemon", True)) if isinstance(oci_config, Mapping) else True,
            )
        else:
            self.oci_executor = self.injected_oci_executor
        statuses["oci"] = self.oci_executor.preflight()
        self.preflight = statuses

    @staticmethod
    def _component(value: str) -> str:
        value = re.sub(r"[^A-Za-z0-9_.-]+", "_", value)
        return value[:80] or "item"

    def _write_run(self, value: Mapping[str, Any]) -> None:
        path = self.output / "run.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        temporary = path.with_name(f".{path.name}.tmp")
        temporary.write_bytes(pretty_json_bytes(value))
        os.replace(temporary, path)

    def _read_run(self) -> dict[str, Any]:
        value, _ = read_json(self.output / "run.json")
        if not isinstance(value, Mapping) or value.get("input_identity") != self.snapshot["input_identity"]:
            raise EvaluationConflict("run artifact identity is invalid")
        return dict(value)

    def _verify_completed_artifacts(self) -> None:
        run = self._read_run()
        for relative, digest in run.get("artifact_hashes", {}).items():
            if not isinstance(relative, str) or not isinstance(digest, str) or not HEX_SHA256.fullmatch(digest):
                raise EvaluationConflict("run contains an invalid artifact hash")
            path = (self.output / relative).resolve()
            if not _under(path, self.output) or not path.is_file() or sha256_bytes(path.read_bytes()) != digest:
                raise EvaluationConflict(f"artifact bytes conflict: {relative}")
        invocation_ids = {
            entry.get("id")
            for entry in run.get("completed_invocations", [])
            if isinstance(entry, Mapping) and isinstance(entry.get("id"), str)
        }
        invocation_root = self.output / "invocations"
        if invocation_root.is_dir():
            for directory in invocation_root.iterdir():
                if directory.is_dir() and (directory / "result.json").is_file():
                    value, _ = read_json(directory / "result.json")
                    if isinstance(value, Mapping) and value.get("status") == "success":
                        invocation_ids.add(directory.name)
        for invocation_id in sorted(invocation_ids):
            if not isinstance(invocation_id, str):
                raise EvaluationConflict("run has an invalid completed invocation id")
            packet_path = self._invocation_paths(invocation_id) / "packet.json"
            if not packet_path.is_file():
                raise EvaluationConflict(f"completed invocation packet is missing: {invocation_id}")
            packet_record, _ = read_json(packet_path)
            if not isinstance(packet_record, Mapping) or not isinstance(packet_record.get("packet"), Mapping):
                raise EvaluationConflict(f"completed invocation packet is invalid: {invocation_id}")
            self._completed_invocation(invocation_id, packet_record["packet"])

    def _invocation_paths(self, invocation_id: str) -> Path:
        return self.output / "invocations" / self._component(invocation_id)

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
        packet_hash = sha256_bytes(canonical_json(packet).encode("utf-8"))
        if result.get("packet_sha256") != packet_hash:
            raise EvaluationConflict(f"completed invocation packet conflict: {invocation_id}")
        if result.get("status") == "success":
            packet_path = directory / "packet.json"
            if not packet_path.is_file() or sha256_bytes(packet_path.read_bytes()) != result.get("packet_artifact_sha256"):
                raise EvaluationConflict(f"completed invocation bytes conflict: {invocation_id}/packet.json")
            for filename, key in (("raw.stdout", "stdout_sha256"), ("raw.stderr", "stderr_sha256"), ("final.json", "final_sha256"), ("argv.json", "argv_sha256")):
                file_path = directory / filename
                if not file_path.is_file() or sha256_bytes(file_path.read_bytes()) != result.get(key):
                    raise EvaluationConflict(f"completed invocation bytes conflict: {invocation_id}/{filename}")
            final, _ = read_json(directory / "final.json")
            result = dict(result)
            result["structured_output"] = final
        return dict(result)

    def _invoke(self, role: str, invocation_id: str, packet: Mapping[str, Any], schema: Mapping[str, Any]) -> dict[str, Any]:
        directory = self._invocation_paths(invocation_id)
        directory.mkdir(parents=True, exist_ok=True)
        packet_hash = sha256_bytes(canonical_json(packet).encode("utf-8"))
        completed = self._completed_invocation(invocation_id, packet)
        if completed is not None:
            return completed
        profile = self.models[role]
        adapter = adapters.adapter_for_profile(profile, runner=self.adapter_runner)
        if role == "validation":
            output_schema = oci_plan_schema()
        else:
            output_schema = schema
        packet_record = {"packet_sha256": packet_hash, "packet": packet, "schema": output_schema}
        write_json_immutable(directory / "packet.json", packet_record)
        if isinstance(adapter, adapters.ClaudeAdapter):
            argv = adapter.build_argv(profile["executable"], profile["model_id"], output_schema, ROLE_PROMPTS[role])
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
                "failure_class": failure_class,
                "reason": probe.get("reason", "adapter_not_ready"),
                "packet_sha256": packet_hash,
                "argv_sha256": argv_hash,
                "attempts": 0,
            }
            write_immutable(directory / "raw.stdout", b"")
            write_immutable(directory / "raw.stderr", str(failure["reason"]).encode("utf-8"))
            write_json_immutable(directory / "result.json", failure)
            return failure
        try:
            with tempfile.TemporaryDirectory(prefix=f"openab-eval-{self._component(invocation_id)}-") as session:
                if isinstance(adapter, adapters.ClaudeAdapter):
                    response = adapter.invoke(packet, output_schema, profile["model_id"], system_prompt=ROLE_PROMPTS[role], session_dir=Path(session))
                else:
                    response = adapter.invoke(packet, output_schema, profile["model_id"], session_dir=Path(session))
        except adapters.AdapterError as exc:
            raw_error = str(exc).encode("utf-8", "replace")[:64 * 1024]
            failure = {"status": "failed", "failure_class": "invocation_failed", "reason": type(exc).__name__, "packet_sha256": packet_hash, "argv_sha256": argv_hash, "attempts": 1}
            write_immutable(directory / "raw.stdout", b"")
            write_immutable(directory / "raw.stderr", raw_error)
            write_json_immutable(directory / "result.json", failure)
            return failure
        write_immutable(directory / "raw.stdout", response.raw_stdout)
        write_immutable(directory / "raw.stderr", response.raw_stderr)
        write_json_immutable(directory / "final.json", response.structured_output)
        result = {
            "status": "success",
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
        }
        write_json_immutable(directory / "result.json", result)
        result["structured_output"] = response.structured_output
        return result

    def _update_run_invocation(self, invocation_id: str, record: Mapping[str, Any]) -> None:
        run = self._read_run()
        completed = list(run.get("completed_invocations", []))
        failed = list(run.get("failed_invocations", []))
        entry = {"id": invocation_id, "status": record.get("status"), "failure_class": record.get("failure_class")}
        target = completed if record.get("status") == "success" else failed
        if not any(item.get("id") == invocation_id for item in target):
            target.append(entry)
        run["completed_invocations"] = sorted(completed, key=lambda item: item["id"])
        run["failed_invocations"] = sorted(failed, key=lambda item: item["id"])
        self._write_run(run)

    def _source_lines_for(self) -> dict[str, int]:
        return _source_lines(self.source_packet)

    def _run_judges(self, item_id: str, finding: Mapping[str, Any]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
        valid: list[dict[str, Any]] = []
        raw_records: list[dict[str, Any]] = []
        for role in ("judge_a", "judge_b"):
            invocation_id = f"{role}-{item_id}"
            packet = build_judge_packet(invocation_id, self.source_packet, finding, self.evidence, role)
            record = self._invoke(role, invocation_id, packet, JUDGE_SCHEMA)
            self._update_run_invocation(invocation_id, record)
            raw_records.append({"role": role, "invocation_id": invocation_id, "status": record.get("status"), "failure_class": record.get("failure_class")})
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
        record = self._invoke("validation", invocation_id, packet, VALIDATION_SCHEMA)
        self._update_run_invocation(invocation_id, record)
        if record.get("status") != "success":
            return {"status": record.get("failure_class", "unproven"), "classification": record.get("failure_class", "unproven"), "invocation_id": invocation_id}
        try:
            plan = oci.validate_generated_plan(record["structured_output"], set(self.evidence))
        except Exception as exc:
            return {"status": "unproven", "classification": "unproven", "reason": type(exc).__name__, "invocation_id": invocation_id}
        if not _plan_mentions_frozen_source(plan, finding):
            return {"status": "unproven", "classification": "unproven", "reason": "generated_plan_not_bound_to_source", "invocation_id": invocation_id}
        if self._source_temp is None:
            raise EvaluationError("source tree was not prepared")
        item_dir = self.output / "validation" / self._component(item_id)
        item_dir.mkdir(parents=True, exist_ok=True)
        write_json_immutable(item_dir / "plan.json", plan)
        write_json_immutable(item_dir / "generated-file-digests.json", [{"path": item["path"], "sha256": item["sha256"], "bytes": item["bytes"]} for item in plan["files"]])
        oci_config = self.environment.get("oci", {}) if isinstance(self.environment.get("oci", {}), Mapping) else {}
        operator_checks = self.environment.get("operator_checks", oci_config.get("operator_checks", []))
        if operator_checks is not None and not isinstance(operator_checks, list):
            operator_checks = []
        execution = self.oci_executor.execute(plan, Path(self._source_temp.name), evidence_ids=set(self.evidence), item_dir=item_dir, operator_checks=operator_checks)
        for observed in execution.get("runs", []):
            if isinstance(observed, Mapping) and isinstance(observed.get("name"), str):
                write_json_immutable(item_dir / "runs" / f"{self._component(observed['name'])}.json", observed)
        return {"status": execution.get("status"), "classification": execution.get("classification", "unproven"), "invocation_id": invocation_id, "execution": execution}

    def _run_synthesis(self, item_id: str, finding: Mapping[str, Any], judgments: Sequence[Mapping[str, Any]], executed: Mapping[str, Any]) -> Optional[dict[str, Any]]:
        if len(judgments) != 2:
            return None
        invocation_id = f"synthesis-{item_id}"
        packet = build_synthesis_packet(invocation_id, self.source_packet, finding, self.evidence, executed, judgments)
        record = self._invoke("synthesis", invocation_id, packet, SYNTHESIS_SCHEMA)
        self._update_run_invocation(invocation_id, record)
        if record.get("status") != "success":
            return None
        try:
            return _validate_synthesis_result(record["structured_output"], item_id, finding, self.evidence, self._source_lines_for())
        except EvaluationError:
            return None

    def _item(self, item_id: str, finding: Mapping[str, Any], kind: str, *, relation: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
        judgments, raw_judges = self._run_judges(item_id, finding)
        validation = self._run_validation(item_id, finding)
        synthesis = self._run_synthesis(item_id, finding, judgments, validation)
        classification = validation.get("classification", "unproven")
        if classification not in {"static_evidence", "executed_reproduced", "executed_refuted", "environment_blocked", "unproven"}:
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
                and len(judgments) == 2
                and synthesis is not None
                and synthesis.get("verdict") in {"supported", "refuted"}
                and classification.startswith("executed_")
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
        self._update_run_invocation(invocation_id, record)
        if record.get("status") != "success":
            return []
        try:
            raw_candidates = _validate_discovery_result(record["structured_output"], self.evidence, self._source_lines_for())
        except EvaluationError:
            return []
        self.discovery_complete = True
        candidates: list[dict[str, Any]] = []
        for index, candidate in enumerate(raw_candidates):
            relation = self._overlap_relation(candidate)
            item_id = self._candidate_id(candidate, index)
            finding = {
                "finding_id": item_id,
                "title": "blind discovery candidate",
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
        synth = [item["synthesis"] for item in all_items if isinstance(item.get("synthesis"), Mapping)]
        original_synth = [item["synthesis"] for item in originals if isinstance(item.get("synthesis"), Mapping)]
        verdicts = {key: sum(1 for item in original_synth if item.get("verdict") == key) for key in ("supported", "refuted", "unknown")}
        usefulness = {
            key: sum(1 for item in originals for judge in item.get("judges", []) if isinstance(judge.get("assessment"), Mapping) and judge["assessment"].get("usefulness") == key)
            for key in ("useful", "not_useful", "unknown")
        }
        judge_valid = sum(item.get("valid_judge_count", 0) for item in all_items)
        classes = {key: sum(1 for item in all_items if item.get("classification") == key) for key in ("static_evidence", "executed_reproduced", "executed_refuted", "environment_blocked", "unproven")}
        automatic = sum(1 for item in omissions if self.discovery_complete and item.get("scoreable") and isinstance(item.get("synthesis"), Mapping) and item["synthesis"].get("verdict") == "supported" and item.get("relation", {}).get("is_new_candidate"))
        disagreements = sum(1 for item in all_items if item.get("judge_disagreement") or (isinstance(item.get("synthesis"), Mapping) and item["synthesis"].get("disagreement")))
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
                "unresolved": verdicts["unknown"] + sum(1 for item in originals if not item.get("synthesis")),
                "valid_judgments": judge_valid,
                "disagreement_items": disagreements,
                "usefulness": usefulness,
                "scoreable_items": sum(1 for item in all_items if self.discovery_complete and item.get("scoreable")),
                "actual_cost": "unknown",
            },
            "candidate_model_assessment": {
                "assessed": len(omissions),
                "supported": sum(1 for item in omissions if isinstance(item.get("synthesis"), Mapping) and item["synthesis"].get("verdict") == "supported"),
                "refuted": sum(1 for item in omissions if isinstance(item.get("synthesis"), Mapping) and item["synthesis"].get("verdict") == "refuted"),
                "unresolved": sum(1 for item in omissions if not isinstance(item.get("synthesis"), Mapping) or item["synthesis"].get("verdict") == "unknown"),
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
            "identity": {role: {"requested_model_id": self.models[role]["model_id"], "observed_model_id": "unavailable"} for role in ROLE_NAMES},
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

    def run(self) -> dict[str, Any]:
        self.prepare()
        run = self._read_run()
        if run.get("state") == "complete" and (self.output / "summary.json").is_file():
            try:
                summary, _ = read_json(self.output / "summary.json")
                if sha256_bytes(pretty_json_bytes(summary)) != run.get("summary_sha256"):
                    raise EvaluationConflict("completed summary bytes conflict on restart")
                return summary
            finally:
                if self._source_temp is not None:
                    self._source_temp.cleanup()
                    self._source_temp = None
        try:
            self.item_records = []
            for finding in self.findings:
                self.item_records.append(self._item(finding["finding_id"], finding, "original"))
            self.candidates = self._run_discovery()
            self.item_records.extend(self.candidates)
            write_json_immutable(self.output / "omissions.json", {"schema_version": CONTROLLER_VERSION, "candidates": self.candidates})
            successful_stages = sum(1 for item in self.item_records if item.get("synthesis") is not None)
            required = len(self.item_records) * 4 + 1
            attempted = sum(
                len(item.get("judges", [])) + (1 if item.get("validation") else 0) + (1 if item.get("synthesis") is not None else 0)
                for item in self.item_records
            ) + 1
            if not self.discovery_complete:
                state = "failed"
            elif attempted >= required and successful_stages == len(self.item_records) and self.snapshot["source_complete"]:
                state = "complete"
            elif successful_stages or any(item.get("classification") == "executed_reproduced" for item in self.item_records):
                state = "partial"
            else:
                state = "failed"
            summary = self._summary(state)
            write_json_immutable(self.output / "summary.json", summary)
            write_immutable(self.output / "summary.md", self._summary_markdown(summary).encode("utf-8"))
            run = self._read_run()
            run["state"] = state
            run["summary_sha256"] = sha256_bytes(pretty_json_bytes(summary))
            run["scope_complete"] = self.snapshot["source_complete"]
            run["actual_cost_status"] = "unknown"
            artifact_hashes = {}
            for path in sorted(self.output.rglob("*")):
                if path.is_file() and path.name != "run.json" and not path.name.endswith(".tmp"):
                    artifact_hashes[str(path.relative_to(self.output))] = sha256_bytes(path.read_bytes())
            run["artifact_hashes"] = artifact_hashes
            self._write_run(run)
            return summary
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
