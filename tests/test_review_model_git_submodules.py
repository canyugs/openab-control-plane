import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def load_module():
    path = ROOT / "scripts/review_model_evaluation.py"
    spec = importlib.util.spec_from_file_location("review_model_git_submodules", path)
    if spec is None or spec.loader is None:
        return None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class GitSubmoduleSnapshotBoundaryTests(unittest.TestCase):
    def test_initialized_submodule_filter_cannot_execute_during_clean_check(self):
        module = load_module()
        self.assertIsNotNone(module, "the evaluation controller must exist")
        for filter_command in ("clean", "process"):
            with self.subTest(filter_command=filter_command), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                parent, marker, _, _ = self._git_repo_with_submodule(root, filter_command)

                with self.assertRaisesRegex(module.EvaluationError, "submodule"):
                    module._validate_clean_repo(parent)
                self.assertFalse(marker.exists(), "nested filter helper must not execute")

    def test_submodule_revision_is_not_claimed_as_complete_source(self):
        module = load_module()
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            parent, marker, base, revision = self._git_repo_with_submodule(root, "clean")

            with self.assertRaisesRegex(module.EvaluationError, "submodule"):
                module.build_source_packet(parent, revision, base, {})
            self.assertFalse(marker.exists(), "nested filter helper must not execute")

    def test_normal_repository_snapshot_control_still_succeeds(self):
        module = load_module()
        self.assertIsNotNone(module, "the evaluation controller must exist")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / "repo"
            repo.mkdir()
            self._git(repo, "init", "-q")
            self._git(repo, "config", "user.email", "tests@example.invalid")
            self._git(repo, "config", "user.name", "tests")
            (repo / "app.py").write_text("print(1)\n", encoding="utf-8")
            self._git(repo, "add", "app.py")
            self._git(repo, "commit", "-qm", "base")
            base = self._git(repo, "rev-parse", "HEAD")
            (repo / "app.py").write_text("print(2)\n", encoding="utf-8")
            self._git(repo, "commit", "-qam", "revision")
            revision = self._git(repo, "rev-parse", "HEAD")

            module._validate_clean_repo(repo)
            packet = module.build_source_packet(repo, revision, base, {})
            self.assertTrue(packet["complete"])
            self.assertEqual(packet["omissions"], [])

            (repo / "app.py").write_text("dirty\n", encoding="utf-8")
            with self.assertRaises(module.EvaluationError):
                module._validate_clean_repo(repo)

    @classmethod
    def _git_repo_with_submodule(cls, root, filter_command):
        child_source = root / "child-source"
        parent = root / "parent"
        child_source.mkdir()
        parent.mkdir()

        cls._git(child_source, "init", "-q")
        cls._git(child_source, "config", "user.email", "tests@example.invalid")
        cls._git(child_source, "config", "user.name", "tests")
        (child_source / ".gitattributes").write_text("data.txt filter=probe\n", encoding="utf-8")
        (child_source / "data.txt").write_text("same bytes\n", encoding="utf-8")
        cls._git(child_source, "add", ".")
        cls._git(child_source, "commit", "-qm", "child")

        cls._git(parent, "init", "-q")
        cls._git(parent, "config", "user.email", "tests@example.invalid")
        cls._git(parent, "config", "user.name", "tests")
        (parent / "root.txt").write_text("root\n", encoding="utf-8")
        cls._git(parent, "add", "root.txt")
        cls._git(parent, "commit", "-qm", "parent base")
        base = cls._git(parent, "rev-parse", "HEAD")
        cls._git(parent, "-c", "protocol.file.allow=always", "submodule", "add", "-q", str(child_source), "nested")
        cls._git(parent, "commit", "-qam", "add nested")
        revision = cls._git(parent, "rev-parse", "HEAD")

        marker = root / f"{filter_command}.marker"
        helper = root / f"{filter_command}-helper.sh"
        if filter_command == "clean":
            body = "cat\n"
        else:
            body = "exit 1\n"
        helper.write_text(f"#!/bin/sh\nprintf marker > {marker}\n{body}", encoding="utf-8")
        helper.chmod(0o700)
        cls._git(parent / "nested", "config", f"filter.probe.{filter_command}", str(helper))
        os.utime(parent / "nested" / "data.txt", (1, 1))
        return parent, marker, base, revision

    @staticmethod
    def _git(cwd, *args):
        env = os.environ.copy()
        env.update(
            {
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_CONFIG_GLOBAL": os.devnull,
                "GIT_ATTR_NOSYSTEM": "1",
                "LC_ALL": "C",
            }
        )
        return subprocess.check_output(["git", "-C", str(cwd), *args], env=env, text=True).strip()


if __name__ == "__main__":
    unittest.main()
