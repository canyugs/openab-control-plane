import contextlib
import hashlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
AS_OF = "2026-09-09T08:10:00+08:00"
AS_OF_DT = datetime.fromisoformat(AS_OF)
CUTOFF_MS = int(AS_OF_DT.timestamp() * 1000)
BASE_MS = CUTOFF_MS - 600_000
CUTOFF_SECONDS = int(AS_OF_DT.timestamp())
WEEK = "2026-W37"


def load_module():
    path = ROOT / "scripts/review_round_weekly_report.py"
    spec = importlib.util.spec_from_file_location("review_round_weekly_report", path)
    if spec is None or spec.loader is None:
        raise AssertionError("weekly report module is missing")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class EvidenceFixture:
    def __init__(self, root: Path, *, coverage=None):
        self.root = root
        self.bundle = root / "bundle"
        self.bundle.mkdir()
        self.audit = []
        self.targets = []
        self.rounds = []
        self.findings = []
        self.writes = []
        self.runtime = []
        self.human = []
        self.cost = []
        self.next_event = 1
        self.next_write = 1
        self.next_round = 1
        self.coverage = coverage or {
            "audit": "complete",
            "product": "complete",
            "human": "unknown",
            "cost": "unknown",
        }

    def event(self, kind, outcome, offset, *, correlation=None, detail=None, event_id=None, recorded_at=None):
        number = self.next_event
        self.next_event += 1
        row = {
            "seq": number,
            "version": 1,
            "event_id": event_id or f"event-{number}",
            "event_key": event_id or f"event-{number}",
            "occurred_at": BASE_MS + offset,
            "recorded_at": BASE_MS + offset if recorded_at is None else recorded_at,
            "service": "github-pr-controller",
            "kind": kind,
            "outcome": outcome,
            "correlation": correlation or {},
            "detail": detail or {},
        }
        self.audit.append(row)
        return row

    def action(self, session, *, accepted_offset=10, completed=True, correlation=None):
        corr = correlation or {
            "delivery_id": f"delivery-{session}",
            "action_id": f"action-{session}",
            "trigger_ref": f"github:pr/org/repo#{self.pr_for(session)}",
        }
        self.event("action.accepted", "accepted", accepted_offset, correlation=corr)
        if completed:
            completed_corr = {**corr, "session_id": session}
            self.event("action.completed", "succeeded", accepted_offset + 1, correlation=completed_corr, detail={"http_status": 200})

    def pr_for(self, session):
        for target in self.targets:
            if target["session_id"] == session:
                return target["pr_number"]
        return 1

    def target(self, session, *, pr=1, sha=None, reason="review"):
        self.targets.append({
            "session_id": session,
            "repo": "org/repo",
            "pr_number": pr,
            "head_sha": sha if sha is not None else "a" * 40,
            "created_at": CUTOFF_SECONDS - 100,
            "reason": reason,
            "required_valid_reviewers": 2,
        })

    def round(self, session, *, pr=1, sha=None, decision="approve", integrity="verified", verified=None):
        commit = sha if sha is not None else "a" * 40
        self.rounds.append({
            "id": self.next_round,
            "repo": "org/repo",
            "pr_number": pr,
            "round": 1,
            "session_id": session,
            "head_sha": commit,
            "comment_id": None,
            "decision": decision,
            "red": 0,
            "yellow": 0,
            "green": 1,
            "created_at": CUTOFF_SECONDS - 90,
            "verified_commit_id": verified if verified is not None else (commit if integrity == "verified" else None),
            "integrity_disposition": integrity,
        })
        self.next_round += 1

    def write(self, session, kind, payload, provider=None, *, offset=300, outcome="succeeded", reconciled=False, write_id=None):
        identifier = self.next_write if write_id is None else write_id
        if write_id is None:
            self.next_write += 1
        payload_text = json.dumps(payload, separators=(",", ":"), ensure_ascii=False)
        self.writes.append({
            "id": identifier,
            "session_id": session,
            "kind": kind,
            "payload_json": payload_text,
            "state": "done",
            "attempts": 1,
            "created_at": CUTOFF_SECONDS - 80,
            "claimed_at": CUTOFF_SECONDS - 70,
            "done_at": CUTOFF_SECONDS - 60,
        })
        if provider is not None:
            event_kind = "github.write.reconciled" if reconciled else "github.write.succeeded"
            event_outcome = "reconciled" if reconciled else outcome
            self.event(
                event_kind,
                event_outcome,
                offset,
                correlation={"session_id": session, "write_id": str(identifier)},
                detail={
                    "operation": kind,
                    "attempt": 1,
                    "request_sha256": hashlib.sha256(payload_text.encode("utf-8")).hexdigest(),
                    "provider_receipt": provider,
                },
            )
        return identifier, payload_text

    def reliable(self, session, *, pr=1, sha=None, decision="approve", start=300, reconciled_review=False):
        commit = sha if sha is not None else "a" * 40
        self.target(session, pr=pr, sha=commit)
        self.action(session)
        self.round(session, pr=pr, sha=commit, decision=decision)
        self.write(session, "comment", {"repo": "org/repo", "pr_number": pr, "comment_id": None, "body": f"report {session}"}, {"comment_id": 100 + pr, "reconciled": False}, offset=start)
        state = "success" if decision == "approve" else "failure"
        self.write(session, "status", {"repo": "org/repo", "sha": commit, "commit_id": commit, "state": state, "context": "openab/council", "description": "council"}, {"sha": commit, "context": "openab/council", "state": state}, offset=start + 1)
        event = "APPROVE" if decision == "approve" else "REQUEST_CHANGES"
        review_state = "APPROVED" if decision == "approve" else "CHANGES_REQUESTED"
        self.write(session, "review", {"repo": "org/repo", "pr_number": pr, "event": event, "commit_id": commit, "body": f"review {session}"}, {"review_id": 200 + pr, "state": review_state, "commit_id": commit, "reconciled": reconciled_review}, offset=start + 2, reconciled=reconciled_review)

    def diagnostic(self, session, *, pr=1, disposition="unparseable", sha=None, full_target=True, decision="unknown", start=300):
        target_sha = sha if sha is not None else ("b" * 40 if full_target else "not-a-sha")
        self.target(session, pr=pr, sha=target_sha)
        self.action(session)
        self.round(session, pr=pr, sha=target_sha, decision=decision, integrity=disposition)
        self.write(session, "comment", {"repo": "org/repo", "pr_number": pr, "comment_id": None, "body": f"diagnostic {session}"}, {"comment_id": 300 + pr, "reconciled": False}, offset=start)
        if full_target:
            self.write(session, "status", {"repo": "org/repo", "sha": target_sha, "commit_id": target_sha, "state": "error", "context": "openab/council", "description": "error"}, {"sha": target_sha, "context": "openab/council", "state": "error"}, offset=start + 1)

    def tombstone(self, session, *, event_type="session.timeout", comment_id=500, start=300, bind=True):
        self.target(session, pr=1)
        self.action(session)
        self.runtime.append({
            "event_id": f"runtime-{session}",
            "body_sha256": "c" * 64,
            "event_type": event_type,
            "session_id": session,
            "occurred_at": BASE_MS + start,
            "received_at": CUTOFF_SECONDS - 1,
        })
        payload = {"repo": "org/repo", "pr_number": 1, "body": f"abandoned {session}"}
        provider = {"comment_id": comment_id, "reconciled": True}
        self.write(session, "comment_abandon", payload, provider if bind else None, offset=start)

    def finish(self, *, human=False, cost=False, cursor_complete=True, audit_coverage=None, product_coverage=None):
        product = {
            "session_targets": self.targets,
            "review_rounds": self.rounds,
            "review_findings": self.findings,
            "github_writes": self.writes,
            "runtime_event_receipts": self.runtime,
        }
        product_raw = json.dumps(product, sort_keys=True, separators=(",", ":")).encode("utf-8")
        (self.bundle / "product.json").write_bytes(product_raw)
        audit_raw = b"\n".join(json.dumps(row, sort_keys=True, separators=(",", ":")).encode("utf-8") for row in self.audit) + (b"\n" if self.audit else b"")
        (self.bundle / "audit.ndjson").write_bytes(audit_raw)
        if human:
            raw = b"\n".join(json.dumps(row, sort_keys=True, separators=(",", ":")).encode("utf-8") for row in self.human) + b"\n"
            (self.bundle / "human.ndjson").write_bytes(raw)
        if cost:
            raw = b"\n".join(json.dumps(row, sort_keys=True, separators=(",", ":")).encode("utf-8") for row in self.cost) + b"\n"
            (self.bundle / "cost.ndjson").write_bytes(raw)
        table_coverage = product_coverage or {name: "complete" for name in ("session_targets", "review_rounds", "review_findings", "github_writes", "runtime_event_receipts")}
        coverage = {
            "audit": audit_coverage or self.coverage["audit"],
            "product": self.coverage["product"],
            "product_tables": table_coverage,
            "human": "partial" if human else self.coverage["human"],
            "cost": "partial" if cost else self.coverage["cost"],
        }
        sources = {}
        for source in ("audit", *table_coverage, "human", "cost"):
            source_coverage = coverage["audit"] if source == "audit" else coverage["human"] if source == "human" else coverage["cost"] if source == "cost" else table_coverage[source]
            sources[source] = {"coverage": source_coverage, "start": AS_OF, "end": AS_OF}
        manifest = {
            "schema_version": "review-round-weekly-evidence/v1",
            "bundle_id": "fixture-bundle",
            "snapshot_at": AS_OF,
            "timezone": "Asia/Taipei",
            "coverage": coverage,
            "sources": sources,
            "audit_cursor": {"first_cursor": "0", "last_cursor": None, "page_count": 1, "final_null_cursor": cursor_complete},
            "files": {
                "audit.ndjson": {"sha256": hashlib.sha256(audit_raw).hexdigest(), "record_count": len(self.audit)},
                "product.json": {"sha256": hashlib.sha256(product_raw).hexdigest()},
            },
        }
        for name in ("human.ndjson", "cost.ndjson"):
            path = self.bundle / name
            if path.exists():
                raw = path.read_bytes()
                manifest["files"][name] = {"sha256": hashlib.sha256(raw).hexdigest(), "record_count": len(raw.splitlines())}
        (self.bundle / "evidence-manifest.json").write_text(json.dumps(manifest, sort_keys=True), encoding="utf-8")
        return self.bundle


