#!/usr/bin/env python3
"""Render a deterministic offline weekly review-round evidence report.

This module reads only the operator-captured bundle described in the frozen
Stage 2 design.  It never contacts the controller, GitHub, a database, or a
model, and its result cannot influence controller authority or retries.
"""

from __future__ import annotations

import argparse
import hashlib
import html
import json
import os
import re
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Mapping, Optional, Sequence
from zoneinfo import ZoneInfo


DEFINITION_VERSION = "review-round-weekly-report/v1"
MANIFEST_VERSION = "review-round-weekly-evidence/v1"
TAIPEI = ZoneInfo("Asia/Taipei")
REQUIRED_PRODUCT_TABLES = (
    "session_targets",
    "review_rounds",
    "review_findings",
    "github_writes",
    "runtime_event_receipts",
)
PRODUCT_COLUMNS = {
    "session_targets": {"session_id", "repo", "pr_number", "head_sha", "created_at", "reason", "required_valid_reviewers"},
    "review_rounds": {"id", "repo", "pr_number", "round", "session_id", "head_sha", "comment_id", "decision", "red", "yellow", "green", "created_at", "verified_commit_id", "integrity_disposition"},
    "review_findings": {"id", "session_id", "repo", "pr_number", "stable_id", "severity", "status", "head_sha", "created_at", "raised_by", "angle"},
    "github_writes": {"id", "session_id", "kind", "payload_json", "state", "attempts", "created_at", "claimed_at", "done_at"},
    "runtime_event_receipts": {"event_id", "body_sha256", "event_type", "session_id", "occurred_at", "received_at"},
}
OPTIONAL_LINES = ("human.ndjson", "cost.ndjson")
FULL_SHA = re.compile(r"^[0-9a-fA-F]{40}$")
HEX_SHA256 = re.compile(r"^[0-9a-f]{64}$")
MAX_FILE_BYTES = 128 * 1024 * 1024


