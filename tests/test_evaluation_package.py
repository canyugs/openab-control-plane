import hashlib
import importlib.util
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.9/3.10 test hosts
    tomllib = None


ROOT = Path(__file__).resolve().parents[1]
PACKAGE_MODULES = (
    "review_model_evaluation.py",
    "review_model_adapters.py",
    "review_model_oci_executor.py",
    "review_round_weekly_report.py",
)
SOURCE_HASHES = {
    "review_model_evaluation.py": "ef6ca77e1466ebbada4d7d8c3c911c18934e673cafa62e8d39dded4d5a3f89bf",
    "review_model_adapters.py": "dac5dcebff8438ef49d1b437faf87eb5f6cc09759ef0656dcbf3c5504e8547e7",
    "review_model_oci_executor.py": "5fdcfff0a0ffa0714f0681949aba31d7c7b1494b02248fdb51bea798ecd2f73f",
    "review_round_weekly_report.py": "1493f337c6afb05921912a0712314859fdefa97f9929e640e650e4b60c453e29",
}
LAUNCHER = ROOT / "packaging/evaluation/run.sh"


def _offline_build_command(output: Path):
    if importlib.util.find_spec("build") is not None:
        return [
            sys.executable,
            "-m",
            "build",
            "--wheel",
            "--outdir",
            str(output),
            "--no-isolation",
        ]
    if importlib.util.find_spec("setuptools") is not None:
        return [
            sys.executable,
            "-m",
            "pip",
            "wheel",
            "--no-deps",
            "--no-build-isolation",
            "--wheel-dir",
            str(output),
            ".",
        ]
    return None


