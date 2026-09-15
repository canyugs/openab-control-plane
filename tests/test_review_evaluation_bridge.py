import hashlib
import hmac
import importlib.util
import json
import os
import subprocess
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock
from urllib.parse import parse_qs, urlsplit


ROOT = Path(__file__).resolve().parents[1]
SECRET = "bridge-test-secret"


def rust_audit_event(seq):
    return {
        "seq": seq,
        "version": 1,
        "event_id": f"event-{seq}",
        "event_key": f"event-key-{seq}",
        "occurred_at": seq,
        "recorded_at": seq,
        "service": "test-service",
        "kind": "test.event",
        "outcome": "succeeded",
        "correlation": {},
        "detail": {},
    }


def load_module():
    path = ROOT / "scripts/review_evaluation_bridge.py"
    spec = importlib.util.spec_from_file_location("review_evaluation_bridge", path)
    if spec is None or spec.loader is None:
        raise AssertionError("the evaluation bridge module is missing")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CaptureHandler(BaseHTTPRequestHandler):
    requests = []
    pages = {}

    def do_GET(self):  # noqa: N802 - stdlib handler API
        target = self.path
        timestamp = self.headers.get("x-canary-audit-timestamp")
        signature = self.headers.get("x-canary-audit-signature-256")
        expected = hmac.new(
            SECRET.encode(),
            f"v1\n{timestamp}\nGET\n{target}".encode(),
            hashlib.sha256,
        ).hexdigest()
        self.__class__.requests.append(
            {"target": target, "timestamp": timestamp, "signature": signature}
        )
        if signature != f"sha256={expected}":
            self.send_response(403)
            self.end_headers()
            return
        query = parse_qs(urlsplit(target).query)
        if urlsplit(target).path.endswith("/findings"):
            body = json.dumps({"findings": [], "limit": 5000}).encode()
        elif query.get("cursor") == ["opaque-next"]:
            body = json.dumps({"events": []}).encode()
        else:
            body = json.dumps({"events": [], "next_cursor": "opaque-next"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        return


class FakeResponse:
    def __init__(self, body):
        self.fp = None
        self.status = 200
        self.headers = {"Content-Length": str(len(body))}
        self._body = body
        self._read = False

    def getcode(self):
        return self.status

    def read1(self, _size=-1):
        if self._read:
            return b""
        self._read = True
        return self._body

    def close(self):
        return


class FakeOpener:
    def __init__(self, secret, pages):
        self.secret = secret
        self.pages = pages
        self.requests = []

    def open(self, request, timeout):
        target = urlsplit(request.full_url).path
        if urlsplit(request.full_url).query:
            target += "?" + urlsplit(request.full_url).query
        timestamp = request.get_header("X-canary-audit-timestamp")
        signature = request.get_header("X-canary-audit-signature-256")
        expected = hmac.new(
            self.secret.encode(),
            f"v1\n{timestamp}\nGET\n{target}".encode(),
            hashlib.sha256,
        ).hexdigest()
        self.requests.append({"target": target, "timeout": timeout})
        if signature != f"sha256={expected}":
            raise AssertionError("bridge signed a different target")
        return FakeResponse(self.pages[target])


class CaptureFixtureOpener(FakeOpener):
    def __init__(self, secret, findings, *, audit_events=None, next_cursor=None, explicit_null_cursor=False):
        super().__init__(secret, {})
        self.findings = findings
        self.audit_events = [] if audit_events is None else audit_events
        self.next_cursor = next_cursor
        self.explicit_null_cursor = explicit_null_cursor

    def open(self, request, timeout):
        parsed = urlsplit(request.full_url)
        target = parsed.path + (("?" + parsed.query) if parsed.query else "")
        if parsed.path.endswith("/findings"):
            body = json.dumps({"findings": self.findings, "limit": 5000}).encode()
        elif parsed.query.endswith("cursor=opaque-next"):
            body = json.dumps({"events": self.audit_events}).encode()
        elif self.next_cursor is not None:
            body = json.dumps({"events": self.audit_events, "next_cursor": self.next_cursor}).encode()
        elif self.explicit_null_cursor:
            body = json.dumps({"events": self.audit_events, "next_cursor": None}).encode()
        else:
            body = json.dumps({"events": self.audit_events}).encode()
        self.pages[target] = body
        return super().open(request, timeout)


class ErrorOpener:
    def __init__(self, error):
        self.error = error

    def open(self, _request, timeout):
        raise self.error


def init_git_repo(root):
    repo = root / "repo"
    repo.mkdir()
    env = {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull, "LC_ALL": "C"}

    def git(*args):
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
        ).stdout.strip()

    git("init", "-q")
    git("config", "user.email", "tests@example.invalid")
    git("config", "user.name", "bridge tests")
    source = repo / "src" / "app.py"
    source.parent.mkdir()
    source.write_text("def check(value):\n    return value\n", encoding="utf-8")
    git("add", "src/app.py")
    git("commit", "-qm", "base")
    base = git("rev-parse", "HEAD")
    source.write_text("def check(value):\n    return value == 'safe'\n", encoding="utf-8")
    git("add", "src/app.py")
    git("commit", "-qm", "revision")
    revision = git("rev-parse", "HEAD")
    (repo / "current-only.txt").write_text("not part of requested revision\n", encoding="utf-8")
    git("add", "current-only.txt")
    git("commit", "-qm", "current checkout")
    return repo, base, revision


def capture_fixture(module, root, findings, *, session="session-1", repository="owner/repo", pr=17):
    capture_dir = root / "capture"
    opener = CaptureFixtureOpener(SECRET, findings)
    with mock.patch.object(module, "_OPENER", opener), mock.patch.dict(
        module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
    ):
        module.capture(
            "http://127.0.0.1:8090",
            repository,
            pr,
            session,
            capture_dir,
            allow_local_http=True,
        )
    return capture_dir


def models_fixture(path):
    roles = {}
    for role, model_id in (
        ("judge_a", "model-a"),
        ("judge_b", "model-b"),
        ("synthesis", "model-synthesis"),
        ("discovery", "model-discovery"),
        ("validation", "model-validation"),
    ):
        roles[role] = {
            "adapter": "claude",
            "model_id": model_id,
            "family": "test-family",
            "strength": "strong",
            "transport": "bare",
        }
    path.write_text(json.dumps({"roles": roles}), encoding="utf-8")


class ReviewEvaluationBridgeTests(unittest.TestCase):
    def test_many_findings_share_large_file_evidence(self):
        module = load_module()
        content = "x" * (2 * 1024 * 1024 - 1) + "\n"
        revision = "a" * 40
        scope = {"repository": "owner/repo", "pr": 17, "session": "session-1"}
        row = {"id": 1, "session_id": "session-1", "repo": "owner/repo",
               "pr_number": 17, "stable_id": "stable", "severity": "red",
               "status": "open", "title": "claim", "path": "large.txt",
               "line": 1, "head_sha": revision, "created_at": 1}
        packet = {"files": [{"path": "large.txt", "utf8": content,
                             "sha256": hashlib.sha256(content.encode()).hexdigest()}]}
        capture = {"metadata": {"scope": scope}, "findings_rows": [
            {"page": 1, "row_index": i, "row": {**row, "id": i + 1}}
            for i in range(32)]}
        selection = module._select_findings(capture, packet, revision)
        findings, evidence = module._core_input_values(selection)
        module._validate_core_input_values(findings, evidence)
        self.assertEqual(len(selection["accepted"]), 32)
        self.assertEqual(len(selection["evidence_entries"]), 1)
        self.assertEqual(len({item["evidence_id"] for item in selection["accepted"]}), 1)
        # Identical bytes at another path must retain their own allowed range.
        packet["files"].append({**packet["files"][0], "path": "other.txt"})
        capture["findings_rows"].append(
            {"page": 1, "row_index": 32, "row": {**row, "id": 33, "path": "other.txt"}})
        selection = module._select_findings(capture, packet, revision)
        self.assertEqual(len(selection["evidence_entries"]), 2)

    def test_http_deadline_interrupts_trickling_and_stalled_bodies(self):
        module = load_module()
        for mode in ("trickle", "stall"):
            with self.subTest(mode=mode):
                stopped = threading.Event()

                class Handler(BaseHTTPRequestHandler):
                    def log_message(self, *_args):
                        pass

                    def do_GET(self):
                        self.send_response(200)
                        self.send_header("Content-Length", "1000")
                        self.end_headers()
                        try:
                            if mode == "stall":
                                stopped.wait(2)
                            else:
                                until = time.monotonic() + 1.2
                                while not stopped.is_set() and time.monotonic() < until:
                                    self.wfile.write(b" ")
                                    self.wfile.flush()
                                    stopped.wait(0.03)
                        except (BrokenPipeError, ConnectionResetError):
                            pass

                server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                worker = threading.Thread(target=server.serve_forever, daemon=True)
                worker.start()
                try:
                    with mock.patch.object(module, "MAX_REQUEST_SECONDS", 0.2):
                        start = time.monotonic()
                        with self.assertRaises(module.BridgeError):
                            module._request_json(
                                f"http://127.0.0.1:{server.server_port}/", SECRET)
                        elapsed = time.monotonic() - start
                    self.assertLess(elapsed, 0.8)
                finally:
                    stopped.set()
                    server.shutdown()
                    server.server_close()
                    worker.join()

    def test_capture_signs_exact_queries_and_follows_opaque_cursor(self):
        module = load_module()
        CaptureHandler.requests = []
        CaptureHandler.pages = {}
        try:
            server = ThreadingHTTPServer(("127.0.0.1", 0), CaptureHandler)
        except PermissionError:
            self.skipTest("the test runner forbids local listener sockets")
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as temp:
                output = Path(temp) / "capture"
                with mock.patch.dict(module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False):
                    result = module.capture(
                        f"http://127.0.0.1:{server.server_port}",
                        "owner/repo",
                        17,
                        "session-1",
                        output,
                        allow_local_http=True,
                    )
                self.assertEqual(result["status"], "complete")
                targets = [request["target"] for request in CaptureHandler.requests]
                self.assertEqual(
                    targets,
                    [
                        "/api/v1/review/findings?repo=owner%2Frepo&pr=17&limit=5000",
                        "/api/v1/audit/events?session_id=session-1&until="
                        f"{result['capture_start_ms']}&limit=500",
                        "/api/v1/audit/events?session_id=session-1&until="
                        f"{result['capture_start_ms']}&limit=500&cursor=opaque-next",
                    ],
                )
                for request in CaptureHandler.requests:
                    self.assertTrue(request["signature"].startswith("sha256="))
                self.assertNotIn(SECRET, (output / "capture.json").read_text())
                self.assertEqual(result["audit"]["last_cursor"], "opaque-next")
        finally:
            server.shutdown()
            thread.join(timeout=2)

    def test_fake_transport_preserves_signature_contract_without_network(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            findings_target = "/api/v1/review/findings?repo=owner%2Frepo&pr=17&limit=5000"
            audit_prefix = "/api/v1/audit/events?session_id=session-1&until="
            audit_target = audit_prefix + "0&limit=500"
            # The timestamp is part of the request target, so route audit
            # responses by suffix rather than guessing the wall clock.
            class RoutingOpener(FakeOpener):
                def open(self, request, timeout):
                    target = urlsplit(request.full_url).path
                    if urlsplit(request.full_url).query:
                        target += "?" + urlsplit(request.full_url).query
                    if target.startswith(audit_prefix) and "&cursor=opaque-next" in target:
                        self.pages[target] = json.dumps({"events": []}).encode()
                    elif target.startswith(audit_prefix):
                        self.pages[target] = json.dumps({"events": [], "next_cursor": "opaque-next"}).encode()
                    return super().open(request, timeout)

            opener = RoutingOpener(SECRET, {findings_target: b'{"findings":[],"limit":5000}'})
            output = Path(temp) / "capture"
            with mock.patch.object(module, "_OPENER", opener), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                result = module.capture(
                    "http://127.0.0.1:8090",
                    "owner/repo",
                    17,
                    "session-1",
                    output,
                    allow_local_http=True,
                )
            self.assertEqual(result["status"], "complete")
            self.assertEqual(len(opener.requests), 3)
            self.assertEqual(opener.requests[0]["target"], findings_target)
            self.assertIn("cursor=opaque-next", opener.requests[-1]["target"])
            self.assertEqual(audit_target.split("&until=")[0], "/api/v1/audit/events?session_id=session-1")

    def test_prepare_selects_exact_scope_and_requested_revision_and_weekly_accepts(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)
            finding = {
                "id": 41,
                "session_id": "session-1",
                "repo": "owner/repo",
                "pr_number": 17,
                "stable_id": "stable-id-is-not-the-identity",
                "severity": "red",
                "status": "open",
                "title": "literal ledger title; do not reinterpret",
                "path": "src/app.py",
                "line": 2,
                "raised_by": "reviewer-model",
                "angle": "correctness",
                "head_sha": revision.upper(),
                "created_at": 1,
            }
            opener = CaptureFixtureOpener(SECRET, [finding])
            capture_dir = root / "capture"
            with mock.patch.object(module, "_OPENER", opener), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                module.capture(
                    "http://127.0.0.1:8090",
                    "owner/repo",
                    17,
                    "session-1",
                    capture_dir,
                    allow_local_http=True,
                )
            before = subprocess.run(
                ["git", "-C", str(repo), "status", "--porcelain"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout
            prepared = module.prepare(capture_dir, repo, revision, base, root / "prepared")
            after = subprocess.run(
                ["git", "-C", str(repo), "status", "--porcelain"],
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            ).stdout
            self.assertEqual(prepared["status"], "ready")
            self.assertEqual(before, after)
            self.assertEqual(prepared["selected"][0]["finding_id"], "session-1:41")
            findings = json.loads((root / "prepared" / "findings.json").read_text())
            self.assertEqual(findings["findings"][0]["claim"], finding["title"])
            self.assertEqual(findings["findings"][0]["title"], finding["title"])
            self.assertEqual(findings["findings"][0]["claim_provenance"], "ledger_title_only")
            self.assertNotIn("reviewer-model", (root / "prepared" / "evidence" / "manifest.json").read_text())
            packet = json.loads((root / "prepared" / "source-packet.json").read_text())
            self.assertEqual(packet["revision"], revision.lower())
            self.assertNotIn("current-only.txt", {entry["path"] for entry in packet["files"]})
            weekly = module._weekly()
            cutoff = weekly._parse_datetime(prepared["snapshot_at"], "snapshot_at")
            report = weekly.build_report(
                root / "prepared" / "weekly-bundle",
                weekly._iso_week(cutoff),
                prepared["snapshot_at"],
            )
            self.assertEqual(report["snapshot_at"], prepared["snapshot_at"])
            self.assertEqual(report["capture"]["coverage"]["audit"], "complete")
            self.assertEqual(report["capture"]["coverage"]["product_tables"]["session_targets"], "unknown")

    def test_prepare_blocks_missing_location_and_conflicting_duplicate_identity(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)

            def row(identifier, title="finding"):
                return {
                    "id": identifier,
                    "session_id": "session-1",
                    "repo": "owner/repo",
                    "pr_number": 17,
                    "stable_id": f"stable-{identifier}",
                    "severity": "red",
                    "status": "open",
                    "title": title,
                    "path": "src/app.py",
                    "line": 2,
                    "raised_by": None,
                    "angle": None,
                    "head_sha": revision,
                    "created_at": 1,
                }

            valid = row(41, "valid")
            missing_location = row(42, "missing location")
            missing_location.pop("path")
            conflicting_a = row(43, "first identity")
            conflicting_b = row(43, "second identity")
            outside = row(44, "outside")
            outside["repo"] = "other/repo"
            capture_dir = capture_fixture(module, root, [valid, dict(valid), missing_location, conflicting_a, conflicting_b, outside])
            prepared = module.prepare(capture_dir, repo, revision, base, root / "prepared")
            self.assertEqual(prepared["status"], "blocked")
            self.assertIn("invalid_or_conflicting_selected_findings", prepared["block_reasons"])
            self.assertEqual(prepared["selection"]["duplicate_exact_count"], 1)
            self.assertEqual(prepared["selection"]["conflicting_identity_count"], 1)
            reasons = [entry["reason"] for entry in prepared["excluded"]]
            self.assertTrue(any("invalid_path" in reason for reason in reasons))
            self.assertIn("conflicting_duplicate_finding_id", reasons)
            self.assertIn("scope_mismatch_repository", reasons)
            self.assertEqual(prepared["selection"]["accepted_count"], 1)
            with self.assertRaises(module.BridgeError):
                module.run(
                    root / "prepared",
                    repo,
                    root / "models.json",
                    root / "run",
                )

    def test_capture_limit_and_page_cap_are_explicitly_partial(self):
        module = load_module()
        rows = [{"id": number} for number in range(5000)]
        opener = CaptureFixtureOpener(SECRET, rows, next_cursor="opaque-next")
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "capture"
            with mock.patch.object(module, "_OPENER", opener), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                result = module.capture(
                    "http://127.0.0.1:8090",
                    "owner/repo",
                    17,
                    "session-1",
                    output,
                    max_pages=1,
                    allow_local_http=True,
                )
            self.assertEqual(result["status"], "partial")
            self.assertEqual(result["findings"]["coverage"], "partial")
            self.assertEqual(result["audit"]["coverage"], "partial")
            self.assertEqual(result["audit"]["partial_reason"], "page_cap")
            self.assertEqual(result["audit"]["page_count"], 1)

            missing_cursor = CaptureFixtureOpener(
                SECRET,
                [],
                audit_events=[rust_audit_event(number) for number in range(500)],
            )
            with mock.patch.object(module, "_OPENER", missing_cursor), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                missing = module.capture(
                    "http://127.0.0.1:8090",
                    "owner/repo",
                    17,
                    "session-1",
                    Path(temp) / "missing-cursor",
                    allow_local_http=True,
                )
            self.assertEqual(missing["audit"]["coverage"], "complete")
            self.assertTrue(missing["audit"]["final_null_cursor"])
            self.assertIsNone(missing["audit"]["partial_reason"])

            short_missing_cursor = CaptureFixtureOpener(SECRET, [], audit_events=[rust_audit_event(1)])
            with mock.patch.object(module, "_OPENER", short_missing_cursor), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                short_missing = module.capture(
                    "http://127.0.0.1:8090",
                    "owner/repo",
                    17,
                    "session-1",
                    Path(temp) / "short-missing-cursor",
                    allow_local_http=True,
                )
            self.assertEqual(short_missing["audit"]["coverage"], "complete")
            self.assertTrue(short_missing["audit"]["final_null_cursor"])
            self.assertIsNone(short_missing["audit"]["partial_reason"])

    def test_nonempty_rust_terminal_pages_without_cursor_are_complete_and_preserved(self):
        module = load_module()
        cases = (
            ("short-omitted", 22, False),
            ("short-explicit-null", 22, True),
            ("full-omitted", module.AUDIT_LIMIT, False),
        )
        with tempfile.TemporaryDirectory() as temp:
            for name, event_count, explicit_null_cursor in cases:
                with self.subTest(name=name):
                    events = [rust_audit_event(number) for number in range(1, event_count + 1)]
                    opener = CaptureFixtureOpener(
                        SECRET,
                        [],
                        audit_events=events,
                        explicit_null_cursor=explicit_null_cursor,
                    )
                    output = Path(temp) / name
                    with mock.patch.object(module, "_OPENER", opener), mock.patch.dict(
                        module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
                    ):
                        result = module.capture(
                            "http://127.0.0.1:8090",
                            "owner/repo",
                            17,
                            "session-1",
                            output,
                            allow_local_http=True,
                        )

                    self.assertEqual(result["status"], "complete")
                    self.assertEqual(result["audit"]["coverage"], "complete")
                    self.assertTrue(result["audit"]["final_null_cursor"])
                    self.assertIsNone(result["audit"]["terminal_next_cursor"])
                    self.assertIsNone(result["audit"]["partial_reason"])
                    self.assertEqual(len(opener.requests), 2)
                    self.assertTrue(all("cursor=" not in request["target"] for request in opener.requests))
                    audit_record = next(record for record in result["files"] if record["kind"] == "audit")
                    raw = (output / audit_record["path"]).read_bytes()
                    self.assertEqual(raw, opener.pages[audit_record["request_target"]])
                    self.assertEqual(audit_record["bytes"], len(raw))
                    self.assertEqual(audit_record["sha256"], module.sha256_bytes(raw))
                    response = json.loads(raw)
                    self.assertEqual(response["events"], events)
                    if explicit_null_cursor:
                        self.assertIsNone(response["next_cursor"])
                    else:
                        self.assertNotIn("next_cursor", response)

    def test_capture_rejects_repeated_cursor_and_http_errors_without_body(self):
        module = load_module()
        repeated = CaptureFixtureOpener(SECRET, [], next_cursor="same-cursor")
        with tempfile.TemporaryDirectory() as temp:
            with mock.patch.object(module, "_OPENER", repeated), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                with self.assertRaisesRegex(module.BridgeError, "repeated"):
                    module.capture(
                        "http://127.0.0.1:8090",
                        "owner/repo",
                        17,
                        "session-1",
                        Path(temp) / "repeated",
                        allow_local_http=True,
                    )
            error = module.urllib.error.HTTPError(
                "https://controller.invalid",
                401,
                "unauthorized secret-body-must-not-leak",
                {},
                None,
            )
            with mock.patch.object(module, "_OPENER", ErrorOpener(error)), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                with self.assertRaisesRegex(module.BridgeError, "HTTP 401") as raised:
                    module.capture(
                        "https://controller.invalid",
                        "owner/repo",
                        17,
                        "session-1",
                        Path(temp) / "unauthorized",
                    )
            self.assertNotIn("secret-body", str(raised.exception))

    def test_capture_rejects_unsafe_urls_and_redirects(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            for url in (
                "https://user:password@example.invalid",
                "https://example.invalid/?existing=query",
                "http://example.invalid",
            ):
                with self.subTest(url=url):
                    with self.assertRaises(module.BridgeError):
                        module.capture(url, "owner/repo", 17, "session-1", root / "rejected")
            redirect = module.urllib.error.HTTPError(
                "https://controller.invalid",
                302,
                "redirect",
                {"Location": "https://other.invalid"},
                None,
            )
            with mock.patch.object(module, "_OPENER", ErrorOpener(redirect)), mock.patch.dict(
                module.os.environ, {"OCP_EVAL_OBSERVER_SECRET": SECRET}, clear=False
            ):
                with self.assertRaisesRegex(module.BridgeError, "redirect"):
                    module.capture(
                        "https://controller.invalid",
                        "owner/repo",
                        17,
                        "session-1",
                        root / "redirect",
                    )

    def test_prepared_capture_hash_tampering_is_rejected_before_run_output(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)
            finding = {
                "id": 41,
                "session_id": "session-1",
                "repo": "owner/repo",
                "pr_number": 17,
                "stable_id": "stable",
                "severity": "red",
                "status": "open",
                "title": "frozen claim",
                "path": "src/app.py",
                "line": 2,
                "raised_by": None,
                "angle": None,
                "head_sha": revision,
                "created_at": 1,
            }
            capture_fixture(module, root, [finding])
            module.prepare(root / "capture", repo, revision, base, root / "prepared")
            raw_path = root / "prepared" / "capture" / "raw" / "findings-page-0001.json"
            raw_path.write_bytes(raw_path.read_bytes() + b" ")
            models = root / "models.json"
            models.write_text("{}", encoding="utf-8")
            with self.assertRaisesRegex(module.BridgeError, "hash"):
                module.run(root / "prepared", repo, models, root / "run")
            self.assertFalse((root / "run").exists())

    def test_product_snapshot_is_scope_checked_and_supplied_tables_remain_partial(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)
            finding = {
                "id": 41,
                "session_id": "session-1",
                "repo": "owner/repo",
                "pr_number": 17,
                "stable_id": "stable",
                "severity": "red",
                "status": "open",
                "title": "frozen claim",
                "path": "src/app.py",
                "line": 2,
                "raised_by": None,
                "angle": None,
                "head_sha": revision,
                "created_at": 1,
            }
            capture_fixture(module, root, [finding])
            product = root / "product.json"
            product.write_text(
                json.dumps(
                    {
                        "session_targets": [
                            {
                                "session_id": "session-1",
                                "repo": "owner/repo",
                                "pr_number": 17,
                                "head_sha": revision.upper(),
                                "created_at": 1,
                                "reason": "review",
                                "required_valid_reviewers": 2,
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )
            prepared = module.prepare(root / "capture", repo, revision, base, root / "prepared", product)
            self.assertEqual(prepared["product"]["table_coverage"]["session_targets"], "partial")
            self.assertEqual(prepared["product"]["table_coverage"]["review_rounds"], "unknown")
            product_value = json.loads((root / "prepared" / "weekly-bundle" / "product.json").read_text())
            self.assertEqual(len(product_value["session_targets"]), 1)
            self.assertEqual(product_value["review_rounds"], [])
            bad_product = root / "bad-product.json"
            bad_product.write_text(
                json.dumps(
                    {
                        "session_targets": [
                            {
                                "session_id": "session-1",
                                "repo": "other/repo",
                                "pr_number": 17,
                                "head_sha": revision,
                                "created_at": 1,
                                "reason": "review",
                                "required_valid_reviewers": 2,
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaises(module.BridgeError):
                module.prepare(root / "capture", repo, revision, base, root / "bad-prepared", bad_product)

    def test_duplicate_json_keys_and_symlink_or_overlap_outputs_are_rejected(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)
            capture_dir = capture_fixture(module, root, [])
            duplicate_product = root / "duplicate-product.json"
            duplicate_product.write_bytes(b'{"session_targets": [], "session_targets": []}')
            with self.assertRaisesRegex(module.BridgeError, "duplicate JSON key"):
                module.prepare(capture_dir, repo, revision, base, root / "duplicate-output", duplicate_product)
            link = root / "link"
            link.symlink_to(root / "link-target", target_is_directory=True)
            with self.assertRaises(module.BridgeError):
                module.prepare(capture_dir, repo, revision, base, link / "prepared")
            with self.assertRaises(module.BridgeError):
                module.prepare(capture_dir, repo, revision, base, capture_dir / "child")

    def test_run_passes_frozen_inputs_to_evaluator_and_retains_failed_artifacts(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)
            finding = {
                "id": 41,
                "session_id": "session-1",
                "repo": "owner/repo",
                "pr_number": 17,
                "stable_id": "stable",
                "severity": "red",
                "status": "open",
                "title": "frozen claim",
                "path": "src/app.py",
                "line": 2,
                "raised_by": None,
                "angle": None,
                "head_sha": revision,
                "created_at": 1,
            }
            capture_fixture(module, root, [finding])
            module.prepare(root / "capture", repo, revision, base, root / "prepared")
            models = root / "models.json"
            models_fixture(models)
            calls = []

            def failed_evaluator(**kwargs):
                calls.append(kwargs)
                kwargs["output"].mkdir(parents=True)
                (kwargs["output"] / "failed-attempt.txt").write_text("retained", encoding="utf-8")
                raise RuntimeError("model seam failure")

            weekly = module._weekly()
            with mock.patch.object(weekly, "build_report") as build_report, mock.patch.object(
                weekly, "write_report"
            ) as write_report:
                build_report.return_value = {"report_id": "test-report"}
                write_report.return_value = (root / "report.md", root / "report.json")
                result = module.run(root / "prepared", repo, models, root / "run", evaluator=failed_evaluator)
            self.assertEqual(result["state"], "failed")
            self.assertEqual(result["quality"], "not_scoreable")
            self.assertEqual(
                result["weekly_report"],
                {"status": "blocked", "reason": "evaluation_not_verified"},
            )
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0]["revision"], revision.lower())
            self.assertEqual(calls[0]["base"], base.lower())
            self.assertEqual(calls[0]["findings_path"], root / "prepared" / "findings.json")
            self.assertEqual(calls[0]["evidence_dir"], root / "prepared" / "evidence")
            self.assertTrue((root / "run" / "evaluation" / "failed-attempt.txt").is_file())
            self.assertFalse((root / "run" / "weekly-report").exists())
            build_report.assert_not_called()
            write_report.assert_not_called()
            ledger = json.loads((root / "run" / "run.json").read_text())
            self.assertEqual(ledger["state"], "failed")
            self.assertEqual(ledger["quality"], "not_scoreable")
            self.assertEqual(ledger["evaluation"]["status"], "failed")
            self.assertFalse(ledger["evaluation"]["verified"])
            self.assertEqual(ledger["evaluation"]["error"], "RuntimeError")
            self.assertEqual(
                ledger["weekly_report"],
                {"status": "blocked", "reason": "evaluation_not_verified"},
            )

    def test_run_accepts_the_existing_core_failure_artifact_and_marks_quality_unknown(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)
            finding = {
                "id": 41,
                "session_id": "session-1",
                "repo": "owner/repo",
                "pr_number": 17,
                "stable_id": "stable",
                "severity": "red",
                "status": "open",
                "title": "frozen claim",
                "path": "src/app.py",
                "line": 2,
                "raised_by": None,
                "angle": None,
                "head_sha": revision,
                "created_at": 1,
            }
            capture_fixture(module, root, [finding])
            module.prepare(root / "capture", repo, revision, base, root / "prepared")
            models = root / "models.json"
            models_fixture(models)
            core = module._core()

            def failed_runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return core.adapters.ProcessCapture(tuple(argv), 0, b"fixture\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return core.adapters.ProcessCapture(
                        tuple(argv),
                        0,
                        b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n",
                        b"",
                    )
                return core.adapters.ProcessCapture(tuple(argv), 17, b"partial stdout", b"transport failed")

            class BlockedOCI:
                def preflight(self):
                    return {"status": "environment_blocked", "reason": "test seam"}

                def execute(self, *args, **kwargs):
                    raise AssertionError("blocked test OCI must not execute")

            def evaluator(**kwargs):
                return core.run_evaluation(
                    **kwargs,
                    adapter_runner=failed_runner,
                    oci_executor=BlockedOCI(),
                )

            result = module.run(root / "prepared", repo, models, root / "run", evaluator=evaluator)
            self.assertEqual(result["state"], "failed")
            self.assertEqual(result["quality"], "not_scoreable")
            self.assertTrue((root / "run" / "evaluation" / "summary.json").is_file())
            self.assertEqual(json.loads((root / "run" / "run.json").read_text())["evaluation"]["verified"], True)
            self.assertEqual(json.loads((root / "run" / "run.json").read_text())["weekly_report"]["status"], "complete")

    def test_run_blocks_tampered_evaluation_after_core_verifier_rejects_it(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = init_git_repo(root)
            finding = {
                "id": 41,
                "session_id": "session-1",
                "repo": "owner/repo",
                "pr_number": 17,
                "stable_id": "stable",
                "severity": "red",
                "status": "open",
                "title": "frozen claim",
                "path": "src/app.py",
                "line": 2,
                "raised_by": None,
                "angle": None,
                "head_sha": revision,
                "created_at": 1,
            }
            capture_fixture(module, root, [finding])
            module.prepare(root / "capture", repo, revision, base, root / "prepared")
            models = root / "models.json"
            models_fixture(models)
            core = module._core()

            def failed_runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return core.adapters.ProcessCapture(tuple(argv), 0, b"fixture\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return core.adapters.ProcessCapture(
                        tuple(argv),
                        0,
                        b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n",
                        b"",
                    )
                return core.adapters.ProcessCapture(tuple(argv), 17, b"partial stdout", b"transport failed")

            class BlockedOCI:
                def preflight(self):
                    return {"status": "environment_blocked", "reason": "test seam"}

                def execute(self, *args, **kwargs):
                    raise AssertionError("blocked test OCI must not execute")

            def tampering_evaluator(**kwargs):
                result = core.run_evaluation(
                    **kwargs,
                    adapter_runner=failed_runner,
                    oci_executor=BlockedOCI(),
                )
                summary = kwargs["output"] / "summary.json"
                tampered = json.loads(summary.read_text())
                tampered["state"] = "complete"
                summary.write_text(json.dumps(tampered), encoding="utf-8")
                return result

            weekly = module._weekly()
            with mock.patch.object(weekly, "build_report") as build_report, mock.patch.object(
                weekly, "write_report"
            ) as write_report:
                build_report.return_value = {"report_id": "test-report"}
                write_report.return_value = (root / "report.md", root / "report.json")
                result = module.run(
                    root / "prepared",
                    repo,
                    models,
                    root / "run",
                    evaluator=tampering_evaluator,
                )

            self.assertEqual(result["state"], "failed")
            self.assertEqual(result["quality"], "not_scoreable")
            self.assertEqual(
                result["weekly_report"],
                {"status": "blocked", "reason": "evaluation_not_verified"},
            )
            self.assertEqual(
                json.loads((root / "run" / "evaluation" / "summary.json").read_text())["state"],
                "complete",
            )
            self.assertFalse((root / "run" / "weekly-report").exists())
            build_report.assert_not_called()
            write_report.assert_not_called()
            ledger = json.loads((root / "run" / "run.json").read_text())
            self.assertEqual(ledger["state"], "failed")
            self.assertEqual(ledger["quality"], "not_scoreable")
            self.assertEqual(ledger["evaluation"]["status"], "failed")
            self.assertFalse(ledger["evaluation"]["verified"])
            self.assertEqual(ledger["evaluation"]["error"], "EvaluationConflict")
            self.assertEqual(
                ledger["weekly_report"],
                {"status": "blocked", "reason": "evaluation_not_verified"},
            )


if __name__ == "__main__":
    unittest.main()
