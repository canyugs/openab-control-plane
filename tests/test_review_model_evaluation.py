import importlib.util
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
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
    def test_git_snapshot_refuses_repo_fsmonitor_before_helper_execution(self):
        import os
        import shlex
        import subprocess
        import tempfile

        module = load_module("review_model_evaluation_git_boundary_fsmonitor", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            from pathlib import Path

            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            env = {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "LC_ALL": "C",
            }

            def git(*args):
                return subprocess.check_output(["git", "-C", str(repo), *args], env=env, text=True).strip()

            git("init", "-q")
            git("config", "user.email", "tests@example.invalid")
            git("config", "user.name", "tests")
            (repo / "app.py").write_text("print(1)\n", encoding="utf-8")
            git("add", "app.py")
            git("commit", "-qm", "base")
            marker = root / "fsmonitor.marker"
            helper = repo / ".git" / "fsmonitor-helper.sh"
            helper.write_text(f"#!/bin/sh\nprintf marker > {shlex.quote(str(marker))}\nexit 1\n", encoding="utf-8")
            helper.chmod(0o700)
            git("config", "core.fsmonitor", str(helper))

            with self.assertRaises(module.EvaluationError):
                module._validate_clean_repo(repo)
            self.assertFalse(marker.exists(), "repository fsmonitor helper must not execute")

    def test_git_snapshot_refuses_worktree_filter_before_helper_execution(self):
        import os
        import shlex
        import subprocess
        import tempfile

        module = load_module("review_model_evaluation_git_boundary_worktree_filter", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            from pathlib import Path

            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            env = {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "LC_ALL": "C",
            }

            def git(*args):
                return subprocess.check_output(["git", "-C", str(repo), *args], env=env, text=True).strip()

            git("init", "-q")
            git("config", "user.email", "tests@example.invalid")
            git("config", "user.name", "tests")
            (repo / "app.py").write_text("print(1)\n", encoding="utf-8")
            (repo / ".gitattributes").write_text("app.py filter=probe\n", encoding="utf-8")
            git("add", "app.py", ".gitattributes")
            git("commit", "-qm", "base")
            git("config", "extensions.worktreeConfig", "true")
            marker = root / "worktree-filter.marker"
            helper = repo / ".git" / "worktree-filter-helper.sh"
            helper.write_text(f"#!/bin/sh\nprintf marker > {shlex.quote(str(marker))}\nexit 1\n", encoding="utf-8")
            helper.chmod(0o700)
            git("config", "--worktree", "filter.probe.process", str(helper))

            with self.assertRaises(module.EvaluationError):
                module._validate_clean_repo(repo)
            self.assertFalse(marker.exists(), "worktree-local filter helper must not execute")

    def test_git_snapshot_refuses_repo_filter_commands_before_helper_execution(self):
        import os
        import shlex
        import subprocess
        import tempfile

        module = load_module("review_model_evaluation_git_boundary_filters", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            from pathlib import Path

            root = Path(temp)
            env = {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "LC_ALL": "C",
            }
            for filter_command in ("clean", "process"):
                repo = root / filter_command
                repo.mkdir()

                def git(*args):
                    return subprocess.check_output(["git", "-C", str(repo), *args], env=env, text=True).strip()

                git("init", "-q")
                git("config", "user.email", "tests@example.invalid")
                git("config", "user.name", "tests")
                (repo / "app.py").write_text("print(1)\n", encoding="utf-8")
                (repo / ".gitattributes").write_text("app.py filter=probe\n", encoding="utf-8")
                git("add", "app.py", ".gitattributes")
                git("commit", "-qm", "base")
                marker = root / f"{filter_command}.marker"
                helper = repo / ".git" / f"{filter_command}-helper.sh"
                helper.write_text(
                    f"#!/bin/sh\nprintf marker > {shlex.quote(str(marker))}\n"
                    + ("cat\n" if filter_command == "clean" else "exit 1\n"),
                    encoding="utf-8",
                )
                helper.chmod(0o700)
                git("config", f"filter.probe.{filter_command}", str(helper))

                with self.assertRaises(module.EvaluationError):
                    module._validate_clean_repo(repo)
                self.assertFalse(marker.exists(), f"repository filter.{filter_command} helper must not execute")

    def test_git_snapshot_refuses_repo_external_diff_before_helper_execution(self):
        import os
        import shlex
        import subprocess
        import tempfile

        module = load_module("review_model_evaluation_git_boundary_diff", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            from pathlib import Path

            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            env = {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "LC_ALL": "C",
            }

            def git(*args):
                return subprocess.check_output(["git", "-C", str(repo), *args], env=env, text=True).strip()

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
            marker = root / "external-diff.marker"
            helper = repo / ".git" / "external-diff-helper.sh"
            helper.write_text(f"#!/bin/sh\nprintf marker > {shlex.quote(str(marker))}\nexit 1\n", encoding="utf-8")
            helper.chmod(0o700)
            git("config", "diff.external", str(helper))

            with self.assertRaises(module.EvaluationError):
                module.build_source_packet(repo, revision, base, {})
            self.assertFalse(marker.exists(), "repository external diff helper must not execute")

    def test_git_snapshot_refuses_repo_archive_command_before_helper_execution(self):
        import os
        import shlex
        import subprocess
        import tempfile

        module = load_module("review_model_evaluation_git_boundary_archive", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            from pathlib import Path

            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            env = {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "LC_ALL": "C",
            }

            def git(*args):
                return subprocess.check_output(["git", "-C", str(repo), *args], env=env, text=True).strip()

            git("init", "-q")
            git("config", "user.email", "tests@example.invalid")
            git("config", "user.name", "tests")
            (repo / "app.py").write_text("print(1)\n", encoding="utf-8")
            git("add", "app.py")
            git("commit", "-qm", "base")
            revision = git("rev-parse", "HEAD")
            marker = root / "archive-command.marker"
            helper = repo / ".git" / "archive-command-helper.sh"
            helper.write_text(f"#!/bin/sh\nprintf marker > {shlex.quote(str(marker))}\nexit 1\n", encoding="utf-8")
            helper.chmod(0o700)
            git("config", "tar.tar.command", str(helper))

            with self.assertRaises(module.EvaluationError):
                module._archive_files(repo, revision, {})
            self.assertFalse(marker.exists(), "repository archive command must not execute")

    def test_clean_repo_snapshot_still_succeeds_without_repo_execution_config(self):
        import os
        import subprocess
        import tempfile

        module = load_module("review_model_evaluation_git_boundary_clean", "scripts/review_model_evaluation.py")
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            from pathlib import Path

            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            env = {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "LC_ALL": "C",
            }

            def git(*args):
                return subprocess.check_output(["git", "-C", str(repo), *args], env=env, text=True).strip()

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

            module._validate_clean_repo(repo)
            packet = module.build_source_packet(repo, revision, base, {})
            self.assertTrue(packet["complete"])

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
            "validation_verdict": "valid",
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
            "validation_verdict": "valid",
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
        self.assertEqual(argv[1:5], ["--print", "--safe-mode", "--restricted", "--disable-slash-commands"])
        self.assertEqual(json.loads(argv[argv.index("--json-schema") + 1]), schema)
        self.assertEqual(argv[argv.index("--tools") + 1], "")
        self.assertIn("--strict-mcp-config", argv)
        self.assertNotIn("--allowedTools", argv)

    def test_claude_parses_structured_output_and_does_not_trust_model_identity(self):
        module = load_module("review_model_evaluation_claude_parse", "scripts/review_model_evaluation.py")

        def runner(argv, payload, cwd, env, timeout, max_output):
            if len(argv) == 2 and argv[1] == "--version":
                return module.adapters.ProcessCapture(tuple(argv), 0, b"claude 1\n", b"")
            if len(argv) == 2 and argv[1] == "--help":
                return module.adapters.ProcessCapture(tuple(argv), 0, b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n", b"")
            return module.adapters.ProcessCapture(tuple(argv), 0, b'{"model":"provider-model","structured_output":{"item_id":"x","model_id":"model-authored"},"usage":{"input_tokens":1}}', b"")

        adapter = module.adapters.ClaudeAdapter("fake-claude", runner=runner)
        with tempfile.TemporaryDirectory() as temp:
            response = adapter.invoke({"role": "synthesis"}, {"type": "object"}, "requested-model", system_prompt="fixed", session_dir=Path(temp))
        self.assertEqual(response.structured_output["item_id"], "x")
        self.assertEqual(response.observed_model_id, "unavailable")
        self.assertEqual(response.actual_metadata["usage"]["input_tokens"], 1)

    def test_claude_event_array_uses_oauth_safe_flags_and_preserves_estimated_model_usage(self):
        module = load_module("review_model_evaluation_event_array", "scripts/review_model_evaluation.py")
        seen = {}

        def runner(argv, payload, cwd, env, timeout, max_output):
            if len(argv) == 2 and argv[1] == "--version":
                return module.adapters.ProcessCapture(tuple(argv), 0, b"claude 2.1.266\n", b"")
            if len(argv) == 2 and argv[1] == "--help":
                return module.adapters.ProcessCapture(
                    tuple(argv),
                    0,
                    (
                        b"--safe-mode --restricted --disable-slash-commands "
                        b"--no-session-persistence --output-format --json-schema --model "
                        b"--permission-mode --permission-prompts --tools --strict-mcp-config "
                        b"--setting-sources --system-prompt\n"
                    ),
                    b"",
                )
            seen["argv"] = tuple(argv)
            seen["env"] = dict(env or {})
            envelope = json.loads((ROOT / "tests/fixtures/model_evaluation/claude-event-array.json").read_text(encoding="utf-8"))
            return module.adapters.ProcessCapture(tuple(argv), 0, json.dumps(envelope).encode(), b"")

        adapter = module.adapters.ClaudeAdapter(
            "fake-claude",
            runner=runner,
            environment={"CLAUDE_CODE_OAUTH_TOKEN": "oauth", "HOME": "/tmp", "INJECTED": "must-not-pass"},
        )
        with tempfile.TemporaryDirectory() as temp:
            response = adapter.invoke(
                {"role": "judge_a"},
                {"type": "object"},
                "fixture-requested-model",
                system_prompt="fixed",
                session_dir=Path(temp),
            )

        self.assertEqual(response.structured_output["item_id"], "fixture-F1")
        self.assertEqual(response.observed_model_id, "fixture-backend-model")
        self.assertEqual(response.actual_metadata["model_usage"]["fixture-requested-model"]["costBasis"], "list")
        self.assertEqual(response.actual_metadata["estimated_cost_usd"], 0.03)
        self.assertEqual(response.actual_metadata["actual_cost_usd"], "unknown")
        self.assertIn("fixture-auxiliary-model", response.actual_metadata["auxiliary_model_usage"])
        self.assertNotIn("INJECTED", seen["env"])
        argv = list(seen["argv"])
        for flag in ("--safe-mode", "--restricted", "--disable-slash-commands", "--strict-mcp-config"):
            self.assertIn(flag, argv)
        self.assertEqual(argv[argv.index("--tools") + 1], "")
        self.assertEqual(argv[argv.index("--setting-sources") + 1], "")
        self.assertEqual(json.loads(argv[argv.index("--json-schema") + 1]), {"type": "object"})

    def test_direct_capture_bounds_streams_and_retains_partial_output_on_failure(self):
        module = load_module("review_model_evaluation_capture", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            cwd = Path(temp)
            overflow = module.adapters.run_direct(
                [sys.executable, "-c", "import sys; sys.stdout.write('x' * 1000); sys.stderr.write('err')"],
                b"",
                cwd,
                {"PATH": os.environ.get("PATH", ""), "UNSAFE": "not inherited"},
                2,
                64,
            )
            self.assertEqual(overflow.error, "output_limit")
            self.assertTrue(overflow.output_limited)
            self.assertFalse(overflow.stdout_complete)
            self.assertLessEqual(len(overflow.stdout), 64)
            self.assertIn(b"err", overflow.stderr)

        with tempfile.TemporaryDirectory() as temp:
            timeout = module.adapters.run_direct(
                [sys.executable, "-c", "import sys,time; print('partial', flush=True); time.sleep(2)"],
                b"",
                Path(temp),
                {"PATH": os.environ.get("PATH", "")},
                0.5,
                4096,
            )
            self.assertEqual(timeout.error, "timeout")
            self.assertTrue(timeout.timed_out)
            self.assertIn(b"partial", timeout.stdout)

    def test_print_only_source_reference_is_not_semantic_source_binding(self):
        module = load_module("review_model_evaluation_source_binding", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = self._git_repo(root)
            findings, evidence, models = self._inputs(root)
            calls = []

            def runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"fake\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return module.adapters.ProcessCapture(
                        tuple(argv),
                        0,
                        b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n",
                        b"",
                    )
                packet = json.loads(payload)
                calls.append(packet)
                role = packet["role"]
                if role == "validation":
                    output = {
                        "item_id": "F-1",
                        "files": [{"path": "generated/check.py", "utf8": "print('/source/app.py')\n"}],
                        "runs": [
                            {"name": "baseline", "argv": ["python3", "/work/generated/check.py"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": True}}, "evidence_ids": ["E-1"]},
                            {"name": "counterexample", "argv": ["python3", "/work/generated/check.py", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": False}}, "evidence_ids": ["E-1"]},
                        ],
                        "claim_observed": "claim",
                    }
                elif role in {"judge_a", "judge_b"}:
                    self.assertIn("validation", packet)
                    output = {
                        "finding_id": "F-1",
                        "verdict": "support",
                        "severity": "high",
                        "usefulness": "useful",
                        "citations": [{"path": "app.py", "start": 1, "end": 1, "evidence_id": "E-1"}],
                        "counterexample": "safe",
                        "validation_verdict": "unproven",
                    }
                else:
                    output = {"item_id": "F-1", "verdict": "unknown", "citations": [], "disagreement": "", "reason": "not source-bound"}
                return module.adapters.ProcessCapture(
                    tuple(argv),
                    0,
                    json.dumps({"type": "result", "subtype": "success", "structured_output": output}).encode(),
                    b"",
                )

            class FakeOCI:
                def preflight(self):
                    return {"status": "ready"}

                def execute(self, plan, *args, **kwargs):
                    return {
                        "status": "success",
                        "classification": "executed_reproduced",
                        "controls_passed": True,
                        "claim_present": True,
                        "runs": [
                            {"name": "baseline", "valid": True, "actual": {"name": "baseline", "exit": 0, "timeout": False, "observation": {"claim_present": True}}},
                            {"name": "counterexample", "valid": True, "actual": {"name": "counterexample", "exit": 0, "timeout": False, "observation": {"claim_present": False}}},
                        ],
                    }

            summary = module.run_evaluation(
                repo=repo,
                revision=revision,
                base=base,
                findings_path=findings,
                evidence_dir=evidence,
                models_path=models,
                environment_path=None,
                output=root / "out",
                adapter_runner=runner,
                oci_executor=FakeOCI(),
            )
            self.assertEqual(summary["validation_coverage"]["unproven"], 1)
            self.assertEqual(summary["model_assessment"]["scoreable_items"], 0)
            self.assertLess(
                [packet["role"] for packet in calls].index("validation"),
                [packet["role"] for packet in calls].index("judge_a"),
            )

    def test_complete_replay_uses_frozen_verifier_and_tampering_fails(self):
        module = load_module("review_model_evaluation_verifier", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = self._git_repo(root)
            findings, evidence, models = self._inputs(root)
            model_calls = []
            oci_calls = []

            def runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"fake\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return module.adapters.ProcessCapture(
                        tuple(argv),
                        0,
                        b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n",
                        b"",
                    )
                model_calls.append(json.loads(payload)["role"])
                packet = json.loads(payload)
                if packet["role"] == "validation":
                    output = {
                        "item_id": "F-1",
                        "files": [{"path": "generated/check.py", "utf8": "print(open('/source/app.py').read())\n"}],
                        "runs": [
                            {"name": "baseline", "argv": ["python3", "/work/generated/check.py"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": True}}, "evidence_ids": ["E-1"]},
                            {"name": "counterexample", "argv": ["python3", "/work/generated/check.py", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": False}}, "evidence_ids": ["E-1"]},
                        ],
                        "claim_observed": "claim",
                    }
                elif packet["role"] in {"judge_a", "judge_b"}:
                    output = {"finding_id": "F-1", "verdict": "support", "severity": "high", "usefulness": "useful", "citations": [{"path": "app.py", "start": 1, "end": 1, "evidence_id": "E-1"}], "counterexample": "safe", "validation_verdict": "valid"}
                elif packet["role"] == "discovery":
                    output = {"candidates": []}
                else:
                    output = {"item_id": "F-1", "verdict": "supported", "citations": [{"path": "app.py", "start": 1, "end": 1, "evidence_id": "E-1"}], "disagreement": "", "reason": "x"}
                return module.adapters.ProcessCapture(tuple(argv), 0, json.dumps({"type": "result", "subtype": "success", "structured_output": output}).encode(), b"")

            class FakeOCI:
                def preflight(self):
                    return {"status": "ready"}

                def execute(self, *args, **kwargs):
                    oci_calls.append(True)
                    return {"status": "success", "classification": "unproven", "controls_passed": True, "claim_present": True, "runs": [{"name": "baseline", "valid": True, "actual": {"name": "baseline", "exit": 0, "timeout": False, "observation": {"claim_present": True}}}, {"name": "counterexample", "valid": True, "actual": {"name": "counterexample", "exit": 0, "timeout": False, "observation": {"claim_present": False}}}]}

            out = root / "out"
            first = module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=out, adapter_runner=runner, oci_executor=FakeOCI())
            self.assertEqual(first["state"], "complete")
            calls_after_first = (len(model_calls), len(oci_calls))
            second = module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=out, adapter_runner=runner, oci_executor=FakeOCI())
            self.assertEqual(second, first)
            self.assertEqual((len(model_calls), len(oci_calls)), calls_after_first)
            self.assertEqual(module.verify_evaluation_artifacts(out), first)

            for relative in ("summary.json", "findings.json", "validation/F-1/plan.json"):
                original = (out / relative).read_bytes()
                (out / relative).write_bytes(original + b"\n")
                with self.assertRaises(Exception):
                    module.verify_evaluation_artifacts(out)
                (out / relative).write_bytes(original)

    def test_failed_discovery_and_all_failed_models_never_complete_or_score(self):
        module = load_module("review_model_evaluation_failures", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = self._git_repo(root)
            findings, evidence, models = self._inputs(root)

            def failed_runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"fake\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n", b"")
                return module.adapters.ProcessCapture(tuple(argv), 17, b"partial stdout", b"transport failed")

            class BlockedOCI:
                def preflight(self):
                    return {"status": "environment_blocked", "reason": "fixture"}

                def execute(self, *args, **kwargs):
                    raise AssertionError("blocked discovery must not execute OCI")

            summary = module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=root / "out", adapter_runner=failed_runner, oci_executor=BlockedOCI())
            self.assertIn(summary["state"], {"failed", "partial"})
            self.assertNotEqual(summary["state"], "complete")
            self.assertEqual(summary["model_assessment"]["scoreable_items"], 0)
            run = json.loads((root / "out" / "run.json").read_text(encoding="utf-8"))
            self.assertTrue(run["failed_invocations"])
            self.assertIn("partial stdout", (root / "out" / "invocations" / "judge_a-F-1" / "raw.stdout").read_text(encoding="utf-8"))

            empty_root = root / "empty-case"
            empty_root.mkdir()
            empty_repo, empty_base, empty_revision = self._git_repo(empty_root)
            empty_findings, empty_evidence, empty_models = self._inputs(empty_root)
            empty_findings.write_text(json.dumps({"schema_version": "review-model-findings/v1", "findings": []}), encoding="utf-8")
            empty_summary = module.run_evaluation(
                repo=empty_repo,
                revision=empty_revision,
                base=empty_base,
                findings_path=empty_findings,
                evidence_dir=empty_evidence,
                models_path=empty_models,
                environment_path=None,
                output=empty_root / "out",
                adapter_runner=failed_runner,
                oci_executor=BlockedOCI(),
            )
            self.assertEqual(empty_summary["stages"]["discovery"]["status"], "failed")
            self.assertNotEqual(empty_summary["state"], "complete")
            self.assertEqual(empty_summary["model_assessment"]["scoreable_items"], 0)

    def test_full_fake_journey_keeps_originals_omissions_controls_and_disagreement_separate(self):
        module = load_module("review_model_evaluation_full_journey", "scripts/review_model_evaluation.py")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo, base, revision = self._git_repo_with_files(root)
            findings = root / "findings.json"
            findings.write_text(
                json.dumps(
                    {
                        "schema_version": "review-model-findings/v1",
                        "findings": [
                            {
                                "finding_id": "F-1",
                                "title": "safe behavior",
                                "severity": "high",
                                "claim": "safe refutation",
                                "location": {"path": "app.py", "start_line": 1, "end_line": 1},
                                "evidence_ids": ["E-app"],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            evidence = root / "evidence"
            evidence.mkdir()
            evidence_rows = []
            for evidence_id, path in (
                ("E-app", "app.py"),
                ("E-other", "other.py"),
                ("E-print", "print.py"),
                ("E-comment", "comment.py"),
                ("E-disagreement", "disagreement.py"),
                ("E-blocked", "blocked.py"),
            ):
                text = f"captured {path}\n"
                evidence_rows.append({"id": evidence_id, "utf8": text, "sha256": hashlib.sha256(text.encode()).hexdigest(), "allowed_ranges": [{"path": path, "start": 1, "end": 1}]})
            (evidence / "manifest.json").write_text(json.dumps({"schema_version": "review-model-evidence/v1", "entries": evidence_rows}), encoding="utf-8")
            models = root / "models.json"
            models.write_text(json.dumps({"roles": {role: {"adapter": "claude", "model_id": "journey-" + role, "family": "journey", "strength": "strong", "executable": "fake-claude"} for role in module_roles()}}), encoding="utf-8")
            packets = []
            oci_calls = []

            def help_capture(argv):
                return module.adapters.ProcessCapture(
                    tuple(argv),
                    0,
                    b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n",
                    b"",
                )

            def runner(argv, payload, cwd, env, timeout, max_output):
                if len(argv) == 2 and argv[1] == "--version":
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"claude fixture\n", b"")
                if len(argv) == 2 and argv[1] == "--help":
                    return help_capture(argv)
                packet = json.loads(payload)
                packets.append(packet)
                role = packet["role"]
                if role == "discovery":
                    output = {
                        "candidates": [
                            {"claim": "real omission", "path": "other.py", "start": 1, "end": 1, "evidence_ids": ["E-other"]},
                            {"claim": "print-only control", "path": "print.py", "start": 1, "end": 1, "evidence_ids": ["E-print"]},
                            {"claim": "source-comment control", "path": "comment.py", "start": 1, "end": 1, "evidence_ids": ["E-comment"]},
                            {"claim": "disagreement", "path": "disagreement.py", "start": 1, "end": 1, "evidence_ids": ["E-disagreement"]},
                            {"claim": "blocked control", "path": "blocked.py", "start": 1, "end": 1, "evidence_ids": ["E-blocked"]},
                        ]
                    }
                elif role == "validation":
                    finding = packet["finding"]
                    claim = finding["claim"]
                    path = finding["location"]["path"]
                    if "print-only" in claim:
                        content = "print('/source/print.py')\n"
                    elif "source-comment" in claim:
                        content = "# /source/comment.py\nprint('constant')\n"
                    else:
                        content = f"from pathlib import Path\nprint(Path('/source/{path}').read_text())\n"
                    baseline = claim != "safe refutation"
                    output = {
                        "item_id": finding["finding_id"],
                        "files": [{"path": "generated/check.py", "utf8": content}],
                        "runs": [
                            {"name": "baseline", "argv": ["python3", "/work/generated/check.py"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": baseline}}, "evidence_ids": finding["evidence_ids"]},
                            {"name": "counterexample", "argv": ["python3", "/work/generated/check.py", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": not baseline}}, "evidence_ids": finding["evidence_ids"]},
                        ],
                        "claim_observed": claim,
                    }
                elif role in {"judge_a", "judge_b"}:
                    finding = packet["finding"]
                    claim = finding["claim"]
                    if claim == "safe refutation":
                        verdict = "refute"
                    elif claim == "disagreement":
                        verdict = "support" if role == "judge_a" else "refute"
                    else:
                        verdict = "support"
                    validation_verdict = "unproven" if "control" in claim else "valid"
                    output = {"finding_id": finding["finding_id"], "verdict": verdict, "severity": finding["severity"], "usefulness": "useful", "citations": [{"path": finding["location"]["path"], "start": 1, "end": 1, "evidence_id": finding["evidence_ids"][0]}], "counterexample": "safe", "validation_verdict": validation_verdict}
                    self.assertIn("validation", packet)
                    self.assertNotIn("discovery_candidate", packet)
                else:
                    finding = packet["finding"]
                    judgments = packet["judgments"]
                    claim = finding["claim"]
                    if claim == "safe refutation":
                        verdict = "refuted"
                    elif claim == "real omission":
                        verdict = "supported"
                    elif claim == "disagreement":
                        verdict = "unknown"
                    else:
                        verdict = "unknown"
                    output = {"item_id": finding["finding_id"], "verdict": verdict, "citations": [] if verdict == "unknown" else [{"path": finding["location"]["path"], "start": 1, "end": 1, "evidence_id": finding["evidence_ids"][0]}], "disagreement": "judge disagreement" if len({item["assessment"]["verdict"] for item in judgments}) > 1 else "", "reason": "fixture"}
                model_id = argv[argv.index("--model") + 1]
                envelope = [
                    {"type": "system", "subtype": "init", "tools": ["StructuredOutput"], "plugins": [], "skills": [], "mcp_servers": []},
                    {"type": "assistant", "message": {"model": "backend-" + model_id, "content": [{"type": "text", "text": "fixture"}]}},
                    {"type": "result", "subtype": "success", "structured_output": output, "modelUsage": {model_id: {"inputTokens": 1, "outputTokens": 1, "costUSD": 0.01, "costBasis": "list"}, "claude-haiku-4-5": {"inputTokens": 1, "outputTokens": 1, "costUSD": 0.001, "costBasis": "list"}}},
                ]
                return module.adapters.ProcessCapture(tuple(argv), 0, json.dumps(envelope).encode(), b"")

            class FakeOCI:
                def preflight(self):
                    return {"status": "ready"}

                def execute(self, plan, *args, **kwargs):
                    claim = plan["claim_observed"]
                    oci_calls.append(claim)
                    if claim == "blocked control":
                        return {"status": "environment_blocked", "classification": "environment_blocked", "controls_passed": False, "claim_present": None, "runs": []}
                    baseline = claim != "safe refutation"
                    return {"status": "success", "classification": "unproven", "controls_passed": True, "claim_present": baseline, "runs": [{"name": "baseline", "valid": True, "actual": {"name": "baseline", "exit": 0, "timeout": False, "observation": {"claim_present": baseline}, "stdout": "source", "stderr": "", "stdout_sha256": hashlib.sha256(b"source").hexdigest(), "stderr_sha256": hashlib.sha256(b"").hexdigest()}}, {"name": "counterexample", "valid": True, "actual": {"name": "counterexample", "exit": 0, "timeout": False, "observation": {"claim_present": not baseline}, "stdout": "safe", "stderr": "", "stdout_sha256": hashlib.sha256(b"safe").hexdigest(), "stderr_sha256": hashlib.sha256(b"").hexdigest()}}]}

            out = root / "out"
            summary = module.run_evaluation(repo=repo, revision=revision, base=base, findings_path=findings, evidence_dir=evidence, models_path=models, environment_path=None, output=out, adapter_runner=runner, oci_executor=FakeOCI())
            self.assertEqual(summary["state"], "partial")
            self.assertEqual(summary["model_assessment"]["scoreable_items"], 2)
            self.assertEqual(summary["model_assessment"]["refuted"], 1)
            self.assertEqual(summary["omissions"]["automatically_supported_omission"], 1)
            self.assertGreater(summary["model_assessment"]["disagreement_items"], 0)
            self.assertEqual(summary["validation_coverage"]["unproven"], 3)
            self.assertEqual(summary["validation_coverage"]["environment_blocked"], 1)
            findings_result = json.loads((out / "findings.json").read_text(encoding="utf-8"))
            self.assertIn("judges", findings_result["findings"][0])
            omissions_result = json.loads((out / "omissions.json").read_text(encoding="utf-8"))
            real = next(item for item in omissions_result["candidates"] if item["claim"] == "real omission")
            self.assertEqual(real["classification"], "executed_reproduced")
            self.assertTrue(real["scoreable"])
            self.assertEqual(len(oci_calls), 6)
            for item_id in {packet["finding"]["finding_id"] for packet in packets if packet["role"] == "judge_a"}:
                roles = [packet["role"] for packet in packets if packet.get("finding", {}).get("finding_id") == item_id]
                self.assertLess(roles.index("validation"), roles.index("judge_a"))


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
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n", b"")
                packet = json.loads(payload)
                if packet["role"] in {"judge_a", "judge_b"}:
                    output = {"finding_id": "F-1", "verdict": "support", "severity": "high", "usefulness": "useful", "citations": [{"path": "app.py", "start": 99, "end": 99, "evidence_id": "E-1"}], "counterexample": "none", "validation_verdict": "valid"}
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
                    return module.adapters.ProcessCapture(tuple(argv), 0, b"--safe-mode --restricted --disable-slash-commands --no-session-persistence --output-format --json-schema --model --tools --strict-mcp-config --setting-sources --permission-mode --permission-prompts --system-prompt\n", b"")
                calls.append(tuple(argv))
                packet = json.loads(payload)
                role = packet["role"]
                if role in {"judge_a", "judge_b"}:
                    finding = packet["finding"]
                    output = {"finding_id": finding["finding_id"], "verdict": "support", "severity": "high", "usefulness": "useful", "citations": [{"path": "app.py", "start": 1, "end": 1, "evidence_id": "E-1"}], "counterexample": "none", "validation_verdict": "valid"}
                elif role == "discovery": output = {"candidates": []}
                elif role == "validation": output = {"item_id": "F-1", "files": [{"path": "generated/check.py", "utf8": "open('/source/app.py').read()"}], "runs": [{"name": "baseline", "argv": ["python", "/work/generated/check.py"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": True}}, "evidence_ids": ["E-1"]}, {"name": "counterexample", "argv": ["python", "/work/generated/check.py", "safe"], "cwd": "/work", "expect": {"exit": 0, "observation": {"claim_present": False}}, "evidence_ids": ["E-1"]}], "claim_observed": "claim"}
                else: output = {"item_id": "F-1", "verdict": "supported", "citations": [{"path": "app.py", "start": 1, "end": 1, "evidence_id": "E-1"}], "disagreement": "", "reason": "x"}
                return module.adapters.ProcessCapture(tuple(argv), 0, json.dumps({"structured_output": output}).encode(), b"")

            class FakeOCI:
                def preflight(self): return {"status": "ready"}
                def execute(self, *args, **kwargs): return {"status": "success", "classification": "unproven", "controls_passed": True, "claim_present": True, "runs": [{"name": "baseline", "valid": True, "actual": {"name": "baseline", "exit": 0, "timeout": False, "observation": {"claim_present": True}}}, {"name": "counterexample", "valid": True, "actual": {"name": "counterexample", "exit": 0, "timeout": False, "observation": {"claim_present": False}}}]}

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
    def _git_repo_with_files(root):
        repo = root / "repo"
        repo.mkdir()

        def git(*args):
            return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()

        git("init", "-q")
        git("config", "user.email", "tests@example.invalid")
        git("config", "user.name", "tests")
        for name in ("app.py", "other.py", "print.py", "comment.py", "disagreement.py", "blocked.py"):
            (repo / name).write_text(f"print('{name}')\n", encoding="utf-8")
        git("add", "-A")
        git("commit", "-qm", "base")
        base = git("rev-parse", "HEAD")
        (repo / "app.py").write_text("print('changed app')\n", encoding="utf-8")
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