class EvaluationPackageTests(unittest.TestCase):
    def test_pyproject_declares_the_standalone_surface(self):
        if tomllib is None:
            raw = (ROOT / "pyproject.toml").read_text(encoding="utf-8")
            for expected in (
                'name = "ocp-review-eval"',
                'version = "0.1.0"',
                'requires-python = ">=3.9"',
                'ocp-review-eval = "review_model_evaluation:main"',
                'ocp-review-weekly = "review_round_weekly_report:main"',
                'license = "MIT"',
                'license-files = ["LICENSE"]',
                'platforms = ["Unix"]',
            ):
                self.assertIn(expected, raw)
            self.assertIn("dependencies = []", raw)
            return
        with (ROOT / "pyproject.toml").open("rb") as handle:
            config = tomllib.load(handle)

        project = config["project"]
        self.assertEqual(project["name"], "ocp-review-eval")
        self.assertEqual(project["version"], "0.1.0")
        self.assertEqual(project["requires-python"], ">=3.9")
        self.assertEqual(project["dependencies"], [])
        self.assertEqual(project["license"], "MIT")
        self.assertEqual(project["license-files"], ["LICENSE"])
        self.assertEqual(
            project["scripts"],
            {
                "ocp-review-eval": "review_model_evaluation:main",
                "ocp-review-weekly": "review_round_weekly_report:main",
            },
        )
        self.assertEqual(
            config["tool"]["setuptools"]["py-modules"],
            [module.removesuffix(".py") for module in PACKAGE_MODULES],
        )
        self.assertEqual(config["tool"]["setuptools"]["package-dir"], {"": "scripts"})
        self.assertEqual(config["tool"]["setuptools"]["platforms"], ["Unix"])

    def test_frozen_source_modules_remain_byte_identical(self):
        for module, expected in SOURCE_HASHES.items():
            actual = hashlib.sha256((ROOT / "scripts" / module).read_bytes()).hexdigest()
            self.assertEqual(actual, expected, module)

    def test_fresh_wheel_installs_outside_checkout_and_exposes_entrypoints(self):
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-package-") as temp:
            root = Path(temp)
            source = root / "source"
            source.mkdir()
            (source / "scripts").mkdir()
            for name in ("pyproject.toml", "MANIFEST.in", "LICENSE"):
                shutil.copy2(ROOT / name, source / name)
            for name in PACKAGE_MODULES:
                shutil.copy2(ROOT / "scripts" / name, source / "scripts" / name)
            dist = root / "dist"
            dist.mkdir()
            command = _offline_build_command(dist)
            if command is None:
                self.skipTest("offline wheel builder is not installed")
            build_environment = os.environ.copy()
            build_environment["PIP_NO_INDEX"] = "1"
            build_environment["PIP_DISABLE_PIP_VERSION_CHECK"] = "1"
            clean_environment = build_environment.copy()
            clean_environment.pop("PYTHONPATH", None)
            built = subprocess.run(
                command,
                cwd=source,
                env=build_environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(built.returncode, 0, built.stdout + built.stderr)
            wheels = sorted(dist.glob("ocp_review_eval-0.1.0-*.whl"))
            self.assertEqual(len(wheels), 1)
            with zipfile.ZipFile(wheels[0]) as archive:
                wheel_names = {Path(name).name for name in archive.namelist()}
            self.assertTrue(set(PACKAGE_MODULES).issubset(wheel_names))
            self.assertIn("LICENSE", wheel_names)

            venv = root / "venv"
            subprocess.run([sys.executable, "-m", "venv", str(venv)], check=True, capture_output=True, text=True)
            venv_python = venv / "bin" / "python"
            pip_install = subprocess.run(
                [str(venv_python), "-m", "pip", "install", "--no-index", "--no-deps", str(wheels[0])],
                cwd=root,
                env=clean_environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(pip_install.returncode, 0, pip_install.stdout + pip_install.stderr)
            for command_name in ("ocp-review-eval", "ocp-review-weekly"):
                executable = venv / "bin" / command_name
                help_run = subprocess.run(
                    [str(executable), "--help"],
                    cwd=root,
                    env=clean_environment,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(help_run.returncode, 0, help_run.stdout + help_run.stderr)
            imported = subprocess.run(
                [
                    str(venv_python),
                    "-c",
                    "import review_model_adapters, review_model_evaluation, review_model_oci_executor, review_round_weekly_report",
                ],
                cwd=root,
                env=clean_environment,
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(imported.returncode, 0, imported.stdout + imported.stderr)

    def test_launcher_forwards_identical_scratch_path_and_literal_socket_controls(self):
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-launcher-") as temp:
            root = Path(temp).resolve()
            inputs = root / "inputs"
            inputs.mkdir()
            repo = inputs / "repo"
            repo.mkdir()
            evidence = inputs / "evidence"
            evidence.mkdir()
            findings = inputs / "findings.json"
            findings.write_text("{}\n", encoding="utf-8")
            models = inputs / "models.json"
            models.write_text("{}\n", encoding="utf-8")
            scratch = root / "scratch"
            output = root / "output"
            capture = root / "docker-argv.txt"
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake_docker = fake_bin / "docker"
            fake_docker.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$@\" > \"$CAPTURE\"\n",
                encoding="utf-8",
            )
            fake_docker.chmod(0o755)
            socket_path = root / "docker.sock"
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                server.bind(str(socket_path))
            except PermissionError:
                server.close()
                candidates = [Path("/var/run/docker.sock"), Path("/run/docker.sock")]
                socket_path = next((candidate for candidate in candidates if candidate.is_socket()), None)
                if socket_path is None:
                    self.skipTest("the test sandbox has no usable Unix socket")
                server = None
            try:
                environment = os.environ.copy()
                environment["PATH"] = f"{fake_bin}{os.pathsep}{environment['PATH']}"
                environment["CAPTURE"] = str(capture)
                result = subprocess.run(
                    [
                        str(LAUNCHER),
                        "--scratch-dir",
                        str(scratch),
                        "--output-dir",
                        str(output),
                        "--repo",
                        str(repo),
                        "--revision",
                        "a" * 40,
                        "--base",
                        "b" * 40,
                        "--findings",
                        str(findings),
                        "--evidence",
                        str(evidence),
                        "--models",
                        str(models),
                        "--docker-socket",
                        str(socket_path),
                        "--docker-gid",
                        "0",
                        "--uid",
                        "1001",
                        "--gid",
                        "0",
                        "--image",
                        "ghcr.io/canyugs/ocp-review-eval:0.1.0",
                    ],
                    cwd=ROOT,
                    env=environment,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

                root_uid_args = list(result.args)
                root_uid_args[root_uid_args.index("--uid") + 1] = "0"
                root_uid = subprocess.run(
                    root_uid_args,
                    cwd=ROOT,
                    env=environment,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertNotEqual(root_uid.returncode, 0, root_uid.stdout + root_uid.stderr)
            finally:
                if server is not None:
                    server.close()

            argv = capture.read_text(encoding="utf-8").splitlines()
            self.assertIn(f"type=bind,src={scratch},dst={scratch}", argv)
            self.assertIn(f"type=bind,src={output},dst={output}", argv)
            self.assertIn(f"TMPDIR={scratch}", argv)
            self.assertIn(f"type=bind,src={socket_path},dst=/var/run/docker.sock", argv)
            self.assertIn("--user", argv)
            self.assertEqual(argv[argv.index("--user") + 1], "1001:0")
            self.assertIn("--group-add", argv)
            self.assertEqual(argv[argv.index("--group-add") + 1], "0")
            self.assertNotIn("--privileged", argv)
            self.assertNotIn("--network=host", argv)
            self.assertNotIn("--network", argv)
            image_index = argv.index("ghcr.io/canyugs/ocp-review-eval:0.1.0")
            self.assertEqual(argv[image_index + 1], "run")
            self.assertEqual(argv[image_index + 2 : image_index + 4], ["--repo", str(repo)])

    def test_launcher_rejects_shared_or_symlinked_directories_before_docker(self):
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-launcher-invalid-") as temp:
            root = Path(temp).resolve()
            shared = root / "shared"
            shared.mkdir()
            symlink = root / "symlink"
            symlink.symlink_to(shared, target_is_directory=True)
            common = [
                str(LAUNCHER),
                "--scratch-dir",
                str(shared),
                "--output-dir",
                str(shared),
                "--repo",
                str(root),
                "--revision",
                "a" * 40,
                "--base",
                "b" * 40,
                "--findings",
                str(root / "missing-findings.json"),
                "--evidence",
                str(root),
                "--models",
                str(root / "missing-models.json"),
            ]
            same = subprocess.run(common, capture_output=True, text=True, check=False)
            self.assertNotEqual(same.returncode, 0)

            unsafe_image = subprocess.run(
                common + ["--image", "--privileged"], capture_output=True, text=True, check=False
            )
            self.assertNotEqual(unsafe_image.returncode, 0)

            symlinked = list(common)
            symlinked[symlinked.index(str(shared))] = str(symlink)
            rejected = subprocess.run(symlinked, capture_output=True, text=True, check=False)
            self.assertNotEqual(rejected.returncode, 0)


if __name__ == "__main__":
    unittest.main()
