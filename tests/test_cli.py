"""Operator commands must preserve exit codes, local execution, and private files."""
from contextlib import redirect_stderr
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("enfour_cli", ROOT / "scripts/_cli.py")
cli = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cli)


class Commands(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.root_patch = patch.object(cli, "ROOT", self.root)
        self.root_patch.start()
        self.env_patch = patch.dict(os.environ, {"DOCKER_HOST": "ssh://wrong-host", "DOCKER_CONTEXT": "wrong-host"})
        self.env_patch.start()

    def tearDown(self):
        self.env_patch.stop()
        self.root_patch.stop()
        self.temporary.cleanup()

    def file(self, name, content="fixture"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        return path

    def test_help_never_runs_docker_or_fetches_data(self):
        commands = [[], ["up"], ["connect"], ["cargo"], ["cli"], ["models"], ["clients"],
                    ["language"], ["check"], ["bench"], ["bench", "run"],
                    ["bench", "prepare", "locomo"], ["bench", "prepare", "scifact"],
                    ["bench", "score"], ["bench", "compare"], ["bench", "merge"], ["repo"], ["repo", "check"], ["repo", "export"], ["repo", "import"]]
        for command in commands:
            with self.subTest(command=command):
                result = subprocess.run([sys.executable, str(ROOT / "scripts/enfour"), *command, "--help"],
                                        cwd=self.root, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("usage:", result.stdout.lower())
        self.assertEqual(list(self.root.iterdir()), [])

    def test_unknown_command_is_usage_error(self):
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            cli.main(["unknwon"])
        self.assertEqual(error.exception.code, 2)

    @patch.object(cli.subprocess, "run")
    def test_up_checks_language_before_mutation(self, run):
        self.file("models/manifest.json")
        with redirect_stderr(io.StringIO()):
            self.assertEqual(cli.entry(["up"]), 1)
        run.assert_not_called()
        self.assertFalse((self.root / "state").exists())

    @patch.object(cli.subprocess, "run")
    def test_up_preserves_token_and_does_not_rebuild(self, run):
        for file in ("models/manifest.json", "language-private/dictionary.json"):
            self.file(file)
        token = self.file("state/access.token", "private-token")
        run.return_value.returncode = 0
        self.assertEqual(cli.main(["up"]), 0)
        self.assertEqual(token.read_text(), "private-token")
        run.assert_called_once()
        command = run.call_args.args[0]
        self.assertEqual(command[:3], ["docker", "--host", "unix:///var/run/docker.sock"])
        self.assertIn("--no-build", command)
        self.assertNotIn("build", command)
        self.assertTrue((self.root / "state/server.url").exists())

    @patch.object(cli.subprocess, "run")
    def test_failed_build_stops_startup(self, run):
        for file in ("models/manifest.json", "language-private/dictionary.json", "target/release/enfour-memory"):
            self.file(file)
        run.return_value.returncode = 37
        self.assertEqual(cli.main(["up", "--build"]), 37)
        run.assert_called_once()
        self.assertFalse((self.root / "state").exists())

    @patch.object(cli.subprocess, "run")
    def test_connect_passes_token_file_without_reading_secret(self, run):
        token = self.file("private token", "never-print-this")
        run.return_value.returncode = 23
        self.assertEqual(cli.main(["connect", "--token-file", str(token), "--url", "http://example.test:7463/mcp",
                                   "--minify", "none"]), 23)
        command = run.call_args.args[0]
        self.assertEqual(command[:3], cli.DOCKER)
        self.assertIn(f"type=bind,src={token},dst=/access.token,readonly", command)
        self.assertNotIn("never-print-this", str(run.call_args))
        self.assertEqual(command[-7:], ["--minify", "none", "connect", "--url", "http://example.test:7463/mcp", "--token-file", "/access.token"])

    @patch.object(cli.subprocess, "run")
    def test_cache_fallback_is_offline_and_failures_propagate(self, run):
        run.side_effect = [subprocess.CompletedProcess([], 1), subprocess.CompletedProcess([], 42)]
        self.assertEqual(cli.main(["cargo", "test", "--locked", "--offline"]), 42)
        command = run.call_args.args[0]
        self.assertEqual(command[:3], cli.DOCKER)
        self.assertIn("ENFOUR_CACHE_MODE=disk", command)
        self.assertEqual(command[command.index("--network") + 1], "none")
        self.assertEqual(command[-3:], ["test", "--locked", "--offline"])

    @patch.object(cli.subprocess, "run")
    def test_shared_sccache_is_used_when_present(self, run):
        run.return_value.returncode = 0
        self.assertEqual(cli.main(["cargo", "check", "--offline"]), 0)
        command = run.call_args.args[0]
        self.assertEqual(command[command.index("--network") + 1], "enfour-compiler-cache")
        self.assertNotIn("ENFOUR_CACHE_MODE=disk", command)

    @patch.object(cli.subprocess, "run")
    def test_language_benchmark_mounts_private_data_read_only(self, run):
        dictionary = self.file("private language/dictionary.json")
        run.return_value.returncode = 0
        self.assertEqual(cli.main(["bench", "language", "--language", str(dictionary)]), 0)
        command = run.call_args.args[0]
        self.assertIn(f"type=bind,src={dictionary},dst=/enfour-language.json,readonly", command)
        self.assertIn("ENFOUR_LANGUAGE=/enfour-language.json", command)
        self.assertEqual(command[-2:], ["--example", "language_bench"])

    @patch.object(cli.subprocess, "run")
    def test_native_options_are_forwarded_in_order(self, run):
        run.return_value.returncode = 2
        self.assertEqual(cli.main(["cli", "--", "--lexical-only", "backup", "/data/backup.sqlite"]), 2)
        self.assertEqual(run.call_args.args[0][-3:], ["--lexical-only", "backup", "/data/backup.sqlite"])

    def test_scoring_from_other_directory_keeps_relative_paths(self):
        queries = self.file("queries.jsonl", '{"id":"one","scope":"test"}\n')
        result = subprocess.run([sys.executable, str(ROOT / "scripts/enfour"), "bench", "merge",
                                 queries.name, "output.jsonl", "missing-shard.jsonl"],
                                cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing-shard.jsonl", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertFalse((self.root / "output.jsonl").exists())


class CargoCleanup(unittest.TestCase):
    def test_cargo_failure_survives_cache_cleanup_failure(self):
        import importlib.machinery
        loader = importlib.machinery.SourceFileLoader("cargo_cached", str(ROOT / "build/cargo-cached"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        with patch.dict(os.environ, {"RUSTC_WRAPPER": "sccache", "LOCAL_OWNER": ""}), \
                patch.object(module, "call", side_effect=[0, 42, 1, 0]) as run, \
                patch.object(sys, "argv", ["cargo-cached", "test", "--offline"]):
            self.assertEqual(module.main(), 42)
            self.assertEqual(run.call_args_list[1].args[0], ["cargo", "test", "--offline"])
            self.assertEqual(run.call_args_list[-1].args[0], ["sccache", "--stop-server"])


if __name__ == "__main__":
    unittest.main()
