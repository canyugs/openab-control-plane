import copy
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
STRUCTURED_OUTPUT_FIXTURE = ROOT / "tests/fixtures/model_evaluation/claude-structured-tool-events.json"


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