class WeeklyReportBehaviorTests(unittest.TestCase):
    def test_actual_formal_review_receipts_cover_approve_and_request_changes(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("approve", pr=1, decision="approve")
            fixture.reliable("changes", pr=2, decision="request_changes")
            bundle = fixture.finish()
            report = module.build_report(bundle, WEEK, AS_OF)
            self.assertEqual(report["reliability"]["reliable"], 2)
            self.assertEqual(report["reliability"]["pending_or_unknown"], 0)
            self.assertEqual(report["latency"]["counts"]["observed"], 2)
            self.assertEqual(report["latency"]["counts"]["reconciliation_upper_bound"], 0)
            by_session = {row["session_id"]: row for row in report["cohort"]["sessions"]}
            for session in ("approve", "changes"):
                self.assertEqual(by_session[session]["latency"]["status"], "observed")
                self.assertEqual(by_session[session]["latency"]["qualification"], "original_receipt")

    def test_opening_and_decision_writes_are_not_terminal_requirements(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("noise", pr=1)
            fixture.write("noise", "comment_open", {"repo": "org/repo", "pr_number": 1, "body": "opened"}, {"comment_id": 99, "reconciled": False}, offset=100)
            fixture.write("noise", "decision_comment:55", {"repo": "org/repo", "pr_number": 1, "comment_id": 99, "body": "decision"}, {"comment_id": 99, "reconciled": False}, offset=101)
            fixture.write("noise", "decision_status:55", {"repo": "org/repo", "sha": "a" * 40, "commit_id": "a" * 40, "state": "success", "context": "openab/council"}, {"sha": "a" * 40, "context": "openab/council", "state": "success"}, offset=102)
            fixture.write("noise", "decision_review:55", {"repo": "org/repo", "pr_number": 1, "event": "APPROVE", "commit_id": "a" * 40, "body": "decision"}, {"review_id": 99, "state": "APPROVED", "commit_id": "a" * 40, "reconciled": False}, offset=103)
            bundle = fixture.finish()
            report = module.build_report(bundle, WEEK, AS_OF)
            self.assertEqual(report["reliability"]["reliable"], 1)

    def test_diagnostic_contract_is_table_driven_and_legacy_is_not_diagnostic(self):
        module = load_module()
        cases = (
            ("unparseable", True, "visible_failure"),
            ("insufficient_valid_reviewers", True, "visible_failure"),
            ("invalid_target", False, "visible_failure"),
            ("legacy_unverified", True, "pending_or_unknown"),
            ("unsupported_disposition", True, "pending_or_unknown"),
        )
        for disposition, full_target, expected in cases:
            with self.subTest(disposition=disposition):
                with tempfile.TemporaryDirectory() as temp:
                    fixture = EvidenceFixture(Path(temp))
                    fixture.diagnostic("s", disposition=disposition, full_target=full_target)
                    bundle = fixture.finish()
                    if disposition == "legacy_unverified":
                        fixture.rounds[0]["decision"] = "approve"
                        # Re-write the immutable fixture after the intentional mutation.
                        bundle = fixture.finish()
                    report = module.build_report(bundle, WEEK, AS_OF)
                    self.assertEqual(report["reliability"][expected], 1)

    def test_timeout_and_superseded_tombstones_need_bound_nonnull_provider_comment_id(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.tombstone("sup", event_type="session.superseded", comment_id=501)
            fixture.tombstone("timeout", event_type="session.timeout", comment_id=502)
            fixture.tombstone("null", event_type="session.timeout", comment_id=None)
            fixture.tombstone("sup-null", event_type="session.superseded", comment_id=None)
            fixture.tombstone("unbound", event_type="session.timeout", comment_id=503, bind=False)
            bundle = fixture.finish()
            report = module.build_report(bundle, WEEK, AS_OF)
            by_session = {row["session_id"]: row for row in report["cohort"]["sessions"]}
            self.assertEqual(by_session["sup"]["category"], "superseded")
            self.assertTrue(by_session["sup"]["tombstone_visible"])
            self.assertEqual(by_session["timeout"]["category"], "visible_failure")
            self.assertEqual(by_session["timeout"]["latency"]["status"], "observed")
            self.assertEqual(by_session["timeout"]["latency"]["duration_ms"], 290)
            self.assertEqual(by_session["timeout"]["latency"]["qualification"], "original_receipt")
            self.assertEqual(by_session["null"]["category"], "pending_or_unknown")
            self.assertFalse(by_session["null"]["tombstone_visible"])
            self.assertEqual(by_session["unbound"]["category"], "pending_or_unknown")

    def test_incomplete_projection_latency_is_unavailable_for_timeout_and_supersession(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.tombstone("timeout-noop", event_type="session.timeout", comment_id=None)
            fixture.tombstone("superseded-noop", event_type="session.superseded", comment_id=None)
            bundle = fixture.finish()
            report = module.build_report(bundle, WEEK, AS_OF)
            by_session = {row["session_id"]: row for row in report["cohort"]["sessions"]}
            self.assertEqual(by_session["timeout-noop"]["category"], "pending_or_unknown")
            self.assertEqual(by_session["superseded-noop"]["category"], "superseded")
            for session in ("timeout-noop", "superseded-noop"):
                self.assertFalse(by_session[session]["tombstone_visible"])
                self.assertEqual(by_session[session]["latency"]["status"], "unknown")
                self.assertIsNone(by_session[session]["latency"]["duration_ms"])
                self.assertEqual(by_session[session]["latency"]["qualification"], "unavailable")

    def test_write_binding_corruption_stays_unknown(self):
        module = load_module()
        corruptions = ("write_id", "digest", "operation", "provider")
        for corruption in corruptions:
            with self.subTest(corruption=corruption):
                with tempfile.TemporaryDirectory() as temp:
                    fixture = EvidenceFixture(Path(temp))
                    fixture.reliable("s", pr=1)
                    review_receipt = next(event for event in fixture.audit if event["kind"] == "github.write.succeeded" and event["detail"].get("operation") == "review")
                    if corruption == "write_id":
                        review_receipt["correlation"]["write_id"] = "999"
                    elif corruption == "digest":
                        review_receipt["detail"]["request_sha256"] = "0" * 64
                    elif corruption == "operation":
                        review_receipt["detail"]["operation"] = "status"
                    else:
                        review_receipt["detail"]["provider_receipt"] = {"review_id": 3, "state": "APPROVED", "reconciled": False}
                    bundle = fixture.finish()
                    report = module.build_report(bundle, WEEK, AS_OF)
                    self.assertEqual(report["reliability"]["pending_or_unknown"], 1)

    def test_cohort_joins_delivery_action_trigger_and_deduplicates_before_week_assignment(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.target("redelivered", pr=1)
            corr = {"delivery_id": "d-redelivered", "action_id": "a-redelivered", "trigger_ref": "github:pr/org/repo#1"}
            fixture.action("redelivered", accepted_offset=-8 * 24 * 60 * 60 * 1000, correlation=corr)
            fixture.event("action.accepted", "accepted", 20, correlation=corr)
            fixture.reliable("ask", pr=2)
            fixture.targets[-1]["reason"] = "ask"
            fixture.reliable("current", pr=3)
            fixture.event("action.accepted", "accepted", 21, correlation={"delivery_id": "d-no", "action_id": "a-no", "trigger_ref": "github:pr/org/repo#99"})
            fixture.event("ingress.accepted", "accepted", 22, correlation={"delivery_id": "plan"})
            bundle = fixture.finish()
            report = module.build_report(bundle, WEEK, AS_OF)
            self.assertEqual(report["cohort"]["accepted_total"], 3)
            self.assertEqual(report["cohort"]["excluded_ask"], 1)
            self.assertEqual(report["cohort"]["eligible_review_total"], 1)
            self.assertEqual(report["cohort"]["unknown_eligibility"], 1)
            self.assertEqual(report["cohort"]["plan_only"], 1)
            self.assertEqual(report["reliability"]["reliable"], 1)

    def test_clock_cutoff_and_reconciliation_latency_use_required_terminal_writes(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("upper", pr=1, reconciled_review=True)
            # The opening and decision projections are unrelated to latency.
            fixture.write("upper", "comment_open", {"repo": "org/repo", "pr_number": 1, "body": "open"}, {"comment_id": 90, "reconciled": False}, offset=1)
            late = fixture.audit[-1]
            late["recorded_at"] = CUTOFF_MS + 1
            fixture.reliable("negative", pr=2, start=400)
            negative = next(event for event in fixture.audit if event["kind"] == "github.write.succeeded" and event["correlation"].get("session_id") == "negative" and event["detail"].get("operation") == "comment")
            negative["occurred_at"] = -1
            fixture.event("github.write.succeeded", "succeeded", 500, correlation={"session_id": "late-unrelated", "write_id": "999"}, detail={"operation": "comment", "request_sha256": "0" * 64, "provider_receipt": {"comment_id": 1, "reconciled": False}}, recorded_at=CUTOFF_MS + 2)
            bundle = fixture.finish()
            report = module.build_report(bundle, WEEK, AS_OF)
            by_session = {row["session_id"]: row for row in report["cohort"]["sessions"]}
            self.assertEqual(by_session["upper"]["latency"]["qualification"], "reconciliation_upper_bound")
            self.assertEqual(by_session["upper"]["latency"]["duration_ms"], 292)
            self.assertEqual(by_session["negative"]["latency"]["status"], "clock_invalid")
            self.assertEqual(report["capture"]["future_audit_records_excluded"], 2)

    def test_missing_human_and_cost_data_do_not_erase_reliability(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("known", pr=1)
            fixture.reliable("missing", pr=2)
            fixture.findings.append({"id": 1, "session_id": "known", "repo": "org/repo", "pr_number": 1, "stable_id": "F1", "severity": "red", "status": "open", "title": "finding", "path": "src/lib.rs", "line": 1, "raised_by": "reviewer", "angle": "correctness", "head_sha": "a" * 40, "created_at": CUTOFF_SECONDS - 50})
            fixture.human.append({"annotator_id": "human-1", "version": "v1", "session_id": "known", "finding_id": "F1", "repo": "org/repo", "pr_number": 1, "head_sha": "a" * 40, "verdict": "valid_useful", "evidence_reference": "evidence-1", "observed_at": AS_OF})
            fixture.human.append({"type": "escape", "confirming_human_id": "human-1", "version": "v1", "reviewed_window_id": "window-1", "evidence_reference": "escape-1", "status": "confirmed_escape", "repo": "org/repo", "pr_number": 1, "session_id": "known", "finding_id": "F1", "head_sha": "a" * 40, "confirmed_at": AS_OF})
            fixture.cost.append({"session_id": "known", "currency": "USD", "amount_minor": 10, "source_minor_unit": "cent", "reconciliation_time": AS_OF, "reconciliation_reference": "cost-1", "completeness": "complete", "attempt_coverage": "all_attempts_and_retries"})
            bundle = fixture.finish(human=True, cost=True)
            report = module.build_report(bundle, WEEK, AS_OF)
            self.assertEqual(report["reliability"]["reliable"], 2)
            self.assertEqual(report["human_quality"]["confirmed_escapes"], 1)
            self.assertEqual(report["cost"]["per_currency"]["USD"]["known_minor"], 10)
            self.assertEqual(report["cost"]["unknown_sessions"], 1)
            self.assertNotIn("actual_dollars", json.dumps(report))

    def test_manifest_snapshot_spans_cursor_and_table_coverage_are_validated(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("s")
            bundle = fixture.finish()
            manifest_path = bundle / "evidence-manifest.json"
            manifest = json.loads(manifest_path.read_text())
            manifest["sources"]["audit"]["end"] = "2026-09-09T08:00:00+08:00"
            manifest["sources"]["audit"]["start"] = "2026-09-09T09:00:00+08:00"
            manifest_path.write_text(json.dumps(manifest))
            with self.assertRaises(module.ReportError):
                module.build_report(bundle, WEEK, AS_OF)

    def test_evaluation_uses_injected_verifier_identity_hashes_and_keeps_partial_unscored(self):
        module = load_module()
        summary = {
            "state": "complete",
            "evaluation_identity": "eval-1",
            "artifact_hashes": {"summary.json": "a" * 64},
            "model_assessment": {
                "original_findings": 3,
                "supported": 1,
                "refuted": 1,
                "unresolved": 1,
                "scoreable_items": 3,
                "usefulness": {"useful": 2, "not_useful": 1, "unknown": 0},
                "disagreement_items": 1,
            },
            "validation_coverage": {"static_evidence": 1, "executed_reproduced": 1, "executed_refuted": 0, "environment_blocked": 0, "unproven": 1},
            "omissions": {"candidate_count": 2, "automatically_supported_omission": 1, "human_confirmed_escape": 0, "unknown": 1},
        }
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("s")
            bundle = fixture.finish()
            evaluation = Path(temp) / "evaluation"
            evaluation.mkdir()
            (evaluation / "summary.json").write_text(json.dumps({"state": "failed", "model_assessment": {"supported": 999}}))
            with mock.patch.object(module, "verify_evaluation_artifacts", return_value=summary):
                report = module.build_report(bundle, WEEK, AS_OF, evaluation)
            self.assertEqual(report["evaluation"]["availability"], "available")
            self.assertEqual(report["evaluation"]["denominator"], 3)
            self.assertEqual(report["evaluation"]["validation"]["unproven"], 1)
            self.assertEqual(report["evaluation"]["omissions"]["automatically_supported_omission"], 1)
            self.assertEqual(report["evaluation"]["quality"]["status"], "available")
            without = module.build_report(bundle, WEEK, AS_OF)
            self.assertNotEqual(report["report_id"], without["report_id"])

            partial = {**summary, "state": "partial"}
            with mock.patch.object(module, "verify_evaluation_artifacts", return_value=partial):
                partial_report = module.build_report(bundle, WEEK, AS_OF, evaluation)
            self.assertEqual(partial_report["evaluation"]["supported"], 0)
            self.assertEqual(partial_report["evaluation"]["quality"]["status"], "not_scoreable")

    def test_evaluation_duplicate_identity_dedupes_and_conflict_rejects(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("s")
            bundle = fixture.finish()
            evaluation = Path(temp) / "evaluation"
            evaluation.mkdir()
            summary = {"state": "complete", "evaluation_identity": "same", "model_assessment": {"supported": 1, "refuted": 0, "unresolved": 0}}
            with mock.patch.object(module, "verify_evaluation_artifacts", return_value={"evaluations": [summary, summary]}):
                report = module.build_report(bundle, WEEK, AS_OF, evaluation)
            self.assertEqual(report["evaluation"]["evaluations"], 1)
            conflict = {**summary, "model_assessment": {"supported": 0, "refuted": 1, "unresolved": 0}}
            with mock.patch.object(module, "verify_evaluation_artifacts", return_value={"evaluations": [summary, conflict]}):
                with self.assertRaises(module.ReportError):
                    module.build_report(bundle, WEEK, AS_OF, evaluation)

    def test_integrated_evaluation_without_core_helper_reports_dependency_not_raw_summary(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            fixture = EvidenceFixture(Path(temp))
            fixture.reliable("s")
            bundle = fixture.finish()
            evaluation = Path(temp) / "evaluation"
            evaluation.mkdir()
            (evaluation / "summary.json").write_text(json.dumps({"state": "complete", "model_assessment": {"supported": 99}}))
            with mock.patch.dict(sys.modules, {"review_model_evaluation": None}):
                report = module.build_report(bundle, WEEK, AS_OF, evaluation)
            self.assertEqual(report["evaluation"]["availability"], "dependency_unavailable")
            self.assertEqual(report["evaluation"]["supported"], 0)

    def test_integrated_invalid_core_artifact_is_report_error_without_class_leak(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fixture = EvidenceFixture(root)
            fixture.reliable("s")
            bundle = fixture.finish()
            evaluation = root / "evaluation"
            evaluation.mkdir()
            (evaluation / "snapshot.json").mkdir()
            (evaluation / "summary.json").write_text(
                json.dumps({"state": "complete", "model_assessment": {"supported": 99}}),
                encoding="utf-8",
            )
            cli_args = [
                "--bundle", str(bundle),
                "--week", WEEK,
                "--as-of", AS_OF,
                "--output", str(root / "output"),
                "--evaluation-root", str(evaluation),
            ]
            with mock.patch.dict(sys.modules):
                sys.modules.pop("review_model_evaluation", None)
                with mock.patch.object(sys, "path", [str(ROOT / "scripts"), *sys.path]):
                    with self.assertRaises(module.ReportError) as raised:
                        module.build_report(bundle, WEEK, AS_OF, evaluation)
                    message = str(raised.exception)
                    self.assertIn("evaluation artifact verification failed", message)
                    self.assertIn("snapshot.json", message)
                    self.assertNotIn("EvaluationConflict", message)

                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr):
                        status = module.main(cli_args)
                    self.assertEqual(status, 2)
                    cli_error = stderr.getvalue()
                    self.assertIn("weekly report failed: evaluation artifact verification failed", cli_error)
                    self.assertIn("snapshot.json", cli_error)
                    self.assertNotIn("EvaluationConflict", cli_error)
                    self.assertNotIn("EvaluationError", cli_error)
                    self.assertNotIn("Traceback", cli_error)

    def test_integrated_validated_core_result_is_returned_unchanged(self):
        module = load_module()
        expected = {"state": "complete", "evaluation_identity": "verified"}
        with tempfile.TemporaryDirectory() as temp:
            evaluation = Path(temp) / "evaluation"
            evaluation.mkdir()
            with mock.patch.dict(sys.modules):
                sys.modules.pop("review_model_evaluation", None)
                with mock.patch.object(sys, "path", [str(ROOT / "scripts"), *sys.path]):
                    import review_model_evaluation

                    with mock.patch.object(review_model_evaluation, "verify_evaluation_artifacts", return_value=expected):
                        result = module.verify_evaluation_artifacts(evaluation)
                    self.assertIs(result, expected)

    def test_integrated_unexpected_core_fault_is_not_suppressed(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            evaluation = Path(temp) / "evaluation"
            evaluation.mkdir()
            with mock.patch.dict(sys.modules):
                sys.modules.pop("review_model_evaluation", None)
                with mock.patch.object(sys, "path", [str(ROOT / "scripts"), *sys.path]):
                    import review_model_evaluation

                    with mock.patch.object(review_model_evaluation, "verify_evaluation_artifacts", side_effect=ValueError("programming fault")):
                        with self.assertRaisesRegex(ValueError, "programming fault"):
                            module.verify_evaluation_artifacts(evaluation)

    def test_deterministic_output_and_parent_overlap_refuse_overwrite(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            fixture = EvidenceFixture(root)
            fixture.reliable("s")
            bundle = fixture.finish()
            report_a = module.build_report(bundle, WEEK, AS_OF)
            report_b = module.build_report(bundle, WEEK, AS_OF)
            self.assertEqual(report_a, report_b)
            output = root / "output"
            markdown, json_path = module.write_report(report_a, output, bundle)
            self.assertTrue(markdown.is_file())
            self.assertTrue(json_path.is_file())
            with self.assertRaises(module.ReportError):
                module.write_report(report_a, output, bundle)
            with self.assertRaises(module.ReportError):
                module.write_report(report_a, root, bundle)


if __name__ == "__main__":
    unittest.main()
