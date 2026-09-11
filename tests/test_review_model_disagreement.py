import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]


def load_module(name, relative_path):
    path = ROOT / relative_path
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"module is missing: {relative_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ReviewModelDisagreementTests(unittest.TestCase):
    @staticmethod
    def item(item_id, verdicts, *, valid_judge_count=None, judge_disagreement=False, failed_judge=False):
        if valid_judge_count is None:
            valid_judge_count = len(verdicts)
        judges = [
            {
                "status": "success",
                "assessment": {
                    "verdict": verdict,
                    "usefulness": "useful",
                    "validation_verdict": "valid",
                },
            }
            for verdict in verdicts
        ]
        if failed_judge:
            judges.append({"status": "failed", "failure_class": "provider_unavailable"})
        return {
            "item_id": item_id,
            "kind": "original",
            "judges": judges,
            "valid_judge_count": valid_judge_count,
            "judge_disagreement": judge_disagreement,
            "synthesis": {
                "verdict": "supported",
                "disagreement": "The assessments agree; this is explanatory synthesis prose.",
            },
            "validation": {"status": "unproven"},
            "classification": "unproven",
            "scoreable": False,
        }

    @staticmethod
    def summary_for(eval_module, items, state="complete"):
        controller = eval_module.EvaluationController.__new__(eval_module.EvaluationController)
        controller.item_records = items
        controller.discovery_complete = True
        controller.snapshot = {
            "revision": "revision",
            "base": "base",
            "source_complete": True,
            "source_omissions": [],
        }
        controller._role_records = {}
        controller.models = {
            role: {
                "model_id": f"fixture-{role}",
                "adapter": "fixture",
                "family": "fixture",
            }
            for role in eval_module.ROLE_NAMES
        }
        controller.preflight = {}
        controller.discovery_status = {"status": "success"}
        return controller._summary(state)

    @staticmethod
    def evaluation_summary(identity, *, state="complete", disagreement_items=0, disagreement_denominator=None, include_denominator=True):
        model = {
            "original_findings": 1,
            "supported": 1,
            "refuted": 0,
            "unresolved": 0,
            "scoreable_items": 1,
            "usefulness": {"useful": 2, "not_useful": 0, "unknown": 0},
            "disagreement_items": disagreement_items,
        }
        if include_denominator:
            model["disagreement_denominator"] = disagreement_denominator
        return {
            "state": state,
            "evaluation_identity": identity,
            "model_assessment": model,
            "validation_coverage": {
                "static_evidence": 0,
                "executed_reproduced": 0,
                "executed_refuted": 0,
                "environment_blocked": 0,
                "unproven": 1,
            },
            "omissions": {
                "candidate_count": 0,
                "automatically_supported_omission": 0,
                "human_confirmed_escape": 0,
                "unknown": 0,
            },
        }

    def test_summary_counts_typed_disagreement_and_eligible_pairs_only(self):
        eval_module = load_module("review_model_evaluation_disagreement_summary", "scripts/review_model_evaluation.py")
        agreeing = [
            self.item(f"agree-{index}", ["support", "support"], valid_judge_count=2)
            for index in range(3)
        ]
        summary = self.summary_for(eval_module, agreeing)
        model = summary["model_assessment"]
        self.assertEqual(model["disagreement_items"], 0)
        self.assertEqual(model["disagreement_denominator"], 3)
        self.assertEqual(agreeing[0]["synthesis"]["disagreement"], "The assessments agree; this is explanatory synthesis prose.")

        differing = self.item(
            "differing",
            ["support", "refute"],
            valid_judge_count=2,
            judge_disagreement=True,
        )
        differing_summary = self.summary_for(eval_module, [differing])
        differing_model = differing_summary["model_assessment"]
        self.assertEqual(differing_model["disagreement_items"], 1)
        self.assertEqual(differing_model["disagreement_denominator"], 1)

    def test_summary_excludes_absent_failed_and_single_judges(self):
        eval_module = load_module("review_model_evaluation_disagreement_ineligible", "scripts/review_model_evaluation.py")
        items = [
            self.item("absent", [], valid_judge_count=0, judge_disagreement=True),
            self.item("failed", ["support"], valid_judge_count=1, judge_disagreement=True, failed_judge=True),
            self.item("single", ["support"], valid_judge_count=1, judge_disagreement=True),
        ]
        model = self.summary_for(eval_module, items)["model_assessment"]
        self.assertEqual(model["disagreement_items"], 0)
        self.assertEqual(model["disagreement_denominator"], 0)

    def test_weekly_consumes_eligible_denominator_and_keeps_partial_quality_unscoreable(self):
        weekly_module = load_module("review_round_weekly_report_disagreement", "scripts/review_round_weekly_report.py")
        complete = self.evaluation_summary("complete", disagreement_items=1, disagreement_denominator=1)
        agreeing = self.evaluation_summary("agreeing", disagreement_items=0, disagreement_denominator=3)
        partial = self.evaluation_summary("partial", state="partial", disagreement_items=99, disagreement_denominator=99)
        failed = self.evaluation_summary("failed", state="failed", disagreement_items=99, disagreement_denominator=99)
        with tempfile.TemporaryDirectory() as temp:
            evaluation_root = Path(temp) / "evaluation"
            evaluation_root.mkdir()
            with mock.patch.object(
                weekly_module,
                "verify_evaluation_artifacts",
                return_value={"evaluations": [complete, agreeing, partial, failed]},
            ):
                metrics = weekly_module._evaluation_metrics(evaluation_root)
        self.assertEqual(metrics["disagreement_items"], 1)
        self.assertEqual(metrics["disagreement_denominator"], 4)
        self.assertEqual(metrics["quality"]["status"], "not_scoreable")
        self.assertIsNone(metrics["quality"]["score"])

    def test_weekly_marks_legacy_missing_denominator_unknown(self):
        weekly_module = load_module("review_round_weekly_report_disagreement_legacy", "scripts/review_round_weekly_report.py")
        legacy = self.evaluation_summary("legacy", disagreement_items=3, include_denominator=False)
        with tempfile.TemporaryDirectory() as temp:
            evaluation_root = Path(temp) / "evaluation"
            evaluation_root.mkdir()
            with mock.patch.object(weekly_module, "verify_evaluation_artifacts", return_value=legacy):
                metrics = weekly_module._evaluation_metrics(evaluation_root)
        self.assertEqual(metrics["disagreement_items"], 3)
        self.assertIsNone(metrics["disagreement_denominator"])


if __name__ == "__main__":
    unittest.main()
