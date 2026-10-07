"""Python commands for the local Enfour checkout. No runtime dependencies."""
import argparse
import os
from pathlib import Path
import runpy
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
DOCKER = ["docker", "--host", "unix:///var/run/docker.sock"]
BUILDER = "enfour-memory-rust:1.99.0"


def run(arguments, *, cwd=ROOT, env=None):
    result = subprocess.run([str(arg) for arg in arguments], cwd=cwd, env=env)
    return result.returncode if result.returncode >= 0 else 128 - result.returncode


def require(path, instruction):
    if not path.is_file() or path.stat().st_size == 0:
        raise ValueError(f"Missing file: {path}\n{instruction}")


def compose(*arguments):
    env = dict(os.environ, ENFOUR_UID=str(os.getuid()), ENFOUR_GID=str(os.getgid()))
    return run([*DOCKER, "compose", "-p", "enfour-memory", *arguments], env=env)


def cargo(arguments, *, language=None):
    if arguments == ["--help"] or not arguments:
        print("Usage: scripts/enfour cargo image|fetch|COMMAND [ARGUMENTS]\n"
              "Build with Rust 1.99.0 and sccache on the local Linux host.\n"
              "Use image to build the compiler image. Use fetch to get locked dependencies.\n"
              "Example: scripts/enfour cargo build --release --locked --offline")
        return 0
    if arguments[0] == "image":
        if len(arguments) != 1:
            raise ValueError("The image command accepts no arguments.")
        return run([*DOCKER, "build", "--tag", BUILDER, "build"])
    common = [*DOCKER, "run", "--rm", "--cpus", "2", "--memory", "3g",
              "--env", f"LOCAL_OWNER={os.getuid()}:{os.getgid()}",
              "--mount", f"type=bind,src={ROOT},dst=/workspace",
              "--mount", "type=volume,src=enfour-cargo-downloads,dst=/cache/cargo"]
    if arguments[0] == "fetch":
        return run([*common, "--env", "RUSTC_WRAPPER=", BUILDER, "fetch", "--locked", *arguments[1:]])
    network = subprocess.run([*DOCKER, "network", "inspect", "enfour-compiler-cache"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    cache = (["--network", "enfour-compiler-cache"] if network.returncode == 0
             else ["--network", "none", "--env", "ENFOUR_CACHE_MODE=disk"])
    language_args = ["--env", "ENFOUR_LANGUAGE"]
    if language is not None:
        require(language, "Import the private dictionary first.")
        language_args = ["--mount", f"type=bind,src={language.resolve()},dst=/enfour-language.json,readonly",
                         "--env", "ENFOUR_LANGUAGE=/enfour-language.json"]
    return run([*common, *cache, "--env", "ENFOUR_MODELS", *language_args,
                "--mount", "type=volume,src=enfour-sccache,dst=/cache/sccache", BUILDER, *arguments])


def benchmark(arguments):
    binary = Path(os.environ.get("ENFOUR_BENCH_BINARY", "target/release/enfour-benchmark"))
    binary = (ROOT / binary).resolve()
    relative = binary.relative_to(ROOT)
    require(binary, "Build the benchmark binary first.")
    return run([*DOCKER, "run", "--rm", "--network", "none", "--cpus", "2",
                "--memory", os.environ.get("ENFOUR_BENCH_MEMORY", "3g"),
                "--user", f"{os.getuid()}:{os.getgid()}",
                "--mount", f"type=bind,src={ROOT},dst=/workspace",
                "--entrypoint", f"/workspace/{relative}", BUILDER, *arguments])


def helper(name, arguments, *, project_cwd=False):
    # Keep caller-relative file arguments intact. Wire checks use project fixtures.
    previous_argv, previous_cwd = sys.argv, Path.cwd()
    try:
        command = {"diagrams.py": "diagrams", "download-models": "models", "install-clients": "clients",
                   "score-retrieval": "bench score", "compare-retrieval": "bench compare",
                   "merge-benchmark": "bench merge", "prepare-locomo": "bench prepare locomo",
                   "prepare-scifact": "bench prepare scifact"}.get(name, name)
        sys.argv = [f"scripts/enfour {command}", *arguments]
        if project_cwd:
            os.chdir(ROOT)
        runpy.run_path(str(ROOT / "scripts" / name), run_name="__main__")
    finally:
        sys.argv = previous_argv
        os.chdir(previous_cwd)
    return 0


def main(arguments=None):
    arguments = list(sys.argv[1:] if arguments is None else arguments)
    # These commands have their own parsers. Forward options without reinterpretation.
    if arguments:
        command, rest = arguments[0], arguments[1:]
        if command == "cargo":
            return cargo(rest)
        if command == "cli":
            if not rest or rest == ["--help"]:
                print("Usage: scripts/enfour cli [--] NATIVE_ARGUMENTS\n"
                      "Run the Rust CLI in the checkout container. Paths refer to container mounts.\n"
                      "Use /data, /models, and /language. Use cli -- --help for native help.\n"
                      "Stop the service before migration or index changes.")
                return 0
            return compose("run", "--rm", "--no-deps", "-T", "memory", *(rest[1:] if rest[0] == "--" else rest))
        if command == "diagrams":
            return helper("diagrams.py", rest)
        if command in {"models", "clients"}:
            return helper({"models": "download-models", "clients": "install-clients"}[command], rest)
        if command == "bench" and rest:
            action, tail = rest[0], rest[1:]
            if action in {"score", "compare", "merge"}:
                return helper({"score": "score-retrieval", "compare": "compare-retrieval", "merge": "merge-benchmark"}[action], tail)
            if action == "prepare" and tail and tail[0] in {"locomo", "scifact"}:
                return helper(f"prepare-{tail[0]}", tail[1:])
            if action == "run":
                if not tail or tail == ["--help"]:
                    print("Usage: scripts/enfour bench run [--] BENCHMARK_ARGUMENTS\n"
                          "Run the benchmark on the local Linux host. Paths refer to `/workspace`.\n"
                          "Use `bench run -- --help` for native help.")
                    return 0
                return benchmark(tail[1:] if tail[0] == "--" else tail)
    parser = argparse.ArgumentParser(
        prog="scripts/enfour", description="Control the local Enfour checkout.",
        epilog="Python 3.11 or newer. Docker commands use the local Linux socket. "
               "For a NixOS service, use `systemctl`. The commands here control this checkout.")
    commands = parser.add_subparsers(dest="command", required=True)
    up = commands.add_parser("up", help="Start the checkout service.")
    up.add_argument("--build", action="store_true", help="Build the runtime image before startup.")
    for name, help_text in [("down", "Stop the checkout service."), ("status", "Show the container state.")]:
        commands.add_parser(name, help=help_text)
    logs = commands.add_parser("logs", help="Read service logs.")
    logs.add_argument("--tail", type=int, default=80)
    logs.add_argument("--follow", action="store_true")
    connect = commands.add_parser("connect", help="Connect a stdio client to the resident server.")
    connect.add_argument("--url", default=os.environ.get("ENFOUR_URL"))
    connect.add_argument("--token-file", type=Path, default=ROOT / "state/access.token")
    connect.add_argument("--minify", choices=("toon", "uglify-json", "none"), default="toon")
    language = commands.add_parser("language", help="Import the private Issue 9 dictionary.")
    language.add_argument("--pdf", type=Path, required=True)
    language.add_argument("--poppler", default="pdftotext")
    language.add_argument("--output", type=Path, default=ROOT / "language-private/dictionary.json")
    for name, help_text in [("cargo", "Build or test Rust with the compiler cache."),
                            ("cli", "Run a native command in the checkout container."),
                            ("models", "Fetch and verify local model files."),
                            ("clients", "Configure a local client with a file token.")]:
        commands.add_parser(name, help=help_text)
    commands.add_parser("diagrams", help="Create or check the README graphs.")
    check = commands.add_parser("check", help="Run one group of checks.")
    check.add_argument("suite", choices=("helpers", "product", "mcp", "output", "skills", "dashboard"))
    check.add_argument("--language", type=Path, default=Path(os.environ.get("ENFOUR_LANGUAGE", ROOT / "language-private/dictionary.json")))
    check.add_argument("--browser", help="Chromium command for dashboard checks.")
    bench = commands.add_parser("bench", help="Prepare data or run a selected benchmark.")
    actions = bench.add_subparsers(dest="action", required=True)
    prepare = actions.add_parser("prepare", help="Fetch a pinned dataset.")
    prepare.add_argument("dataset", choices=("locomo", "scifact"))
    for name, help_text in [("run", "Run a retrieval benchmark."), ("score", "Score a completed run."),
                            ("compare", "Compare two runs."), ("merge", "Merge query groups."),
                            ("output", "Measure result encoding."), ("language", "Measure writing checks.")]:
        action = actions.add_parser(name, help=help_text)
        if name == "language":
            action.add_argument("--language", type=Path, default=Path(os.environ.get("ENFOUR_LANGUAGE", ROOT / "language-private/dictionary.json")))
    args = parser.parse_args(arguments)
    if args.command == "up":
        for path, instruction in [(ROOT / "models/manifest.json", "Run scripts/enfour models."),
                                  (ROOT / "language-private/dictionary.json", "Run scripts/enfour language --help.")]:
            require(path, instruction)
        if args.build:
            require(ROOT / "target/release/enfour-memory", "Build the release binary first.")
            status = compose("build")
            if status:
                return status
        state = ROOT / "state"
        state.mkdir(mode=0o700, exist_ok=True)
        if not (state / "access.token").exists():
            status = compose("run", "--rm", "--no-deps", "-T", "memory", "init", "--token-file", "/data/access.token")
            if status:
                return status
        status = compose("up", "-d", "--wait", "--no-build")
        if status == 0 and (os.environ.get("ENFOUR_URL") or not (state / "server.url").exists()):
            (state / "server.url").write_text(os.environ.get("ENFOUR_URL", "http://127.0.0.1:7463/mcp") + "\n")
        return status
    if args.command == "down":
        return compose("down")
    if args.command == "status":
        return compose("ps")
    if args.command == "logs":
        if args.tail < 0:
            parser.error("`--tail` must be zero or more")
        return compose("logs", "--tail", str(args.tail), *(["--follow"] if args.follow else []), "memory")
    if args.command == "connect":
        token = args.token_file.resolve()
        require(token, "Specify an existing access token file.")
        url = args.url
        if not url:
            saved = ROOT / "state/server.url"
            url = saved.read_text().strip() if saved.exists() else "http://127.0.0.1:7463/mcp"
        return run([*DOCKER, "run", "--rm", "-i", "--read-only", "--network", "host",
                    "--user", f"{os.getuid()}:{os.getgid()}", "--cap-drop", "ALL",
                    "--security-opt", "no-new-privileges:true", "--memory", "128m", "--cpus", "0.5",
                    "--pids-limit", "32", "--mount", f"type=bind,src={token},dst=/access.token,readonly",
                    "--entrypoint", "/usr/local/bin/enfour-memory", "enfour-memory:local",
                    "--minify", args.minify, "connect", "--url", url, "--token-file", "/access.token"])
    if args.command == "language":
        binary = ROOT / "target/release/enfour-language-import"
        require(binary, "Build the release binaries first.")
        args.output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        return run([binary, "--pdf", args.pdf.resolve(), "--poppler", args.poppler,
                    "--output", args.output.resolve()], cwd=Path.cwd())
    if args.command == "check":
        if args.suite != "dashboard" and args.browser:
            parser.error("--browser applies to dashboard checks")
        if args.suite == "helpers":
            return run([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-p", "test_*.py"])
        if args.suite == "product":
            status = run([ROOT / "target/debug/enfour-product-check", ROOT, args.language.resolve()])
            if status:
                return status
            return helper("check-python.py", ["--language", str(args.language.resolve())])
        tail = []
        if args.browser:
            tail += ["--browser", args.browser]
        return helper(f"test-{args.suite}.py", tail, project_cwd=True)
    if args.command == "bench":
        return cargo(["run", "--release", "--locked", "--offline", "--example", f"{args.action}_bench"],
                     language=args.language if args.action == "language" else None)
    return 0


def entry(arguments=None):
    try:
        return main(arguments)
    except KeyboardInterrupt:
        return 130
    except (OSError, ValueError, AssertionError, RuntimeError) as error:
        print(f"enfour: {error}", file=sys.stderr)
        return 1
