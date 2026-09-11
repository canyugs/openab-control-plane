#!/usr/bin/env python3
"""Read-only bridge from the controller observation API to the evaluators.

The bridge has three deliberately separate stages:

* ``capture`` signs two bounded controller GETs and retains their exact JSON
  response bytes;
* ``prepare`` binds those bytes to one repository revision and produces the
  existing evaluator and weekly-report input formats; and
* ``run`` verifies that prepared material, invokes the released evaluator API,
  and feeds only verified evaluator output to the existing weekly reporter.

This module is an integration seam, not a new service or persistence layer.
It never calls a provider during capture or preparation and never forwards the
observation credential to a model or to an artifact.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import importlib.util
import ipaddress
import json
import os
import re
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Iterable, Mapping, Optional, Sequence
from zoneinfo import ZoneInfo


BRIDGE_VERSION = "review-evaluation-bridge/v1"
CAPTURE_VERSION = "review-evaluation-capture/v1"
PREPARATION_VERSION = "review-evaluation-preparation/v1"
FINDINGS_VERSION = "review-model-findings/v1"
EVIDENCE_VERSION = "review-model-evidence/v1"
WEEKLY_MANIFEST_VERSION = "review-round-weekly-evidence/v1"

TAIPEI = ZoneInfo("Asia/Taipei")
FULL_SHA = re.compile(r"^[0-9a-fA-F]{40}$")
HEX_SHA256 = re.compile(r"^[0-9a-f]{64}$")

FINDINGS_LIMIT = 5000
AUDIT_LIMIT = 500
DEFAULT_MAX_PAGES = 100
MAX_PAGE_BYTES = 16 * 1024 * 1024
MAX_REQUEST_SECONDS = 15.0
MAX_INPUT_BYTES = 64 * 1024 * 1024
MAX_SESSION_BYTES = 512
MAX_REPOSITORY_BYTES = 512

DEFAULT_SOURCE_LIMITS = {
    "max_files": 10_000,
    "max_file_bytes": 2 * 1024 * 1024,
    "max_total_bytes": 32 * 1024 * 1024,
    "max_diff_bytes": 8 * 1024 * 1024,
}

PRODUCT_TABLES = (
    "session_targets",
    "review_rounds",
    "review_findings",
    "github_writes",
    "runtime_event_receipts",
)

PRODUCT_SCOPE_FIELDS = {
    "session_targets": ("repo", "pr_number", "session_id", "head_sha"),
    "review_rounds": ("repo", "pr_number", "session_id", "head_sha"),
    "review_findings": ("repo", "pr_number", "session_id", "head_sha"),
    "github_writes": ("session_id",),
    "runtime_event_receipts": ("session_id",),
}


class BridgeError(RuntimeError):
    """The bridge input, remote observation, or generated artifact is invalid."""


class BridgeConflict(BridgeError):
    """An immutable input or output identity conflicts with another value."""


class _RedirectRejected(BridgeError):
    pass


_CORE: Any = None
_WEEKLY: Any = None


def _load_sibling(filename: str, name: str) -> Any:
    path = Path(__file__).with_name(filename)
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise BridgeError(f"required integration module is unavailable: {filename}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def _core() -> Any:
    global _CORE
    if _CORE is None:
        _CORE = _load_sibling("review_model_evaluation.py", "review_model_evaluation_bridge_core")
    return _CORE


def _weekly() -> Any:
    global _WEEKLY
    if _WEEKLY is None:
        # The weekly reporter imports the released evaluator by its normal
        # module name when verifying an evaluation root. Register the exact
        # sibling module used by this bridge when the bridge was imported by
        # path rather than launched from scripts/.
        if sys.modules.get("review_model_evaluation") is None:
            sys.modules["review_model_evaluation"] = _core()
        _WEEKLY = _load_sibling("review_round_weekly_report.py", "review_round_weekly_report_bridge")
    return _WEEKLY


def run_evaluation(**kwargs: Any) -> Mapping[str, Any]:
    """Public evaluator seam; tests may replace it without claiming a live run."""

    result = _core().run_evaluation(**kwargs)
    if not isinstance(result, Mapping):
        raise BridgeError("evaluator returned a non-object result")
    return result


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
            raise BridgeError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _read_json(path: Path, *, maximum: int = MAX_INPUT_BYTES) -> tuple[Any, bytes]:
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise BridgeError(f"input is not a regular file: {path.name}")
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise BridgeError(f"cannot read input file: {path.name}") from exc
    if len(raw) > maximum:
        raise BridgeError(f"input exceeds the bound: {path.name}")
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_reject_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BridgeError(f"invalid JSON input: {path.name}") from exc
    return value, raw


def _bounded_text(value: Any, field: str, maximum: int = 4096, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value) or len(value.encode("utf-8")) > maximum:
        raise BridgeError(f"{field} must be bounded UTF-8 text")
    if "\x00" in value or any(ord(char) < 0x20 and char not in "\t\n\r" for char in value):
        raise BridgeError(f"{field} contains control characters")
    return value


def _safe_relative_path(value: Any, field: str = "path") -> str:
    path = _bounded_text(value, field, 1024)
    if "\\" in path or path.startswith("/"):
        raise BridgeError(f"{field} must be a relative POSIX path")
    parsed = PurePosixPath(path)
    if parsed.is_absolute() or ".." in parsed.parts or str(parsed) != path or path.endswith("/"):
        raise BridgeError(f"{field} is unsafe")
    return path


def _validate_sha256(value: Any, field: str) -> str:
    if not isinstance(value, str) or not HEX_SHA256.fullmatch(value):
        raise BridgeError(f"{field} is not a lowercase SHA-256")
    return value


def _validate_full_sha(value: Any, field: str) -> str:
    if not isinstance(value, str) or not FULL_SHA.fullmatch(value):
        raise BridgeError(f"{field} must be a full 40-hex SHA")
    return value.lower()


def _path_has_symlink_component(path: Path) -> Optional[Path]:
    absolute = Path(path).absolute()
    current = Path(absolute.anchor) if absolute.anchor else Path()
    parts = absolute.parts[1:] if absolute.anchor else absolute.parts
    for part in parts:
        current = current / part
        if os.path.lexists(current) and current.is_symlink():
            # macOS exposes the ordinary temporary roots through these
            # system aliases (for example /var -> /private/var). They are not
            # operator-controlled input components; links below them remain
            # rejected.
            if str(current) in {"/var", "/tmp"}:
                continue
            return current
    return None


def _assert_no_symlink_components(path: Path, field: str) -> None:
    symlink = _path_has_symlink_component(path)
    if symlink is not None:
        raise BridgeError(f"{field} contains a symlink component")


def _absolute_path(path: Path, field: str) -> Path:
    path = Path(path)
    if any(part == ".." for part in path.parts):
        raise BridgeError(f"{field} contains traversal")
    absolute = path.absolute()
    _assert_no_symlink_components(absolute, field)
    return absolute


def _under(child: Path, parent: Path) -> bool:
    try:
        child.resolve(strict=False).relative_to(parent.resolve(strict=False))
        return True
    except ValueError:
        return False


def _validate_output_path(output: Path, inputs: Iterable[Optional[Path]]) -> Path:
    output = _absolute_path(Path(output), "output")
    if os.path.lexists(output):
        raise BridgeConflict("output must be a new directory")
    for item in inputs:
        if item is None:
            continue
        item_path = _absolute_path(Path(item), "input")
        if _under(output, item_path) or _under(item_path, output):
            raise BridgeError("output overlaps an input")
    return output


def _make_output(output: Path) -> None:
    _assert_no_symlink_components(output, "output")
    try:
        output.mkdir(parents=True, exist_ok=False)
    except FileExistsError as exc:
        raise BridgeConflict("output must be a new directory") from exc
    except OSError as exc:
        raise BridgeError("cannot create output directory") from exc


def _write_new(path: Path, data: bytes) -> None:
    path = Path(path)
    _assert_no_symlink_components(path.parent, "artifact parent")
    path.parent.mkdir(parents=True, exist_ok=True)
    if os.path.lexists(path):
        raise BridgeConflict(f"artifact already exists: {path.name}")
    path.write_bytes(data)


def _write_json_new(path: Path, value: Any) -> None:
    _write_new(path, pretty_json(value))


def _replace_json(path: Path, value: Any) -> None:
    _assert_no_symlink_components(path.parent, "ledger parent")
    if path.is_symlink() or (os.path.lexists(path) and not path.is_file()):
        raise BridgeConflict(f"unsafe ledger path: {path.name}")
    path.write_bytes(pretty_json(value))


def _all_regular_files(root: Path) -> set[str]:
    root = Path(root)
    if root.is_symlink() or not root.is_dir():
        raise BridgeError("artifact root is not a regular directory")
    files: set[str] = set()
    for current, directories, names in os.walk(root, topdown=True, followlinks=False):
        current_path = Path(current)
        kept: list[str] = []
        for name in directories:
            path = current_path / name
            if path.is_symlink() or not path.is_dir():
                raise BridgeError("artifact tree contains an unsafe directory")
            kept.append(name)
        directories[:] = kept
        for name in names:
            path = current_path / name
            if path.is_symlink() or not path.is_file():
                raise BridgeError("artifact tree contains an unsafe file")
            relative = str(path.relative_to(root)).replace(os.sep, "/")
            _safe_relative_path(relative, "artifact path")
            files.add(relative)
    return files


def _snapshot_at(start_ms: int) -> str:
    return datetime.fromtimestamp(start_ms / 1000, tz=TAIPEI).isoformat(timespec="milliseconds")


def _validate_repository(value: Any) -> str:
    repository = _bounded_text(value, "repository", MAX_REPOSITORY_BYTES)
    pieces = repository.split("/")
    if len(pieces) != 2 or not all(pieces) or any(part in {".", ".."} for part in pieces):
        raise BridgeError("repository must use owner/repo")
    return repository


def _validate_pr(value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise BridgeError("pr must be a positive integer")
    return value


def _validate_session(value: Any) -> str:
    return _bounded_text(value, "session", MAX_SESSION_BYTES)


def _loopback(hostname: str) -> bool:
    normalized = hostname.rstrip(".").lower()
    if normalized == "localhost":
        return True
    try:
        return ipaddress.ip_address(normalized).is_loopback
    except ValueError:
        return False


def _validate_controller_url(value: Any, allow_local_http: bool) -> str:
    raw = _bounded_text(value, "controller-url", 2048)
    try:
        parts = urllib.parse.urlsplit(raw)
        hostname = parts.hostname
        _ = parts.port
    except ValueError as exc:
        raise BridgeError("controller URL is invalid") from exc
    if not parts.scheme or not parts.netloc or hostname is None:
        raise BridgeError("controller URL is invalid")
    if parts.username is not None or parts.password is not None or "@" in parts.netloc:
        raise BridgeError("controller URL userinfo is forbidden")
    if parts.query or parts.fragment:
        raise BridgeError("controller URL query and fragment are forbidden")
    scheme = parts.scheme.lower()
    if scheme == "https":
        return raw.rstrip("/")
    if scheme == "http" and allow_local_http and _loopback(hostname):
        return raw.rstrip("/")
    raise BridgeError("controller URL must be HTTPS; local HTTP requires --allow-local-http")


def _target_and_url(base: str, endpoint: str, params: Sequence[tuple[str, Any]]) -> tuple[str, str]:
    parts = urllib.parse.urlsplit(base)
    prefix = parts.path.rstrip("/")
    path = f"{prefix}{endpoint}" if prefix else endpoint
    query = urllib.parse.urlencode([(key, str(value)) for key, value in params])
    target = f"{path}?{query}" if query else path
    url = urllib.parse.urlunsplit((parts.scheme, parts.netloc, path, query, ""))
    return target, url


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req: Any, fp: Any, code: int, msg: str, headers: Any, newurl: str) -> Any:
        raise _RedirectRejected("HTTP redirect rejected")


_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())


def _request_json(url: str, secret: str) -> tuple[Any, bytes, str, int]:
    parsed = urllib.parse.urlsplit(url)
    target = parsed.path + (f"?{parsed.query}" if parsed.query else "")
    timestamp = str(int(time.time()))
    canonical = f"v1\n{timestamp}\nGET\n{target}".encode("utf-8")
    signature = hmac.new(secret.encode("utf-8"), canonical, hashlib.sha256).hexdigest()
    request = urllib.request.Request(
        url,
        method="GET",
        headers={
            "x-canary-audit-timestamp": timestamp,
            "x-canary-audit-signature-256": f"sha256={signature}",
            "Accept": "application/json",
        },
    )
    response: Any = None
    try:
        response = _OPENER.open(request, timeout=MAX_REQUEST_SECONDS)
        status_value = getattr(response, "status", None)
        status = int(status_value if status_value is not None else response.getcode())
        if 300 <= status < 400:
            raise _RedirectRejected("HTTP redirect rejected")
        if status < 200 or status >= 300:
            raise BridgeError(f"HTTP {status}")
        content_length = response.headers.get("Content-Length")
        if content_length is not None:
            try:
                if int(content_length) > MAX_PAGE_BYTES:
                    raise BridgeError("HTTP response exceeds the bound")
            except ValueError as exc:
                raise BridgeError("HTTP response has an invalid length") from exc
        deadline = time.monotonic() + MAX_REQUEST_SECONDS
        chunks: list[bytes] = []
        total = 0
        while True:
            if time.monotonic() > deadline:
                raise BridgeError("HTTP request timed out")
            chunk = response.read(min(64 * 1024, MAX_PAGE_BYTES + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > MAX_PAGE_BYTES:
                raise BridgeError("HTTP response exceeds the bound")
        raw = b"".join(chunks)
    except _RedirectRejected:
        raise
    except BridgeError:
        raise
    except urllib.error.HTTPError as exc:
        status = int(exc.code)
        try:
            if 300 <= status < 400:
                raise _RedirectRejected("HTTP redirect rejected") from exc
            raise BridgeError(f"HTTP {status}") from exc
        finally:
            exc.close()
    except (urllib.error.URLError, TimeoutError, OSError):
        raise BridgeError("HTTP request failed")
    finally:
        if response is not None:
            try:
                response.close()
            except OSError:
                pass
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_reject_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BridgeError("HTTP response is not valid JSON") from exc
    return value, raw, target, int(timestamp)


def _validate_findings_page(value: Any) -> list[Mapping[str, Any]]:
    if not isinstance(value, Mapping) or not isinstance(value.get("findings"), list):
        raise BridgeError("findings response shape is invalid")
    limit = value.get("limit")
    if isinstance(limit, bool) or not isinstance(limit, int) or limit != FINDINGS_LIMIT:
        raise BridgeError("findings response limit is invalid")
    rows = value["findings"]
    if len(rows) > FINDINGS_LIMIT:
        raise BridgeError("findings response exceeds the bound")
    if any(not isinstance(row, Mapping) for row in rows):
        raise BridgeError("findings response contains a non-object row")
    return list(rows)


def _validate_audit_page(value: Any) -> tuple[list[Mapping[str, Any]], Optional[str], bool]:
    if not isinstance(value, Mapping) or not isinstance(value.get("events"), list):
        raise BridgeError("audit response shape is invalid")
    events = value["events"]
    if any(not isinstance(event, Mapping) for event in events):
        raise BridgeError("audit response contains a non-object event")
    has_cursor = "next_cursor" in value
    cursor = value.get("next_cursor")
    if cursor is not None and (not isinstance(cursor, str) or not cursor):
        raise BridgeError("audit response cursor is invalid")
    return list(events), cursor, has_cursor


def _capture_file_record(relative: str, raw: bytes, kind: str, page: int, target: str, count: int) -> dict[str, Any]:
    return {
        "path": relative,
        "kind": kind,
        "page": page,
        "request_target": target,
        "sha256": sha256_bytes(raw),
        "bytes": len(raw),
        "record_count": count,
    }


def capture(
    controller_url: str,
    repository: str,
    pr: int,
    session: str,
    output: Path,
    *,
    max_pages: int = DEFAULT_MAX_PAGES,
    allow_local_http: bool = False,
) -> dict[str, Any]:
    """Capture the exact bounded findings and audit API responses."""

    base = _validate_controller_url(controller_url, allow_local_http)
    repository = _validate_repository(repository)
    pr = _validate_pr(pr)
    session = _validate_session(session)
    if isinstance(max_pages, bool) or not isinstance(max_pages, int) or max_pages <= 0 or max_pages > 10_000:
        raise BridgeError("max-pages must be a positive bounded integer")
    output = _validate_output_path(Path(output), ())
    secret = os.environ.get("OCP_EVAL_OBSERVER_SECRET")
    if not isinstance(secret, str) or not secret:
        raise BridgeError("OCP_EVAL_OBSERVER_SECRET is required")

    capture_start_ms = int(time.time() * 1000)
    snapshot_at = _snapshot_at(capture_start_ms)
    _make_output(output)
    raw_root = output / "raw"
    raw_root.mkdir()
    files: list[dict[str, Any]] = []

    findings_target, findings_url = _target_and_url(
        base,
        "/api/v1/review/findings",
        (("repo", repository), ("pr", pr), ("limit", FINDINGS_LIMIT)),
    )
    findings_value, findings_raw, observed_target, _ = _request_json(findings_url, secret)
    if observed_target != findings_target:
        raise BridgeError("findings request target changed")
    findings_rows = _validate_findings_page(findings_value)
    findings_relative = "raw/findings-page-0001.json"
    _write_new(output / findings_relative, findings_raw)
    files.append(
        _capture_file_record(
            findings_relative,
            findings_raw,
            "findings",
            1,
            findings_target,
            len(findings_rows),
        )
    )
    findings_coverage = "partial" if len(findings_rows) >= FINDINGS_LIMIT else "complete"

    audit_pages: list[dict[str, Any]] = []
    audit_events = 0
    cursor: Optional[str] = None
    seen_cursors: set[str] = set()
    audit_last_cursor: Optional[str] = None
    audit_terminal = False
    audit_partial_reason: Optional[str] = None
    for page in range(1, max_pages + 1):
        audit_params: list[tuple[str, Any]] = [
            ("session_id", session),
            ("until", capture_start_ms),
            ("limit", AUDIT_LIMIT),
        ]
        if cursor is not None:
            audit_params.append(("cursor", cursor))
        target, url = _target_and_url(base, "/api/v1/audit/events", audit_params)
        value, raw, observed_target, _ = _request_json(url, secret)
        if observed_target != target:
            raise BridgeError("audit request target changed")
        events, next_cursor, _ = _validate_audit_page(value)
        relative = f"raw/audit-page-{page:04d}.json"
        _write_new(output / relative, raw)
        file_record = _capture_file_record(relative, raw, "audit", page, target, len(events))
        file_record.update({"request_cursor": cursor, "next_cursor": next_cursor})
        files.append(file_record)
        audit_pages.append({"page": page, "request_cursor": cursor, "next_cursor": next_cursor})
        audit_events += len(events)
        audit_last_cursor = cursor
        if next_cursor is None:
            # Rust omits an Option::None cursor on every terminal page;
            # an explicit null decodes to the same None value. This applies
            # to non-empty and exactly full pages too.
            audit_terminal = True
            break
        if next_cursor in seen_cursors or next_cursor == cursor:
            raise BridgeError("audit pagination repeated an opaque cursor")
        seen_cursors.add(next_cursor)
        cursor = next_cursor
    else:
        audit_partial_reason = "page_cap"

    if cursor is not None and not audit_terminal and audit_partial_reason is None:
        audit_partial_reason = "page_cap"
    audit_coverage = "complete" if audit_terminal and audit_partial_reason is None else "partial"
    overall_status = "complete" if findings_coverage == "complete" and audit_coverage == "complete" else "partial"
    metadata: dict[str, Any] = {
        "schema_version": CAPTURE_VERSION,
        "status": overall_status,
        "non_atomic": True,
        "scope": {
            "repository": repository,
            "pr": pr,
            "session": session,
        },
        "controller_url": base,
        "capture_start_ms": capture_start_ms,
        "captured_at_ms": int(time.time() * 1000),
        "snapshot_at": snapshot_at,
        "bounds": {
            "max_pages": max_pages,
            "max_page_bytes": MAX_PAGE_BYTES,
            "request_timeout_seconds": MAX_REQUEST_SECONDS,
            "findings_limit": FINDINGS_LIMIT,
            "audit_limit": AUDIT_LIMIT,
        },
        "queries": {
            "findings": {
                "path": "/api/v1/review/findings",
                "repository": repository,
                "pr": pr,
                "limit": FINDINGS_LIMIT,
                "target": findings_target,
            },
            "audit": {
                "path": "/api/v1/audit/events",
                "session": session,
                "until": capture_start_ms,
                "limit": AUDIT_LIMIT,
            },
        },
        "findings": {
            "coverage": findings_coverage,
            "page_count": 1,
            "record_count": len(findings_rows),
            "response_limit_reached": len(findings_rows) >= FINDINGS_LIMIT,
        },
        "audit": {
            "coverage": audit_coverage,
            "page_count": len(audit_pages),
            "record_count": audit_events,
            "first_cursor": audit_pages[0]["request_cursor"] if audit_pages else None,
            "last_cursor": audit_last_cursor,
            "terminal_next_cursor": None if audit_terminal else cursor,
            "final_null_cursor": audit_terminal,
            "partial_reason": audit_partial_reason,
        },
        "files": files,
        "capture_pages": {
            "findings": [{"page": 1, "path": findings_relative}],
            "audit": audit_pages,
        },
        "credential": "observer HMAC not recorded",
    }
    _write_json_new(output / "capture.json", metadata)
    return {**metadata, "output": str(output)}


def _validate_capture_metadata(metadata: Mapping[str, Any]) -> None:
    if metadata.get("schema_version") != CAPTURE_VERSION:
        raise BridgeError("capture schema is invalid")
    if metadata.get("non_atomic") is not True:
        raise BridgeError("capture must declare non-atomic collection")
    scope = metadata.get("scope")
    if not isinstance(scope, Mapping):
        raise BridgeError("capture scope is missing")
    _validate_repository(scope.get("repository"))
    _validate_pr(scope.get("pr"))
    _validate_session(scope.get("session"))
    start_ms = metadata.get("capture_start_ms")
    if isinstance(start_ms, bool) or not isinstance(start_ms, int) or start_ms <= 0:
        raise BridgeError("capture start timestamp is invalid")
    snapshot = _bounded_text(metadata.get("snapshot_at"), "capture.snapshot_at", 128)
    try:
        parsed = datetime.fromisoformat(snapshot)
    except ValueError as exc:
        raise BridgeError("capture snapshot_at is invalid") from exc
    if parsed.tzinfo is None or parsed.utcoffset() is None or parsed.tzname() is None:
        raise BridgeError("capture snapshot_at must include an offset")
    if snapshot != _snapshot_at(start_ms):
        raise BridgeError("capture snapshot_at conflicts with capture start")
    controller_url = metadata.get("controller_url")
    _validate_controller_url(controller_url, allow_local_http=True)
    bounds = metadata.get("bounds")
    if not isinstance(bounds, Mapping):
        raise BridgeError("capture bounds are missing")
    if bounds.get("max_page_bytes") != MAX_PAGE_BYTES or bounds.get("request_timeout_seconds") != MAX_REQUEST_SECONDS or bounds.get("findings_limit") != FINDINGS_LIMIT or bounds.get("audit_limit") != AUDIT_LIMIT:
        raise BridgeError("capture bounds conflict")
    if isinstance(bounds.get("max_pages"), bool) or not isinstance(bounds.get("max_pages"), int) or not 0 < bounds["max_pages"] <= 10_000:
        raise BridgeError("capture max-pages bound is invalid")
    queries = metadata.get("queries")
    if not isinstance(queries, Mapping) or not isinstance(queries.get("findings"), Mapping) or not isinstance(queries.get("audit"), Mapping):
        raise BridgeError("capture query scope is missing")
    findings_query = queries["findings"]
    audit_query = queries["audit"]
    if findings_query.get("path") != "/api/v1/review/findings" or findings_query.get("repository") != scope["repository"] or findings_query.get("pr") != scope["pr"] or findings_query.get("limit") != FINDINGS_LIMIT:
        raise BridgeError("capture findings query scope conflicts")
    if audit_query.get("path") != "/api/v1/audit/events" or audit_query.get("session") != scope["session"] or audit_query.get("until") != start_ms or audit_query.get("limit") != AUDIT_LIMIT:
        raise BridgeError("capture audit query scope conflicts")
    for section in ("findings", "audit"):
        value = metadata.get(section)
        if not isinstance(value, Mapping) or value.get("coverage") not in {"complete", "partial"}:
            raise BridgeError(f"capture {section} coverage is invalid")
        count = value.get("record_count")
        pages = value.get("page_count")
        if isinstance(count, bool) or not isinstance(count, int) or count < 0 or isinstance(pages, bool) or not isinstance(pages, int) or pages < 1:
            raise BridgeError(f"capture {section} bounds are invalid")
    files = metadata.get("files")
    if not isinstance(files, list) or not files:
        raise BridgeError("capture file ledger is missing")


def _validate_capture_file_record(record: Mapping[str, Any], capture_root: Path) -> tuple[bytes, Any]:
    relative = _safe_relative_path(record.get("path"), "capture file path")
    if not relative.startswith("raw/"):
        raise BridgeError("capture files must be below raw/")
    path = capture_root / relative
    _assert_no_symlink_components(path, "capture input")
    if path.is_symlink() or not path.is_file():
        raise BridgeError("capture raw page is missing")
    raw = path.read_bytes()
    if len(raw) != record.get("bytes") or sha256_bytes(raw) != record.get("sha256"):
        raise BridgeError("capture raw page hash mismatch")
    try:
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_reject_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise BridgeError("capture raw page is invalid JSON") from exc
    if record.get("kind") == "findings":
        rows = _validate_findings_page(value)
    elif record.get("kind") == "audit":
        rows, _, _ = _validate_audit_page(value)
    else:
        raise BridgeError("capture raw page kind is invalid")
    if len(rows) != record.get("record_count"):
        raise BridgeError("capture raw page record count mismatch")
    if isinstance(record.get("page"), bool) or not isinstance(record.get("page"), int) or record["page"] <= 0:
        raise BridgeError("capture raw page number is invalid")
    return raw, value


def _read_capture(root: Path) -> dict[str, Any]:
    root = _absolute_path(Path(root), "capture")
    if root.is_symlink() or not root.is_dir():
        raise BridgeError("capture must be a regular directory")
    _assert_no_symlink_components(root, "capture input")
    metadata, metadata_raw = _read_json(root / "capture.json")
    if not isinstance(metadata, Mapping):
        raise BridgeError("capture metadata must be an object")
    _validate_capture_metadata(metadata)
    records = metadata["files"]
    expected: set[str] = set()
    findings_rows: list[dict[str, Any]] = []
    audit_events: list[dict[str, Any]] = []
    base = _validate_controller_url(metadata["controller_url"], allow_local_http=True)
    scope = metadata["scope"]
    expected_findings_target, _ = _target_and_url(
        base,
        "/api/v1/review/findings",
        (("repo", scope["repository"]), ("pr", scope["pr"]), ("limit", FINDINGS_LIMIT)),
    )
    audit_pages: list[Mapping[str, Any]] = []
    for record in records:
        if not isinstance(record, Mapping):
            raise BridgeError("capture file ledger entry is invalid")
        relative = _safe_relative_path(record.get("path"), "capture file path")
        if relative in expected:
            raise BridgeError("duplicate capture file ledger path")
        expected.add(relative)
        _, value = _validate_capture_file_record(record, root)
        if record.get("kind") == "findings":
            if record.get("page") != 1 or record.get("request_target") != expected_findings_target:
                raise BridgeError("capture findings page scope conflicts")
            for index, row in enumerate(value["findings"]):
                findings_rows.append({"page": int(record["page"]), "row_index": index, "row": row})
        else:
            request_cursor = record.get("request_cursor")
            next_cursor = record.get("next_cursor")
            if next_cursor is not None and (not isinstance(next_cursor, str) or not next_cursor):
                raise BridgeError("capture response cursor is invalid")
            if next_cursor != value.get("next_cursor"):
                raise BridgeError("capture response cursor ledger conflicts")
            audit_params: list[tuple[str, Any]] = [
                ("session_id", scope["session"]),
                ("until", metadata["capture_start_ms"]),
                ("limit", AUDIT_LIMIT),
            ]
            if request_cursor is not None:
                if not isinstance(request_cursor, str) or not request_cursor:
                    raise BridgeError("capture request cursor is invalid")
                audit_params.append(("cursor", request_cursor))
            expected_audit_target, _ = _target_and_url(base, "/api/v1/audit/events", audit_params)
            if record.get("request_target") != expected_audit_target:
                raise BridgeError("capture audit page scope conflicts")
            if record.get("page") != len(audit_pages) + 1:
                raise BridgeError("capture audit page order conflicts")
            audit_pages.append(record)
            audit_events.extend(value["events"])
    actual = _all_regular_files(root / "raw")
    actual = {f"raw/{item}" for item in actual}
    if actual != expected:
        raise BridgeError("capture raw file ledger coverage conflicts")
    queries = metadata["queries"]
    if not isinstance(queries["findings"].get("target"), str):
        raise BridgeError("capture findings target is missing")
    if queries["findings"]["target"] != expected_findings_target:
        raise BridgeError("capture findings target conflicts")
    findings_records = [record for record in records if isinstance(record, Mapping) and record.get("kind") == "findings"]
    if len(findings_records) != 1 or metadata["findings"]["page_count"] != 1 or metadata["findings"]["record_count"] != len(findings_rows):
        raise BridgeError("capture findings page ledger conflicts")
    if metadata["audit"]["page_count"] != len(audit_pages) or metadata["audit"]["record_count"] != len(audit_events):
        raise BridgeError("capture audit page ledger conflicts")
    if not audit_pages:
        raise BridgeError("capture audit page ledger is empty")
    expected_first = audit_pages[0].get("request_cursor")
    expected_last = audit_pages[-1].get("request_cursor")
    if metadata["audit"].get("first_cursor") != expected_first or metadata["audit"].get("last_cursor") != expected_last:
        raise BridgeError("capture audit cursor ledger conflicts")
    last_next_cursor = audit_pages[-1].get("next_cursor")
    final_null = metadata["audit"].get("final_null_cursor") is True
    terminal = last_next_cursor is None and final_null
    if metadata["audit"].get("coverage") == "complete" and not terminal:
        raise BridgeError("complete audit capture lacks a terminal cursor")
    if metadata["audit"].get("coverage") == "partial" and terminal and metadata["audit"].get("partial_reason") is None:
        raise BridgeError("partial audit capture lacks a reason")
    expected_status = "complete" if metadata["findings"]["coverage"] == "complete" and metadata["audit"]["coverage"] == "complete" else "partial"
    if metadata.get("status") != expected_status:
        raise BridgeError("capture status conflicts with coverage")
    return {
        "root": root,
        "metadata": dict(metadata),
        "metadata_raw": metadata_raw,
        "metadata_sha256": sha256_bytes(metadata_raw),
        "findings_rows": findings_rows,
        "audit_events": audit_events,
        "files": records,
    }


def _source_line_count(entry: Mapping[str, Any]) -> int:
    if not isinstance(entry.get("utf8"), str):
        return 0
    text = entry["utf8"]
    return max(1, text.count("\n") + (0 if text.endswith("\n") else 1))


def _source_entries(source_packet: Mapping[str, Any]) -> dict[str, Mapping[str, Any]]:
    result: dict[str, Mapping[str, Any]] = {}
    for entry in source_packet.get("files", []):
        if isinstance(entry, Mapping) and isinstance(entry.get("path"), str):
            result[entry["path"]] = entry
    return result


def _row_digest(row: Any) -> str:
    try:
        material = canonical_json(row).encode("utf-8")
    except (TypeError, UnicodeEncodeError) as exc:
        raise BridgeError("finding row cannot be hashed") from exc
    return sha256_bytes(material)


def _scope_reason(row: Any, scope: Mapping[str, Any], revision: str) -> Optional[str]:
    if not isinstance(row, Mapping):
        return "row_not_object"
    if row.get("repo") != scope["repository"]:
        return "scope_mismatch_repository" if row.get("repo") is not None else "missing_repository"
    if row.get("pr_number") != scope["pr"]:
        return "scope_mismatch_pr" if row.get("pr_number") is not None else "missing_pr"
    if row.get("session_id") != scope["session"]:
        return "scope_mismatch_session" if row.get("session_id") is not None else "missing_session"
    head = row.get("head_sha")
    if not isinstance(head, str) or not FULL_SHA.fullmatch(head) or head.lower() != revision.lower():
        return "missing_or_mismatched_head_sha"
    return None


def _finding_validation(row: Mapping[str, Any], source: Mapping[str, Mapping[str, Any]], revision: str) -> list[str]:
    reasons: list[str] = []
    identifier = row.get("id")
    if isinstance(identifier, bool) or not isinstance(identifier, int) or identifier <= 0:
        reasons.append("invalid_numeric_id")
    for field, maximum in (("session_id", MAX_SESSION_BYTES), ("stable_id", 512), ("severity", 128), ("status", 128), ("title", 4096)):
        try:
            _bounded_text(row.get(field), f"finding.{field}", maximum)
        except BridgeError:
            reasons.append(f"invalid_{field}")
    if not isinstance(row.get("repo"), str):
        reasons.append("invalid_repository")
    path = row.get("path")
    try:
        path = _safe_relative_path(path, "finding.path")
    except BridgeError:
        reasons.append("invalid_path")
        path = None
    line = row.get("line")
    if isinstance(line, bool) or not isinstance(line, int) or line < 1:
        reasons.append("invalid_line")
    elif path is not None:
        entry = source.get(path)
        if entry is None:
            reasons.append("source_path_not_in_frozen_packet")
        elif "utf8" not in entry:
            reasons.append("source_file_not_regular_utf8")
        elif line > _source_line_count(entry):
            reasons.append("line_outside_frozen_source")
    head = row.get("head_sha")
    if not isinstance(head, str) or not FULL_SHA.fullmatch(head) or head.lower() != revision.lower():
        reasons.append("invalid_head_sha")
    created_at = row.get("created_at")
    if isinstance(created_at, bool) or not isinstance(created_at, int) or created_at < 0:
        reasons.append("invalid_created_at")
    return reasons


def _finding_identity(row: Any) -> Optional[tuple[str, int]]:
    if not isinstance(row, Mapping):
        return None
    session_id = row.get("session_id")
    identifier = row.get("id")
    if not isinstance(session_id, str) or isinstance(identifier, bool) or not isinstance(identifier, int):
        return None
    return session_id, identifier


def _excluded(row_record: Mapping[str, Any], reason: str, *, identity: Optional[tuple[str, int]] = None) -> dict[str, Any]:
    value: dict[str, Any] = {
        "source_page": row_record.get("page"),
        "source_row_index": row_record.get("row_index"),
        "row": row_record.get("row"),
        "row_sha256": _row_digest(row_record.get("row")),
        "reason": reason,
    }
    if identity is not None:
        value["finding_identity"] = {"session_id": identity[0], "numeric_id": identity[1]}
    return value


def _select_findings(capture_data: Mapping[str, Any], source_packet: Mapping[str, Any], revision: str) -> dict[str, Any]:
    metadata = capture_data["metadata"]
    scope = metadata["scope"]
    source = _source_entries(source_packet)
    records = list(capture_data["findings_rows"])
    groups: dict[tuple[str, int], list[Mapping[str, Any]]] = {}
    for record in records:
        identity = _finding_identity(record.get("row"))
        if identity is not None:
            groups.setdefault(identity, []).append(record)
    conflicting: set[tuple[str, int]] = set()
    for identity, group in groups.items():
        if len({_row_digest(item.get("row")) for item in group}) > 1:
            conflicting.add(identity)

    seen_exact: set[tuple[str, int]] = set()
    accepted: list[dict[str, Any]] = []
    accepted_core: list[dict[str, Any]] = []
    excluded: list[dict[str, Any]] = []
    invalid_selected: list[dict[str, Any]] = []
    selected_scope_count = 0
    duplicate_exact_count = 0
    for record in records:
        row = record.get("row")
        identity = _finding_identity(row)
        reason = _scope_reason(row, scope, revision)
        if reason is not None:
            excluded.append(_excluded(record, reason, identity=identity))
            continue
        selected_scope_count += 1
        if identity in conflicting if identity is not None else False:
            invalid = _excluded(record, "conflicting_duplicate_finding_id", identity=identity)
            excluded.append(invalid)
            invalid_selected.append(invalid)
            continue
        if identity is None:
            invalid = _excluded(record, "missing_numeric_finding_identity")
            excluded.append(invalid)
            invalid_selected.append(invalid)
            continue
        if identity in seen_exact:
            duplicate_exact_count += 1
            excluded.append(_excluded(record, "duplicate_exact_finding_row", identity=identity))
            continue
        reasons = _finding_validation(row, source, revision)
        if reasons:
            invalid = _excluded(record, ";".join(reasons), identity=identity)
            excluded.append(invalid)
            invalid_selected.append(invalid)
            continue
        seen_exact.add(identity)
        finding_id = f"{identity[0]}:{identity[1]}"
        evidence_id = f"E-{sha256_bytes(finding_id.encode('utf-8'))[:24]}"
        path = row["path"]
        line = row["line"]
        normalized = {
            "finding_id": finding_id,
            "title": row["title"],
            "claim": row["title"],
            "claim_provenance": "ledger_title_only",
            "severity": row["severity"],
            "location": {"path": path, "start_line": line, "end_line": line},
            "evidence_ids": [evidence_id],
            "author_model_identity": "unknown",
        }
        accepted.append(
            {
                "finding_id": finding_id,
                "evidence_id": evidence_id,
                "source_page": record["page"],
                "source_row_index": record["row_index"],
                "source_row_sha256": _row_digest(row),
                "row": dict(row),
                "finding": normalized,
            }
        )
        accepted_core.append(normalized)

    source_files = _source_entries(source_packet)
    evidence_entries: list[dict[str, Any]] = []
    for item in accepted:
        path = item["finding"]["location"]["path"]
        entry = source_files[path]
        data = entry.get("utf8")
        if not isinstance(data, str):
            raise BridgeError("accepted finding source is not regular UTF-8")
        evidence_entries.append(
            {
                "id": item["evidence_id"],
                "utf8": data,
                "sha256": entry["sha256"],
                "allowed_ranges": [{"path": path, "start": 1, "end": _source_line_count(entry)}],
            }
        )
    status = "ready"
    block_reasons: list[str] = []
    if selected_scope_count == 0:
        status = "blocked"
        block_reasons.append("no_selected_findings")
    if invalid_selected:
        status = "blocked"
        block_reasons.append("invalid_or_conflicting_selected_findings")
    return {
        "status": status,
        "block_reasons": block_reasons,
        "accepted": accepted,
        "accepted_core": accepted_core,
        "evidence_entries": evidence_entries,
        "excluded": excluded,
        "invalid_selected": invalid_selected,
        "selection": {
            "capture_row_count": len(records),
            "selected_scope_count": selected_scope_count,
            "accepted_count": len(accepted),
            "excluded_count": len(excluded),
            "invalid_selected_count": len(invalid_selected),
            "duplicate_exact_count": duplicate_exact_count,
            "conflicting_identity_count": len(conflicting),
        },
        "finding_ids": [item["finding_id"] for item in accepted],
    }


def _normalise_source_limits(value: Any) -> dict[str, int]:
    if value is None:
        value = {}
    if not isinstance(value, Mapping):
        raise BridgeError("environment.source_limits must be an object")
    result = dict(DEFAULT_SOURCE_LIMITS)
    for key, item in value.items():
        if key not in DEFAULT_SOURCE_LIMITS:
            raise BridgeError("unsupported source limit")
        if isinstance(item, bool) or not isinstance(item, int) or item <= 0:
            raise BridgeError("source limits must be positive integers")
        result[key] = item
    return result


def _build_source_packet(repo: Path, revision: str, base: str) -> tuple[dict[str, Any], str, str]:
    core = _core()
    repo = _absolute_path(repo, "repository")
    if not repo.is_dir() or repo.is_symlink():
        raise BridgeError("repository must be a regular directory")
    try:
        core._validate_repository_git_config(repo)
        core._validate_clean_repo(repo)
        resolved_revision = core._resolve_revision(repo, _validate_full_sha(revision, "revision"))
        resolved_base = core._resolve_revision(repo, _validate_full_sha(base, "base"))
        packet = core.build_source_packet(repo, resolved_revision, resolved_base, dict(DEFAULT_SOURCE_LIMITS))
        core._validate_source_packet(packet)
    except BridgeError:
        raise
    except Exception as exc:
        message = str(exc)
        if message:
            raise BridgeError(f"repository snapshot failed: {message[:256]}") from exc
        raise BridgeError(f"repository snapshot failed: {type(exc).__name__}") from exc
    return packet, resolved_revision, resolved_base


def _load_product(path: Optional[Path], scope: Mapping[str, Any], revision: str) -> tuple[dict[str, list[dict[str, Any]]], Optional[bytes], dict[str, Any]]:
    tables: dict[str, list[dict[str, Any]]] = {name: [] for name in PRODUCT_TABLES}
    if path is None:
        return tables, None, {"supplied": [], "coverage": {}}
    path = _absolute_path(path, "product input")
    value, raw = _read_json(path)
    if not isinstance(value, Mapping):
        raise BridgeError("product input must be an object")
    source = value.get("tables") if isinstance(value.get("tables"), Mapping) else value.get("product") if isinstance(value.get("product"), Mapping) else value
    if not isinstance(source, Mapping):
        raise BridgeError("product input tables are invalid")
    supplied: list[str] = []
    coverage: dict[str, str] = {}
    for table in PRODUCT_TABLES:
        if table not in source:
            continue
        supplied.append(table)
        rows = source[table]
        if not isinstance(rows, list) or any(not isinstance(row, Mapping) for row in rows):
            raise BridgeError(f"product table {table} is invalid")
        for row in rows:
            for field in PRODUCT_SCOPE_FIELDS[table]:
                if field == "repo":
                    if row.get(field) != scope["repository"]:
                        raise BridgeError(f"product table {table} is outside the selected repository")
                elif field == "pr_number":
                    if row.get(field) != scope["pr"]:
                        raise BridgeError(f"product table {table} is outside the selected PR")
                elif field == "session_id":
                    if row.get(field) != scope["session"]:
                        raise BridgeError(f"product table {table} is outside the selected session")
                elif field == "head_sha":
                    if not isinstance(row.get(field), str) or not FULL_SHA.fullmatch(row[field]) or row[field].lower() != revision.lower():
                        raise BridgeError(f"product table {table} is outside the selected revision")
            tables[table].append(dict(row))
        # Product snapshots have no independent completeness proof at this
        # seam. Supplied tables are deliberately partial.
        coverage[table] = "partial"
    return tables, raw, {"supplied": supplied, "coverage": coverage}


def _weekly_finding_row(row: Mapping[str, Any]) -> dict[str, Any]:
    result = dict(row)
    result.setdefault("raised_by", None)
    result.setdefault("angle", None)
    return result


def _product_coverage(findings_coverage: str, product_info: Mapping[str, Any]) -> dict[str, str]:
    coverage = {name: "unknown" for name in PRODUCT_TABLES}
    coverage["review_findings"] = findings_coverage
    for table in product_info.get("supplied", []):
        if table != "review_findings":
            coverage[table] = "partial"
    return coverage


def _overall_coverage(values: Mapping[str, str]) -> str:
    if all(value == "complete" for value in values.values()):
        return "complete"
    if any(value in {"complete", "partial"} for value in values.values()):
        return "partial"
    return "unknown"


def _bundle_values(capture_data: Mapping[str, Any], selection: Mapping[str, Any], product_tables: Mapping[str, Sequence[Mapping[str, Any]]], product_info: Mapping[str, Any]) -> dict[str, bytes]:
    metadata = capture_data["metadata"]
    snapshot_at = metadata["snapshot_at"]
    audit_events = list(capture_data["audit_events"])
    audit_raw = b"".join(canonical_json(event).encode("utf-8") + b"\n" for event in audit_events)
    product = {table: list(product_tables.get(table, [])) for table in PRODUCT_TABLES}
    product["review_findings"] = [_weekly_finding_row(item["row"]) for item in selection["accepted"]]
    product_raw = pretty_json(product)
    findings_coverage = metadata["findings"]["coverage"]
    audit_coverage = metadata["audit"]["coverage"]
    table_coverage = _product_coverage(findings_coverage, product_info)
    manifest = {
        "schema_version": WEEKLY_MANIFEST_VERSION,
        "bundle_id": "bridge-" + sha256_bytes((metadata["metadata_sha256"] if "metadata_sha256" in metadata else canonical_json(metadata)).encode("utf-8"))[:24],
        "snapshot_at": snapshot_at,
        "timezone": "Asia/Taipei",
        "scope": dict(metadata["scope"]),
        "coverage_scope": "selected_session_retained_api_rows",
        "coverage_note": "API retention and capture coverage are session-scoped; this bundle is not a whole-week or global dataset.",
        "coverage": {
            "audit": audit_coverage,
            "product": _overall_coverage(table_coverage),
            "product_tables": table_coverage,
            "human": "unknown",
            "cost": "unknown",
        },
        "sources": {
            source: {
                "coverage": audit_coverage if source == "audit" else "unknown" if source in {"human", "cost"} else table_coverage[source],
                "start": snapshot_at,
                "end": snapshot_at,
            }
            for source in ("audit", *PRODUCT_TABLES, "human", "cost")
        },
        "audit_cursor": {
            "first_cursor": metadata["audit"].get("first_cursor"),
            "last_cursor": metadata["audit"].get("last_cursor"),
            "page_count": metadata["audit"]["page_count"],
            "final_null_cursor": metadata["audit"].get("final_null_cursor") is True and audit_coverage == "complete",
        },
        "files": {
            "audit.ndjson": {"sha256": sha256_bytes(audit_raw), "record_count": len(audit_events)},
            "product.json": {"sha256": sha256_bytes(product_raw)},
        },
    }
    manifest_raw = pretty_json(manifest)
    return {
        "weekly-bundle/audit.ndjson": audit_raw,
        "weekly-bundle/product.json": product_raw,
        "weekly-bundle/evidence-manifest.json": manifest_raw,
    }


def _core_input_values(selection: Mapping[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    findings = {"schema_version": FINDINGS_VERSION, "findings": list(selection["accepted_core"])}
    evidence = {"schema_version": EVIDENCE_VERSION, "entries": list(selection["evidence_entries"])}
    return findings, evidence


def _validate_core_input_values(findings: Mapping[str, Any], evidence: Mapping[str, Any]) -> None:
    core = _core()
    with tempfile.TemporaryDirectory(prefix="review-bridge-input-") as temp:
        root = Path(temp)
        findings_path = root / "findings.json"
        evidence_dir = root / "evidence"
        evidence_dir.mkdir()
        (evidence_dir / "manifest.json").write_bytes(pretty_json(evidence))
        findings_path.write_bytes(pretty_json(findings))
        try:
            core.load_findings(findings_path)
            core.load_evidence(evidence_dir)
        except Exception as exc:
            raise BridgeError(f"prepared evaluator input rejected: {str(exc)[:256]}") from exc


def _prepare_artifacts(
    capture_root: Path,
    repo: Path,
    revision: str,
    base: str,
    output: Path,
    product_path: Optional[Path],
) -> dict[str, Any]:
    capture_data = _read_capture(capture_root)
    packet, resolved_revision, resolved_base = _build_source_packet(repo, revision, base)
    scope = capture_data["metadata"]["scope"]
    selection = _select_findings(capture_data, packet, resolved_revision)
    findings_value, evidence_value = _core_input_values(selection)
    _validate_core_input_values(findings_value, evidence_value)
    product_tables, product_raw, product_info = _load_product(product_path, scope, resolved_revision)
    try:
        weekly = _weekly()
        weekly._validate_product_tables(product_tables)
        weekly._validate_product_units(product_tables)
    except Exception as exc:
        raise BridgeError(f"product snapshot rejected: {str(exc)[:256]}") from exc
    bundle_values = _bundle_values(capture_data, selection, product_tables, product_info)

    # Validate the actual weekly consumer against the exact bytes that will be
    # written, before making the requested output directory visible.
    with tempfile.TemporaryDirectory(prefix="review-bridge-weekly-") as temp:
        bundle = Path(temp) / "weekly-bundle"
        bundle.mkdir()
        for relative, data in bundle_values.items():
            path = Path(temp) / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        try:
            weekly = _weekly()
            cutoff = weekly._parse_datetime(capture_data["metadata"]["snapshot_at"], "snapshot_at")
            week = weekly._iso_week(cutoff)
            weekly.build_report(bundle, week, capture_data["metadata"]["snapshot_at"])
        except Exception as exc:
            raise BridgeError(f"weekly bundle rejected: {str(exc)[:256]}") from exc

    artifact_values: dict[str, bytes] = {
        "source-packet.json": pretty_json(packet),
        "findings.json": pretty_json(findings_value),
        "evidence/manifest.json": pretty_json(evidence_value),
        **bundle_values,
        "capture/capture.json": capture_data["metadata_raw"],
    }
    for record in capture_data["files"]:
        relative = record["path"]
        artifact_values[f"capture/{relative}"] = (capture_data["root"] / relative).read_bytes()
    if product_raw is not None:
        artifact_values["product-input.json"] = product_raw

    source_omissions = list(packet.get("omissions", []))
    metadata_without_files: dict[str, Any] = {
        "schema_version": PREPARATION_VERSION,
        "status": selection["status"],
        "block_reasons": selection["block_reasons"],
        "scope": {
            "repository": scope["repository"],
            "pr": scope["pr"],
            "session": scope["session"],
            "head_sha": resolved_revision,
            "base_sha": resolved_base,
        },
        "snapshot_at": capture_data["metadata"]["snapshot_at"],
        "source_limits": dict(DEFAULT_SOURCE_LIMITS),
        "source": {
            "revision": resolved_revision,
            "base": resolved_base,
            "packet_sha256": packet["packet_sha256"],
            "complete": bool(packet.get("complete")),
            "omissions": source_omissions,
        },
        "capture_provenance": {
            "metadata_sha256": capture_data["metadata_sha256"],
            "capture_status": capture_data["metadata"]["status"],
            "findings_coverage": capture_data["metadata"]["findings"]["coverage"],
            "audit_coverage": capture_data["metadata"]["audit"]["coverage"],
            "raw_pages": [dict(record) for record in capture_data["files"]],
        },
        "selection": selection["selection"],
        "selected": [
            {
                "finding_id": item["finding_id"],
                "evidence_id": item["evidence_id"],
                "source_page": item["source_page"],
                "source_row_index": item["source_row_index"],
                "source_row_sha256": item["source_row_sha256"],
                "claim_provenance": "ledger_title_only",
            }
            for item in selection["accepted"]
        ],
        "excluded": selection["excluded"],
        "invalid_selected": selection["invalid_selected"],
        "product": {
            "supplied_tables": list(product_info["supplied"]),
            "table_coverage": _product_coverage(capture_data["metadata"]["findings"]["coverage"], product_info),
            "coverage_note": "supplied product tables are partial; no completeness proof is inferred",
        },
        "claims": {"provenance": "ledger_title_only", "status_is_not_accuracy": True},
    }
    metadata_without_files["files"] = {
        relative: {"sha256": sha256_bytes(data), "bytes": len(data)}
        for relative, data in sorted(artifact_values.items())
    }
    preparation_raw = pretty_json(metadata_without_files)
    artifact_values["preparation.json"] = preparation_raw

    # The final directory is created only after remote hashes, Git, core
    # validators, and weekly format acceptance have all passed.
    _make_output(output)
    for relative, data in sorted(artifact_values.items()):
        _write_new(output / relative, data)
    return {**metadata_without_files, "output": str(output)}


def prepare(
    capture: Path,
    repo: Path,
    revision: str,
    base: str,
    output: Path,
    product: Optional[Path] = None,
) -> dict[str, Any]:
    """Bind a capture to one clean Git revision and generate immutable inputs."""

    capture = _absolute_path(capture, "capture")
    repo = _absolute_path(repo, "repository")
    output = _validate_output_path(output, (capture, repo, product))
    _validate_full_sha(revision, "revision")
    _validate_full_sha(base, "base")
    if product is not None:
        _absolute_path(product, "product input")
    try:
        return _prepare_artifacts(capture, repo, revision, base, output, product)
    except BridgeError:
        raise
    except Exception as exc:
        raise BridgeError(f"preparation failed: {str(exc)[:256]}") from exc


def _verify_artifact_ledger(root: Path, preparation: Mapping[str, Any]) -> None:
    declared = preparation.get("files")
    if not isinstance(declared, Mapping):
        raise BridgeError("prepared artifact ledger is missing")
    expected = set(declared)
    actual = _all_regular_files(root)
    actual.discard("preparation.json")
    if actual != expected:
        raise BridgeError("prepared artifact ledger coverage conflicts")
    for relative, record in declared.items():
        _safe_relative_path(relative, "prepared artifact path")
        if not isinstance(record, Mapping):
            raise BridgeError("prepared artifact ledger entry is invalid")
        path = root / relative
        raw = path.read_bytes()
        if record.get("bytes") != len(raw) or record.get("sha256") != sha256_bytes(raw):
            raise BridgeError(f"prepared artifact hash mismatch: {relative}")


def _load_prepared(root: Path, repo: Path) -> dict[str, Any]:
    root = _absolute_path(root, "prepared")
    if root.is_symlink() or not root.is_dir():
        raise BridgeError("prepared must be a regular directory")
    _assert_no_symlink_components(root, "prepared input")
    preparation, preparation_raw = _read_json(root / "preparation.json")
    if not isinstance(preparation, Mapping) or preparation.get("schema_version") != PREPARATION_VERSION:
        raise BridgeError("prepared preparation schema is invalid")
    if preparation.get("status") != "ready":
        raise BridgeError("prepared input is blocked and cannot be evaluated")
    _verify_artifact_ledger(root, preparation)
    scope = preparation.get("scope")
    source_info = preparation.get("source")
    if not isinstance(scope, Mapping) or not isinstance(source_info, Mapping):
        raise BridgeError("prepared scope is missing")
    repository = _validate_repository(scope.get("repository"))
    pr = _validate_pr(scope.get("pr"))
    session = _validate_session(scope.get("session"))
    revision = _validate_full_sha(scope.get("head_sha"), "prepared head_sha")
    base = _validate_full_sha(scope.get("base_sha"), "prepared base_sha")
    snapshot_at = _bounded_text(preparation.get("snapshot_at"), "prepared snapshot_at", 128)
    packet, resolved_revision, resolved_base = _build_source_packet(repo, revision, base)
    if resolved_revision != revision or resolved_base != base:
        raise BridgeError("repository revision identity conflicts with prepared scope")
    source_packet, source_packet_raw = _read_json(root / "source-packet.json")
    if not isinstance(source_packet, Mapping):
        raise BridgeError("prepared source packet is invalid")
    try:
        _core()._validate_source_packet(source_packet)
    except Exception as exc:
        raise BridgeError("prepared source packet failed core validation") from exc
    if source_packet != packet or source_info.get("packet_sha256") != packet.get("packet_sha256"):
        raise BridgeError("prepared source packet does not match the frozen repository revision")
    findings_value, findings_raw = _read_json(root / "findings.json")
    evidence_value, evidence_raw = _read_json(root / "evidence" / "manifest.json")
    try:
        findings, _ = _core().load_findings(root / "findings.json")
        evidence, _ = _core().load_evidence(root / "evidence")
    except Exception as exc:
        raise BridgeError("prepared evaluator input failed core validation") from exc
    capture_data = _read_capture(root / "capture")
    if capture_data["metadata_sha256"] != preparation.get("capture_provenance", {}).get("metadata_sha256"):
        raise BridgeError("prepared capture provenance hash conflicts")
    if capture_data["metadata"]["scope"] != {"repository": repository, "pr": pr, "session": session}:
        raise BridgeError("prepared capture scope conflicts")
    product_input_path = root / "product-input.json"
    product_input = product_input_path if product_input_path.is_file() else None
    product_tables, product_input_raw, product_info = _load_product(product_input, capture_data["metadata"]["scope"], revision)
    if preparation.get("source_limits") != DEFAULT_SOURCE_LIMITS:
        raise BridgeError("prepared source limits conflict")
    selection = _select_findings(capture_data, source_packet, revision)
    expected_findings, expected_evidence = _core_input_values(selection)
    if findings_value != expected_findings or evidence_value != expected_evidence:
        raise BridgeError("prepared evaluator inputs do not derive from the captured rows")
    expected_selected = [
        {
            "finding_id": item["finding_id"],
            "evidence_id": item["evidence_id"],
            "source_page": item["source_page"],
            "source_row_index": item["source_row_index"],
            "source_row_sha256": item["source_row_sha256"],
            "claim_provenance": "ledger_title_only",
        }
        for item in selection["accepted"]
    ]
    provenance = preparation.get("capture_provenance")
    expected_provenance = {
        "metadata_sha256": capture_data["metadata_sha256"],
        "capture_status": capture_data["metadata"]["status"],
        "findings_coverage": capture_data["metadata"]["findings"]["coverage"],
        "audit_coverage": capture_data["metadata"]["audit"]["coverage"],
        "raw_pages": [dict(record) for record in capture_data["files"]],
    }
    expected_product = {
        "supplied_tables": list(product_info["supplied"]),
        "table_coverage": _product_coverage(capture_data["metadata"]["findings"]["coverage"], product_info),
        "coverage_note": "supplied product tables are partial; no completeness proof is inferred",
    }
    if preparation.get("snapshot_at") != capture_data["metadata"].get("snapshot_at") or preparation.get("block_reasons") != selection["block_reasons"] or preparation.get("selection") != selection["selection"] or preparation.get("selected") != expected_selected or preparation.get("excluded") != selection["excluded"] or preparation.get("invalid_selected") != selection["invalid_selected"] or preparation.get("source") != {
        "revision": revision,
        "base": base,
        "packet_sha256": packet["packet_sha256"],
        "complete": bool(packet.get("complete")),
        "omissions": list(packet.get("omissions", [])),
    } or provenance != expected_provenance or preparation.get("product") != expected_product or preparation.get("claims") != {"provenance": "ledger_title_only", "status_is_not_accuracy": True}:
        raise BridgeError("prepared provenance metadata conflicts")
    if not isinstance(findings, list) or not isinstance(evidence, Mapping):
        raise BridgeError("prepared evaluator inputs are malformed")
    weekly_bundle = root / "weekly-bundle"
    product_value, product_raw = _read_json(weekly_bundle / "product.json")
    weekly_manifest, weekly_manifest_raw = _read_json(weekly_bundle / "evidence-manifest.json")
    if not isinstance(product_value, Mapping) or not isinstance(weekly_manifest, Mapping):
        raise BridgeError("prepared weekly bundle is malformed")
    expected_bundle = _bundle_values(capture_data, selection, product_tables, product_info)
    if product_raw != expected_bundle["weekly-bundle/product.json"]:
        raise BridgeError("prepared weekly product snapshot conflicts")
    if (weekly_bundle / "audit.ndjson").read_bytes() != expected_bundle["weekly-bundle/audit.ndjson"] or weekly_manifest_raw != expected_bundle["weekly-bundle/evidence-manifest.json"]:
        raise BridgeError("prepared weekly audit bundle conflicts")
    return {
        "root": root,
        "preparation": dict(preparation),
        "preparation_raw": preparation_raw,
        "source_packet": dict(source_packet),
        "source_packet_raw": source_packet_raw,
        "findings_value": findings_value,
        "findings_raw": findings_raw,
        "evidence_value": evidence_value,
        "evidence_raw": evidence_raw,
        "capture": capture_data,
        "repository": repository,
        "pr": pr,
        "session": session,
        "revision": revision,
        "base": base,
        "snapshot_at": snapshot_at,
    }


def _run_environment(path: Optional[Path]) -> tuple[Optional[Path], Optional[bytes], dict[str, Any]]:
    if path is None:
        return None, None, {"source_limits": dict(DEFAULT_SOURCE_LIMITS)}
    path = _absolute_path(path, "environment input")
    value, raw = _read_json(path)
    if not isinstance(value, Mapping):
        raise BridgeError("environment input must be an object")
    limits = _normalise_source_limits(value.get("source_limits", {}))
    if limits != DEFAULT_SOURCE_LIMITS:
        raise BridgeError("environment source limits differ from prepared source limits")
    return path, raw, {"source_limits": limits}


def _validate_models(path: Path) -> bytes:
    path = _absolute_path(path, "models input")
    _, raw = _read_json(path)
    try:
        _core().load_models(path)
    except Exception as exc:
        raise BridgeError(f"models input rejected: {str(exc)[:256]}") from exc
    return raw


def _run_status_payload(result: Mapping[str, Any]) -> dict[str, Any]:
    return {
        "status": result.get("status"),
        "state": result.get("state"),
        "quality": result.get("quality"),
        "output": result.get("output"),
    }


def run(
    prepared: Path,
    repo: Path,
    models: Path,
    output: Path,
    environment: Optional[Path] = None,
    *,
    evaluator: Optional[Callable[..., Mapping[str, Any]]] = None,
) -> dict[str, Any]:
    """Verify prepared input, run the existing evaluator, and render weekly output."""

    prepared = _absolute_path(prepared, "prepared")
    repo = _absolute_path(repo, "repository")
    models = _absolute_path(models, "models input")
    environment_abs = _absolute_path(environment, "environment input") if environment is not None else None
    output = _validate_output_path(output, (prepared, repo, models, environment_abs))
    prepared_data = _load_prepared(prepared, repo)
    models_raw = _validate_models(models)
    environment_path, environment_raw, _ = _run_environment(environment_abs)
    output = Path(output)
    _make_output(output)
    snapshot_at = prepared_data["snapshot_at"]
    weekly = _weekly()
    cutoff = weekly._parse_datetime(snapshot_at, "snapshot_at")
    week = weekly._iso_week(cutoff)
    ledger: dict[str, Any] = {
        "schema_version": BRIDGE_VERSION,
        "status": "running",
        "state": "running",
        "quality": "not_scoreable",
        "scope": {
            "repository": prepared_data["repository"],
            "pr": prepared_data["pr"],
            "session": prepared_data["session"],
            "revision": prepared_data["revision"],
            "base": prepared_data["base"],
        },
        "snapshot_at": snapshot_at,
        "week": week,
        "prepared_artifacts_sha256": sha256_bytes(canonical_json(prepared_data["preparation"].get("files", {})).encode("utf-8")),
        "models_sha256": sha256_bytes(models_raw),
        "environment_sha256": sha256_bytes(environment_raw) if environment_raw is not None else sha256_bytes(b"{}"),
        "evaluation": {"status": "not_started", "verified": False},
        "weekly_report": {"status": "not_started"},
    }
    _write_json_new(output / "run.json", ledger)
    evaluation_root = output / "evaluation"
    evaluation_result: Optional[Mapping[str, Any]] = None
    evaluation_error: Optional[str] = None
    evaluator = run_evaluation if evaluator is None else evaluator
    try:
        evaluation_result = evaluator(
            repo=repo,
            revision=prepared_data["revision"],
            base=prepared_data["base"],
            findings_path=prepared / "findings.json",
            evidence_dir=prepared / "evidence",
            models_path=models,
            environment_path=environment_path,
            output=evaluation_root,
        )
    except Exception as exc:
        evaluation_error = type(exc).__name__

    verified_evaluation: Optional[Mapping[str, Any]] = None
    if evaluation_root.is_dir() and not evaluation_root.is_symlink():
        try:
            verified_evaluation = _core().verify_evaluation_artifacts(evaluation_root)
            if not isinstance(verified_evaluation, Mapping):
                raise BridgeError("evaluator verification returned a non-object")
            snapshot, _ = _read_json(evaluation_root / "snapshot.json")
            if not isinstance(snapshot, Mapping) or snapshot.get("revision") != prepared_data["revision"] or snapshot.get("base") != prepared_data["base"] or snapshot.get("findings_sha256") != sha256_bytes(prepared_data["findings_raw"]) or snapshot.get("source_packet_sha256") != prepared_data["source_packet"].get("packet_sha256"):
                raise BridgeError("verified evaluation scope conflicts with prepared input")
        except Exception as exc:
            evaluation_error = evaluation_error or type(exc).__name__
            verified_evaluation = None
    if verified_evaluation is not None:
        evaluation_status = verified_evaluation.get("state")
        evaluation_verified = True
    elif evaluation_error is not None:
        evaluation_status = "failed"
        evaluation_verified = False
    else:
        evaluation_status = "failed"
        evaluation_error = "missing_evaluation_artifacts"
        evaluation_verified = False
    ledger["evaluation"] = {
        "status": evaluation_status,
        "verified": evaluation_verified,
        "error": evaluation_error,
    }
    report: Optional[Mapping[str, Any]] = None
    report_error: Optional[str] = None
    report_dir = output / "weekly-report"
    if verified_evaluation is None:
        ledger["weekly_report"] = {
            "status": "blocked",
            "reason": "evaluation_not_verified",
        }
    else:
        try:
            report = weekly.build_report(
                prepared / "weekly-bundle",
                week,
                snapshot_at,
                evaluation_root,
            )
            markdown_path, json_path = weekly.write_report(
                report,
                report_dir,
                prepared / "weekly-bundle",
                evaluation_root,
            )
            ledger["weekly_report"] = {
                "status": "complete",
                "markdown": str(markdown_path),
                "json": str(json_path),
                "report_id": report.get("report_id"),
            }
        except Exception as exc:
            report_error = type(exc).__name__
            ledger["weekly_report"] = {"status": "failed", "error": report_error}

    if report_error is not None:
        state = "failed"
    elif evaluation_status == "complete" and verified_evaluation is not None:
        state = "complete"
    elif evaluation_status in {"partial", "failed"}:
        state = "partial" if report is not None and evaluation_status == "partial" else "failed"
    else:
        state = "failed"
    quality = "not_scoreable"
    if isinstance(report, Mapping):
        evaluation_report = report.get("evaluation")
        if isinstance(evaluation_report, Mapping) and evaluation_report.get("quality", {}).get("status") == "available" and state == "complete":
            quality = "available"
    ledger.update({"status": state, "state": state, "quality": quality})
    _replace_json(output / "run.json", ledger)
    return {
        "status": state,
        "state": state,
        "quality": quality,
        "output": str(output),
        "weekly_report": dict(ledger["weekly_report"]),
        "evaluation": dict(ledger["evaluation"]),
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Capture, prepare, and run the read-only review evaluation bridge")
    subparsers = parser.add_subparsers(dest="command", required=True)

    capture_parser = subparsers.add_parser("capture")
    capture_parser.add_argument("--controller-url", required=True)
    capture_parser.add_argument("--repository", required=True)
    capture_parser.add_argument("--pr", required=True, type=int)
    capture_parser.add_argument("--session", required=True)
    capture_parser.add_argument("--output", required=True, type=Path)
    capture_parser.add_argument("--max-pages", type=int, default=DEFAULT_MAX_PAGES)
    capture_parser.add_argument("--allow-local-http", action="store_true")

    prepare_parser = subparsers.add_parser("prepare")
    prepare_parser.add_argument("--capture", required=True, type=Path)
    prepare_parser.add_argument("--repo", required=True, type=Path)
    prepare_parser.add_argument("--revision", required=True)
    prepare_parser.add_argument("--base", required=True)
    prepare_parser.add_argument("--output", required=True, type=Path)
    prepare_parser.add_argument("--product", type=Path)

    run_parser = subparsers.add_parser("run")
    run_parser.add_argument("--prepared", required=True, type=Path)
    run_parser.add_argument("--repo", required=True, type=Path)
    run_parser.add_argument("--models", required=True, type=Path)
    run_parser.add_argument("--environment", type=Path)
    run_parser.add_argument("--output", required=True, type=Path)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "capture":
            result = capture(
                args.controller_url,
                args.repository,
                args.pr,
                args.session,
                args.output,
                max_pages=args.max_pages,
                allow_local_http=args.allow_local_http,
            )
            print(json.dumps({"status": result["status"], "output": result["output"]}, sort_keys=True))
            return 0
        if args.command == "prepare":
            result = prepare(args.capture, args.repo, args.revision, args.base, args.output, args.product)
            print(json.dumps({"status": result["status"], "output": result["output"]}, sort_keys=True))
            return 0 if result["status"] == "ready" else 2
        result = run(args.prepared, args.repo, args.models, args.output, args.environment)
        print(json.dumps(_run_status_payload(result), sort_keys=True))
        return 0 if result["state"] in {"complete", "partial"} else 1
    except BridgeError as exc:
        print(f"bridge failed: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