class ReportError(RuntimeError):
    """An invalid, conflicting, or incomplete source bundle."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def pretty_json(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _read_bytes(path: Path) -> bytes:
    if path.is_symlink() or not path.is_file():
        raise ReportError(f"bundle path is not a regular file: {path.name}")
    data = path.read_bytes()
    if len(data) > MAX_FILE_BYTES:
        raise ReportError(f"bundle file exceeds the bound: {path.name}")
    return data


def _read_json(path: Path) -> tuple[Any, bytes]:
    raw = _read_bytes(path)
    try:
        return json.loads(raw.decode("utf-8")), raw
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ReportError(f"invalid JSON: {path.name}") from exc


def _parse_datetime(value: Any, field: str) -> datetime:
    if not isinstance(value, str) or not value:
        raise ReportError(f"{field} must be an offset-bearing ISO timestamp")
    text = value.replace("Z", "+00:00") if value.endswith("Z") else value
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError as exc:
        raise ReportError(f"{field} is not an ISO timestamp") from exc
    if parsed.tzinfo is None or parsed.utcoffset() is None:
        raise ReportError(f"{field} must include an offset")
    return parsed


def _timestamp_seconds(value: Any, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ReportError(f"{field} must be numeric")
    return float(value)


def _safe_ref(value: Any, field: str, maximum: int = 256) -> str:
    if not isinstance(value, (str, int)) or isinstance(value, bool):
        raise ReportError(f"{field} is not a safe source reference")
    text = str(value).replace("\r", "\\r").replace("\n", "\\n").replace("\t", "\\t")
    return html.escape(text[:maximum], quote=True)


def _session_id(value: Any) -> Optional[str]:
    if isinstance(value, Mapping):
        for key in ("session_id", "sessionId", "session"):
            candidate = value.get(key)
            if isinstance(candidate, str) and candidate:
                return candidate
        for nested in value.values():
            found = _session_id(nested)
            if found:
                return found
    elif isinstance(value, list):
        for nested in value:
            found = _session_id(nested)
            if found:
                return found
    return None


def _nested_value(value: Any, names: set[str]) -> Any:
    if isinstance(value, Mapping):
        for key, nested in value.items():
            if key in names:
                return nested
            found = _nested_value(nested, names)
            if found is not None:
                return found
    elif isinstance(value, list):
        for nested in value:
            found = _nested_value(nested, names)
            if found is not None:
                return found
    return None


def _event_kind(event: Mapping[str, Any]) -> str:
    value = event.get("kind")
    return value if isinstance(value, str) else ""


def _manifest_file_entry(manifest: Mapping[str, Any], name: str) -> Optional[Mapping[str, Any]]:
    files = manifest.get("files", {})
    if isinstance(files, Mapping):
        value = files.get(name)
        return value if isinstance(value, Mapping) else None
    if isinstance(files, list):
        for value in files:
            if isinstance(value, Mapping) and value.get("path", value.get("name")) == name:
                return value
    return None


def _verify_manifest_file(manifest: Mapping[str, Any], name: str, raw: bytes, record_count: Optional[int] = None) -> None:
    entry = _manifest_file_entry(manifest, name)
    if entry is None:
        raise ReportError(f"manifest lacks file hash for {name}")
    digest = entry.get("sha256")
    if not isinstance(digest, str) or not HEX_SHA256.fullmatch(digest) or digest != sha256_bytes(raw):
        raise ReportError(f"manifest hash mismatch for {name}")
    if record_count is not None:
        declared = entry.get("record_count", entry.get("records"))
        if declared is not None and declared != record_count:
            raise ReportError(f"manifest record count mismatch for {name}")


def _read_ndjson(path: Path, *, required: bool) -> tuple[list[dict[str, Any]], bytes]:
    if not path.exists():
        if required:
            raise ReportError(f"missing required bundle file: {path.name}")
        return [], b""
    raw = _read_bytes(path)
    rows: list[dict[str, Any]] = []
    for line_number, line in enumerate(raw.splitlines(), 1):
        if not line.strip():
            continue
        try:
            value = json.loads(line.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ReportError(f"invalid {path.name} record {line_number}") from exc
        if not isinstance(value, dict):
            raise ReportError(f"{path.name} record {line_number} is not an object")
        rows.append(value)
    return rows, raw


def _coverage(manifest: Mapping[str, Any], name: str) -> str:
    coverage = manifest.get("coverage", {})
    value = coverage.get(name) if isinstance(coverage, Mapping) else None
    return value if value in {"complete", "partial", "unknown"} else "unknown"


def _unique_index(rows: Sequence[Mapping[str, Any]], key_fn, label: str) -> tuple[dict[Any, Mapping[str, Any]], set[Any]]:
    index: dict[Any, Mapping[str, Any]] = {}
    conflicts: set[Any] = set()
    for row in rows:
        key = key_fn(row)
        if key is None:
            continue
        if key in index and canonical_json(index[key]) != canonical_json(row):
            conflicts.add(key)
        else:
            index[key] = row
    if conflicts:
        # Preserve the conflict as a source fact.  Callers classify affected
        # sessions unknown instead of selecting a last-write-wins row.
        return index, conflicts
    return index, conflicts


def _product_tables(product: Any) -> dict[str, list[dict[str, Any]]]:
    if not isinstance(product, Mapping) or set(REQUIRED_PRODUCT_TABLES) - set(product):
        raise ReportError("product.json must map every required source table to an array")
    result: dict[str, list[dict[str, Any]]] = {}
    for name in REQUIRED_PRODUCT_TABLES:
        rows = product[name]
        if not isinstance(rows, list) or any(not isinstance(row, dict) for row in rows):
            raise ReportError(f"product table {name} must be an array of objects")
        for row in rows:
            if not PRODUCT_COLUMNS[name].issubset(row):
                raise ReportError(f"product table {name} row lacks an exported source column")
            if "terminal_event" in row or "terminal_branch" in row:
                raise ReportError("product input may not invent terminal branch fields")
        result[name] = rows
    return result


def _audit_filter(rows: Sequence[Mapping[str, Any]], cutoff_ms: int) -> tuple[list[dict[str, Any]], int]:
    kept: list[dict[str, Any]] = []
    future = 0
    required = {"seq", "version", "event_id", "event_key", "occurred_at", "recorded_at", "service", "kind", "outcome", "correlation", "detail"}
    for row in rows:
        if not required.issubset(row):
            raise ReportError("audit record lacks a required envelope field")
        if not isinstance(row["event_id"], str) or not 1 <= len(row["event_id"].encode("utf-8")) <= 256:
            raise ReportError("audit event_id is outside the source bound")
        if not isinstance(row["event_key"], str) or not 1 <= len(row["event_key"].encode("utf-8")) <= 256:
            raise ReportError("audit event_key is outside the source bound")
        if isinstance(row["occurred_at"], bool) or not isinstance(row["occurred_at"], int):
            raise ReportError("audit occurred_at must be integer milliseconds")
        if isinstance(row["recorded_at"], bool) or not isinstance(row["recorded_at"], int):
            raise ReportError("audit recorded_at must be integer milliseconds")
        if row["recorded_at"] > cutoff_ms:
            future += 1
        else:
            kept.append(row)
    return kept, future


def _runtime_receipts(rows: Sequence[Mapping[str, Any]], cutoff_seconds: float) -> dict[str, list[dict[str, Any]]]:
    result: dict[str, list[dict[str, Any]]] = defaultdict(list)
    required = {"event_id", "body_sha256", "event_type", "session_id", "occurred_at", "received_at"}
    for row in rows:
        if not required.issubset(row):
            raise ReportError("runtime_event_receipts row lacks a required column")
        received = _timestamp_seconds(row["received_at"], "runtime receipt received_at")
        if received <= cutoff_seconds:
            session = row.get("session_id")
            if isinstance(session, str) and session:
                result[session].append(row)
    return result


def _kind_family(value: Any) -> str:
    text = str(value or "").lower()
    if "abandon" in text:
        return "comment_abandon"
    if "opening" in text:
        return "opening_comment"
    if "status" in text:
        return "status"
    if "review" in text and "reviewer" not in text:
        return "review"
    if "comment" in text:
        return "comment"
    return text


def _payload_fields(payload: str) -> dict[str, Any]:
    try:
        value = json.loads(payload)
    except (TypeError, json.JSONDecodeError):
        return {}
    return value if isinstance(value, dict) else {}


def _receipt_events(audit: Sequence[Mapping[str, Any]]) -> dict[str, list[dict[str, Any]]]:
    result: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for event in audit:
        kind = _event_kind(event).lower()
        if "github.write" not in kind and not kind.startswith("github_"):
            continue
        detail = event.get("detail") if isinstance(event.get("detail"), Mapping) else {}
        operation = detail.get("operation", detail.get("kind", detail.get("write_kind")))
        if operation is None:
            operation = _nested_value(detail, {"operation", "write_kind", "kind"})
        family = _kind_family(operation)
        if family not in {"comment", "status", "review", "comment_abandon"}:
            continue
        outcome = str(event.get("outcome", "")).lower()
        if not ("succeed" in kind or "reconcil" in kind or outcome in {"success", "succeeded", "reconciled"}):
            continue
        session = _session_id(event)
        if not session:
            continue
        request_sha = detail.get("request_sha256", detail.get("request_sha"))
        provider = detail.get("provider_receipt", detail.get("receipt", {}))
        if not isinstance(provider, Mapping):
            provider = {}
        result[session].append(
            {
                "event": event,
                "family": family,
                "request_sha256": request_sha,
                "provider": dict(provider),
                "reconciled": "reconcil" in kind or outcome == "reconciled",
            }
        )
    return result


def _provider_value(receipt: Mapping[str, Any], names: Sequence[str]) -> Any:
    for name in names:
        if name in receipt:
            return receipt[name]
    return None


def _visible_comment(receipts: Sequence[Mapping[str, Any]], family: str = "comment") -> bool:
    for receipt in receipts:
        if receipt.get("family") != family:
            continue
        provider = receipt.get("provider", {})
        if _provider_value(provider, ("comment_id", "id", "commentId")) not in (None, "", 0):
            return True
    return False


def _write_rows_for_session(rows: Sequence[Mapping[str, Any]], session: str) -> list[Mapping[str, Any]]:
    return [row for row in rows if row.get("session_id") == session]


def _bound_receipts_for_row(row: Mapping[str, Any], receipts: Sequence[Mapping[str, Any]]) -> list[Mapping[str, Any]]:
    payload = row.get("payload_json")
    if not isinstance(payload, str):
        return []
    digest = sha256_bytes(payload.encode("utf-8"))
    write_id = row.get("id")
    family = _kind_family(row.get("kind"))
    result = []
    for receipt in receipts:
        event = receipt["event"]
        detail = event.get("detail") if isinstance(event.get("detail"), Mapping) else {}
        detail_write_id = detail.get("write_id", detail.get("id"))
        if receipt.get("request_sha256") == digest and receipt.get("family") == family and (detail_write_id in (None, write_id)):
            result.append(receipt)
    return result


def _expected_status(payload_fields: Mapping[str, Any]) -> Optional[str]:
    value = payload_fields.get("state", payload_fields.get("status"))
    return str(value).lower() if value is not None else None


def _receipt_state(receipt: Mapping[str, Any]) -> Optional[str]:
    provider = receipt.get("provider", {})
    value = _provider_value(provider, ("state", "status"))
    return str(value).lower() if value is not None else None


def _receipt_event(receipt: Mapping[str, Any]) -> Optional[str]:
    provider = receipt.get("provider", {})
    value = _provider_value(provider, ("event", "event_type", "review_event"))
    return str(value).upper() if value is not None else None


def _receipt_time(receipt: Mapping[str, Any]) -> Optional[int]:
    value = receipt.get("event", {}).get("occurred_at")
    if isinstance(value, int) and value >= 0:
        return value
    return None


def _classify_session(
    session: str,
    target: Optional[Mapping[str, Any]],
    round_rows: Sequence[Mapping[str, Any]],
    write_rows: Sequence[Mapping[str, Any]],
    runtime: Sequence[Mapping[str, Any]],
    receipts: Sequence[Mapping[str, Any]],
    *,
    target_conflict: bool = False,
    accepted_ms: Optional[int] = None,
    action_completed: bool = False,
) -> dict[str, Any]:
    superseded = any(row.get("event_type") == "session.superseded" for row in runtime)
    timeout = any(row.get("event_type") == "session.timeout" for row in runtime)
    tombstone = _visible_comment(receipts, "comment_abandon")
    if superseded:
        return {"category": "superseded", "tombstone_visible": tombstone, "timeout_evidence": timeout, "latency": _latency(accepted_ms, receipts, None)}
    if target_conflict or target is None:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": timeout, "latency": _latency(accepted_ms, receipts, None)}
    full_sha = isinstance(target.get("head_sha"), str) and bool(FULL_SHA.fullmatch(target["head_sha"]))
    round_by_number: dict[Any, Mapping[str, Any]] = {}
    conflict = False
    for row in round_rows:
        if row.get("repo") != target.get("repo") or row.get("pr_number") != target.get("pr_number") or str(row.get("head_sha", "")).lower() != str(target.get("head_sha", "")).lower():
            conflict = True
        key = row.get("round")
        if key in round_by_number and canonical_json(round_by_number[key]) != canonical_json(row):
            conflict = True
        else:
            round_by_number[key] = row
    round_row = round_by_number[max(round_by_number)] if round_by_number and all(isinstance(key, int) for key in round_by_number) else None
    if timeout:
        category = "visible_failure" if tombstone else "pending_or_unknown"
        return {"category": category, "tombstone_visible": tombstone, "timeout_evidence": True, "latency": _latency(accepted_ms, receipts, "comment_abandon" if tombstone else None)}
    if round_row is None or conflict:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    if not action_completed:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    decision = str(round_row.get("decision", "")).lower()
    integrity = str(round_row.get("integrity_disposition", ""))
    if integrity == "legacy_unverified":
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    diagnostic = integrity != "verified" or decision == "insufficient_valid_reviewers"
    expected = {"comment"}
    status_expected: Optional[str] = None
    review_expected: Optional[str] = None
    if not diagnostic and decision in {"approve", "request_changes"}:
        if str(round_row.get("verified_commit_id", "")).lower() != str(target.get("head_sha", "")).lower():
            return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
        expected.update({"status", "review"})
        status_expected = "success" if decision == "approve" else "failure"
        review_expected = "APPROVE" if decision == "approve" else "REQUEST_CHANGES"
    elif not diagnostic:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    if diagnostic and full_sha:
        expected.add("status")
        status_expected = "error"
    relevant_rows = [row for row in write_rows if _kind_family(row.get("kind")) in {"comment", "status", "review", "comment_abandon"}]
    actual_families = {_kind_family(row.get("kind")) for row in relevant_rows}
    if "comment_abandon" in actual_families:
        # A no-op abandon is not a terminal round projection.  It is only used
        # by the timeout branch above.
        actual_families.discard("comment_abandon")
    if actual_families != expected or len(relevant_rows) != len(expected):
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    bound: dict[str, list[Mapping[str, Any]]] = {}
    for row in relevant_rows:
        family = _kind_family(row.get("kind"))
        bound.setdefault(family, []).extend(_bound_receipts_for_row(row, receipts))
    if any(len(bound.get(family, [])) != 1 for family in expected):
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    if not _visible_comment(bound["comment"]):
        return {"category": "pending_or_unknown", "tombstone_visible": False, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    status_ok = True
    if "status" in expected:
        status_ok = _receipt_state(bound["status"][0]) == status_expected
    review_ok = True
    if "review" in expected:
        review_ok = _receipt_event(bound["review"][0]) == review_expected
    if not status_ok or not review_ok:
        return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    target_sha = str(target.get("head_sha", "")).lower()
    for family in ("status", "review"):
        if family not in bound:
            continue
        provider_sha = _provider_value(bound[family][0].get("provider", {}), ("sha", "head_sha", "commit_id", "commit_sha"))
        if provider_sha is None or str(provider_sha).lower() != target_sha:
            return {"category": "pending_or_unknown", "tombstone_visible": tombstone, "timeout_evidence": False, "latency": _latency(accepted_ms, receipts, None)}
    category = "visible_failure" if diagnostic else "reliable"
    return {
        "category": category,
        "tombstone_visible": tombstone,
        "timeout_evidence": False,
        "latency": _latency(accepted_ms, receipts, expected),
        "planned_families": sorted(expected),
    }


def _latency(accepted_ms: Optional[int], receipts: Sequence[Mapping[str, Any]], required: Any) -> dict[str, Any]:
    if accepted_ms is None or not required:
        return {"status": "unknown", "duration_ms": None, "qualification": "unavailable"}
    families = {required} if isinstance(required, str) else set(required)
    relevant = [receipt for receipt in receipts if receipt.get("family") in families and _receipt_time(receipt) is not None]
    if len({receipt.get("family") for receipt in relevant}) != len(families):
        return {"status": "unknown", "duration_ms": None, "qualification": "incomplete_receipts"}
    end = max(_receipt_time(receipt) for receipt in relevant if _receipt_time(receipt) is not None)
    if end < accepted_ms:
        return {"status": "clock_invalid", "duration_ms": None, "qualification": "clock_invalid"}
    upper = any(receipt.get("reconciled") for receipt in relevant)
    return {"status": "observed", "duration_ms": end - accepted_ms, "qualification": "reconciliation_upper_bound" if upper else "original_receipt"}


def _human_metrics(rows: Sequence[Mapping[str, Any]], cutoff: datetime, finding_rows: Sequence[Mapping[str, Any]], coverage: str = "unknown") -> dict[str, Any]:
    counts = Counter({key: 0 for key in ("valid_useful", "valid_not_useful", "invalid", "unknown")})
    escape = Counter({key: 0 for key in ("confirmed_escape", "not_escape", "unknown")})
    valid_rows = 0
    conflicts: set[tuple[Any, ...]] = set()
    seen: dict[tuple[Any, ...], str] = {}
    for row in rows:
        stamp = row.get("observed_at", row.get("annotated_at", row.get("confirmed_at")))
        try:
            if isinstance(stamp, str):
                observed = _parse_datetime(stamp, "human timestamp")
            else:
                observed = datetime.fromtimestamp(_timestamp_seconds(stamp, "human timestamp"), timezone.utc)
        except ReportError:
            counts["unknown"] += 1
            continue
        if observed > cutoff:
            continue
        kind = row.get("type", row.get("kind", "annotation"))
        if kind == "escape":
            if not all(key in row for key in ("confirming_human_id", "confirmed_at", "version", "reviewed_window_id", "evidence_reference")):
                escape["unknown"] += 1
                continue
            status = row.get("status", row.get("verdict"))
            if status in {"confirmed_escape", "not_escape", "unknown"}:
                escape[status] += 1
            else:
                escape["unknown"] += 1
            continue
        required = ("annotator_id", "version", "session_id", "finding_id", "repo", "pr_number", "head_sha", "verdict", "evidence_reference")
        if not all(key in row for key in required):
            counts["unknown"] += 1
            continue
        if not any(
            finding.get("session_id") == row["session_id"]
            and (finding.get("stable_id", finding.get("id")) == row["finding_id"])
            and finding.get("repo") == row["repo"]
            and finding.get("pr_number") == row["pr_number"]
            and finding.get("head_sha") == row["head_sha"]
            for finding in finding_rows
        ):
            counts["unknown"] += 1
            continue
        key = (row["session_id"], row["finding_id"], row["repo"], row["pr_number"], row["head_sha"])
        verdict = row["verdict"]
        if key in seen and seen[key] != verdict:
            conflicts.add(key)
        seen[key] = verdict
        if verdict in counts:
            valid_rows += 1
            counts[verdict] += 1
        else:
            counts["unknown"] += 1
    for key in conflicts:
        old = seen.get(key)
        if old in counts:
            counts[old] -= 1
        counts["unknown"] += 1
    return {
        "coverage": coverage if coverage in {"complete", "partial", "unknown"} else "unknown",
        "annotation_counts": dict(sorted(counts.items())),
        "annotation_denominator": sum(counts.values()),
        "valid_annotation_rows": valid_rows,
        "confirmed_escapes": escape["confirmed_escape"],
        "escape_counts": dict(sorted(escape.items())),
        "reviewed_window_coverage": "known" if rows else "unknown",
        "recall_claim": "not_available",
    }


def _cost_metrics(rows: Sequence[Mapping[str, Any]], cutoff: datetime, eligible_sessions: set[str], coverage: str = "unknown") -> dict[str, Any]:
    currencies: dict[str, dict[str, Any]] = defaultdict(lambda: {"known_minor": 0, "partial_known_minor": 0, "known_sessions": 0, "partial_sessions": 0, "unknown_sessions": 0, "minor_unit_definitions": set()})
    session_seen: dict[str, list[Mapping[str, Any]]] = defaultdict(list)
    unknown_rows = 0
    for row in rows:
        if row.get("session_id") not in eligible_sessions:
            continue
        stamp = row.get("reconciliation_time", row.get("reconciled_at"))
        try:
            observed = _parse_datetime(stamp, "cost reconciliation time") if isinstance(stamp, str) else datetime.fromtimestamp(_timestamp_seconds(stamp, "cost reconciliation time"), timezone.utc)
        except ReportError:
            unknown_rows += 1
            continue
        if observed <= cutoff:
            session_seen[row["session_id"]].append(row)
    for session, session_rows in session_seen.items():
        for row in session_rows:
            currency = row.get("currency")
            minor_unit = row.get("source_minor_unit", row.get("minor_unit_definition", row.get("source_reference")))
            if not isinstance(currency, str) or isinstance(row.get("amount_minor"), bool) or not isinstance(row.get("amount_minor"), int) or not isinstance(minor_unit, (str, int)) or not row.get("reconciliation_reference", row.get("source_reference")):
                unknown_rows += 1
                continue
            bucket = currencies[currency]
            bucket["minor_unit_definitions"].add(str(minor_unit))
            complete = row.get("completeness") == "complete" and row.get("attempt_coverage") == "all_attempts_and_retries"
            if complete:
                bucket["known_minor"] += row["amount_minor"]
                bucket["known_sessions"] += 1
            elif row.get("completeness") == "partial":
                bucket["partial_known_minor"] += row["amount_minor"]
                bucket["partial_sessions"] += 1
            else:
                bucket["unknown_sessions"] += 1
    for bucket in currencies.values():
        if len(bucket["minor_unit_definitions"]) > 1:
            bucket["unknown_sessions"] += bucket["known_sessions"] + bucket["partial_sessions"]
            bucket["known_minor"] = 0
            bucket["partial_known_minor"] = 0
            bucket["known_sessions"] = 0
            bucket["partial_sessions"] = 0
    rendered_currencies = {}
    for currency, bucket in sorted(currencies.items()):
        rendered_currencies[currency] = {key: (sorted(value) if isinstance(value, set) else value) for key, value in sorted(bucket.items())}
    return {
        "coverage": coverage if coverage in {"complete", "partial", "unknown"} else "unknown",
        "per_currency": rendered_currencies,
        "unknown_rows": unknown_rows,
        "actual_total_status": "unknown" if unknown_rows or any(bucket["partial_sessions"] or bucket["unknown_sessions"] for bucket in currencies.values()) else "known_per_currency",
        "pricing_table_used": False,
        "cross_currency_arithmetic": False,
    }


def _evaluation_metrics(root: Optional[Path]) -> dict[str, Any]:
    if root is None:
        return {"availability": "not_supplied", "denominator": 0, "supported": 0, "refuted": 0, "unresolved": 0, "validation_coverage": {}, "omission_candidates": 0, "automatically_supported_omission": 0, "human_confirmed_escapes": 0}
    root = Path(root).resolve()
    if not root.is_dir() or root.is_symlink():
        return {"availability": "unavailable", "reason": "evaluation_root_not_directory", "denominator": 0, "supported": 0, "refuted": 0, "unresolved": 0, "validation_coverage": {}, "omission_candidates": 0, "automatically_supported_omission": 0, "human_confirmed_escapes": 0}
    paths = [root / "summary.json"] + sorted(root.glob("*/summary.json"))
    summaries = []
    for path in paths:
        if not path.is_file() or path.is_symlink():
            continue
        try:
            value, _ = _read_json(path)
        except ReportError:
            continue
        if isinstance(value, Mapping) and isinstance(value.get("model_assessment"), Mapping):
            summaries.append(value)
    if not summaries:
        return {"availability": "incomplete", "denominator": 0, "supported": 0, "refuted": 0, "unresolved": 0, "validation_coverage": {}, "omission_candidates": 0, "automatically_supported_omission": 0, "human_confirmed_escapes": 0}
    supported = sum(int(item["model_assessment"].get("supported", 0)) for item in summaries)
    refuted = sum(int(item["model_assessment"].get("refuted", 0)) for item in summaries)
    unresolved = sum(int(item["model_assessment"].get("unresolved", 0)) for item in summaries)
    coverage: Counter[str] = Counter()
    for item in summaries:
        if isinstance(item.get("validation_coverage"), Mapping):
            for key, value in item["validation_coverage"].items():
                coverage[key] += int(value)
    omission = sum(int(item.get("omissions", {}).get("candidate_count", 0)) for item in summaries if isinstance(item.get("omissions"), Mapping))
    automatic = sum(int(item.get("omissions", {}).get("automatically_supported_omission", 0)) for item in summaries if isinstance(item.get("omissions"), Mapping))
    availability = "available" if all(item.get("state") == "complete" for item in summaries) else "incomplete"
    return {
        "availability": availability,
        "denominator": supported + refuted + unresolved,
        "supported": supported,
        "refuted": refuted,
        "unresolved": unresolved,
        "validation_coverage": dict(sorted(coverage.items())),
        "omission_candidates": omission,
        "automatically_supported_omission": automatic,
        "human_confirmed_escapes": 0,
        "recall_claim": "not_available",
    }


def _latency_summary(rows: Sequence[Mapping[str, Any]]) -> dict[str, Any]:
    counts = Counter({key: 0 for key in ("observed", "reconciliation_upper_bound", "unknown", "clock_invalid")})
    durations = []
    for row in rows:
        latency = row.get("latency", {})
        status = latency.get("status", "unknown")
        qualification = latency.get("qualification")
        if status == "observed" and qualification == "reconciliation_upper_bound":
            counts["reconciliation_upper_bound"] += 1
        elif status == "observed":
            counts["observed"] += 1
        elif status == "clock_invalid":
            counts["clock_invalid"] += 1
        else:
            counts["unknown"] += 1
        if isinstance(latency.get("duration_ms"), int):
            durations.append(latency["duration_ms"])
    return {"counts": dict(sorted(counts.items())), "denominator": len(rows), "durations_ms": sorted(durations), "actual_latency_status": "known" if len(durations) == len(rows) and rows else "unknown"}


def build_report(bundle: Path, week: str, as_of: str, evaluation_root: Optional[Path] = None) -> dict[str, Any]:
    bundle = Path(bundle).resolve()
    if not bundle.is_dir() or bundle.is_symlink():
        raise ReportError("bundle must be a regular directory")
    cutoff = _parse_datetime(as_of, "--as-of")
    manifest, manifest_raw = _read_json(bundle / "evidence-manifest.json")
    if not isinstance(manifest, Mapping) or manifest.get("schema_version") != MANIFEST_VERSION:
        raise ReportError("unsupported evidence manifest version")
    manifest_snapshot = _parse_datetime(manifest.get("snapshot_at"), "manifest.snapshot_at")
    if manifest_snapshot != cutoff or manifest.get("timezone") != "Asia/Taipei":
        raise ReportError("manifest snapshot_at/timezone does not equal --as-of")
    coverage_map = manifest.get("coverage")
    if not isinstance(coverage_map, Mapping) or any(name not in coverage_map for name in ("audit", "product", "human", "cost")):
        raise ReportError("manifest must declare metric-specific source coverage")
    audit_cursor = manifest.get("audit_cursor")
    if coverage_map.get("audit") == "complete":
        if not isinstance(audit_cursor, Mapping) or audit_cursor.get("final_null_cursor") is not True or not isinstance(audit_cursor.get("page_count"), int):
            raise ReportError("complete audit coverage requires a completed null-cursor capture")
    taipei_week = cutoff.astimezone(TAIPEI).isocalendar()
    expected_week = f"{taipei_week.year:04d}-W{taipei_week.week:02d}"
    if week != expected_week:
        raise ReportError("--week is not the Taipei ISO week of --as-of")
    audit_rows, audit_raw = _read_ndjson(bundle / "audit.ndjson", required=True)
    product_value, product_raw = _read_json(bundle / "product.json")
    tables = _product_tables(product_value)
    _verify_manifest_file(manifest, "audit.ndjson", audit_raw, len(audit_rows))
    _verify_manifest_file(manifest, "product.json", product_raw, None)
    optional: dict[str, list[dict[str, Any]]] = {}
    optional_raw: dict[str, bytes] = {}
    for name in OPTIONAL_LINES:
        optional[name], optional_raw[name] = _read_ndjson(bundle / name, required=False)
        if optional_raw[name]:
            _verify_manifest_file(manifest, name, optional_raw[name], len(optional[name]))
        elif coverage_map.get(name.removesuffix(".ndjson")) in {"complete", "partial"}:
            raise ReportError(f"manifest declares {name} coverage but the file is absent")
    cutoff_ms = int(cutoff.timestamp() * 1000)
    cutoff_seconds = cutoff.timestamp()
    audit, future_audit = _audit_filter(audit_rows, cutoff_ms)
    targets, target_conflicts = _unique_index(tables["session_targets"], lambda row: row.get("session_id"), "session_targets")
    runtime = _runtime_receipts(tables["runtime_event_receipts"], cutoff_seconds)
    receipt_events = _receipt_events(audit)
    completed_sessions = {session for event in audit if _event_kind(event) == "action.completed" and _session_id(event) for session in [_session_id(event)]}
    plan_only = sum(1 for event in audit if _event_kind(event) == "ingress.accepted" and str(event.get("outcome")) == "accepted")
    accepted_events: dict[str, Mapping[str, Any]] = {}
    unmatched = 0
    for event in audit:
        if _event_kind(event) != "action.accepted" or event.get("outcome") != "accepted":
            continue
        session = _session_id(event)
        if not session:
            local = datetime.fromtimestamp(event["occurred_at"] / 1000, timezone.utc).astimezone(TAIPEI)
            if f"{local.isocalendar().year:04d}-W{local.isocalendar().week:02d}" == week:
                unmatched += 1
            continue
        current = accepted_events.get(session)
        if current is None or event["occurred_at"] < current["occurred_at"]:
            accepted_events[session] = event
    accepted_in_week: dict[str, Mapping[str, Any]] = {}
    for session, event in accepted_events.items():
        local = datetime.fromtimestamp(event["occurred_at"] / 1000, timezone.utc).astimezone(TAIPEI)
        if f"{local.isocalendar().year:04d}-W{local.isocalendar().week:02d}" == week:
            accepted_in_week[session] = event
    session_reports: list[dict[str, Any]] = []
    excluded_ask = 0
    unknown_eligibility = unmatched
    eligible_sessions: set[str] = set()
    reliability_source_complete = _coverage(manifest, "audit") == "complete" and _coverage(manifest, "product") == "complete" and isinstance(audit_cursor, Mapping) and audit_cursor.get("final_null_cursor") is True
    for session, event in sorted(accepted_in_week.items()):
        target = targets.get(session)
        if session in target_conflicts:
            unknown_eligibility += 1
            reason = "conflicting_target"
        elif target is None:
            unknown_eligibility += 1
            reason = "missing_target"
        elif target.get("reason") == "ask":
            excluded_ask += 1
            reason = "ask_excluded"
        else:
            eligible_sessions.add(session)
            reason = "review"
        round_rows = [row for row in tables["review_rounds"] if row.get("session_id") == session]
        write_rows = _write_rows_for_session(tables["github_writes"], session)
        classification = _classify_session(
            session,
            target,
            round_rows,
            write_rows,
            runtime.get(session, []),
            receipt_events.get(session, []),
            target_conflict=session in target_conflicts,
            accepted_ms=event["occurred_at"],
            action_completed=session in completed_sessions,
        )
        if session not in eligible_sessions:
            classification["category"] = "excluded" if reason == "ask_excluded" else "pending_or_unknown"
        elif not reliability_source_complete:
            classification["category"] = "pending_or_unknown"
            classification["source_coverage_blocked"] = True
        session_reports.append(
            {
                "session_id": _safe_ref(session, "session_id"),
                "repo": _safe_ref(target.get("repo"), "repo") if isinstance(target, Mapping) and target.get("repo") is not None else None,
                "pr_number": target.get("pr_number") if isinstance(target, Mapping) and isinstance(target.get("pr_number"), int) else None,
                "eligibility": reason,
                "action_completed": session in completed_sessions,
                **classification,
            }
        )
    reliability_counts = Counter({key: 0 for key in ("reliable", "visible_failure", "superseded", "pending_or_unknown")})
    for row in session_reports:
        category = row.get("category")
        if category in reliability_counts and row.get("eligibility") == "review":
            reliability_counts[category] += 1
    human = _human_metrics(optional["human.ndjson"], cutoff, tables["review_findings"], _coverage(manifest, "human"))
    cost = _cost_metrics(optional["cost.ndjson"], cutoff, eligible_sessions, _coverage(manifest, "cost"))
    manifest_digest = sha256_bytes(manifest_raw)
    report_id = sha256_bytes((DEFINITION_VERSION + week + as_of + manifest_digest).encode("utf-8"))
    report = {
        "definition_version": DEFINITION_VERSION,
        "week": week,
        "snapshot_at": as_of,
        "timezone": "Asia/Taipei",
        "report_id": report_id,
        "manifest_sha256": manifest_digest,
        "capture": {
            "coverage": {name: _coverage(manifest, name) for name in ("audit", "product", "human", "cost")},
            "future_audit_records_excluded": future_audit,
            "non_atomic_capture": True,
            "file_hashes": {name: sha256_bytes(raw) for name, raw in {"audit.ndjson": audit_raw, "product.json": product_raw, **optional_raw}.items() if raw},
        },
        "cohort": {
            "accepted_total": len(accepted_in_week) + unmatched,
            "eligible_review_total": len(eligible_sessions),
            "excluded_ask": excluded_ask,
            "unknown_eligibility": unknown_eligibility,
            "unmatched_actions": unmatched,
            "plan_only": plan_only,
            "deduplicated_sessions": len(accepted_events),
            "sessions": session_reports,
        },
        "reliability": {
            **dict(sorted(reliability_counts.items())),
            "denominator": len(eligible_sessions),
            "unknown_coverage": unknown_eligibility + (len(eligible_sessions) if not reliability_source_complete else 0),
        },
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
        "evaluation": _evaluation_metrics(evaluation_root),
        "limitations": [
            "Reliability is source-bound and mutually classified; missing joins remain pending_or_unknown.",
            "Human annotations and evaluation metrics have separate denominators and do not establish recall.",
            "No model call, price table, estimate, or production write is performed by this report.",
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
        f"# Review-round weekly report — {report['week']}",
        "",
        f"Snapshot: `{report['snapshot_at']}`  ",
        f"Report ID: `{report['report_id']}`  ",
        "Source: frozen local bundle; no live collection or model call.",
        "",
        "## Cohort",
        "",
        "| metric | count |",
        "|---|---:|",
        f"| accepted action records | {cohort['accepted_total']} |",
        f"| eligible review sessions | {cohort['eligible_review_total']} |",
        f"| ask exclusions | {cohort['excluded_ask']} |",
        f"| eligibility unknown | {cohort['unknown_eligibility']} |",
        f"| plan-only ingress accepted | {cohort['plan_only']} |",
        "",
        "## Reliability",
        "",
        "| category | count |",
        "|---|---:|",
    ]
    for key in ("reliable", "visible_failure", "superseded", "pending_or_unknown"):
        lines.append(f"| {key} | {reliability[key]} |")
    lines += [
        f"| denominator | {reliability['denominator']} |",
        "",
        "## Latency",
        "",
        f"Observed exact: {latency['counts']['observed']}; reconciliation upper bound: {latency['counts']['reconciliation_upper_bound']}; unknown: {latency['counts']['unknown']}; clock-invalid: {latency['counts']['clock_invalid']}.",
        "",
        "## Human quality and cost",
        "",
        f"Human coverage: `{human['coverage']}`; useful: {human['annotation_counts']['valid_useful']}; not useful: {human['annotation_counts']['valid_not_useful']}; invalid: {human['annotation_counts']['invalid']}; unknown: {human['annotation_counts']['unknown']}; confirmed escapes: {human['confirmed_escapes']}. Recall is not claimed.",
        f"Cost coverage: `{cost['coverage']}`; actual total status: `{cost['actual_total_status']}`; pricing table used: `{cost['pricing_table_used']}`.",
        "",
        "## Model evaluation (separate denominator)",
        "",
        f"Availability: `{evaluation['availability']}`; denominator: {evaluation['denominator']}; supported: {evaluation['supported']}; refuted: {evaluation['refuted']}; unresolved: {evaluation['unresolved']}; automatic omission candidates: {evaluation['automatically_supported_omission']}. Human-confirmed escapes remain separate.",
        "",
        "## Definitions and limitations",
        "",
    ]
    lines.extend(f"- {item}" for item in report["limitations"])
    lines.append("")
    return "\n".join(lines)


def write_report(report: Mapping[str, Any], output: Path, bundle: Optional[Path] = None) -> tuple[Path, Path]:
    output = Path(output).resolve()
    if bundle is not None:
        bundle = Path(bundle).resolve()
        try:
            output.relative_to(bundle)
            raise ReportError("report output must not overlap the input bundle")
        except ValueError:
            pass
        try:
            bundle.relative_to(output)
            raise ReportError("report output must not overlap the input bundle")
        except ValueError:
            pass
    if output.exists():
        if output.is_symlink() or not output.is_dir():
            raise ReportError("report output must be a directory")
        if any(output.iterdir()):
            raise ReportError("report output must be new or empty; overwrite is refused")
    else:
        output.mkdir(parents=True)
    snapshot = _parse_datetime(report["snapshot_at"], "snapshot_at").astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    stem = f"review-round-weekly-{report['week']}-snapshot-{snapshot}-{report['report_id'][:12]}"
    json_path = output / f"{stem}.json"
    markdown_path = output / f"{stem}.md"
    json_path.write_bytes(pretty_json(report))
    markdown_path.write_text(render_markdown(report), encoding="utf-8", newline="")
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
        markdown_path, json_path = write_report(report, args.output, args.bundle)
    except ReportError as exc:
        print(f"weekly report failed: {type(exc).__name__}", file=sys.stderr)
        return 2
    print(json.dumps({"markdown": str(markdown_path), "json": str(json_path), "report_id": report["report_id"]}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
