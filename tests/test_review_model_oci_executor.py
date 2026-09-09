import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load_module():
    path = ROOT / "scripts/review_model_oci_executor.py"
    spec = importlib.util.spec_from_file_location("review_model_oci_executor", path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    sys.modules["review_model_oci_executor"] = module
    try:
        spec.loader.exec_module(module)
    except (FileNotFoundError, ImportError):
        return None
    return module


class OciExecutorBehaviorTests(unittest.TestCase):
    def test_docker_argv_has_isolation_and_no_host_shell(self):
        module = load_module()
        self.assertIsNotNone(module, "the OCI executor must exist")
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            runner = Path(temp) / "runner.py"
            runner.write_text("pass", encoding="utf-8")
            argv = module.build_docker_run_argv(
                image="python:3.12-slim@sha256:" + "a" * 64,
                source_dir=source,
                runner_path=runner,
                container_name="eval-item-1",
            )
        self.assertIn("--network", argv)
        self.assertIn("none", argv)
        self.assertIn("--read-only", argv)
        self.assertIn("--cap-drop", argv)
        self.assertIn("ALL", argv)
        self.assertIn("no-new-privileges", " ".join(argv))
        self.assertNotIn("sh", [part.lower() for part in argv])

    def test_plan_requires_two_literal_controls_and_rejects_assertion_harnesses(self):
        module = load_module()
        base = {
            "item_id": "F-1",
            "files": [{"path": "generated/check.py", "utf8": "print('x')"}],
            "runs": [
                {"name": "baseline", "argv": ["python3", "/work/generated/check.py"], "cwd": "/work", "expect": {"exit": 0, "observation": {"x": 1}}, "evidence_ids": ["E1"]},
                {"name": "counterexample", "argv": ["python3", "/work/generated/check.py", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"x": 2}}, "evidence_ids": ["E1"]},
            ],
            "claim_observed": "claim",
        }
        normalized = module.validate_generated_plan(base, {"E1"})
        self.assertEqual(normalized["files"][0]["path"], "generated/check.py")
        bad = dict(base)
        bad["files"] = [{"path": "generated/check.py", "utf8": "assert False"}]
        with self.assertRaises(Exception):
            module.validate_generated_plan(bad, {"E1"})
        bad = dict(base)
        bad["runs"] = [dict(base["runs"][0], argv=["sh", "-c", "echo unsafe"]), base["runs"][1]]
        with self.assertRaises(Exception):
            module.validate_generated_plan(bad, {"E1"})

    def test_executor_records_observed_baseline_control_and_is_environment_blockable(self):
        module = load_module()
        plan = {
            "item_id": "F-1",
            "files": [],
            "runs": [
                {"name": "baseline", "argv": ["python3", "baseline"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": True}}, "evidence_ids": ["E1"]},
                {"name": "counterexample", "argv": ["python3", "counterexample"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": False}}, "evidence_ids": ["E1"]},
            ],
            "claim_observed": "claim",
        }

        def fake_process(argv, payload, cwd, timeout, max_output):
            return 0, b'{"runs":[{"name":"baseline","exit":0,"observation":{"claim_present":true}},{"name":"counterexample","exit":0,"observation":{"claim_present":false}}]}', b"", False

        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            item = Path(temp) / "item"
            result = module.OCIExecutor(
                "fixture@sha256:" + "a" * 64,
                process_runner=fake_process,
                probe_daemon=False,
            ).execute(plan, source, evidence_ids={"E1"}, item_dir=item)
            self.assertEqual(result["classification"], "executed_reproduced")
            self.assertEqual(len(result["runs"]), 2)
            command = result["argv"]
            self.assertIn("--network", command)
            self.assertIn("none", command)
            self.assertIn("--read-only", command)
            self.assertIn("--user", command)
            self.assertIn("65532:65532", command)
            self.assertNotIn("--env-file", command)
            self.assertNotIn("/var/run/docker.sock", " ".join(command))

        blocked = module.OCIExecutor(
            "fixture@sha256:" + "a" * 64,
            process_runner=lambda *args: (1, b"", b"Cannot connect to the Docker daemon", False),
        ).preflight()
        self.assertEqual(blocked["status"], "environment_blocked")

    def test_generated_paths_and_shell_forms_are_rejected(self):
        module = load_module()
        self.assertIsNotNone(module, "the OCI executor must exist")
        with self.assertRaises(Exception):
            module.validate_generated_plan(
                {
                    "item_id": "F-1",
                    "files": [{"path": "generated/../escape.py", "utf8": "pass"}],
                    "runs": [],
                    "claim_observed": "x",
                },
                evidence_ids=set(),
            )


if __name__ == "__main__":
    unittest.main()
