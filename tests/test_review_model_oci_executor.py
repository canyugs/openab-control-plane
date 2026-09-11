import base64
import importlib.util
import inspect
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
IMAGE = "python@sha256:b64631e04e4920160c50fbe8d8df828f7f35f06f425cb44aa09bca53e708a35a"


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


def plan_with_claims(module, *, files=None, baseline=True, counterexample=False):
    return {
        "item_id": "F-1",
        "files": files if files is not None else [{"path": "generated/check.py", "utf8": "print('fixture')"}],
        "runs": [
            {
                "name": "baseline",
                "argv": ["python3", "/work/generated/check.py", "baseline"],
                "cwd": "/work",
                "expect": {"exit": 0, "observation": {"claim_present": baseline}},
                "evidence_ids": ["E1"],
            },
            {
                "name": "counterexample",
                "argv": ["python3", "/work/generated/check.py", "safe"],
                "cwd": "/work",
                "expect": {"exit": 0, "observation": {"claim_present": counterexample}},
                "evidence_ids": ["E1"],
            },
        ],
        "claim_observed": "claim",
    }


class FakeDocker:
    def __init__(self, module, *, mode="success", metadata_diff=False, output_size=None):
        self.module = module
        self.mode = mode
        self.metadata_diff = metadata_diff
        self.output_size = output_size
        self.calls = []

    def __call__(self, argv, payload, cwd, timeout, max_output):
        self.calls.append(
            {
                "argv": list(argv),
                "payload": bytes(payload),
                "cwd": Path(cwd),
                "timeout": timeout,
                "max_output": max_output,
            }
        )
        if self.mode == "timeout":
            return 124, b"partial stdout", b"partial stderr", True
        if self.mode == "overflow":
            size = self.output_size or max_output + 1
            return 0, b"x" * size, b"e" * size, False
        if self.mode == "failure":
            return 1, b"runner output", b"runner failed", False
        request = json.loads(payload.decode("utf-8"))
        requested_run = request["runs"][0]["name"]
        claim = requested_run == "baseline"
        observation = {"claim_present": claim}
        if self.metadata_diff:
            observation["metadata"] = requested_run
        actual = {
            "name": requested_run,
            "exit": 0,
            "timeout": False,
            "duration_ms": 1,
            "observation": observation,
            "stdout": f"raw-{requested_run}",
            "stderr": f"err-{requested_run}",
            "stdout_b64": base64.b64encode(f"raw-{requested_run}".encode()).decode(),
            "stderr_b64": base64.b64encode(f"err-{requested_run}".encode()).decode(),
        }
        return 0, json.dumps({"runs": [actual]}).encode("utf-8"), b"docker stderr", False


