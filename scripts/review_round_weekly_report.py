#!/usr/bin/env python3
"""Build a deterministic, offline weekly report from captured evidence.

The report is deliberately a read-only observer. It reads the operator's
hash-bound bundle and, optionally, a hash-verified model-evaluation summary
through ``verify_evaluation_artifacts``. It never contacts a service, invokes
a model, reads provider credentials, or changes controller state.
"""

from __future__ import annotations

import argparse
import hashlib
import html
import json
import re
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Optional, Sequence
from zoneinfo import ZoneInfo


DEFINITION_VERSION = "review-round-weekly-report/v1"
MANIFEST_VERSION = "review-round-weekly-evidence/v1"
TAIPEI = ZoneInfo("Asia/Taipei")
MAX_FILE_BYTES = 128 * 1024 * 1024
FULL_SHA = re.compile(r"^[0-9a-fA-F]{40}$")
HEX_SHA256 = re.compile(r"^[0-9a-f]{64}$")
WEEK = re.compile(r"^[0-9]{4}-W[0-9]{2}$")

PRODUCT_TABLES = (
    "session_targets",
    "review_rounds",
    "review_findings",
    "github_writes",
    "runtime_event_receipts",
)
OPTIONAL_FILES = ("human.ndjson", "cost.ndjson")
AUDIT_OUTCOMES = {
    "pending",
    "accepted",
    "ignored",
    "denied",
    "succeeded",
    "failed",
    "retry_scheduled",
    "outcome_unknown",
    "reconciled",
}
TERMINAL_KINDS = {"comment", "status", "review", "comment_abandon"}
DECISION_KINDS = ("decision_status", "decision_review", "decision_comment")
DIAGNOSTIC_DISPOSITIONS = {
    "unparseable",
    "insufficient_valid_reviewers",
    "missing_target",
    "invalid_target",
    "missing_reviewed_sha",
    "invalid_reviewed_sha",
    "reviewed_sha_mismatch",
}
STATUS_CONTEXT = "openab/council"
RELIABILITY_CATEGORIES = ("reliable", "visible_failure", "superseded", "pending_or_unknown")
HUMAN_VERDICTS = ("valid_useful", "valid_not_useful", "invalid", "unknown")
ESCAPE_STATUSES = ("confirmed_escape", "not_escape", "unknown")
VALIDATION_CLASSES = (
    "static_evidence",
    "executed_reproduced",
    "executed_refuted",
    "environment_blocked",
    "unproven",
)

PRODUCT_COLUMNS = {
    "session_targets": {
        "session_id", "repo", "pr_number", "head_sha", "created_at", "reason", "required_valid_reviewers"
    },
    "review_rounds": {
        "id", "repo", "pr_number", "round", "session_id", "head_sha", "comment_id", "decision",
        "red", "yellow", "green", "created_at", "verified_commit_id", "integrity_disposition"
    },
    "review_findings": {
        "id", "session_id", "repo", "pr_number", "stable_id", "severity", "status", "head_sha",
        "created_at", "raised_by", "angle"
    },
    "github_writes": {
        "id", "session_id", "kind", "payload_json", "state", "attempts", "created_at", "claimed_at", "done_at"
    },
    "runtime_event_receipts": {
        "event_id", "body_sha256", "event_type", "session_id", "occurred_at", "received_at"
    },
}


class ReportError(RuntimeError):
    """The captured input cannot support the requested report."""


