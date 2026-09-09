import importlib.util
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load_module(name, relative_path):
    path = ROOT / relative_path
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    except (FileNotFoundError, ImportError):
        return None
    return module


class ModelEvaluationBehaviorTests(unittest.TestCase):
    def test_invalid_citation_is_not_a_model_assessment(self):
        module = load_module("review_model_evaluation", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        finding = {
            "finding_id": "F-1",
            "title": "unsafe path",
            "severity": "high",
            "claim": "the changed code is unsafe",
            "location": {"path": "src/app.py", "start_line": 1, "end_line": 1},
            "evidence_ids": ["E-1"],
        }
        evidence = {
            "E-1": {
                "utf8": "a line\n",
                "sha256": "c4d1d6c2c5c1af4a6b4f9d9c6c5f5a2c29d6d1e22e5b4b5b1f5c3c5d7a8b9c0d",
                "allowed_ranges": [{"path": "src/app.py", "start": 1, "end": 1}],
            }
        }
        result = {
            "finding_id": "F-1",
            "verdict": "support",
            "severity": "high",
            "usefulness": "useful",
            "citations": [
                {"path": "src/app.py", "start": 99, "end": 99, "evidence_id": "E-1"}
            ],
            "counterexample": "none",
        }
        with self.assertRaises(Exception):
            module.validate_judge_result(result, finding, evidence, {"src/app.py": 3})

    def test_judge_result_requires_cited_evidence_for_a_positive_assessment(self):
        module = load_module("review_model_evaluation_positive", "scripts/review_model_evaluation.py")
        finding = {
            "finding_id": "F-1",
            "evidence_ids": ["E-1"],
        }
        evidence = {
            "E-1": {
                "allowed_ranges": [{"path": "src/app.py", "start": 1, "end": 1}],
            }
        }
        result = {
            "finding_id": "F-1",
            "verdict": "support",
            "severity": "high",
            "usefulness": "useful",
            "citations": [],
            "counterexample": "none",
        }
        with self.assertRaises(Exception):
            module.validate_judge_result(result, finding, evidence, {"src/app.py": 1})

    def test_discovery_packet_has_no_original_findings(self):
        module = load_module("review_model_evaluation", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        packet = module.build_discovery_packet(
            invocation_id="discovery-1",
            source_packet={"packet_sha256": "source", "files": []},
            evidence_catalog={"E-1": {"utf8": "evidence"}},
            original_findings=[{"finding_id": "F-1", "claim": "secret finding"}],
        )
        encoded = json.dumps(packet, sort_keys=True)
        self.assertNotIn("F-1", encoded)
        self.assertNotIn("secret finding", encoded)

    def test_anonymized_synthesis_has_two_independent_labels(self):
        module = load_module("review_model_evaluation_synthesis", "scripts/review_model_evaluation.py")
        finding = {
            "finding_id": "F-1",
            "claim": "claim",
            "location": {"path": "src/app.py", "start_line": 1, "end_line": 1},
            "evidence_ids": ["E-1"],
        }
        evidence = {"E-1": {"utf8": "x\n", "allowed_ranges": [{"path": "src/app.py", "start": 1, "end": 1}]}}
        judgments = [
            {"verdict": "support", "provenance": "model_assessment"},
            {"verdict": "refute", "provenance": "model_assessment"},
        ]
        packet = module.build_synthesis_packet("s-1", {"packet_sha256": "p"}, finding, evidence, {"classification": "unproven"}, judgments)
        self.assertEqual([item["label"] for item in packet["judgments"]], ["judge_1", "judge_2"])
        self.assertNotIn("judge_a_model_id", json.dumps(packet))
        with self.assertRaises(Exception):
            module.build_synthesis_packet("s-2", {}, finding, evidence, {}, judgments[:1])

    def test_claude_argv_uses_literal_schema_and_strict_no_tools_json_transport(self):
        module = load_module("review_model_evaluation_adapter", "scripts/review_model_evaluation.py")
        schema = {"type": "object", "required": ["item_id"]}
        argv = module.adapters.ClaudeAdapter.build_argv("claude", "strong-claude", schema, "fixed prompt")
        self.assertEqual(argv[1:6], ["--print", "--bare", "--no-session-persistence", "--output-format", "json"])
        self.assertEqual(json.loads(argv[7]), schema)
        self.assertEqual(argv[argv.index("--tools") + 1], "")
        self.assertIn("--strict-mcp-config", argv)
        self.assertNotIn("--allowedTools", argv)

    def test_claude_parses_structured_output_and_does_not_trust_model_identity(self):
        module = load_module("review_model_evaluation_claude_parse", "scripts/review_model_evaluation.py")

        def runner(argv, payload, cwd, env, timeout, max_output):
            if len(argv) == 2 and argv[1] == "--version":
                return module.adapters.ProcessCapture(tuple(argv), 0, b"claude 1\n", b"")
            if len(argv) == 2 and argv[1] == "--help":
                return module.adapters.ProcessCapture(tuple(argv), 0, b"--bare --no-session-persistence --output-format --json-schema --tools --strict-mcp-config --permission-mode --permission-prompts --system-prompt\n", b"")
            return module.adapters.ProcessCapture(tuple(argv), 0, b'{"model":"provider-model","structured_output":{"item_id":"x","model_id":"model-authored"},"usage":{"input_tokens":1}}', b"")

        adapter = module.adapters.ClaudeAdapter("fake-claude", runner=runner)
        with tempfile.TemporaryDirectory() as temp:
            response = adapter.invoke({"role": "synthesis"}, {"type": "object"}, "requested-model", system_prompt="fixed", session_dir=Path(temp))
        self.assertEqual(response.structured_output["item_id"], "x")
        self.assertEqual(response.observed_model_id, "unavailable")
        self.assertEqual(response.actual_metadata["usage"]["input_tokens"], 1)

    def test_duplicate_judges_are_a_preflight_error(self):
        module = load_module("review_model_evaluation_models", "scripts/review_model_evaluation.py")
        roles = {role: {"adapter": "claude", "model_id": "same", "family": "family", "strength": "strong"} for role in module.ROLE_NAMES}
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "models.json"
            path.write_text(json.dumps({"roles": roles}), encoding="utf-8")
            with self.assertRaises(Exception):
                module.load_models(path)

    def test_source_limit_is_a_hard_incomplete_scope_not_a_whole_repo_claim(self):
        module = load_module("review_model_evaluation_scope", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            repo, base, revision = self._git_repo(Path(temp))
            packet = module.build_source_packet(repo, revision, base, {"max_file_bytes": 1})
            self.assertFalse(packet["complete"])
            self.assertTrue(packet["omissions"])

    def test_failed_or_invalid_stages_never_become_scoreable(self):
        module = load_module("review_model_evaluation_failure", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = self._git_repo(root)
            findings, evidence, models = self._inputs(root)

            def invalid_runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"fake\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"--bare --no-session-persistence --output-format --json-schema --tools --strict-mcp-config --permission-mode --permission-prompts --system-prompt\n", b"")
                packet = json.loads(payload)
                if packet["role"] in {"judge_a", "judge_b"}:
                    output = {"finding_id": "F-1", "verdict": "support", "severity": "high", "usefulness": "useful", "citations": [{"path": "app.py", "start": 99, "end": 99, "evidence_id": "E-1"}], "counterexample": "none"}
                elif packet["role"] == "discovery":
                    output = {"candidates": []}
                elif packet["role"] == "validation":
                    output = {"item_id": "F-1", "files": [], "runs": [{"name": "baseline", "argv": ["python"], "cwd": "/work", "expect": {"exit": 0, "observation": {"x": 1}}, "evidence_ids": ["E-1"]}, {"name": "counterexample", "argv": ["python", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"x": 2}}, "evidence_ids": ["E-1"]}], "claim_observed": "x"}
                else:
                    output = {"item_id": "F-1", "verdict": "supported", "citations": [], "disagreement": "", "reason": "x"}
                return module.adapters.ProcessCapture(tuple(argv), 0, json.dumps({"structured_output": output}).encode(), b"")

            class FakeOCI:
                def preflight(self):
                    return {"status": "ready"}

                def execute(self, *args, **kwargs):
                    return {"status": "success", "classification": "executed_reproduced", "runs": []}

            summary = module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=root / "out", adapter_runner=invalid_runner, oci_executor=FakeOCI())
            self.assertIn(summary["state"], {"partial", "failed"})
            self.assertEqual(summary["model_assessment"]["scoreable_items"], 0)

    def test_restart_verifies_completed_bytes_and_rejects_input_conflicts(self):
        module = load_module("review_model_evaluation_restart", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = self._git_repo(root)
            findings, evidence, models = self._inputs(root)
            calls = []

            def runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"fake\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"--bare --no-session-persistence --output-format --json-schema --tools --strict-mcp-config --permission-mode --permission-prompts --system-prompt\n", b"")
                calls.append(tuple(argv))
                packet = json.loads(payload)
                role = packet["role"]
                if role in {"judge_a", "judge_b"}:
                    finding = packet["finding"]
                    output = {"finding_id": finding["finding_id"], "verdict": "support", "severity": "high", "usefulness": "useful", "citations": [{"path": "app.py", "start": 1, "end": 1, "evidence_id": "E-1"}], "counterexample": "none"}
                elif role == "discovery": output = {"candidates": []}
                elif role == "validation": output = {"item_id": "F-1", "files": [{"path": "generated/check.py", "utf8": "open('/source/app.py').read()"}], "runs": [{"name": "baseline", "argv": ["python", "/work/generated/check.py"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": True}}, "evidence_ids": ["E-1"]}, {"name": "counterexample", "argv": ["python", "/work/generated/check.py", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": False}}, "evidence_ids": ["E-1"]}], "claim_observed": "claim"}
                else: output = {"item_id": "F-1", "verdict": "supported", "citations": [{"path": "app.py", "start": 1, "end": 1, "evidence_id": "E-1"}], "disagreement": "", "reason": "x"}
                return module.adapters.ProcessCapture(tuple(argv), 0, json.dumps({"structured_output": output}).encode(), b"")

            class FakeOCI:
                def preflight(self): return {"status": "ready"}
                def execute(self, *args, **kwargs): return {"status": "success", "classification": "executed_reproduced", "runs": [{"name": "baseline", "exit": 0}, {"name": "counterexample", "exit": 0}]}

            out = root / "out"
            first_summary = module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=out, adapter_runner=runner, oci_executor=FakeOCI())
            self.assertEqual(first_summary["state"], "complete")
            self.assertEqual(first_summary["model_assessment"]["scoreable_items"], 1)
            first_call_count = len(calls)
            module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=out, adapter_runner=runner, oci_executor=FakeOCI())
            self.assertEqual(len(calls), first_call_count)
            (out / "invocations" / "judge_a-F-1" / "final.json").write_text("{}", encoding="utf-8")
            with self.assertRaises(Exception):
                module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=out, adapter_runner=runner, oci_executor=FakeOCI())
            findings.write_text(findings.read_text(encoding="utf-8").replace("changed behavior", "different input"), encoding="utf-8")
            with self.assertRaises(Exception):
                module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=out, adapter_runner=runner, oci_executor=FakeOCI())

    @staticmethod
    def _git_repo(root):
        repo = root / "repo"
        repo.mkdir()
        def git(*args):
            return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()
        git("init", "-q")
        git("config", "user.email", "tests@example.invalid")
        git("config", "user.name", "tests")
        (repo / "app.py").write_text("print(1)\n", encoding="utf-8")
        git("add", "app.py")
        git("commit", "-qm", "base")
        base = git("rev-parse", "HEAD")
        (repo / "app.py").write_text("print(2)\n", encoding="utf-8")
        git("commit", "-qam", "revision")
        revision = git("rev-parse", "HEAD")
        return repo, base, revision

    @staticmethod
    def _inputs(root):
        findings = root / "findings.json"
        findings.write_text(json.dumps({"schema_version": "review-model-findings/v1", "findings": [{"finding_id": "F-1", "title": "change", "severity": "high", "claim": "changed behavior", "location": {"path": "app.py", "start_line": 1, "end_line": 1}, "evidence_ids": ["E-1"]}]}), encoding="utf-8")
        evidence = root / "evidence"
        evidence.mkdir()
        text = "evidence\n"
        (evidence / "manifest.json").write_text(json.dumps({"schema_version": "review-model-evidence/v1", "entries": [{"id": "E-1", "utf8": text, "sha256": hashlib.sha256(text.encode()).hexdigest(), "allowed_ranges": [{"path": "app.py", "start": 1, "end": 1}]}]}), encoding="utf-8")
        models = root / "models.json"
        models.write_text(json.dumps({"roles": {role: {"adapter": "claude", "model_id": "model-" + role, "family": "test", "strength": "strong", "executable": "fake-claude"} for role in module_roles()}}), encoding="utf-8")
        return findings, evidence, models


def module_roles():
    return ("judge_a", "judge_b", "synthesis", "discovery", "validation")


if __name__ == "__main__":
    unittest.main()
