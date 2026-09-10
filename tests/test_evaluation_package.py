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


def _launcher_args(scratch, output, repo, evidence, findings, models, socket_path, auth_env_file=None):
    args = [
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
        str(os.getuid()),
        "--gid",
        "0",
    ]
    if auth_env_file is not None:
        args.extend(["--auth-env-file", str(auth_env_file)])
    return args


def _bind_test_socket(root):
    socket_path = root / "docker.sock"
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        server.bind(str(socket_path))
    except OSError:
        server.close()
        candidates = [Path("/var/run/docker.sock"), Path("/run/docker.sock")]
        socket_path = next((candidate for candidate in candidates if candidate.is_socket()), None)
        if socket_path is None:
            raise unittest.SkipTest("the test sandbox has no usable Unix socket")
        return None, socket_path
    return server, socket_path


def _install_fake_docker(root, body):
    fake_bin = root / "bin"
    fake_bin.mkdir()
    fake_docker = fake_bin / "docker"
    fake_docker.write_text("#!/bin/sh\n" + body, encoding="utf-8")
    fake_docker.chmod(0o755)
    environment = os.environ.copy()
    environment["PATH"] = f"{fake_bin}{os.pathsep}{environment['PATH']}"
    return environment


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
        if os.getuid() == 0:
            self.skipTest("launcher requires a non-root invoking UID")
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
                        str(os.getuid()),
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
            self.assertEqual(argv[argv.index("--user") + 1], f"{os.getuid()}:0")
            self.assertIn("--group-add", argv)
            self.assertEqual(argv[argv.index("--group-add") + 1], "0")
            self.assertNotIn("--privileged", argv)
            self.assertNotIn("--network=host", argv)
            self.assertNotIn("--network", argv)
            staged_repo = Path(argv[argv.index("--repo") + 1])
            staging_parent = staged_repo.parent
            self.assertEqual(staging_parent.parent, scratch)
            self.assertIn(
                f"type=bind,src={staging_parent},dst={staging_parent},readonly",
                argv,
            )
            self.assertNotIn(f"type=bind,src={repo},dst={repo},readonly", argv)
            image_index = argv.index("ghcr.io/canyugs/ocp-review-eval:0.1.0")
            self.assertEqual(argv[image_index + 1], "run")
            self.assertEqual(argv[image_index + 2 : image_index + 4], ["--repo", str(staged_repo)])

    def test_launcher_forwards_only_literal_supported_auth_values_without_argv_values(self):
        if os.getuid() == 0:
            self.skipTest("launcher requires a non-root invoking UID")
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-launcher-auth-") as temp:
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
            auth_env_file = inputs / "auth.env"
            auth_env_file.write_text(
                " \n"
                " # comments are ignored, including dummy-comment-value\n"
                "CLAUDE_CODE_OAUTH_TOKEN=dummy-oauth== literal $HOME `ticks`\n"
                "\n"
                "ANTHROPIC_API_KEY=dummy-api key=two\n"
                "ANTHROPIC_AUTH_TOKEN=\n",
                encoding="utf-8",
            )
            scratch = root / "scratch"
            output = root / "output"
            argv_capture = root / "docker-argv.txt"
            environment_capture = root / "docker-environment.txt"
            fake_docker = (
                "#!/bin/sh\n"
                "printf '%s\\n' \"$@\" > \"$ARGV_CAPTURE\"\n"
                "{\n"
                "  printf 'CLAUDE_CODE_OAUTH_TOKEN=%s\\n' \"${CLAUDE_CODE_OAUTH_TOKEN-<unset>}\"\n"
                "  printf 'ANTHROPIC_API_KEY=%s\\n' \"${ANTHROPIC_API_KEY-<unset>}\"\n"
                "  printf 'ANTHROPIC_AUTH_TOKEN=%s\\n' \"${ANTHROPIC_AUTH_TOKEN-<unset>}\"\n"
                "} > \"$ENVIRONMENT_CAPTURE\"\n"
            )
            environment = _install_fake_docker(root, fake_docker)
            environment["ARGV_CAPTURE"] = str(argv_capture)
            environment["ENVIRONMENT_CAPTURE"] = str(environment_capture)
            server, socket_path = _bind_test_socket(root)
            try:
                result = subprocess.run(
                    _launcher_args(
                        scratch,
                        output,
                        repo,
                        evidence,
                        findings,
                        models,
                        socket_path,
                        auth_env_file,
                    ),
                    cwd=ROOT,
                    env=environment,
                    capture_output=True,
                    text=True,
                    check=False,
                )
            finally:
                if server is not None:
                    server.close()

            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            forwarded = dict(
                line.split("=", 1)
                for line in environment_capture.read_text(encoding="utf-8").splitlines()
            )
            self.assertEqual(
                forwarded,
                {
                    "CLAUDE_CODE_OAUTH_TOKEN": "dummy-oauth== literal $HOME `ticks`",
                    "ANTHROPIC_API_KEY": "dummy-api key=two",
                    "ANTHROPIC_AUTH_TOKEN": "",
                },
            )

            argv = argv_capture.read_text(encoding="utf-8").splitlines()
            self.assertNotIn("--env-file", argv)
            for key in forwarded:
                env_indexes = [index for index, value in enumerate(argv) if value == "--env"]
                self.assertIn(key, [argv[index + 1] for index in env_indexes])
            for value in forwarded.values():
                if value:
                    self.assertNotIn(value, argv)
            self.assertNotIn(str(auth_env_file), argv)
            self.assertEqual(list(scratch.iterdir()), [])

    def test_launcher_rejects_invalid_auth_before_docker_or_storage_mutation(self):
        if os.getuid() == 0:
            self.skipTest("launcher requires a non-root invoking UID")
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-launcher-auth-invalid-") as temp:
            root = Path(temp).resolve()
            inputs = root / "inputs"
            inputs.mkdir()
            repo = inputs / "repo"
            repo.mkdir()
            (repo / "source.txt").write_text("source\n", encoding="utf-8")
            evidence = inputs / "evidence"
            evidence.mkdir()
            findings = inputs / "findings.json"
            findings.write_text("{}\n", encoding="utf-8")
            models = inputs / "models.json"
            models.write_text("{}\n", encoding="utf-8")
            auth_env_file = inputs / "auth.env"
            marker = root / "docker-invoked"
            fake_docker = "printf '%s\\n' invoked > \"$DOCKER_MARKER\"\nexit 99\n"
            environment = _install_fake_docker(root, fake_docker)
            environment["DOCKER_MARKER"] = str(marker)
            server, socket_path = _bind_test_socket(root)
            cases = (
                ("PYTHONPATH=dummy-pythonpath-value\n", "PYTHONPATH", "dummy-pythonpath-value"),
                ("PYTHONHOME=dummy-pythonhome-value\n", "PYTHONHOME", "dummy-pythonhome-value"),
                ("PATH=dummy-path-value\n", "PATH", "dummy-path-value"),
                ("DOCKER_HOST=dummy-docker-host-value\n", "DOCKER_HOST", "dummy-docker-host-value"),
                (
                    "GIT_CONFIG_GLOBAL=dummy-git-config-value\n",
                    "GIT_CONFIG_GLOBAL",
                    "dummy-git-config-value",
                ),
                ("GENERIC_UNSUPPORTED=dummy-generic-value\n", "GENERIC_UNSUPPORTED", "dummy-generic-value"),
                ("malformed dummy-record-value\n", "malformed", "dummy-record-value"),
                ("=dummy-empty-key-value\n", "malformed", "dummy-empty-key-value"),
                (
                    "CLAUDE_CODE_OAUTH_TOKEN=dummy-first-value\n"
                    "CLAUDE_CODE_OAUTH_TOKEN=dummy-second-value\n",
                    "duplicate",
                    "dummy-first-value",
                ),
            )
            try:
                for index, (contents, expected_error, secret_value) in enumerate(cases):
                    scratch = root / f"scratch-{index}"
                    output = root / f"output-{index}"
                    auth_env_file.write_text(contents, encoding="utf-8")
                    result = subprocess.run(
                        _launcher_args(
                            scratch,
                            output,
                            repo,
                            evidence,
                            findings,
                            models,
                            socket_path,
                            auth_env_file,
                        ),
                        cwd=ROOT,
                        env=environment,
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    self.assertNotEqual(result.returncode, 0, contents)
                    self.assertIn(expected_error, result.stderr, contents)
                    self.assertNotIn(secret_value, result.stderr, contents)
                    self.assertFalse(marker.exists(), contents)
                    self.assertFalse(scratch.exists(), contents)
                    self.assertFalse(output.exists(), contents)
            finally:
                if server is not None:
                    server.close()

    def test_launcher_stages_repository_bytes_and_symlinks_without_following_them(self):
        if os.getuid() == 0:
            self.skipTest("launcher requires a non-root invoking UID")
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-launcher-stage-") as temp:
            root = Path(temp).resolve()
            inputs = root / "inputs"
            inputs.mkdir()
            repo = inputs / "repo"
            repo.mkdir()
            (repo / "tracked.txt").write_text("staged repository bytes\n", encoding="utf-8")
            target = inputs / "symlink-target.txt"
            target.write_text("must not be copied through the symlink\n", encoding="utf-8")
            (repo / "linked.txt").symlink_to("../symlink-target.txt")
            evidence = inputs / "evidence"
            evidence.mkdir()
            findings = inputs / "findings.json"
            findings.write_text("{}\n", encoding="utf-8")
            models = inputs / "models.json"
            models.write_text("{}\n", encoding="utf-8")
            scratch = root / "scratch"
            output = root / "output"
            capture = root / "staged-repo.txt"
            fake_docker = (
                "repo=\"\"\n"
                "while [ \"$#\" -gt 0 ]; do\n"
                "  if [ \"$1\" = \"--repo\" ]; then repo=\"$2\"; shift 2; else shift; fi\n"
                "done\n"
                "printf '%s\\n' \"$repo\" > \"$CAPTURE\"\n"
                "test -f \"$repo/tracked.txt\" || exit 41\n"
                "test \"$(cat \"$repo/tracked.txt\")\" = 'staged repository bytes' || exit 42\n"
                "test -L \"$repo/linked.txt\" || exit 43\n"
                "test \"$(readlink \"$repo/linked.txt\")\" = '../symlink-target.txt' || exit 44\n"
                "exit 23\n"
            )
            environment = _install_fake_docker(root, fake_docker)
            environment["CAPTURE"] = str(capture)
            server, socket_path = _bind_test_socket(root)
            try:
                result = subprocess.run(
                    _launcher_args(scratch, output, repo, evidence, findings, models, socket_path),
                    cwd=ROOT,
                    env=environment,
                    capture_output=True,
                    text=True,
                    check=False,
                )
            finally:
                if server is not None:
                    server.close()

            self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
            staged_repo = Path(capture.read_text(encoding="utf-8").strip())
            self.assertEqual(staged_repo.parent.parent, scratch)
            self.assertFalse(staged_repo.parent.exists())
            self.assertTrue(scratch.is_dir())
            self.assertEqual(list(scratch.iterdir()), [])
            self.assertTrue((repo / "linked.txt").is_symlink())

    def test_launcher_cleans_staging_directory_after_success(self):
        if os.getuid() == 0:
            self.skipTest("launcher requires a non-root invoking UID")
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-launcher-success-") as temp:
            root = Path(temp).resolve()
            inputs = root / "inputs"
            inputs.mkdir()
            repo = inputs / "repo"
            repo.mkdir()
            (repo / "tracked.txt").write_text("success\n", encoding="utf-8")
            evidence = inputs / "evidence"
            evidence.mkdir()
            findings = inputs / "findings.json"
            findings.write_text("{}\n", encoding="utf-8")
            models = inputs / "models.json"
            models.write_text("{}\n", encoding="utf-8")
            scratch = root / "scratch"
            output = root / "output"
            capture = root / "staged-repo.txt"
            fake_docker = (
                "repo=\"\"\n"
                "while [ \"$#\" -gt 0 ]; do\n"
                "  if [ \"$1\" = \"--repo\" ]; then repo=\"$2\"; shift 2; else shift; fi\n"
                "done\n"
                "printf '%s\\n' \"$repo\" > \"$CAPTURE\"\n"
                "test -f \"$repo/tracked.txt\"\n"
                "exit 0\n"
            )
            environment = _install_fake_docker(root, fake_docker)
            environment["CAPTURE"] = str(capture)
            server, socket_path = _bind_test_socket(root)
            try:
                result = subprocess.run(
                    _launcher_args(scratch, output, repo, evidence, findings, models, socket_path),
                    cwd=ROOT,
                    env=environment,
                    capture_output=True,
                    text=True,
                    check=False,
                )
            finally:
                if server is not None:
                    server.close()

            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            staged_repo = Path(capture.read_text(encoding="utf-8").strip())
            self.assertFalse(staged_repo.parent.exists())
            self.assertTrue(scratch.is_dir())
            self.assertEqual(list(scratch.iterdir()), [])

    def test_launcher_rejects_uid_and_input_overlaps_before_docker_or_copy(self):
        with tempfile.TemporaryDirectory(prefix="ocp-review-eval-launcher-preflight-") as temp:
            root = Path(temp).resolve()
            inputs = root / "inputs"
            inputs.mkdir()
            repo = inputs / "repo"
            repo.mkdir()
            (repo / "tracked.txt").write_text("source\n", encoding="utf-8")
            evidence = inputs / "evidence"
            evidence.mkdir()
            findings = inputs / "findings.json"
            findings.write_text("{}\n", encoding="utf-8")
            models = inputs / "models.json"
            models.write_text("{}\n", encoding="utf-8")
            environment_file = inputs / "environment.json"
            environment_file.write_text("{}\n", encoding="utf-8")
            auth_env_file = inputs / "auth.env"
            auth_env_file.write_text("CLAUDE_CODE_OAUTH_TOKEN=not-used\n", encoding="utf-8")
            marker = root / "docker-invoked"
            fake_docker = "printf '%s\\n' invoked > \"$DOCKER_MARKER\"\nexit 99\n"
            environment = _install_fake_docker(root, fake_docker)
            environment["DOCKER_MARKER"] = str(marker)
            server, socket_path = _bind_test_socket(root)
            actual_uid = os.getuid()
            cases = (
                ("root UID", root / "scratch-root", root / "output-root", "0"),
                (
                    "mismatched UID",
                    root / "scratch-mismatch",
                    root / "output-mismatch",
                    str(actual_uid + 1),
                ),
                ("scratch inside repository", repo / "scratch", root / "output-repo", str(actual_uid)),
                ("output inside repository", root / "scratch-output-repo", repo / "output", str(actual_uid)),
                ("scratch overlaps evidence", evidence, root / "output-evidence", str(actual_uid)),
                ("output overlaps findings", root / "scratch-findings", findings, str(actual_uid)),
                ("scratch overlaps models", models, root / "output-models", str(actual_uid)),
                ("output overlaps environment", root / "scratch-environment", environment_file, str(actual_uid)),
                ("scratch overlaps authentication file", auth_env_file, root / "output-auth", str(actual_uid)),
            )
            try:
                for label, scratch, output, uid in cases:
                    args = _launcher_args(scratch, output, repo, evidence, findings, models, socket_path)
                    args[args.index("--uid") + 1] = uid
                    args.extend(["--environment", str(environment_file), "--auth-env-file", str(auth_env_file)])
                    result = subprocess.run(
                        args,
                        cwd=ROOT,
                        env=environment,
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    self.assertNotEqual(result.returncode, 0, label)
                    self.assertFalse(marker.exists(), label)
                    if scratch not in (evidence, models, auth_env_file):
                        self.assertFalse(scratch.exists(), label)
                    if output not in (findings, environment_file):
                        self.assertFalse(output.exists(), label)
            finally:
                if server is not None:
                    server.close()

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
