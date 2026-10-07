#!/usr/bin/env python3
"""Check or export memory files. Import one reviewed Enfour record."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tempfile
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
LIMIT = 8 * 1024 * 1024


def git(root, *arguments):
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
    result = subprocess.run(["git", "-C", str(root), "-c", "core.hooksPath=/dev/null",
                             "-c", "core.fsmonitor=false", *arguments],
                            check=True, capture_output=True, text=True, env=env)
    return result.stdout.strip()


def read_files(root):
    files, total = {}, 0
    for directory, names, leaves in os.walk(root, followlinks=False):
        names[:] = [name for name in names if name != ".git"]
        for name in [*names, *leaves]:
            path = Path(directory) / name
            if path.is_symlink():
                raise ValueError(f"The repository contains a file link: {path}")
        for name in leaves:
            if name == ".git":
                continue
            path = Path(directory) / name
            if not path.is_file():
                raise ValueError(f"The repository contains a special file: {path}")
            total += path.stat().st_size
            if total > LIMIT or len(files) >= 4096:
                raise ValueError("The repository is above its file or byte limit.")
            relative = path.relative_to(root).as_posix()
            files[relative] = path.read_text() if path.suffix == ".md" else ""
    return {"files": files}


def check(root, binary):
    if Path(git(root, "rev-parse", "--show-toplevel")).resolve() != root.resolve():
        raise ValueError("The selected directory must be the Git repository root.")
    with tempfile.TemporaryDirectory(prefix="enfour-repo-check-") as directory:
        bundle = Path(directory) / "files.json"
        bundle.write_text(json.dumps(read_files(root), ensure_ascii=False))
        result = subprocess.run([str(binary.resolve()), "--minify", "uglify-json", "validate-repo", str(bundle)],
                                capture_output=True, text=True)
        if result.returncode not in (0, 2):
            raise ValueError(result.stderr.strip())
        report = json.loads(result.stdout)
    report["git_repository"] = True
    return report


class Client:
    def __init__(self, url, token):
        self.url = url.rstrip("/")
        self.headers = {"Authorization": "Bearer " + token.read_text().strip(),
                        "Accept": "application/json, text/event-stream", "Content-Type": "application/json", "minify": "uglify-json"}
        self.sequence = 0

    def request(self, method, url, data=None):
        payload = None if data is None else json.dumps(data).encode()
        with urllib.request.urlopen(urllib.request.Request(url, payload, self.headers, method=method), timeout=180) as response:
            session = response.headers.get("Mcp-Session-Id")
            if session:
                self.headers["Mcp-Session-Id"] = session
            raw = response.read(LIMIT * 2 + 1)
            if len(raw) > LIMIT * 2:
                raise ValueError("The server response is above its byte limit.")
            if not raw.strip():
                return None
            if not raw.lstrip().startswith(b"{"):
                raw = next(line[6:] for line in raw.splitlines() if line.startswith(b"data: ") and line[6:].lstrip().startswith(b"{"))
            result = json.loads(raw)
            if "error" in result:
                raise ValueError("The request failed.\n" + json.dumps(result["error"], indent=2))
            return result

    def rpc(self, method, params):
        self.sequence += 1
        return self.request("POST", self.url, {"jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params})["result"]

    def import_record(self, args):
        started = self.rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                           "clientInfo": {"name": "enfour-cli", "version": "0.6.0"}})
        self.headers["MCP-Protocol-Version"] = started["protocolVersion"]
        try:
            self.request("POST", self.url, {"jsonrpc": "2.0", "method": "notifications/initialized"})
            result = self.rpc("tools/call", {"name": "import_memory_record", "arguments": args})
            if result.get("isError"):
                raise ValueError(result["content"][0]["text"])
            return json.loads(result["content"][0]["text"])
        finally:
            if "Mcp-Session-Id" in self.headers:
                try:
                    self.request("DELETE", self.url)
                except OSError:
                    pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    check_parser = sub.add_parser("check", help="Check a Git memory repository.")
    check_parser.add_argument("path", type=Path)
    check_parser.add_argument("--binary", type=Path, default=Path(os.environ.get("ENFOUR_BINARY", ROOT / "target/release/enfour-memory")))
    for name in ["export", "import"]:
        command = sub.add_parser(name)
        command.add_argument("path", type=Path)
        command.add_argument("--scope", required=True)
        command.add_argument("--url", default=os.environ.get("ENFOUR_URL", "http://127.0.0.1:7463/mcp/agent"))
        command.add_argument("--token-file", type=Path, default=ROOT / "state/access.token")
        if name == "import":
            command.add_argument("--id", required=True)
            command.add_argument("--expected-revision", type=int, required=True)
    args = parser.parse_args()
    if args.action == "check":
        report = check(args.path, args.binary)
        print(json.dumps(report, indent=2))
        return 0 if report["accepted"] else 2
    client = Client(args.url, args.token_file)
    if args.action == "import":
        import uuid
        ident = str(uuid.UUID(args.id))
        result = client.import_record({"scope": args.scope, "metadata": json.loads((args.path / "records" / f"{ident}.json").read_text()),
                                       "content": (args.path / "content" / f"{ident}.txt").read_bytes().decode(),
                                       "expected_revision": args.expected_revision})
        print(json.dumps(result, indent=2))
        return 0
    destination = args.path.resolve()
    if destination.exists():
        raise ValueError("The export destination must not exist.")
    parsed = urllib.parse.urlsplit(args.url)
    url = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "/api/memory-repo", urllib.parse.urlencode({"scope": args.scope}), ""))
    result = client.request("GET", url)
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix=".enfour-export-", dir=destination.parent))
    try:
        total = 0
        for name, text in result["files"].items():
            path = PurePosixPath(name)
            if path.is_absolute() or any(part in {"..", ".", ".git"} for part in path.parts) or "\\" in name or ":" in name:
                raise ValueError("The export contains an invalid path.")
            total += len(text.encode())
            if total > LIMIT:
                raise ValueError("The export is above its byte limit.")
            target = temporary / path
            target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            with target.open("x", encoding="utf-8", newline="") as stream:
                stream.write(text)
            target.chmod(0o600)
        git(temporary, "init", "--quiet", "--template=", "--initial-branch=memory")
        git(temporary, "add", "--all")
        git(temporary, "-c", "user.name=Enfour Memory", "-c", "user.email=memory@enfour.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "Export memory snapshot")
        temporary.rename(destination)
    finally:
        if temporary.exists():
            shutil.rmtree(temporary)
    print(f"Memory repository exported: {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
