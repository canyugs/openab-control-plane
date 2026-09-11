import copy
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
STRUCTURED_OUTPUT_FIXTURE = ROOT / "tests/fixtures/model_evaluation/claude-structured-tool-events.json"
STRUCTURED_OUTPUT_RETRY_FIXTURE = ROOT / "tests/fixtures/model_evaluation/claude-structured-retry-events.json"


def load_module():
    path = ROOT / "scripts/review_model_adapters.py"
    spec = importlib.util.spec_from_file_location("review_model_adapters_auth", path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    sys.modules["review_model_adapters_auth"] = module
    spec.loader.exec_module(module)
    return module


class ReviewModelAdaptersAuthTests(unittest.TestCase):
    def _invoke_envelope(self, module, envelope):
        payload = json.dumps(envelope).encode("utf-8")

        def runner(argv, input_bytes, cwd, env, timeout, max_output):
            self.assertEqual(input_bytes, b"{\"packet\":\"fixture\"}")
            return module.ProcessCapture(tuple(argv), 0, payload, b"")

        adapter = module.ClaudeAdapter("fake-claude", runner=runner, environment={"HOME": "/tmp"})
        with tempfile.TemporaryDirectory() as temp:
            return adapter.invoke(
                {"packet": "fixture"},
                {"type": "object", "properties": {"ok": {"type": "boolean"}}},
                "claude-opus-5",
                system_prompt="Return the requested JSON only.",
                session_dir=Path(temp),
            )

    def test_captured_claude_structured_output_tool_envelope_parses(self):
        module = load_module()
        envelope = json.loads(STRUCTURED_OUTPUT_FIXTURE.read_text(encoding="utf-8"))

        response = self._invoke_envelope(module, envelope)

        self.assertEqual(response.structured_output, {"ok": True})
        self.assertEqual(response.observed_model_id, "claude-opus-5")
        self.assertEqual(response.actual_metadata["init"]["tools"], ["StructuredOutput"])
        self.assertEqual(response.actual_metadata["init"]["plugins"], [])
        self.assertEqual(response.actual_metadata["init"]["skills"], [])
        self.assertEqual(response.actual_metadata["init"]["mcp_servers"], [])
        self.assertIn("claude-haiku-4-5-20251001", response.actual_metadata["auxiliary_model_usage"])
        self.assertAlmostEqual(response.actual_metadata["estimated_cost_usd"], 0.013768)
        self.assertEqual(response.actual_metadata["actual_cost_usd"], "unknown")

    def test_captured_claude_structured_output_retry_envelope_retains_attempts(self):
        module = load_module()
        envelope = json.loads(STRUCTURED_OUTPUT_RETRY_FIXTURE.read_text(encoding="utf-8"))

        response = self._invoke_envelope(module, envelope)

        self.assertEqual(response.structured_output, envelope[-1]["structured_output"])
        self.assertEqual(response.attempts, 2)
        self.assertEqual(response.actual_metadata["num_turns"], 3)
        self.assertEqual(response.actual_metadata["structured_output_attempt_count"], 2)
        attempts = response.actual_metadata["structured_output_attempts"]
        self.assertEqual(
            [attempt["id"] for attempt in attempts],
            [
                "toolu_01FNtGkTfGABNLzKbjDm2VBq",
                "toolu_015sizqVidAWXi7ykf4gHkLP",
            ],
        )
        self.assertEqual(attempts[0]["status"], "error")
        self.assertTrue(attempts[0]["result"]["is_error"])
        self.assertIn("schema", attempts[0]["result"]["content"])
        self.assertEqual(attempts[1]["status"], "success")
        self.assertEqual(attempts[1]["call"]["input"], response.structured_output)
        self.assertEqual(attempts[1]["result"]["content"], "Structured output provided successfully")
        self.assertEqual(json.loads(response.raw_stdout), envelope)

    def test_structured_output_envelope_rejects_unapproved_tools_and_bad_result_bindings(self):
        module = load_module()
        original = json.loads(STRUCTURED_OUTPUT_FIXTURE.read_text(encoding="utf-8"))

        cases = {
            "bash": {"type": "tool_use", "id": "toolu-bash", "name": "Bash", "input": {}},
            "read": {"type": "tool_use", "id": "toolu-read", "name": "Read", "input": {}},
            "file": {"type": "tool_use", "id": "toolu-write", "name": "Write", "input": {}},
            "mcp": {"type": "tool_use", "id": "toolu-mcp", "name": "mcp__server__action", "input": {}},
        }
        for name, block in cases.items():
            with self.subTest(name=name):
                envelope = copy.deepcopy(original)
                envelope[5]["message"]["content"].append(block)
                with self.assertRaises(module.AdapterError):
                    self._invoke_envelope(module, envelope)

        top_level = copy.deepcopy(original)
        top_level.insert(5, {"type": "tool_use", "id": "toolu-top-level", "name": "StructuredOutput", "input": {}})
        with self.assertRaises(module.AdapterError):
            self._invoke_envelope(module, top_level)

        unmatched = copy.deepcopy(original)
        unmatched[6]["message"]["content"][0]["tool_use_id"] = "toolu-unmatched"
        with self.assertRaises(module.AdapterError):
            self._invoke_envelope(module, unmatched)

        duplicate_result = copy.deepcopy(original)
        duplicate_result.insert(7, copy.deepcopy(duplicate_result[6]))
        with self.assertRaises(module.AdapterError):
            self._invoke_envelope(module, duplicate_result)

        conflicting_result = copy.deepcopy(original)
        conflicting = copy.deepcopy(conflicting_result[6])
        conflicting["message"]["content"][0]["tool_use_id"] = "toolu-other"
        conflicting_result.insert(7, conflicting)
        with self.assertRaises(module.AdapterError):
            self._invoke_envelope(module, conflicting_result)

        duplicate_call = copy.deepcopy(original)
        duplicate_call[5]["message"]["content"].append(copy.deepcopy(duplicate_call[5]["message"]["content"][0]))
        with self.assertRaises(module.AdapterError):
            self._invoke_envelope(module, duplicate_call)

    def test_structured_output_retry_envelope_rejects_bad_attempt_order_and_completion(self):
        module = load_module()
        original = json.loads(STRUCTURED_OUTPUT_RETRY_FIXTURE.read_text(encoding="utf-8"))

        cases = {}

        unmatched = copy.deepcopy(original)
        unmatched[12]["message"]["content"][0]["tool_use_id"] = "toolu-unmatched"
        cases["unmatched result"] = unmatched

        duplicate_result = copy.deepcopy(original)
        duplicate_result.insert(13, copy.deepcopy(duplicate_result[12]))
        cases["duplicate result"] = duplicate_result

        duplicate_call = copy.deepcopy(original)
        duplicate_call.insert(12, copy.deepcopy(duplicate_call[11]))
        cases["duplicate call"] = duplicate_call

        result_before_call = copy.deepcopy(original)
        result_before_call.insert(11, copy.deepcopy(result_before_call[12]))
        cases["result before call"] = result_before_call

        missing_error_result = copy.deepcopy(original)
        del missing_error_result[12]
        cases["missing error result"] = missing_error_result

        missing_success_result = copy.deepcopy(original)
        del missing_success_result[15]
        cases["missing success result"] = missing_success_result

        success_after_final = copy.deepcopy(original)
        success_event = success_after_final.pop(15)
        success_after_final.append(success_event)
        cases["success after final"] = success_after_final

        mismatched_final_output = copy.deepcopy(original)
        mismatched_final_output[-1]["structured_output"] = {"not": "the final call"}
        cases["mismatched final output"] = mismatched_final_output

        final_error = copy.deepcopy(original)
        final_error[-1]["is_error"] = True
        cases["final error"] = final_error

        for name, envelope in cases.items():
            with self.subTest(name=name):
                with self.assertRaises(module.AdapterError):
                    self._invoke_envelope(module, envelope)

    def test_structured_output_retry_envelope_rejects_other_tools_and_metadata(self):
        module = load_module()
        original = json.loads(STRUCTURED_OUTPUT_RETRY_FIXTURE.read_text(encoding="utf-8"))

        cases = {}
        for name in ("Bash", "Read", "Write", "mcp__server__action"):
            envelope = copy.deepcopy(original)
            envelope[14]["message"]["content"][0]["name"] = name
            cases[f"nested {name}"] = envelope

        top_level_tool = copy.deepcopy(original)
        top_level_tool.insert(11, {"type": "tool_use", "id": "toolu-top-level", "name": "StructuredOutput", "input": {}})
        cases["top-level tool event"] = top_level_tool

        file_event = copy.deepcopy(original)
        file_event.insert(11, {"type": "file_write", "path": "untrusted.py"})
        cases["file event"] = file_event

        init_tool = copy.deepcopy(original)
        init_tool[0]["tools"].append("Bash")
        cases["init executable tool"] = init_tool

        for key, value in (("plugins", ["untrusted-plugin"]), ("skills", ["untrusted-skill"]), ("mcp_servers", ["untrusted-mcp"])):
            envelope = copy.deepcopy(original)
            envelope[0][key] = value
            cases[f"nonempty {key}"] = envelope

        for name, envelope in cases.items():
            with self.subTest(name=name):
                with self.assertRaises(module.AdapterError):
                    self._invoke_envelope(module, envelope)

    def test_structured_output_retry_envelope_is_bounded(self):
        module = load_module()
        original = json.loads(STRUCTURED_OUTPUT_RETRY_FIXTURE.read_text(encoding="utf-8"))
        envelope = copy.deepcopy(original)
        insert_at = 13
        for index in range(module.MAX_STRUCTURED_OUTPUT_ATTEMPTS - 1):
            call = copy.deepcopy(original[11])
            result = copy.deepcopy(original[12])
            tool_use_id = f"toolu-bound-{index}"
            call["message"]["content"][0]["id"] = tool_use_id
            result["message"]["content"][0]["tool_use_id"] = tool_use_id
            envelope[insert_at:insert_at] = [call, result]
            insert_at += 2

        with self.assertRaises(module.AdapterError):
            self._invoke_envelope(module, envelope)

    def test_safe_environment_and_run_direct_propagate_user_only(self):
        module = load_module()
        self.assertIsNotNone(module, "the review model adapter must exist")

        source = {
            "PATH": "/usr/bin:/bin",
            "USER": "host-runtime-user",
            "NODE_OPTIONS": "--require=/tmp/untrusted.js",
            "PYTHONPATH": "/tmp/untrusted",
            "LD_PRELOAD": "/tmp/untrusted.dylib",
            "DYLD_INSERT_LIBRARIES": "/tmp/untrusted.dylib",
            "LOGIN": "must-not-pass",
            "PROJECT_SETTING": "must-not-pass",
        }
        self.assertEqual(module.safe_environment(source), {"PATH": "/usr/bin:/bin", "USER": "host-runtime-user"})

        child_code = (
            "import json, os; "
            "keys = ['USER', 'NODE_OPTIONS', 'PYTHONPATH', 'LD_PRELOAD', "
            "'DYLD_INSERT_LIBRARIES', 'LOGIN', 'PROJECT_SETTING']; "
            "print(json.dumps({key: os.environ.get(key) for key in keys}, sort_keys=True))"
        )
        with tempfile.TemporaryDirectory() as temp:
            capture = module.run_direct(
                [sys.executable, "-c", child_code],
                b"",
                Path(temp),
                source,
                5,
                4096,
            )

        self.assertEqual(capture.returncode, 0)
        self.assertEqual(
            json.loads(capture.stdout.decode("utf-8")),
            {
                "USER": "host-runtime-user",
                "NODE_OPTIONS": None,
                "PYTHONPATH": None,
                "LD_PRELOAD": None,
                "DYLD_INSERT_LIBRARIES": None,
                "LOGIN": None,
                "PROJECT_SETTING": None,
            },
        )


if __name__ == "__main__":
    unittest.main()