class OciExecutorBehaviorTests(unittest.TestCase):
    def test_docker_argv_has_stdin_and_literal_isolation_boundary(self):
        module = load_module()
        self.assertIsNotNone(module, "the OCI executor must exist")
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            runner = Path(temp) / "runner.py"
            runner.write_text("pass", encoding="utf-8")
            argv = module.build_docker_run_argv(
                image=IMAGE,
                source_dir=source,
                runner_path=runner,
                container_name="eval-item-1",
            )
        self.assertIn("-i", argv)
        self.assertIn("--network", argv)
        self.assertEqual(argv[argv.index("--network") + 1], "none")
        self.assertIn("--read-only", argv)
        self.assertIn("--cap-drop", argv)
        self.assertIn("ALL", argv)
        self.assertIn("no-new-privileges", " ".join(argv))
        self.assertIn("--pull=never", argv)
        self.assertTrue(any(value.startswith("/work:rw,") and "size=" in value for value in argv))
        self.assertTrue(any(value.startswith("/tmp:rw,") and "size=" in value for value in argv))
        mounts = [value for value in argv if value.startswith("type=bind,")]
        self.assertEqual(len(mounts), 2)
        self.assertTrue(all(value.endswith(",readonly") for value in mounts))
        self.assertIn("65532:65532", argv)
        self.assertNotIn("--env-file", argv)
        self.assertNotIn("/var/run/docker.sock", " ".join(argv))
        self.assertNotIn("host", [part.lower() for part in argv])

    def test_default_image_is_the_frozen_digest(self):
        module = load_module()
        self.assertEqual(module.DEFAULT_IMAGE, IMAGE)

    def test_plan_requires_structured_distinct_claim_controls(self):
        module = load_module()
        base = plan_with_claims(module)
        normalized = module.validate_generated_plan(base, {"E1"})
        self.assertEqual(normalized["runs"][0]["expect"]["observation"]["claim_present"], True)

        for invalid in (
            {
                **base,
                "runs": [
                    {**base["runs"][0], "expect": {"exit": 0, "observation": {"metadata": "only"}}},
                    base["runs"][1],
                ],
            },
            plan_with_claims(module, baseline=True, counterexample=True),
            {
                **base,
                "runs": [{**base["runs"][0], "expect": {"exit": 1, "observation": {"claim_present": True}}}, base["runs"][1]],
            },
        ):
            with self.assertRaises(module.OCIError):
                module.validate_generated_plan(invalid, {"E1"})

    def test_normalized_plan_can_be_validated_idempotently(self):
        module = load_module()
        raw = plan_with_claims(module)
        normalized = module.validate_generated_plan(raw, {"E1"})

        self.assertEqual(module.validate_generated_plan(normalized, {"E1"}), normalized)

    def test_normalized_file_metadata_must_be_complete_and_derived(self):
        module = load_module()
        normalized = module.validate_generated_plan(plan_with_claims(module), {"E1"})
        file = normalized["files"][0]
        invalid_files = [
            {**file, "bytes": file["bytes"] + 1},
            {**file, "sha256": "0" * 64},
            {key: value for key, value in file.items() if key != "bytes"},
            {key: value for key, value in file.items() if key != "sha256"},
            {**file, "unexpected": True},
        ]

        for invalid_file in invalid_files:
            with self.assertRaises(module.OCIError):
                module.validate_generated_plan({**normalized, "files": [invalid_file]}, {"E1"})

    def test_plan_rejects_paths_shells_assertion_harnesses_and_undeclared_evidence(self):
        module = load_module()
        base = plan_with_claims(module)
        invalid_plans = [
            {**base, "files": [{"path": "generated/../escape.py", "utf8": "pass"}]},
            {**base, "files": [{"path": "generated/check.py", "utf8": "assert False"}]},
            {
                **base,
                "runs": [{**base["runs"][0], "argv": ["sh", "-c", "echo unsafe"]}, base["runs"][1]],
            },
            {
                **base,
                "runs": [{**base["runs"][0], "evidence_ids": ["not-declared"]}, base["runs"][1]],
            },
        ]
        for invalid in invalid_plans:
            with self.assertRaises(module.OCIError):
                module.validate_generated_plan(invalid, {"E1"})

    def test_fake_controls_pass_mechanical_checks_but_never_semantically_promote(self):
        module = load_module()
        fake = FakeDocker(module, metadata_diff=True)
        executor = module.OCIExecutor(IMAGE, process_runner=fake, probe_daemon=False)
        plan = plan_with_claims(module)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(plan, source, evidence_ids={"E1"})

        self.assertTrue(result["controls_passed"])
        self.assertEqual(result["classification"], "unproven")
        self.assertEqual(result["status"], "success")
        self.assertIs(result["claim_present"], True)
        self.assertEqual([run["name"] for run in result["runs"]], ["baseline", "counterexample"])
        self.assertEqual(result["runs"][0]["actual"]["stdout"], "raw-baseline")
        self.assertEqual(result["runs"][0]["actual"]["stderr"], "err-baseline")
        self.assertEqual(len(fake.calls), 2)
        self.assertTrue(all("-i" in call["argv"] for call in fake.calls))

    def test_executor_accepts_a_normalized_plan_and_reaches_both_controls(self):
        module = load_module()
        fake = FakeDocker(module)
        executor = module.OCIExecutor(IMAGE, process_runner=fake, probe_daemon=False)
        normalized = module.validate_generated_plan(plan_with_claims(module), {"E1"})
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(normalized, source, evidence_ids={"E1"})

        self.assertTrue(result["controls_passed"])
        self.assertEqual(result["plan"], normalized)
        self.assertEqual(len(fake.calls), 2)
        payloads = [json.loads(call["payload"].decode("utf-8")) for call in fake.calls]
        self.assertEqual([payload["runs"][0]["name"] for payload in payloads], ["baseline", "counterexample"])
        self.assertEqual(payloads[0]["files"], normalized["files"])

    def test_metadata_or_constant_difference_does_not_pass_controls(self):
        module = load_module()

        def same_claim_runner(argv, payload, cwd, timeout, max_output):
            request = json.loads(payload.decode("utf-8"))
            name = request["runs"][0]["name"]
            actual = {
                "name": name,
                "exit": 0,
                "timeout": False,
                "observation": {"claim_present": True, "constant": name},
                "stdout": name,
                "stderr": "",
            }
            return 0, json.dumps({"runs": [actual]}).encode(), b"", False

        executor = module.OCIExecutor(IMAGE, process_runner=same_claim_runner, probe_daemon=False)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(plan_with_claims(module), source, evidence_ids={"E1"})
        self.assertFalse(result["controls_passed"])
        self.assertEqual(result["classification"], "unproven")
        self.assertIs(result["claim_present"], True)

    def test_nonzero_exit_and_unstructured_output_cannot_pass(self):
        module = load_module()

        def bad_runner(argv, payload, cwd, timeout, max_output):
            request = json.loads(payload.decode("utf-8"))
            name = request["runs"][0]["name"]
            actual = {
                "name": name,
                "exit": 7 if name == "baseline" else 0,
                "timeout": False,
                "observation": {"claim_present": name == "baseline"},
            }
            return 0, json.dumps({"runs": [actual]}).encode(), b"", False

        executor = module.OCIExecutor(IMAGE, process_runner=bad_runner, probe_daemon=False)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(plan_with_claims(module), source, evidence_ids={"E1"})
        self.assertFalse(result["controls_passed"])
        self.assertEqual(result["classification"], "unproven")

    def test_timeout_cleans_each_owned_named_container_and_keeps_failure_evidence(self):
        module = load_module()
        fake = FakeDocker(module, mode="timeout")
        executor = module.OCIExecutor(IMAGE, process_runner=fake, probe_daemon=False)
        cleaned = []
        executor._cleanup_container = cleaned.append
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(plan_with_claims(module), source, evidence_ids={"E1"})

        names = [call["argv"][call["argv"].index("--name") + 1] for call in fake.calls]
        self.assertEqual(cleaned, names)
        self.assertEqual(len(set(cleaned)), 2)
        self.assertFalse(result["controls_passed"])
        self.assertEqual(result["classification"], "unproven")
        self.assertEqual(len(result["runs"]), 2)
        self.assertTrue(all(run["timeout"] for run in result["runs"]))
        self.assertIn("stdout", result["runs"][0])
        self.assertIn("stderr", result["runs"][0])

    def test_stdout_overflow_is_bounded_before_result_accumulation(self):
        module = load_module()
        fake = FakeDocker(module, mode="overflow", output_size=1024)
        executor = module.OCIExecutor(
            IMAGE,
            process_runner=fake,
            probe_daemon=False,
            max_output_bytes=32,
        )
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(plan_with_claims(module), source, evidence_ids={"E1"})

        self.assertFalse(result["controls_passed"])
        self.assertEqual(result["reason"], "output_limit")
        self.assertTrue(all(run["stdout_truncated"] for run in result["runs"]))
        self.assertTrue(all(len(run["stdout"].encode()) <= 32 for run in result["runs"]))
        self.assertTrue(all(len(run["stderr"].encode()) <= 32 for run in result["runs"]))

    def test_each_control_gets_a_fresh_container_and_payload(self):
        module = load_module()
        fake = FakeDocker(module)
        executor = module.OCIExecutor(IMAGE, process_runner=fake, probe_daemon=False)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            item_dir = Path(temp) / "audit-item"
            executor.execute(
                plan_with_claims(module, files=[{"path": "generated/check.py", "utf8": "print('x')"}]),
                source,
                evidence_ids={"E1"},
                item_dir=item_dir,
            )
            host_generated = (item_dir / "generated").exists()

        self.assertEqual(len(fake.calls), 2)
        payloads = [json.loads(call["payload"].decode()) for call in fake.calls]
        self.assertEqual([payload["runs"][0]["name"] for payload in payloads], ["baseline", "counterexample"])
        names = [call["argv"][call["argv"].index("--name") + 1] for call in fake.calls]
        self.assertEqual(len(set(names)), 2)
        self.assertTrue(all(len(payload["runs"]) == 1 for payload in payloads))
        self.assertFalse(host_generated)

    def test_optional_operator_checks_use_the_same_boundary_and_are_not_prerequisites(self):
        module = load_module()
        fake = FakeDocker(module)
        executor = module.OCIExecutor(IMAGE, process_runner=fake, probe_daemon=False)
        primary = plan_with_claims(module)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(primary, source, evidence_ids={"E1"}, operator_checks=[primary])
        self.assertTrue(result["controls_passed"])
        self.assertEqual(len(result["operator_checks"]), 1)
        self.assertTrue(result["operator_checks"][0]["controls_passed"])
        self.assertEqual(len(fake.calls), 4)

        failing_fake = FakeDocker(module, mode="failure")
        failing_executor = module.OCIExecutor(IMAGE, process_runner=failing_fake, probe_daemon=False)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            failed = failing_executor.execute(primary, source, evidence_ids={"E1"}, operator_checks=[primary])
        self.assertFalse(failed["controls_passed"])
        self.assertEqual(failed["operator_checks"], "deferred_until_identical_validation")
        self.assertEqual(len(failing_fake.calls), 2)

    def test_failure_returns_plan_runs_and_raw_captures(self):
        module = load_module()
        fake = FakeDocker(module, mode="failure")
        executor = module.OCIExecutor(IMAGE, process_runner=fake, probe_daemon=False)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(plan_with_claims(module), source, evidence_ids={"E1"})

        self.assertIn("plan", result)
        self.assertEqual(len(result["runs"]), 2)
        self.assertEqual(result["runs"][0]["stdout"], "runner output")
        self.assertEqual(result["runs"][0]["stderr"], "runner failed")
        self.assertFalse(result["controls_passed"])

    def test_source_materialization_rejects_symlink_components_and_digest_conflicts(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            destination = root / "destination"
            destination.mkdir()
            outside = root / "outside"
            outside.mkdir()
            (destination / "linked").symlink_to(outside, target_is_directory=True)
            data = b"print('safe')\n"
            packet = {"files": [{"path": "linked/check.py", "utf8": data.decode(), "sha256": module.sha256_bytes(data)}]}
            with self.assertRaises(module.OCIError):
                module.materialize_source_tree(packet, destination)

            destination_link = root / "destination-link"
            destination_link.symlink_to(outside, target_is_directory=True)
            with self.assertRaises(module.OCIError):
                module.materialize_source_tree(packet, destination_link / "nested")

            conflict = {
                "files": [
                    {"path": "app.py", "utf8": "one", "sha256": module.sha256_bytes(b"one")},
                    {"path": "app.py", "utf8": "two", "sha256": module.sha256_bytes(b"two")},
                ]
            }
            with self.assertRaises(module.OCIError):
                module.materialize_source_tree(conflict, root / "conflict")

    def test_source_materialization_accepts_verified_utf8_and_base64_without_host_generated_files(self):
        module = load_module()
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            destination = root / "source"
            data = "π\n".encode("utf-8")
            encoded = base64.b64encode(b"binary-safe").decode()
            packet = {
                "files": [
                    {"path": "utf8/app.py", "utf8": data.decode(), "sha256": module.sha256_bytes(data)},
                    {"path": "data.bin", "base64": encoded, "sha256": module.sha256_bytes(b"binary-safe")},
                ]
            }
            module.materialize_source_tree(packet, destination)
            self.assertEqual((destination / "utf8/app.py").read_bytes(), data)
            self.assertEqual((destination / "data.bin").read_bytes(), b"binary-safe")
            self.assertEqual((destination / "utf8/app.py").stat().st_mode & 0o222, 0)

    def test_fixed_runner_and_process_boundary_do_not_use_unbounded_communicate(self):
        module = load_module()
        self.assertNotIn("communicate(", inspect.getsource(module._default_process_runner))
        self.assertNotIn("communicate(", module.fixed_runner_source())
        self.assertIn("start_new_session", inspect.getsource(module._default_process_runner))
        self.assertIn("shell=False", module.fixed_runner_source())

    def test_environment_block_is_detailed_and_never_semantically_promoted(self):
        module = load_module()
        executor = module.OCIExecutor(
            IMAGE,
            process_runner=FakeDocker(module, mode="failure"),
            probe_daemon=False,
        )
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            result = executor.execute(plan_with_claims(module), source, evidence_ids={"E1"})
        self.assertEqual(result["classification"], "unproven")
        self.assertIn("plan", result)
        self.assertEqual(len(result["runs"]), 2)

        def daemon_runner(argv, payload, cwd, timeout, max_output):
            return 1, b"", b"Cannot connect to the Docker daemon", False

        blocked_executor = module.OCIExecutor(IMAGE, process_runner=daemon_runner, probe_daemon=False)
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "source"
            source.mkdir()
            blocked = blocked_executor.execute(plan_with_claims(module), source, evidence_ids={"E1"})
        self.assertEqual(blocked["status"], "environment_blocked")
        self.assertEqual(blocked["classification"], "environment_blocked")


if __name__ == "__main__":
    unittest.main()