class EvaluationDependencyError(ReportError):
    """The concurrent core verifier is not available in this checkout."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def pretty_json(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ReportError(f"duplicate JSON key in source object: {key}")
        result[key] = value
    return result


def _read_bytes(path: Path) -> bytes:
    if path.is_symlink() or not path.is_file():
        raise ReportError(f"source path is not a regular file: {path.name}")
    raw = path.read_bytes()
    if len(raw) > MAX_FILE_BYTES:
        raise ReportError(f"source file exceeds the bound: {path.name}")
    return raw


def _read_json(path: Path) -> tuple[Any, bytes]:
    raw = _read_bytes(path)
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_reject_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ReportError(f"invalid JSON source: {path.name}") from exc
    return value, raw


def _read_ndjson(path: Path, *, required: bool) -> tuple[list[dict[str, Any]], bytes]:
    if not path.exists():
        if required:
            raise ReportError(f"missing required source file: {path.name}")
        return [], b""
    raw = _read_bytes(path)
    rows: list[dict[str, Any]] = []
    for number, line in enumerate(raw.splitlines(), 1):
        if not line.strip():
            continue
        try:
            value = json.loads(line.decode("utf-8"), object_pairs_hook=_reject_duplicate_keys)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ReportError(f"invalid {path.name} record {number}") from exc
        if not isinstance(value, dict):
            raise ReportError(f"{path.name} record {number} is not an object")
        rows.append(value)
    return rows, raw


def _parse_datetime(value: Any, field: str) -> datetime:
    if not isinstance(value, str) or not value:
        raise ReportError(f"{field} must be an offset-bearing ISO timestamp")
    text = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError as exc:
        raise ReportError(f"{field} is not an ISO timestamp") from exc
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ReportError(f"{field} must include an offset")
    return parsed


def _iso_week(value: datetime) -> str:
    iso = value.astimezone(TAIPEI).isocalendar()
    return f"{iso.year:04d}-W{iso.week:02d}"


def _safe_ref(value: Any, field: str, maximum: int = 256) -> str:
    if not isinstance(value, (str, int)) or isinstance(value, bool):
        raise ReportError(f"{field} is not a bounded source reference")
    text = str(value)
    if len(text.encode("utf-8")) > maximum:
        text = text.encode("utf-8")[:maximum].decode("utf-8", "ignore")
    text = text.replace("\r", "\\r").replace("\n", "\\n").replace("\t", "\\t")
    return html.escape(text, quote=True)


def _safe_optional(value: Any, field: str, maximum: int = 256) -> Optional[str]:
    if value is None:
        return None
    return _safe_ref(value, field, maximum)


def _require_string(value: Any, field: str, maximum: int = 4096) -> str:
    if not isinstance(value, str) or not value or len(value.encode("utf-8")) > maximum:
        raise ReportError(f"{field} must be bounded UTF-8 text")
    if "\x00" in value:
        raise ReportError(f"{field} contains NUL")
    return value


def _require_int(value: Any, field: str, *, allow_none: bool = False) -> Optional[int]:
    if allow_none and value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise ReportError(f"{field} must be an integer")
    return value


def _full_sha(value: Any) -> Optional[str]:
    if isinstance(value, str) and FULL_SHA.fullmatch(value):
        return value.lower()
    return None


def _sha256(value: Any) -> Optional[str]:
    if isinstance(value, str) and HEX_SHA256.fullmatch(value):
        return value
    return None


def _manifest_file_entry(manifest: Mapping[str, Any], name: str) -> Optional[Mapping[str, Any]]:
    files = manifest.get("files")
    if not isinstance(files, Mapping):
        return None
    entry = files.get(name)
    return entry if isinstance(entry, Mapping) else None


def _verify_manifest_file(
    manifest: Mapping[str, Any], name: str, raw: bytes, record_count: Optional[int] = None
) -> None:
    entry = _manifest_file_entry(manifest, name)
    if entry is None:
        raise ReportError(f"manifest lacks file hash for {name}")
    digest = entry.get("sha256")
    if _sha256(digest) != sha256_bytes(raw):
        raise ReportError(f"manifest hash mismatch for {name}")
    if record_count is not None and entry.get("record_count") != record_count:
        raise ReportError(f"manifest record count mismatch for {name}")


def _coverage_value(value: Any, field: str) -> str:
    if value not in {"complete", "partial", "unknown"}:
        raise ReportError(f"{field} must be complete, partial, or unknown")
    return value


def _source_span_entry(manifest: Mapping[str, Any], source: str) -> Mapping[str, Any]:
    sources = manifest.get("sources")
    if isinstance(sources, Mapping) and isinstance(sources.get(source), Mapping):
        return sources[source]
    spans = manifest.get("capture_spans")
    if isinstance(spans, Mapping) and isinstance(spans.get(source), Mapping):
        entry = dict(spans[source])
        coverage = manifest.get("coverage")
        if isinstance(coverage, Mapping):
            if source in PRODUCT_TABLES:
                table_coverage = coverage.get("product_tables")
                if isinstance(table_coverage, Mapping):
                    entry.setdefault("coverage", table_coverage.get(source))
            else:
                entry.setdefault("coverage", coverage.get(source))
        return entry
    raise ReportError(f"manifest lacks capture span for {source}")


def _source_coverage(manifest: Mapping[str, Any], source: str) -> str:
    entry = _source_span_entry(manifest, source)
    if "coverage" in entry:
        return _coverage_value(entry["coverage"], f"sources.{source}.coverage")
    coverage = manifest.get("coverage")
    if not isinstance(coverage, Mapping):
        raise ReportError("manifest coverage is missing")
    if source in PRODUCT_TABLES:
        table_coverage = coverage.get("product_tables")
        if isinstance(table_coverage, Mapping) and source in table_coverage:
            return _coverage_value(table_coverage[source], f"coverage.product_tables.{source}")
        product = coverage.get("product")
        if isinstance(product, Mapping) and source in product:
            return _coverage_value(product[source], f"coverage.product.{source}")
    if source in coverage:
        return _coverage_value(coverage[source], f"coverage.{source}")
    raise ReportError(f"manifest lacks coverage for {source}")


def _validate_capture_manifest(manifest: Mapping[str, Any]) -> dict[str, str]:
    if manifest.get("schema_version") != MANIFEST_VERSION:
        raise ReportError("unsupported evidence manifest version")
    _require_string(manifest.get("bundle_id"), "manifest.bundle_id", 256)
    if manifest.get("timezone") != "Asia/Taipei":
        raise ReportError("manifest timezone must be Asia/Taipei")
    top_coverage = manifest.get("coverage")
    if not isinstance(top_coverage, Mapping):
        raise ReportError("manifest coverage is missing")
    for metric in ("audit", "product", "human", "cost"):
        if metric not in top_coverage:
            raise ReportError(f"manifest coverage lacks {metric}")
        _coverage_value(top_coverage[metric], f"coverage.{metric}")
    table_coverage = top_coverage.get("product_tables")
    if not isinstance(table_coverage, Mapping):
        product_map = top_coverage.get("product")
        table_coverage = product_map if isinstance(product_map, Mapping) else None
    if not isinstance(table_coverage, Mapping) or any(name not in table_coverage for name in PRODUCT_TABLES):
        raise ReportError("manifest must declare coverage for every product table")

    rendered_spans: dict[str, str] = {}
    for source in ("audit", *PRODUCT_TABLES, "human", "cost"):
        entry = _source_span_entry(manifest, source)
        start_value = entry.get("start", entry.get("capture_start"))
        end_value = entry.get("end", entry.get("capture_end"))
        start = _parse_datetime(start_value, f"sources.{source}.start")
        end = _parse_datetime(end_value, f"sources.{source}.end")
        if end < start:
            raise ReportError(f"capture span is reversed for {source}")
        rendered_spans[source] = f"{start.isoformat()}..{end.isoformat()}"
        _source_coverage(manifest, source)

    cursor = manifest.get("audit_cursor")
    audit_coverage = _source_coverage(manifest, "audit")
    if not isinstance(cursor, Mapping):
        if audit_coverage == "complete":
            raise ReportError("complete audit coverage requires audit_cursor")
    else:
        if "first_cursor" not in cursor or "last_cursor" not in cursor:
            raise ReportError("audit_cursor lacks first_cursor/last_cursor")
        page_count = cursor.get("page_count")
        if isinstance(page_count, bool) or not isinstance(page_count, int) or page_count < 0:
            raise ReportError("audit_cursor.page_count must be a non-negative integer")
        if audit_coverage == "complete" and (page_count < 1 or cursor.get("final_null_cursor") is not True):
            raise ReportError("complete audit coverage requires final null cursor")
    return rendered_spans


def _validate_manifest_snapshot(manifest: Mapping[str, Any], as_of: datetime) -> None:
    manifest_snapshot = _parse_datetime(manifest.get("snapshot_at"), "manifest.snapshot_at")
    if manifest_snapshot != as_of:
        raise ReportError("manifest snapshot_at does not equal --as-of")


def _validate_audit_event(event: Mapping[str, Any]) -> None:
    required = {
        "seq", "version", "event_id", "event_key", "occurred_at", "recorded_at", "service", "kind",
        "outcome", "correlation", "detail"
    }
    if not required.issubset(event):
        raise ReportError("audit record lacks a required envelope field")
    _require_int(event["seq"], "audit.seq")
    if event["version"] != 1:
        raise ReportError("unsupported audit event version")
    for field in ("event_id", "event_key", "service", "kind"):
        text = _require_string(event[field], f"audit.{field}", 256)
        if field in {"event_key", "kind"} and text != text.strip():
            raise ReportError(f"audit.{field} has surrounding whitespace")
    _require_int(event["occurred_at"], "audit.occurred_at")
    _require_int(event["recorded_at"], "audit.recorded_at")
    if event["outcome"] not in AUDIT_OUTCOMES:
        raise ReportError("audit outcome is not a source AuditOutcome")
    if not isinstance(event["correlation"], Mapping):
        raise ReportError("audit correlation must be an object")


def _event_material(event: Mapping[str, Any]) -> str:
    return canonical_json({
        key: event.get(key)
        for key in ("version", "service", "kind", "outcome", "caused_by", "correlation", "actor", "target", "detail", "error")
    })


def _dedupe_audit(rows: Sequence[Mapping[str, Any]]) -> tuple[list[dict[str, Any]], set[str]]:
    by_event: dict[str, Mapping[str, Any]] = {}
    by_key: dict[tuple[str, str], Mapping[str, Any]] = {}
    conflicts: set[str] = set()
    for row in rows:
        _validate_audit_event(row)
        event_id = row["event_id"]
        service_key = (row["service"], row["event_key"])
        existing = by_event.get(event_id)
        if existing is not None and _event_material(existing) != _event_material(row):
            conflicts.add(f"event:{event_id}")
        elif existing is None:
            by_event[event_id] = row
        existing_key = by_key.get(service_key)
        if existing_key is not None and _event_material(existing_key) != _event_material(row):
            conflicts.add(f"key:{service_key[0]}:{service_key[1]}")
        elif existing_key is None:
            by_key[service_key] = row
    result = []
    for row in by_event.values():
        if f"event:{row['event_id']}" in conflicts or f"key:{row['service']}:{row['event_key']}" in conflicts:
            continue
        result.append(dict(row))
    return sorted(result, key=lambda item: (item["recorded_at"], item["seq"], item["event_id"])), conflicts


def _filter_audit(rows: Sequence[Mapping[str, Any]], cutoff_ms: int) -> tuple[list[dict[str, Any]], int, set[str]]:
    deduped, conflicts = _dedupe_audit(rows)
    future = sum(1 for row in deduped if row["recorded_at"] > cutoff_ms)
    return [row for row in deduped if row["recorded_at"] <= cutoff_ms], future, conflicts


def _validate_product_tables(product: Any) -> dict[str, list[dict[str, Any]]]:
    if not isinstance(product, Mapping):
        raise ReportError("product.json must be an object of source tables")
    result: dict[str, list[dict[str, Any]]] = {}
    for table in PRODUCT_TABLES:
        rows = product.get(table)
        if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
            raise ReportError(f"product table {table} must be an array of objects")
        table_rows: list[dict[str, Any]] = []
        for row in rows:
            if not PRODUCT_COLUMNS[table].issubset(row):
                raise ReportError(f"product table {table} row lacks an exported source column")
            if "terminal_event" in row or "terminal_branch" in row:
                raise ReportError("product input may not invent terminal branch fields")
            table_rows.append(dict(row))
        result[table] = table_rows
    return result


def _dedupe_index(
    rows: Sequence[Mapping[str, Any]], key: Callable[[Mapping[str, Any]], Any], label: str
) -> tuple[dict[Any, dict[str, Any]], set[Any]]:
    index: dict[Any, dict[str, Any]] = {}
    conflicts: set[Any] = set()
    for row in rows:
        identity = key(row)
        if identity is None:
            raise ReportError(f"{label} row lacks its source identity")
        previous = index.get(identity)
        if previous is None:
            index[identity] = dict(row)
        elif canonical_json(previous) != canonical_json(row):
            conflicts.add(identity)
    return index, conflicts


def _dedupe_product_tables(
    tables: Mapping[str, Sequence[Mapping[str, Any]]],
) -> tuple[dict[str, list[dict[str, Any]]], dict[str, dict[Any, dict[str, Any]]], dict[str, set[Any]]]:
    identities = {
        "session_targets": lambda row: row.get("session_id"),
        "review_rounds": lambda row: row.get("session_id"),
        "review_findings": lambda row: row.get("id"),
        "github_writes": lambda row: row.get("id"),
        "runtime_event_receipts": lambda row: row.get("event_id"),
    }
    clean: dict[str, list[dict[str, Any]]] = {}
    indexes: dict[str, dict[Any, dict[str, Any]]] = {}
    conflicts: dict[str, set[Any]] = {}
    for table in PRODUCT_TABLES:
        index, table_conflicts = _dedupe_index(tables[table], identities[table], table)
        clean[table] = list(index.values())
        indexes[table] = index
        conflicts[table] = table_conflicts
    natural: dict[tuple[Any, Any], dict[str, Any]] = {}
    for row in clean["github_writes"]:
        pair = (row.get("session_id"), row.get("kind"))
        previous = natural.get(pair)
        if previous is not None and canonical_json(previous) != canonical_json(row):
            conflicts["github_writes"].add(pair)
        else:
            natural[pair] = row
    return clean, indexes, conflicts


def _validate_product_units(tables: Mapping[str, Sequence[Mapping[str, Any]]]) -> None:
    for row in tables["session_targets"]:
        _require_int(row["pr_number"], "session_targets.pr_number")
        _require_int(row["created_at"], "session_targets.created_at")
    for row in tables["review_rounds"]:
        for field in ("id", "pr_number", "round", "red", "yellow", "green", "created_at"):
            _require_int(row[field], f"review_rounds.{field}")
    for row in tables["review_findings"]:
        _require_int(row["id"], "review_findings.id")
        _require_int(row["pr_number"], "review_findings.pr_number", allow_none=True)
        _require_int(row["created_at"], "review_findings.created_at")
    for row in tables["github_writes"]:
        for field in ("id", "attempts", "created_at"):
            _require_int(row[field], f"github_writes.{field}")
        _require_int(row["claimed_at"], "github_writes.claimed_at", allow_none=True)
        _require_int(row["done_at"], "github_writes.done_at", allow_none=True)
        _require_string(row["session_id"], "github_writes.session_id", 256)
        _require_string(row["kind"], "github_writes.kind", 256)
        if not isinstance(row["payload_json"], str):
            raise ReportError("github_writes.payload_json must preserve source text")
    for row in tables["runtime_event_receipts"]:
        _require_int(row["occurred_at"], "runtime_event_receipts.occurred_at")
        _require_int(row["received_at"], "runtime_event_receipts.received_at")


def _runtime_receipts(
    rows: Sequence[Mapping[str, Any]], cutoff_seconds: float
) -> tuple[dict[str, list[dict[str, Any]]], int]:
    result: dict[str, list[dict[str, Any]]] = defaultdict(list)
    future = 0
    for row in rows:
        received = row["received_at"]
        if received < 0:
            continue
        if received > cutoff_seconds:
            future += 1
            continue
        session = row.get("session_id")
        if isinstance(session, str) and session:
            result[session].append(dict(row))
    return result, future


def _correlation_key(event: Mapping[str, Any]) -> Optional[tuple[str, str, str]]:
    correlation = event.get("correlation")
    if not isinstance(correlation, Mapping):
        return None
    values = []
    for name in ("delivery_id", "action_id", "trigger_ref"):
        value = correlation.get(name)
        if not isinstance(value, str) or not value:
            return None
        values.append(value)
    return tuple(values)  # type: ignore[return-value]


def _correlation_session(event: Mapping[str, Any]) -> Optional[str]:
    correlation = event.get("correlation")
    if not isinstance(correlation, Mapping):
        return None
    value = correlation.get("session_id")
    return value if isinstance(value, str) and value else None


def _completed_actions(
    audit: Sequence[Mapping[str, Any]],
) -> tuple[dict[tuple[str, str, str], str], set[tuple[str, str, str]]]:
    by_key: dict[tuple[str, str, str], tuple[str, str]] = {}
    conflicts: set[tuple[str, str, str]] = set()
    for event in audit:
        if event["kind"] != "action.completed" or event["outcome"] != "succeeded":
            continue
        key = _correlation_key(event)
        session = _correlation_session(event)
        if key is None or session is None:
            continue
        material = _event_material(event)
        previous = by_key.get(key)
        if previous is None:
            by_key[key] = (session, material)
        elif previous != (session, material):
            conflicts.add(key)
    return {key: value[0] for key, value in by_key.items() if key not in conflicts}, conflicts


def _accepted_candidates(
    audit: Sequence[Mapping[str, Any]],
    week: str,
) -> tuple[list[dict[str, Any]], int]:
    completed, completed_conflicts = _completed_actions(audit)
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    unresolved_by_key: dict[Any, dict[str, Any]] = {}
    for event in audit:
        if event["kind"] != "action.accepted" or event["outcome"] != "accepted":
            continue
        key = _correlation_key(event)
        session = completed.get(key) if key is not None and key not in completed_conflicts else None
        candidate = {
            "event": event,
            "session_id": session,
            "completed": session is not None,
            "join_conflict": key in completed_conflicts if key is not None else False,
            "correlation_key": key,
        }
        if session is not None:
            grouped[session].append(candidate)
        else:
            natural = ("correlation", key) if key is not None else ("event", event["event_id"])
            previous = unresolved_by_key.get(natural)
            if previous is None:
                unresolved_by_key[natural] = candidate
            elif _event_material(previous["event"]) != _event_material(event):
                previous["join_conflict"] = True
    selected: list[dict[str, Any]] = []
    for candidates in grouped.values():
        candidates.sort(key=lambda item: (item["event"]["occurred_at"], item["event"]["seq"], item["event"]["event_id"]))
        chosen = candidates[0]
        chosen["join_conflict"] = chosen["join_conflict"] or len({_event_material(item["event"]) for item in candidates}) > 1
        selected.append(chosen)
    selected.extend(unresolved_by_key.values())
    in_week = []
    for candidate in selected:
        event = candidate["event"]
        if _iso_week(datetime.fromtimestamp(event["occurred_at"] / 1000, timezone.utc)) == week:
            in_week.append(candidate)
    return sorted(
        in_week,
        key=lambda item: (item["event"]["occurred_at"], item["event"]["seq"], item["event"]["event_id"]),
    ), len(selected)


def _payload(value: Any) -> Optional[dict[str, Any]]:
    if not isinstance(value, str):
        return None
    try:
        parsed = json.loads(value, object_pairs_hook=_reject_duplicate_keys)
    except (ReportError, json.JSONDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def _is_decision_or_opening(kind: Any) -> bool:
    if not isinstance(kind, str):
        return False
    if kind == "comment_open":
        return True
    return any(kind == base or kind.startswith(base + ":") for base in DECISION_KINDS)


def _receipt_observations(audit: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    observations: list[dict[str, Any]] = []
    for event in audit:
        if event["kind"] not in {"github.write.succeeded", "github.write.reconciled"}:
            continue
        expected_outcome = "succeeded" if event["kind"] == "github.write.succeeded" else "reconciled"
        if event["outcome"] != expected_outcome:
            continue
        correlation = event["correlation"]
        session = correlation.get("session_id")
        write_id = correlation.get("write_id")
        detail = event.get("detail")
        if not isinstance(session, str) or not session or not isinstance(write_id, str) or not write_id:
            continue
        if not isinstance(detail, Mapping):
            continue
        operation = detail.get("operation")
        request_sha = detail.get("request_sha256")
        provider = detail.get("provider_receipt")
        if operation not in TERMINAL_KINDS or not isinstance(request_sha, str) or not isinstance(provider, Mapping):
            continue
        observations.append({
            "event": event,
            "session_id": session,
            "write_id": write_id,
            "operation": operation,
            "request_sha256": request_sha,
            "provider": dict(provider),
            "reconciled": event["kind"] == "github.write.reconciled",
        })
    return observations


def _provider_signature(provider: Mapping[str, Any]) -> str:
    return canonical_json({key: value for key, value in provider.items() if key != "reconciled"})


def _validate_provider(
    row: Mapping[str, Any],
    payload: Mapping[str, Any],
    provider: Mapping[str, Any],
    expected_status: Optional[str],
    expected_event: Optional[str],
) -> tuple[bool, bool]:
    kind = row["kind"]
    if kind == "comment":
        if not {"repo", "pr_number", "body", "comment_id"}.issubset(payload):
            return False, False
        if not isinstance(payload["repo"], str) or isinstance(payload["pr_number"], bool) or not isinstance(payload["pr_number"], int):
            return False, False
        if payload["comment_id"] is not None and (isinstance(payload["comment_id"], bool) or not isinstance(payload["comment_id"], int)):
            return False, False
        if not {"comment_id", "reconciled"}.issubset(provider) or not isinstance(provider["reconciled"], bool):
            return False, False
        comment_id = provider["comment_id"]
        visible = isinstance(comment_id, int) and not isinstance(comment_id, bool) and comment_id > 0
        return visible, visible
    if kind == "comment_abandon":
        if not {"repo", "pr_number", "body"}.issubset(payload):
            return False, False
        if not isinstance(payload["repo"], str) or isinstance(payload["pr_number"], bool) or not isinstance(payload["pr_number"], int):
            return False, False
        if not {"comment_id", "reconciled"}.issubset(provider) or not isinstance(provider["reconciled"], bool):
            return False, False
        comment_id = provider["comment_id"]
        if comment_id is not None and (isinstance(comment_id, bool) or not isinstance(comment_id, int) or comment_id <= 0):
            return False, False
        return True, isinstance(comment_id, int) and comment_id > 0
    if kind == "status":
        if not {"repo", "sha", "commit_id", "state", "context"}.issubset(payload) or not {"sha", "context", "state"}.issubset(provider):
            return False, False
        payload_sha = _full_sha(payload["sha"])
        commit_id = _full_sha(payload["commit_id"])
        provider_sha = _full_sha(provider["sha"])
        if payload_sha is None or commit_id is None or payload_sha != commit_id or provider_sha != payload_sha:
            return False, False
        if not isinstance(payload["context"], str) or provider["context"] != payload["context"]:
            return False, False
        if provider["state"] != payload["state"] or expected_status != payload["state"]:
            return False, False
        return True, False
    if kind == "review":
        if not {"repo", "pr_number", "event", "commit_id", "body"}.issubset(payload) or not {"review_id", "state", "commit_id", "reconciled"}.issubset(provider):
            return False, False
        if isinstance(payload["pr_number"], bool) or not isinstance(payload["pr_number"], int):
            return False, False
        payload_sha = _full_sha(payload["commit_id"])
        provider_sha = _full_sha(provider["commit_id"])
        if payload_sha is None or provider_sha != payload_sha or not isinstance(provider["reconciled"], bool):
            return False, False
        if payload["event"] != expected_event:
            return False, False
        expected_provider_state = "APPROVED" if expected_event == "APPROVE" else "CHANGES_REQUESTED"
        if provider["state"] != expected_provider_state:
            return False, False
        review_id = provider["review_id"]
        if isinstance(review_id, bool) or not isinstance(review_id, int) or review_id <= 0:
            return False, False
        return True, False
    return False, False


def _bind_write(
    row: Mapping[str, Any],
    observations: Sequence[Mapping[str, Any]],
    *,
    expected_status: Optional[str] = None,
    expected_event: Optional[str] = None,
) -> dict[str, Any]:
    payload = _payload(row.get("payload_json"))
    if payload is None:
        return {"status": "invalid_payload"}
    digest = sha256_bytes(row["payload_json"].encode("utf-8"))
    row_id = str(row["id"])
    candidates = [
        observation
        for observation in observations
        if observation["session_id"] == row["session_id"]
        and observation["write_id"] == row_id
        and observation["operation"] == row["kind"]
        and observation["request_sha256"] == digest
    ]
    if not candidates:
        return {"status": "missing", "payload": payload}
    valid: list[dict[str, Any]] = []
    invalid = False
    for candidate in candidates:
        ok, visible = _validate_provider(row, payload, candidate["provider"], expected_status, expected_event)
        if ok:
            valid.append({**candidate, "visible": visible, "payload": payload})
        else:
            invalid = True
    if not valid or invalid:
        return {"status": "conflict" if valid else "invalid_receipt", "payload": payload}
    signatures = {_provider_signature(item["provider"]) for item in valid}
    if len(signatures) != 1:
        return {"status": "conflict", "payload": payload}
    valid.sort(key=lambda item: (item["event"]["occurred_at"], item["event"]["seq"], item["event"]["event_id"]))
    return {"status": "ok", "proof": valid[0], "payload": payload}


def _write_rows(rows: Sequence[Mapping[str, Any]], session: str) -> list[dict[str, Any]]:
    return [dict(row) for row in rows if row.get("session_id") == session]


def _terminal_rows(rows: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    return [row for row in rows if row.get("kind") in TERMINAL_KINDS]


def _tombstone_binding(
    rows: Sequence[Mapping[str, Any]], observations: Sequence[Mapping[str, Any]]
) -> tuple[Optional[dict[str, Any]], bool]:
    abandon = [row for row in _terminal_rows(rows) if row.get("kind") == "comment_abandon"]
    if len(abandon) != 1:
        return None, False
    bound = _bind_write(abandon[0], observations)
    if bound.get("status") != "ok":
        return None, False
    if not bound["proof"].get("visible"):
        return None, False
    return bound["proof"], True


def _latency(
    accepted_ms: Optional[int], proofs: Sequence[Mapping[str, Any]], required: bool
) -> dict[str, Any]:
    if not required or accepted_ms is None:
        return {"status": "unknown", "duration_ms": None, "qualification": "unavailable"}
    times = [proof["event"].get("occurred_at") for proof in proofs]
    if not isinstance(accepted_ms, int) or accepted_ms < 0 or any(not isinstance(time, int) for time in times):
        return {"status": "clock_invalid", "duration_ms": None, "qualification": "clock_invalid"}
    if any(time < 0 or time < accepted_ms for time in times):
        return {"status": "clock_invalid", "duration_ms": None, "qualification": "clock_invalid"}
    end = max(times)
    upper_bound = any(bool(proof.get("reconciled")) for proof in proofs)
    return {
        "status": "observed",
        "duration_ms": end - accepted_ms,
        "qualification": "reconciliation_upper_bound" if upper_bound else "original_receipt",
    }


def _round_for_session(
    rows: Sequence[Mapping[str, Any]], session: str
) -> tuple[Optional[dict[str, Any]], bool]:
    matching = [dict(row) for row in rows if row.get("session_id") == session]
    if not matching:
        return None, False
    if len(matching) != 1:
        return None, True
    return matching[0], False


def _classify_session(
    *,
    target: Optional[Mapping[str, Any]],
    target_conflict: bool,
    round_row: Optional[Mapping[str, Any]],
    round_conflict: bool,
    writes: Sequence[Mapping[str, Any]],
    write_conflict: bool,
    runtime: Sequence[Mapping[str, Any]],
    observations: Sequence[Mapping[str, Any]],
    accepted_ms: int,
    action_completed: bool,
    source_complete: bool,
) -> dict[str, Any]:
    superseded = any(row.get("event_type") == "session.superseded" for row in runtime)
    timeout = any(row.get("event_type") == "session.timeout" for row in runtime)
    tombstone_proof, tombstone_visible = _tombstone_binding(writes, observations)
    tombstone_latency = _latency(accepted_ms, [tombstone_proof] if tombstone_proof else [], tombstone_proof is not None)

    if superseded:
        return {
            "category": "superseded",
            "tombstone_visible": tombstone_visible,
            "timeout_evidence": timeout,
            "latency": tombstone_latency,
            "terminal_issue": None if tombstone_proof is not None else "unbound_or_missing_tombstone",
        }
    if target is None or target_conflict or not action_completed:
        return {
            "category": "pending_or_unknown",
            "tombstone_visible": tombstone_visible,
            "timeout_evidence": timeout,
            "latency": tombstone_latency if tombstone_proof else {"status": "unknown", "duration_ms": None, "qualification": "unavailable"},
            "terminal_issue": "missing_target_or_action_join",
        }
    if timeout:
        exact_terminal = [row.get("kind") for row in _terminal_rows(writes)]
        complete = exact_terminal == ["comment_abandon"] and tombstone_proof is not None and tombstone_visible
        return {
            "category": "visible_failure" if complete and source_complete and not write_conflict else "pending_or_unknown",
            "tombstone_visible": tombstone_visible,
            "timeout_evidence": True,
            "latency": tombstone_latency,
            "terminal_issue": None if complete else "unbound_or_invisible_tombstone",
        }
    if round_row is None or round_conflict or write_conflict:
        return {
            "category": "pending_or_unknown",
            "tombstone_visible": tombstone_visible,
            "timeout_evidence": False,
            "latency": {"status": "unknown", "duration_ms": None, "qualification": "incomplete_receipts"},
            "terminal_issue": "missing_or_conflicting_round",
        }

    integrity = round_row.get("integrity_disposition")
    decision = round_row.get("decision")
    if not isinstance(integrity, str) or not isinstance(decision, str):
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "incomplete_receipts"}, "terminal_issue": "invalid_round_fields"}
    if integrity == "legacy_unverified" and decision not in {"unparseable", "insufficient_valid_reviewers"}:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "unsupported_terminal_evidence"}, "terminal_issue": "legacy_unverified_not_diagnostic"}

    diagnostic = integrity in DIAGNOSTIC_DISPOSITIONS or decision in {"unparseable", "insufficient_valid_reviewers"}
    target_sha = _full_sha(target.get("head_sha"))
    expected: set[str]
    expected_status: Optional[str] = None
    expected_event: Optional[str] = None
    verified_sha = _full_sha(round_row.get("verified_commit_id"))
    if diagnostic:
        expected = {"comment"}
        if target_sha is not None:
            expected.add("status")
            expected_status = "error"
        if verified_sha is not None:
            return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "conflicting_authority_fields"}, "terminal_issue": "diagnostic_has_verified_commit"}
    elif integrity == "verified" and decision in {"approve", "request_changes"}:
        if target_sha is None or verified_sha is None or verified_sha != target_sha:
            return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "missing_or_mismatched_commit_proof"}, "terminal_issue": "target_verified_commit_mismatch"}
        expected = {"comment", "status", "review"}
        expected_status = "success" if decision == "approve" else "failure"
        expected_event = "APPROVE" if decision == "approve" else "REQUEST_CHANGES"
    else:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "unsupported_terminal_evidence"}, "terminal_issue": "unsupported_terminal_evidence"}

    terminal = _terminal_rows(writes)
    actual = [row.get("kind") for row in terminal]
    if len(actual) != len(expected) or set(actual) != expected:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "incomplete_receipts"}, "terminal_issue": "terminal_write_set_mismatch", "planned_kinds": sorted(expected)}

    bindings: dict[str, dict[str, Any]] = {}
    for row in terminal:
        kind = row["kind"]
        binding = _bind_write(row, observations, expected_status=expected_status if kind == "status" else None, expected_event=expected_event if kind == "review" else None)
        if binding.get("status") != "ok":
            return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "incomplete_receipts"}, "terminal_issue": f"{kind}_receipt_{binding.get('status')}", "planned_kinds": sorted(expected)}
        bindings[kind] = binding

    comment_proof = bindings["comment"]["proof"]
    if not comment_proof.get("visible"):
        return {"category": "pending_or_unknown", "tombstone_visible": False, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "incomplete_receipts"}, "terminal_issue": "comment_not_visible", "planned_kinds": sorted(expected)}
    for kind in ("status", "review"):
        if kind not in bindings:
            continue
        payload = bindings[kind]["payload"]
        provider = bindings[kind]["proof"]["provider"]
        if target_sha is None:
            return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "missing_target_sha"}, "terminal_issue": "authority_projection_without_target_sha"}
        if kind == "status":
            if _full_sha(payload.get("sha")) != target_sha or _full_sha(payload.get("commit_id")) != target_sha or _full_sha(provider.get("sha")) != target_sha:
                return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "conflicting_authority_fields"}, "terminal_issue": "status_commit_mismatch"}
        elif _full_sha(payload.get("commit_id")) != target_sha or _full_sha(provider.get("commit_id")) != target_sha:
            return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "conflicting_authority_fields"}, "terminal_issue": "review_commit_mismatch"}

    proofs = [bindings[k]["proof"] for k in sorted(expected)]
    latency = _latency(accepted_ms, proofs, True)
    if not source_complete:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone_visible, "timeout_evidence": False, "latency": latency, "terminal_issue": "capture_coverage_incomplete", "planned_kinds": sorted(expected)}
    return {
        "category": "visible_failure" if diagnostic else "reliable",
        "tombstone_visible": tombstone_visible,
        "timeout_evidence": False,
        "latency": latency,
        "terminal_issue": None,
        "planned_kinds": sorted(expected),
    }


def _latency_summary(rows: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    counts = Counter({"observed": 0, "reconciliation_upper_bound": 0, "unknown": 0, "clock_invalid": 0})
    durations: list[int] = []
    for row in rows:
        latency = row.get("latency", {})
        if latency.get("status") == "observed" and latency.get("qualification") == "reconciliation_upper_bound":
            counts["reconciliation_upper_bound"] += 1
        elif latency.get("status") == "observed":
            counts["observed"] += 1
        elif latency.get("status") == "clock_invalid":
            counts["clock_invalid"] += 1
        else:
            counts["unknown"] += 1
        if isinstance(latency.get("duration_ms"), int):
            durations.append(latency["duration_ms"])
    return {
        "counts": dict(sorted(counts.items())),
        "denominator": len(rows),
        "durations_ms": sorted(durations),
        "actual_latency_status": "known" if rows and len(durations) == len(rows) else "unknown",
    }


def _human_metrics(
    rows: Sequence[Mapping[str, Any]], cutoff: datetime, finding_rows: Sequence[Mapping[str, Any]], coverage: str
) -> dict[str, Any]:
    annotation = Counter({key: 0 for key in HUMAN_VERDICTS})
    escapes = Counter({key: 0 for key in ESCAPE_STATUSES})
    annotation_seen: dict[tuple[Any, ...], tuple[str, Any]] = {}
    escape_seen: dict[tuple[Any, ...], tuple[str, Any]] = {}
    conflicts = 0
    future = 0
    unknown_rows = 0
    annotation_drilldown: list[dict[str, Any]] = []
    escape_drilldown: list[dict[str, Any]] = []
    findings = {
        (row.get("session_id"), row.get("stable_id"), row.get("repo"), row.get("pr_number"), row.get("head_sha"))
        for row in finding_rows
    }
    for row in rows:
        kind = row.get("type", "annotation")
        time_field = "confirmed_at" if kind == "escape" else "observed_at"
        try:
            observed = _parse_datetime(row.get(time_field), f"human.{time_field}")
        except ReportError:
            unknown_rows += 1
            continue
        if observed > cutoff:
            future += 1
            continue
        if kind == "escape":
            required = ("confirming_human_id", "version", "reviewed_window_id", "evidence_reference", "status", "repo", "pr_number", "session_id", "finding_id", "head_sha")
            if not all(key in row for key in required) or row.get("status") not in ESCAPE_STATUSES:
                unknown_rows += 1
                continue
            scope = (row["session_id"], row["finding_id"], row["repo"], row["pr_number"], row["head_sha"])
            if scope not in findings:
                unknown_rows += 1
                continue
            identity = (row["reviewed_window_id"], row["confirming_human_id"], row["version"], *scope)
            previous = escape_seen.get(identity)
            current = (row["status"], row["evidence_reference"])
            if previous is not None and previous != current:
                conflicts += 1
                escapes[previous[0]] -= 1
                escapes["unknown"] += 1
            elif previous is None:
                escape_seen[identity] = current
                escapes[row["status"]] += 1
                escape_drilldown.append({
                    "reviewed_window_id": _safe_ref(row["reviewed_window_id"], "reviewed_window_id"),
                    "confirming_human_id": _safe_ref(row["confirming_human_id"], "confirming_human_id"),
                    "version": _safe_ref(row["version"], "human.version"),
                    "confirmed_at": row["confirmed_at"],
                    "evidence_reference": _safe_ref(row["evidence_reference"], "evidence_reference"),
                    "status": row["status"],
                    "repo": _safe_ref(row["repo"], "repo"),
                    "pr_number": row["pr_number"],
                    "session_id": _safe_ref(row["session_id"], "session_id"),
                    "finding_id": _safe_ref(row["finding_id"], "finding_id"),
                    "head_sha": _safe_ref(row["head_sha"], "head_sha"),
                })
            continue
        required = ("annotator_id", "version", "session_id", "finding_id", "repo", "pr_number", "head_sha", "verdict", "evidence_reference")
        if not all(key in row for key in required) or row.get("verdict") not in HUMAN_VERDICTS:
            unknown_rows += 1
            continue
        scope = (row["session_id"], row["finding_id"], row["repo"], row["pr_number"], row["head_sha"])
        if scope not in findings:
            unknown_rows += 1
            continue
        identity = (*scope, row["annotator_id"], row["version"])
        previous = annotation_seen.get(identity)
        current = (row["verdict"], row["evidence_reference"])
        if previous is not None and previous != current:
            conflicts += 1
            annotation[previous[0]] -= 1
            annotation["unknown"] += 1
        elif previous is None:
            annotation_seen[identity] = current
            annotation[row["verdict"]] += 1
            annotation_drilldown.append({
                "annotator_id": _safe_ref(row["annotator_id"], "annotator_id"),
                "version": _safe_ref(row["version"], "human.version"),
                "observed_at": row["observed_at"],
                "evidence_reference": _safe_ref(row["evidence_reference"], "evidence_reference"),
                "verdict": row["verdict"],
                "repo": _safe_ref(row["repo"], "repo"),
                "pr_number": row["pr_number"],
                "session_id": _safe_ref(row["session_id"], "session_id"),
                "finding_id": _safe_ref(row["finding_id"], "finding_id"),
                "head_sha": _safe_ref(row["head_sha"], "head_sha"),
            })
    return {
        "coverage": coverage,
        "annotation_counts": dict(sorted(annotation.items())),
        "annotation_denominator": sum(annotation.values()),
        "confirmed_escapes": escapes["confirmed_escape"],
        "escape_counts": dict(sorted(escapes.items())),
        "reviewed_window_denominator": sum(escapes.values()),
        "reviewed_window_coverage": "known" if sum(escapes.values()) else "unknown",
        "unknown_rows": unknown_rows,
        "conflicting_rows": conflicts,
        "future_rows_excluded": future,
        "annotations": sorted(annotation_drilldown, key=lambda row: (row["session_id"], row["finding_id"], row["annotator_id"])),
        "escapes": sorted(escape_drilldown, key=lambda row: (row["reviewed_window_id"], row["confirming_human_id"])),
        "recall_claim": "not_available",
    }


def _cost_metrics(
    rows: Sequence[Mapping[str, Any]], cutoff: datetime, eligible_sessions: set[str], coverage: str
) -> dict[str, Any]:
    def metadata(session_rows: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
        completeness = "complete" if all(row.get("completeness") == "complete" for row in session_rows) else "partial" if any(row.get("completeness") == "partial" for row in session_rows) else "unknown"
        attempt_coverage = "all_attempts_and_retries" if all(row.get("attempt_coverage") == "all_attempts_and_retries" for row in session_rows) else "partial" if any(row.get("attempt_coverage") == "partial" for row in session_rows) else "unknown"
        references = sorted(_safe_ref(row.get("reconciliation_reference"), "cost.reconciliation_reference") for row in session_rows if isinstance(row.get("reconciliation_reference"), str))
        return {"completeness": completeness, "attempt_coverage": attempt_coverage, "reconciliation_references": references}

    by_session: dict[str, list[Mapping[str, Any]]] = defaultdict(list)
    future = 0
    invalid = 0
    for row in rows:
        session = row.get("session_id")
        if session not in eligible_sessions:
            continue
        try:
            stamp = _parse_datetime(row.get("reconciliation_time"), "cost.reconciliation_time")
        except ReportError:
            invalid += 1
            continue
        if stamp > cutoff:
            future += 1
            continue
        by_session[session].append(row)
    per_session: dict[str, dict[str, Any]] = {}
    for session in sorted(eligible_sessions):
        session_rows = by_session.get(session, [])
        if not session_rows:
            per_session[session] = {"status": "totalunknown", "currency": None, "known_minor": None, "partial_known_minor": None, "completeness": "unknown", "attempt_coverage": "unknown", "reconciliation_references": []}
            continue
        identities: dict[tuple[Any, ...], Mapping[str, Any]] = {}
        conflict = False
        for row in session_rows:
            identity = (row.get("currency"), row.get("source_minor_unit"), row.get("reconciliation_reference"))
            previous = identities.get(identity)
            if previous is not None and canonical_json(previous) != canonical_json(row):
                conflict = True
            else:
                identities[identity] = row
        session_rows = list(identities.values())
        currencies = {row.get("currency") for row in session_rows}
        units = {row.get("source_minor_unit") for row in session_rows}
        valid_rows = all(
            isinstance(row.get("currency"), str) and bool(row["currency"])
            and isinstance(row.get("source_minor_unit"), str) and bool(row["source_minor_unit"])
            and isinstance(row.get("reconciliation_reference"), str) and bool(row["reconciliation_reference"])
            and isinstance(row.get("amount_minor"), int) and not isinstance(row.get("amount_minor"), bool)
            and row["amount_minor"] >= 0
            and row.get("completeness") in {"complete", "partial", "unknown"}
            and row.get("attempt_coverage") in {"all_attempts_and_retries", "partial", "unknown"}
            for row in session_rows
        )
        if conflict or not valid_rows or len(currencies) != 1 or len(units) != 1:
            per_session[session] = {"status": "totalunknown", "currency": None, "known_minor": None, "partial_known_minor": None, **metadata(session_rows)}
            continue
        currency = next(iter(currencies))
        amount = sum(row["amount_minor"] for row in session_rows)
        complete = all(row["completeness"] == "complete" and row["attempt_coverage"] == "all_attempts_and_retries" for row in session_rows)
        partial = any(row["completeness"] == "partial" for row in session_rows)
        if complete:
            per_session[session] = {"status": "known", "currency": currency, "known_minor": amount, "partial_known_minor": None, **metadata(session_rows)}
        elif partial:
            per_session[session] = {"status": "partial_known", "currency": currency, "known_minor": None, "partial_known_minor": amount, **metadata(session_rows)}
        else:
            per_session[session] = {"status": "totalunknown", "currency": currency, "known_minor": None, "partial_known_minor": None, **metadata(session_rows)}

    per_currency: dict[str, dict[str, Any]] = {}
    for value in per_session.values():
        currency = value.get("currency")
        if not isinstance(currency, str):
            continue
        bucket = per_currency.setdefault(currency, {"known_minor": 0, "partial_known_minor": 0, "known_sessions": 0, "partial_sessions": 0, "unknown_sessions": 0})
        if value["status"] == "known":
            bucket["known_minor"] += value["known_minor"]
            bucket["known_sessions"] += 1
        elif value["status"] == "partial_known":
            bucket["partial_known_minor"] += value["partial_known_minor"]
            bucket["partial_sessions"] += 1
        else:
            bucket["unknown_sessions"] += 1
    unknown_sessions = sum(1 for value in per_session.values() if value["status"] == "totalunknown")
    partial_sessions = sum(1 for value in per_session.values() if value["status"] == "partial_known")
    return {
        "coverage": coverage,
        "per_session": {_safe_ref(session, "cost.session_id"): value for session, value in sorted(per_session.items())},
        "per_currency": {currency: per_currency[currency] for currency in sorted(per_currency)},
        "unknown_sessions": unknown_sessions,
        "partial_sessions": partial_sessions,
        "future_rows_excluded": future,
        "invalid_rows": invalid,
        "actual_total_status": "totalunknown" if unknown_sessions or partial_sessions else "known_per_currency",
        "pricing_table_used": False,
        "cross_currency_arithmetic": False,
    }


def verify_evaluation_artifacts(root: Path) -> Mapping[str, Any]:
    """Use the concurrent core verifier; never read raw evaluation summaries."""
    try:
        import review_model_evaluation  # type: ignore
    except ImportError as exc:
        raise EvaluationDependencyError("evaluation verifier dependency unavailable") from exc
    verifier = getattr(review_model_evaluation, "verify_evaluation_artifacts", None)
    if not callable(verifier):
        raise EvaluationDependencyError("evaluation verifier dependency unavailable")
    try:
        result = verifier(Path(root))
    except (review_model_evaluation.EvaluationConflict, review_model_evaluation.EvaluationError) as exc:
        raise ReportError(f"evaluation artifact verification failed: {exc}") from exc
    if not isinstance(result, Mapping):
        raise ReportError("evaluation verifier returned a non-object summary")
    return result


def _evaluation_identity(summary: Mapping[str, Any]) -> tuple[str, str]:
    identity = summary.get("evaluation_identity", summary.get("identity"))
    artifact_hashes = summary.get("artifact_hashes")
    if identity is None:
        identity = {
            "input_identity": summary.get("input_identity"),
            "summary_sha256": summary.get("summary_sha256"),
            "artifact_hashes": artifact_hashes,
        }
    material = {"identity": identity, "artifact_hashes": artifact_hashes}
    return canonical_json(identity), sha256_bytes(canonical_json(material).encode("utf-8"))


def _zero_evaluation(availability: str, *, reason: Optional[str] = None) -> dict[str, Any]:
    result: dict[str, Any] = {
        "availability": availability,
        "evaluations": 0,
        "identity_hashes": [],
        "identity_digest": sha256_bytes(b"[]"),
        "denominator": 0,
        "supported": 0,
        "refuted": 0,
        "unresolved": 0,
        "quality": {"status": "not_scoreable", "score": None, "denominator": 0},
        "usefulness": {"useful": 0, "not_useful": 0, "unknown": 0},
        "usefulness_denominator": 0,
        "disagreement_items": 0,
        "disagreement_denominator": 0,
        "validation": {key: 0 for key in VALIDATION_CLASSES},
        "validation_denominator": 0,
        "omissions": {"candidate_count": 0, "automatically_supported_omission": 0, "unknown": 0},
        "human_confirmed_escapes": 0,
        "human_escape_denominator": 0,
        "recall_claim": "not_available",
    }
    if reason is not None:
        result["reason"] = reason
    return result


def _evaluation_metrics(root: Optional[Path]) -> dict[str, Any]:
    if root is None:
        return _zero_evaluation("not_supplied")
    root = Path(root)
    if root.is_symlink() or not root.is_dir():
        return _zero_evaluation("unavailable", reason="evaluation_root_not_directory")
    try:
        verified = verify_evaluation_artifacts(root)
    except EvaluationDependencyError as exc:
        return _zero_evaluation("dependency_unavailable", reason=str(exc))
    evaluations = verified.get("evaluations") if isinstance(verified.get("evaluations"), list) else [verified]
    summaries: list[Mapping[str, Any]] = [item for item in evaluations if isinstance(item, Mapping)]
    if len(summaries) != len(evaluations) or not summaries:
        raise ReportError("evaluation verifier returned no validated summaries")
    unique: dict[str, tuple[str, Mapping[str, Any]]] = {}
    for summary in summaries:
        identity_key, _ = _evaluation_identity(summary)
        existing = unique.get(identity_key)
        summary_material = canonical_json(summary)
        if existing is not None:
            if existing[0] != summary_material:
                raise ReportError("conflicting duplicate evaluation identity")
            continue
        unique[identity_key] = (summary_material, summary)
    ordered = [item[1] for _, item in sorted(unique.items())]
    identity_hashes = sorted(_evaluation_identity(summary)[1] for summary in ordered)
    identity_digest = sha256_bytes(canonical_json(identity_hashes).encode("utf-8"))
    complete_summaries = [summary for summary in ordered if summary.get("state") == "complete" and (not isinstance(summary.get("source"), Mapping) or summary["source"].get("complete", True))]
    partial_count = sum(1 for summary in ordered if summary.get("state") == "partial")
    failed_count = sum(1 for summary in ordered if summary.get("state") == "failed")
    availability = "available" if complete_summaries and len(complete_summaries) == len(ordered) else "partial" if partial_count or complete_summaries else "failed"
    supported = refuted = unresolved = 0
    usefulness = Counter({"useful": 0, "not_useful": 0, "unknown": 0})
    disagreement = 0
    disagreement_denominator = 0
    disagreement_denominator_known = True
    quality_denominator = 0
    validation = Counter({key: 0 for key in VALIDATION_CLASSES})
    validation_denominator = 0
    candidate_count = automatic = omission_unknown = human_escapes = human_escape_denominator = 0
    raw_counts: list[dict[str, Any]] = []
    for summary in ordered:
        model = summary.get("model_assessment") if isinstance(summary.get("model_assessment"), Mapping) else {}
        classes = summary.get("validation_coverage") if isinstance(summary.get("validation_coverage"), Mapping) else {}
        omissions = summary.get("omissions") if isinstance(summary.get("omissions"), Mapping) else {}
        scoreable = summary in complete_summaries
        raw_counts.append({"state": summary.get("state"), "supported": model.get("supported", 0), "refuted": model.get("refuted", 0), "unresolved": model.get("unresolved", 0)})
        if scoreable:
            supported += int(model.get("supported", 0) or 0)
            refuted += int(model.get("refuted", 0) or 0)
            unresolved += int(model.get("unresolved", 0) or 0)
            quality_denominator += int(model.get("scoreable_items", model.get("original_findings", 0)) or 0)
            raw_usefulness = model.get("usefulness") if isinstance(model.get("usefulness"), Mapping) else {}
            for key in usefulness:
                usefulness[key] += int(raw_usefulness.get(key, 0) or 0)
            disagreement += int(model.get("disagreement_items", 0) or 0)
            raw_disagreement_denominator = model.get("disagreement_denominator")
            if (
                isinstance(raw_disagreement_denominator, int)
                and not isinstance(raw_disagreement_denominator, bool)
                and raw_disagreement_denominator >= 0
            ):
                disagreement_denominator += raw_disagreement_denominator
            else:
                disagreement_denominator_known = False
        for key in VALIDATION_CLASSES:
            validation[key] += int(classes.get(key, 0) or 0)
        validation_denominator += sum(int(classes.get(key, 0) or 0) for key in VALIDATION_CLASSES)
        candidate_count += int(omissions.get("candidate_count", 0) or 0)
        automatic += int(omissions.get("automatically_supported_omission", 0) or 0)
        omission_unknown += int(omissions.get("unknown", 0) or 0)
        human_escapes += int(omissions.get("human_confirmed_escape", 0) or 0)
        human_escape_denominator += int(omissions.get("human_escape_denominator", 0) or 0)
    denominator = supported + refuted + unresolved
    return {
        "availability": availability,
        "evaluations": len(ordered),
        "identity_hashes": identity_hashes,
        "identity_digest": identity_digest,
        "denominator": denominator,
        "supported": supported,
        "refuted": refuted,
        "unresolved": unresolved,
        "quality": {"status": "available" if availability == "available" else "not_scoreable", "score": (supported / denominator) if availability == "available" and denominator else None, "denominator": quality_denominator},
        "usefulness": dict(sorted(usefulness.items())),
        "usefulness_denominator": sum(usefulness.values()),
        "disagreement_items": disagreement,
        "disagreement_denominator": disagreement_denominator if disagreement_denominator_known else None,
        "validation": dict(sorted(validation.items())),
        "validation_denominator": validation_denominator,
        "omissions": {"candidate_count": candidate_count, "automatically_supported_omission": automatic, "unknown": omission_unknown},
        "human_confirmed_escapes": human_escapes,
        "human_escape_denominator": human_escape_denominator,
        "recall_claim": "not_available",
        "partial_evaluations": partial_count,
        "failed_evaluations": failed_count,
        "raw_counts": raw_counts,
    }


def _safe_session_report(
    *, item: Mapping[str, Any], target: Optional[Mapping[str, Any]], round_row: Optional[Mapping[str, Any]],
    writes: Sequence[Mapping[str, Any]], findings: Sequence[Mapping[str, Any]], classification: Mapping[str, Any]
) -> dict[str, Any]:
    event = item["event"]
    session = item.get("session_id")
    result: dict[str, Any] = {
        "session_id": _safe_optional(session, "session_id"),
        "repo": _safe_optional(target.get("repo") if target else None, "repo"),
        "pr_number": target.get("pr_number") if target and isinstance(target.get("pr_number"), int) else None,
        "eligibility": "review" if target is not None and target.get("reason") != "ask" and session is not None else "unknown",
        "action_completed": bool(item.get("completed")),
        "accepted_event_id": _safe_ref(event["event_id"], "accepted_event_id"),
        "category": classification.get("category", "pending_or_unknown"),
        "tombstone_visible": bool(classification.get("tombstone_visible")),
        "timeout_evidence": bool(classification.get("timeout_evidence")),
        "latency": classification.get("latency"),
        "terminal_issue": classification.get("terminal_issue"),
        "source_rows": {
            "target_session_id": _safe_optional(session, "target_session_id"),
            "round_id": _safe_optional(round_row.get("id") if round_row else None, "round_id"),
            "write_ids": sorted(_safe_ref(row.get("id"), "write_id") for row in writes if row.get("kind") in TERMINAL_KINDS),
            "finding_ids": sorted(_safe_ref(row.get("stable_id"), "finding_id") for row in findings),
        },
        "target_sha": _safe_optional(target.get("head_sha") if target else None, "target_sha"),
        "verified_commit_id": _safe_optional(round_row.get("verified_commit_id") if round_row else None, "verified_commit_id"),
    }
    if item.get("join_conflict"):
        result["terminal_issue"] = "conflicting_action_join"
    if target is not None and target.get("reason") == "ask":
        result["eligibility"] = "ask_excluded"
        result["category"] = "excluded"
    elif target is None or session is None or item.get("join_conflict"):
        result["eligibility"] = "unknown"
        result["category"] = "pending_or_unknown"
    return result


def build_report(bundle: Path, week: str, as_of: str, evaluation_root: Optional[Path] = None) -> dict[str, Any]:
    bundle = Path(bundle)
    if bundle.is_symlink() or not bundle.is_dir():
        raise ReportError("bundle must be a regular directory")
    if not WEEK.fullmatch(week):
        raise ReportError("--week must use YYYY-Www")
    cutoff = _parse_datetime(as_of, "--as-of")
    manifest, manifest_raw = _read_json(bundle / "evidence-manifest.json")
    if not isinstance(manifest, Mapping):
        raise ReportError("evidence manifest must be an object")
    _validate_manifest_snapshot(manifest, cutoff)
    spans = _validate_capture_manifest(manifest)
    if _iso_week(cutoff) != week:
        raise ReportError("--week is not the Taipei ISO week of --as-of")

    audit_rows, audit_raw = _read_ndjson(bundle / "audit.ndjson", required=True)
    product_value, product_raw = _read_json(bundle / "product.json")
    raw_tables = _validate_product_tables(product_value)
    tables, indexes, product_conflicts = _dedupe_product_tables(raw_tables)
    _validate_product_units(tables)
    _verify_manifest_file(manifest, "audit.ndjson", audit_raw, len(audit_rows))
    _verify_manifest_file(manifest, "product.json", product_raw, None)

    optional_rows: dict[str, list[dict[str, Any]]] = {}
    optional_raw: dict[str, bytes] = {}
    for filename in OPTIONAL_FILES:
        rows, raw = _read_ndjson(bundle / filename, required=False)
        optional_rows[filename] = rows
        optional_raw[filename] = raw
        source = filename.removesuffix(".ndjson")
        coverage = _source_coverage(manifest, source)
        if raw:
            _verify_manifest_file(manifest, filename, raw, len(rows))
        elif coverage != "unknown":
            raise ReportError(f"manifest declares {filename} coverage but the file is absent")

    cutoff_ms = int(cutoff.timestamp() * 1000)
    cutoff_seconds = cutoff.timestamp()
    audit, future_audit, audit_conflicts = _filter_audit(audit_rows, cutoff_ms)
    runtime, future_runtime = _runtime_receipts(tables["runtime_event_receipts"], cutoff_seconds)
    observations = _receipt_observations(audit)
    accepted, deduplicated_accepted = _accepted_candidates(audit, week)
    plan_only = sum(1 for event in audit if event["kind"] == "ingress.accepted" and event["outcome"] == "accepted")

    table_coverage = {table: _source_coverage(manifest, table) for table in PRODUCT_TABLES}
    audit_coverage = _source_coverage(manifest, "audit")
    declared_audit_coverage = _coverage_value(manifest["coverage"]["audit"], "coverage.audit")
    declared_product_coverage = _coverage_value(manifest["coverage"]["product"], "coverage.product")
    cursor = manifest.get("audit_cursor")
    cursor_complete = isinstance(cursor, Mapping) and cursor.get("final_null_cursor") is True and isinstance(cursor.get("page_count"), int) and cursor.get("page_count", 0) > 0
    source_complete = (
        audit_coverage == "complete"
        and declared_audit_coverage == "complete"
        and declared_product_coverage == "complete"
        and cursor_complete
        and all(value == "complete" for value in table_coverage.values())
        and not audit_conflicts
    )
    runtime_conflict_sessions = {
        indexes["runtime_event_receipts"][identity].get("session_id")
        for identity in product_conflicts["runtime_event_receipts"]
        if identity in indexes["runtime_event_receipts"]
    }
    observation_by_session: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for observation in observations:
        observation_by_session[observation["session_id"]].append(observation)

    session_reports: list[dict[str, Any]] = []
    reliability_counts = Counter({category: 0 for category in RELIABILITY_CATEGORIES})
    excluded_ask = 0
    unknown_eligibility = 0
    eligible_sessions: set[str] = set()
    for item in accepted:
        session = item.get("session_id")
        target = indexes["session_targets"].get(session) if session is not None else None
        target_conflict = session in product_conflicts["session_targets"] if session is not None else False
        if session is None or item.get("join_conflict") or target is None or target_conflict:
            unknown_eligibility += 1
            classification = {"category": "pending_or_unknown", "tombstone_visible": False, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "unavailable"}, "terminal_issue": "unmatched_or_conflicting_action"}
            round_row = None
            writes: list[dict[str, Any]] = []
            findings: list[dict[str, Any]] = []
        elif target.get("reason") == "ask":
            excluded_ask += 1
            round_row, _ = _round_for_session(tables["review_rounds"], session)
            writes = _write_rows(tables["github_writes"], session)
            findings = [row for row in tables["review_findings"] if row.get("session_id") == session]
            classification = {"category": "excluded", "tombstone_visible": False, "timeout_evidence": False, "latency": {"status": "unknown", "duration_ms": None, "qualification": "excluded_ask"}, "terminal_issue": "known_ask_session"}
        else:
            eligible_sessions.add(session)
            round_row, round_conflict = _round_for_session(tables["review_rounds"], session)
            round_conflict = round_conflict or session in product_conflicts["review_rounds"]
            writes = _write_rows(tables["github_writes"], session)
            findings = [row for row in tables["review_findings"] if row.get("session_id") == session]
            conflicted_write_ids = {
                conflict for conflict in product_conflicts["github_writes"] if isinstance(conflict, int)
            }
            write_conflict = (
                any(row.get("id") in conflicted_write_ids for row in writes)
                or any(conflict == session or (isinstance(conflict, tuple) and conflict[0] == session) for conflict in product_conflicts["github_writes"])
                or session in runtime_conflict_sessions
            )
            runtime_rows = [] if session in runtime_conflict_sessions else runtime.get(session, [])
            classification = _classify_session(
                target=target, target_conflict=target_conflict, round_row=round_row, round_conflict=round_conflict,
                writes=writes, write_conflict=write_conflict, runtime=runtime_rows,
                observations=observation_by_session.get(session, []), accepted_ms=item["event"]["occurred_at"],
                action_completed=bool(item.get("completed")), source_complete=source_complete,
            )
            reliability_counts[classification["category"]] += 1
        session_reports.append(_safe_session_report(item=item, target=target, round_row=round_row, writes=writes, findings=findings, classification=classification))

    conflicted_finding_ids = product_conflicts["review_findings"]
    human_findings = [row for row in tables["review_findings"] if row.get("id") not in conflicted_finding_ids]
    human = _human_metrics(optional_rows["human.ndjson"], cutoff, human_findings, _source_coverage(manifest, "human"))
    cost = _cost_metrics(optional_rows["cost.ndjson"], cutoff, eligible_sessions, _source_coverage(manifest, "cost"))
    evaluation = _evaluation_metrics(evaluation_root)
    manifest_digest = sha256_bytes(manifest_raw)
    report_id = sha256_bytes((DEFINITION_VERSION + week + as_of + manifest_digest + evaluation["identity_digest"]).encode("utf-8"))
    report = {
        "definition_version": DEFINITION_VERSION,
        "week": week,
        "snapshot_at": as_of,
        "timezone": "Asia/Taipei",
        "report_id": report_id,
        "manifest_sha256": manifest_digest,
        "capture": {
            "coverage": {
                "audit": audit_coverage,
                "product": "complete" if declared_product_coverage == "complete" and all(value == "complete" for value in table_coverage.values()) else "partial" if declared_product_coverage == "partial" or any(value == "partial" for value in table_coverage.values()) else "unknown",
                "product_tables": dict(sorted(table_coverage.items())),
                "human": _source_coverage(manifest, "human"),
                "cost": _source_coverage(manifest, "cost"),
            },
            "capture_spans": dict(sorted(spans.items())),
            "audit_cursor_complete": cursor_complete,
            "future_audit_records_excluded": future_audit,
            "future_runtime_receipts_excluded": future_runtime,
            "non_atomic_capture": True,
            "file_hashes": {name: sha256_bytes(raw) for name, raw in {"audit.ndjson": audit_raw, "product.json": product_raw, **optional_raw}.items() if raw},
            "conflicts": {"audit": len(audit_conflicts), "product": {table: len(values) for table, values in sorted(product_conflicts.items()) if values}},
        },
        "cohort": {
            "accepted_total": len(accepted),
            "accepted_denominator": len(accepted),
            "deduplicated_sessions": deduplicated_accepted,
            "eligible_review_total": len(eligible_sessions),
            "excluded_ask": excluded_ask,
            "unknown_eligibility": unknown_eligibility,
            "unmatched_actions": unknown_eligibility,
            "plan_only": plan_only,
            "sessions": sorted(session_reports, key=lambda row: (row.get("session_id") or "", row.get("accepted_event_id") or "")),
        },
        "reliability": {**dict(sorted(reliability_counts.items())), "denominator": len(eligible_sessions), "unknown_denominator": unknown_eligibility, "coverage_blocked": not source_complete},
        "latency": _latency_summary([row for row in session_reports if row.get("eligibility") == "review"]),
        "delivery": {
            "accepted_to_projection_denominator": len(eligible_sessions),
            "reliable": reliability_counts["reliable"],
            "visible_failure": reliability_counts["visible_failure"],
            "superseded": reliability_counts["superseded"],
            "pending_or_unknown": reliability_counts["pending_or_unknown"],
            "latency": _latency_summary([row for row in session_reports if row.get("eligibility") == "review"]),
        },
        "human_quality": human,
        "cost": cost,
        "evaluation": evaluation,
        "limitations": [
            "Reliability is source-bound and mutually classified; missing joins, conflicts, and incomplete capture remain pending_or_unknown.",
            "Formal review proof uses provider_receipt.review_id/state/commit_id/reconciled; provider event is not a source field.",
            "Human usefulness, model evaluation, omissions, confirmed escapes, and delivery reliability have separate denominators.",
            "Costs are source minor units only; no price table, estimate, allocation, float-dollar conversion, or cross-currency total is made.",
            "No model, OCI executor, network, database, provider, or live API is invoked by this report.",
        ],
    }
    return report


def render_markdown(report: Mapping[str, Any]) -> str:
    cohort = report["cohort"]
    reliability = report["reliability"]
    latency = report["latency"]
    human = report["human_quality"]
    cost = report["cost"]
    evaluation = report["evaluation"]
    lines = [
        f"# Review-round weekly report — {report['week']}", "", f"Snapshot: `{report['snapshot_at']}`  ",
        f"Report ID: `{report['report_id']}`  ", "Source: frozen local bundle; no live collection or model call.", "",
        "## Cohort", "", "| metric | count |", "|---|---:|",
        f"| accepted denominator | {cohort['accepted_denominator']} |",
        f"| eligible review sessions | {cohort['eligible_review_total']} |",
        f"| ask exclusions | {cohort['excluded_ask']} |",
        f"| eligibility unknown | {cohort['unknown_eligibility']} |",
        f"| plan-only ingress accepted | {cohort['plan_only']} |", "", "## Reliability", "", "| category | count |", "|---|---:|",
    ]
    for category in RELIABILITY_CATEGORIES:
        lines.append(f"| {category} | {reliability[category]} |")
    lines.extend([
        f"| denominator | {reliability['denominator']} |", f"| unknown denominator | {reliability['unknown_denominator']} |", "",
        "## Latency", "",
        f"Observed exact: {latency['counts']['observed']}; reconciliation upper bound: {latency['counts']['reconciliation_upper_bound']}; unknown: {latency['counts']['unknown']}; clock-invalid: {latency['counts']['clock_invalid']}.", "",
        "## Human and cost", "",
        f"Human coverage: `{human['coverage']}`; useful: {human['annotation_counts']['valid_useful']}; not useful: {human['annotation_counts']['valid_not_useful']}; invalid: {human['annotation_counts']['invalid']}; unknown: {human['annotation_counts']['unknown']}; confirmed escapes: {human['confirmed_escapes']}. No recall claim.",
        f"Cost coverage: `{cost['coverage']}`; actual total status: `{cost['actual_total_status']}`; missing/unknown sessions: {cost['unknown_sessions']}; partial sessions: {cost['partial_sessions']}; pricing table used: `{cost['pricing_table_used']}`.", "",
        "## Model evaluation (separate denominator)", "",
        f"Availability: `{evaluation['availability']}`; denominator: {evaluation['denominator']}; supported: {evaluation['supported']}; refuted: {evaluation['refuted']}; unresolved: {evaluation['unresolved']}; usefulness denominator: {evaluation['usefulness_denominator']}; disagreement items: {evaluation['disagreement_items']}; automatic omission candidates: {evaluation['omissions']['automatically_supported_omission']}; human-confirmed escapes: {evaluation['human_confirmed_escapes']}.", "",
        "## Session drilldown", "", "| session | repo | PR | eligibility | category | latency |", "|---|---|---:|---|---|---|",
    ])
    for row in cohort["sessions"]:
        lines.append(f"| `{row.get('session_id') or 'unknown'}` | `{row.get('repo') or 'unknown'}` | {row.get('pr_number') if row.get('pr_number') is not None else 'unknown'} | {row['eligibility']} | {row['category']} | {row['latency'].get('qualification', 'unknown')} |")
    lines.extend(["", "## Definitions and limits", ""])
    lines.extend(f"- {item}" for item in report["limitations"])
    lines.append("")
    return "\n".join(lines)


def _path_under(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def _reject_output_overlap(output: Path, inputs: Iterable[Optional[Path]]) -> None:
    output_resolved = output.resolve()
    for value in inputs:
        if value is None:
            continue
        source_resolved = Path(value).resolve()
        if _path_under(output_resolved, source_resolved) or _path_under(source_resolved, output_resolved):
            raise ReportError("report output overlaps an input bundle or evaluation root")


def write_report(report: Mapping[str, Any], output: Path, bundle: Optional[Path] = None, evaluation_root: Optional[Path] = None) -> tuple[Path, Path]:
    output = Path(output)
    _reject_output_overlap(output, (bundle, evaluation_root))
    if output.exists():
        if output.is_symlink() or not output.is_dir():
            raise ReportError("report output must be a directory")
        if any(output.iterdir()):
            raise ReportError("report output must be new or empty; overwrite is refused")
    else:
        output.mkdir(parents=True)
    snapshot = _parse_datetime(report["snapshot_at"], "snapshot_at").astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    stem = f"review-round-weekly-{report['week']}-snapshot-{snapshot}-{report['report_id'][:12]}"
    markdown_path = output / f"{stem}.md"
    json_path = output / f"{stem}.json"
    markdown_path.write_text(render_markdown(report), encoding="utf-8", newline="")
    json_path.write_bytes(pretty_json(report))
    return markdown_path, json_path


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Render the frozen offline review-round weekly report")
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--week", required=True)
    parser.add_argument("--as-of", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--evaluation-root", type=Path)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        report = build_report(args.bundle, args.week, args.as_of, args.evaluation_root)
        markdown_path, json_path = write_report(report, args.output, args.bundle, args.evaluation_root)
    except ReportError as exc:
        print(f"weekly report failed: {exc}", file=sys.stderr)
        return 2
    print(json.dumps({"markdown": str(markdown_path), "json": str(json_path), "report_id": report["report_id"]}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
