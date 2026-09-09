import importlib.util
import hashlib
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load_module():
    path = ROOT / "scripts/review_round_weekly_report.py"
    spec = importlib.util.spec_from_file_location("review_round_weekly_report", path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except (FileNotFoundError, ImportError):
        return None
    return module


class WeeklyReportBehaviorTests(unittest.TestCase):
    def test_ask_and_plan_only_are_not_reliable_review_cohort(self):
        module = load_module()
        self.assertIsNotNone(module, "the weekly report must exist")
        with tempfile.TemporaryDirectory() as temp:
            bundle = Path(temp) / "bundle"
            output = Path(temp) / "output"
            bundle.mkdir()
            product = {
                "session_targets": [
                    {"session_id": "ask-1", "repo": "o/r", "pr_number": 1, "reason": "ask", "head_sha": "a" * 40, "created_at": 1, "required_valid_reviewers": 2},
                    {"session_id": "review-1", "repo": "o/r", "pr_number": 2, "reason": "review", "head_sha": "b" * 40, "created_at": 1, "required_valid_reviewers": 2},
                ],
                "review_rounds": [],
                "review_findings": [],
                "github_writes": [],
                "runtime_event_receipts": [],
            }
            (bundle / "product.json").write_text(json.dumps(product), encoding="utf-8")
            (bundle / "audit.ndjson").write_text(
                json.dumps({
                    "seq": 1, "version": 1, "event_id": "a1", "event_key": "a1",
                    "occurred_at": 1788912000000, "recorded_at": 1788912000000,
                    "service": "controller", "kind": "ingress.accepted", "outcome": "accepted",
                    "correlation": {"session_id": "review-1"}, "detail": {},
                }) + "\n" +
                json.dumps({
                    "seq": 2, "version": 1, "event_id": "a2", "event_key": "a2",
                    "occurred_at": 1788912000000, "recorded_at": 1788912000000,
                    "service": "controller", "kind": "action.accepted", "outcome": "accepted",
                    "correlation": {"session_id": "ask-1"}, "detail": {},
                }) + "\n",
                encoding="utf-8",
            )
            manifest = {
                "schema_version": "review-round-weekly-evidence/v1",
                "bundle_id": "bundle-1",
                "snapshot_at": "2026-09-09T08:00:00+08:00",
                "timezone": "Asia/Taipei",
                "coverage": {"audit": "complete", "product": "complete", "human": "unknown", "cost": "unknown"},
                "audit_cursor": {"first_cursor": "0", "last_cursor": None, "page_count": 1, "final_null_cursor": True},
                "files": {
                    "audit.ndjson": {"sha256": hashlib.sha256((bundle / "audit.ndjson").read_bytes()).hexdigest(), "record_count": 2},
                    "product.json": {"sha256": hashlib.sha256((bundle / "product.json").read_bytes()).hexdigest()},
                },
            }
            (bundle / "evidence-manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            report = module.build_report(bundle, "2026-W37", "2026-09-09T08:00:00+08:00")
            self.assertEqual(report["cohort"]["excluded_ask"], 1)
            self.assertEqual(report["cohort"]["plan_only"], 1)
            self.assertEqual(report["reliability"]["reliable"], 0)

    def test_receipt_bound_projection_categories_and_cutoff(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            bundle, as_of = make_projection_bundle(Path(temp))
            report = module.build_report(bundle, "2026-W37", as_of)
            self.assertEqual(report["cohort"]["eligible_review_total"], 5)
            self.assertEqual(report["cohort"]["excluded_ask"], 1)
            self.assertEqual(report["reliability"]["reliable"], 1)
            self.assertEqual(report["reliability"]["superseded"], 1)
            self.assertEqual(report["reliability"]["visible_failure"], 1)
            self.assertEqual(report["reliability"]["pending_or_unknown"], 2)
            self.assertEqual(report["latency"]["counts"]["observed"], 2)
            self.assertEqual(report["capture"]["future_audit_records_excluded"], 1)
            self.assertNotIn("generated_at", report)

    def test_human_and_cost_unknowns_are_metric_specific_and_output_is_non_overwriting(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            bundle, as_of = make_projection_bundle(Path(temp), with_optional=True)
            report = module.build_report(bundle, "2026-W37", as_of)
            self.assertEqual(report["capture"]["coverage"]["human"], "partial")
            self.assertEqual(report["capture"]["coverage"]["cost"], "partial")
            self.assertEqual(report["reliability"]["reliable"], 1)
            self.assertEqual(report["human_quality"]["confirmed_escapes"], 1)
            self.assertEqual(report["cost"]["actual_total_status"], "unknown")
            output = Path(temp) / "report"
            markdown, json_path = module.write_report(report, output)
            self.assertTrue(markdown.is_file())
            self.assertTrue(json_path.is_file())
            with self.assertRaises(Exception):
                module.write_report(report, output)

    def test_evaluation_root_is_read_only_and_has_a_separate_denominator(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            bundle, as_of = make_projection_bundle(root)
            evaluation = root / "evaluation"
            evaluation.mkdir()
            (evaluation / "summary.json").write_text(json.dumps({"state": "complete", "model_assessment": {"supported": 2, "refuted": 1, "unresolved": 1}, "validation_coverage": {"executed_reproduced": 2}, "omissions": {"candidate_count": 1, "automatically_supported_omission": 1}}), encoding="utf-8")
            report = module.build_report(bundle, "2026-W37", as_of, evaluation)
            self.assertEqual(report["evaluation"]["availability"], "available")
            self.assertEqual(report["evaluation"]["denominator"], 4)
            self.assertEqual(report["evaluation"]["supported"], 2)
            self.assertEqual(report["reliability"]["denominator"], 5)


def make_projection_bundle(root, with_optional=False):
    bundle = root / "bundle"
    bundle.mkdir()
    as_of = "2026-09-09T08:10:00+08:00"
    base = 1788912000000
    sequence = 1
    audits = []
    targets = []
    rounds = []
    writes = []
    receipts = []

    def event(session, kind, outcome, offset, detail=None):
        nonlocal sequence
        row = {
            "seq": sequence, "version": 1, "event_id": f"event-{sequence}", "event_key": f"event-{sequence}",
            "occurred_at": base + offset, "recorded_at": base + offset, "service": "controller",
            "kind": kind, "outcome": outcome, "correlation": {"session_id": session}, "detail": detail or {},
        }
        sequence += 1
        audits.append(row)
        return row

    def add_target(session, pr, reason="review", sha=None):
        targets.append({"session_id": session, "repo": "org/repo", "pr_number": pr, "head_sha": sha or ("a" * 40), "created_at": 1788912000, "reason": reason, "required_valid_reviewers": 2})

    def add_write(session, kind, payload, provider, offset):
        write_id = f"{session}-{kind}"
        writes.append({"id": write_id, "session_id": session, "kind": kind, "payload_json": payload, "state": "done", "attempts": 1, "created_at": 1788912000, "claimed_at": 1788912001, "done_at": 1788912002})
        event(session, "github.write.succeeded", "succeeded", offset, {"operation": kind, "request_sha256": hashlib.sha256(payload.encode()).hexdigest(), "provider_receipt": provider})

    add_target("s1", 1)
    event("s1", "action.accepted", "accepted", 0)
    event("s1", "action.completed", "completed", 100)
    rounds.append({"id": "round-s1", "repo": "org/repo", "pr_number": 1, "round": 1, "session_id": "s1", "head_sha": "a" * 40, "comment_id": 1, "decision": "approve", "red": 0, "yellow": 0, "green": 1, "created_at": 1788912000, "verified_commit_id": "a" * 40, "integrity_disposition": "verified"})
    add_write("s1", "comment_report", '{"body":"report"}', {"comment_id": 101}, 300)
    add_write("s1", "status", '{"state":"success","sha":"' + "a" * 40 + '"}', {"state": "success", "sha": "a" * 40}, 310)
    add_write("s1", "formal_review", '{"event":"APPROVE","sha":"' + "a" * 40 + '"}', {"event": "APPROVE", "sha": "a" * 40}, 320)

    add_target("s2", 2)
    event("s2", "action.accepted", "accepted", 10)
    event("s2", "session.superseded", "succeeded", 20)
    receipts.append({"event_id": "runtime-s2", "body_sha256": "x", "event_type": "session.superseded", "session_id": "s2", "occurred_at": base + 20, "received_at": 1788912001})

    add_target("s3", 3)
    event("s3", "action.accepted", "accepted", 30)
    payload = "{}"
    add_write("s3", "comment_abandon", payload, {"comment_id": 303}, 350)
    receipts.append({"event_id": "runtime-s3", "body_sha256": "x", "event_type": "session.timeout", "session_id": "s3", "occurred_at": base + 31, "received_at": 1788912001})

    add_target("s4", 4)
    event("s4", "action.accepted", "accepted", 40)
    add_write("s4", "comment_abandon", payload, {"comment_id": None}, 360)
    receipts.append({"event_id": "runtime-s4", "body_sha256": "x", "event_type": "session.timeout", "session_id": "s4", "occurred_at": base + 41, "received_at": 1788912001})

    add_target("s5", 5)
    event("s5", "action.accepted", "accepted", 50)
    event("s5", "action.completed", "completed", 60)

    add_target("ask", 6, reason="ask")
    event("ask", "action.accepted", "accepted", 70)
    event("plan", "ingress.accepted", "accepted", 80)
    event("future", "action.accepted", "accepted", 700000)

    product = {"session_targets": targets, "review_rounds": rounds, "review_findings": [], "github_writes": writes, "runtime_event_receipts": receipts}
    (bundle / "product.json").write_text(json.dumps(product), encoding="utf-8")
    audit_raw = "\n".join(json.dumps(row, sort_keys=True) for row in audits) + "\n"
    (bundle / "audit.ndjson").write_text(audit_raw, encoding="utf-8")
    coverage = {"audit": "complete", "product": "complete", "human": "unknown", "cost": "unknown"}
    files = {
        "audit.ndjson": {"sha256": hashlib.sha256(audit_raw.encode()).hexdigest(), "record_count": len(audits)},
        "product.json": {"sha256": hashlib.sha256((bundle / "product.json").read_bytes()).hexdigest()},
    }
    if with_optional:
        human_rows = [
            {"annotator_id": "human-1", "version": "v1", "session_id": "s1", "finding_id": "F1", "repo": "org/repo", "pr_number": 1, "head_sha": "a" * 40, "verdict": "valid_useful", "evidence_reference": "row-1", "observed_at": as_of},
            {"type": "escape", "confirming_human_id": "human-1", "version": "v1", "reviewed_window_id": "window-1", "evidence_reference": "escape-1", "status": "confirmed_escape", "confirmed_at": as_of},
        ]
        human_raw = "\n".join(json.dumps(row) for row in human_rows) + "\n"
        (bundle / "human.ndjson").write_text(human_raw, encoding="utf-8")
        cost_rows = [{"session_id": "s1", "currency": "USD", "amount_minor": 10, "source_minor_unit": "cent", "reconciliation_time": as_of, "reconciliation_reference": "cost-1", "completeness": "partial", "attempt_coverage": "partial"}]
        cost_raw = "\n".join(json.dumps(row) for row in cost_rows) + "\n"
        (bundle / "cost.ndjson").write_text(cost_raw, encoding="utf-8")
        files["human.ndjson"] = {"sha256": hashlib.sha256(human_raw.encode()).hexdigest(), "record_count": 2}
        files["cost.ndjson"] = {"sha256": hashlib.sha256(cost_raw.encode()).hexdigest(), "record_count": 1}
        coverage["human"] = "partial"
        coverage["cost"] = "partial"
    manifest = {"schema_version": "review-round-weekly-evidence/v1", "bundle_id": "fixture-bundle", "snapshot_at": as_of, "timezone": "Asia/Taipei", "coverage": coverage, "audit_cursor": {"first_cursor": "0", "last_cursor": None, "page_count": 1, "final_null_cursor": True}, "files": files}
    (bundle / "evidence-manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    return bundle, as_of


if __name__ == "__main__":
    unittest.main()
