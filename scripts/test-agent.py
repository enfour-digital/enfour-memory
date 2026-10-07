#!/usr/bin/env python3
"""Check both MCP routes, private Git history, and repository exchange on a temporary server."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

argparse.ArgumentParser(description=__doc__).parse_args()
root = Path(__file__).resolve().parent.parent
binary = str(Path(os.environ.get("ENFOUR_BINARY", root / "target/debug/enfour-memory")).resolve())
with tempfile.TemporaryDirectory(prefix="enfour-agent-test-") as directory:
    state = Path(directory)
    base = [binary, "--db", str(state / "memory.sqlite"), "--lexical-only"]
    token_file = state / "access.token"
    subprocess.run([*base, "init", "--token-file", str(token_file)], check=True, stdout=subprocess.DEVNULL)
    token = token_file.read_text().strip()
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    server = subprocess.Popen([*base, "serve", "--bind", f"127.0.0.1:{port}", "--token-file", str(token_file)])
    sessions = {}
    counter = 0

    def request(method, path, body=None, headers=None, auth=True):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
        merged = {"Accept": "application/json, text/event-stream", "Content-Type": "application/json", "minify": "uglify-json"}
        if auth:
            merged["Authorization"] = "Bearer " + token
        merged.update(headers or {})
        connection.request(method, path, json.dumps(body) if body is not None else None, merged)
        response = connection.getresponse()
        data = response.read()
        status, response_headers = response.status, dict(response.getheaders())
        connection.close()
        if data.strip() and not data.lstrip().startswith((b"{", b"[")) and any(line.startswith(b"data: ") for line in data.splitlines()):
            data = next(line[6:] for line in data.splitlines() if line.startswith(b"data: ") and line[6:].lstrip().startswith(b"{"))
        return status, response_headers, json.loads(data) if data.strip().startswith((b"{", b"[")) else data

    def rpc(path, method, params, minify="uglify-json"):
        global counter
        counter += 1
        headers = {"minify": minify}
        if path in sessions:
            headers.update({"Mcp-Session-Id": sessions[path], "MCP-Protocol-Version": "2025-11-25"})
        code, returned, result = request("POST", path, {"jsonrpc": "2.0", "id": counter, "method": method, "params": params}, headers)
        assert code == 200, (code, result)
        session = next((v for k, v in returned.items() if k.lower() == "mcp-session-id"), None)
        if session:
            sessions[path] = session
        return result

    def tool(path, name, arguments, minify="uglify-json"):
        result = rpc(path, "tools/call", {"name": name, "arguments": arguments}, minify)
        assert "error" not in result, result
        assert not result["result"].get("isError"), result
        text = result["result"]["content"][0]["text"]
        return text if minify == "toon" else json.loads(text)

    try:
        for _ in range(100):
            try:
                if request("GET", "/healthz")[0] == 200:
                    break
            except OSError:
                time.sleep(.05)
        else:
            raise AssertionError("Temporary server did not start")
        for path in ["/mcp", "/mcp/rag", "/mcp/agent"]:
            assert request("POST", path, {}, auth=False)[0] == 401
            rpc(path, "initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "agent-test", "version": "1"}})
            request("POST", path, {"jsonrpc": "2.0", "method": "notifications/initialized"}, {"Mcp-Session-Id": sessions[path]})
        legacy = rpc("/mcp", "tools/list", {})["result"]["tools"]
        rag = rpc("/mcp/rag", "tools/list", {})["result"]["tools"]
        agent = rpc("/mcp/agent", "tools/list", {})["result"]["tools"]
        assert legacy == rag and len(rag) == 7
        assert len(agent) == 11
        scope = "repo:agent-wire"
        args = {"scope": scope, "key": "storage", "title": "Local storage", "content": "Use SQLite for local memory.",
                "kind": "decision", "source": "test://wire", "expected_revision": 0}
        saved = tool("/mcp/agent", "remember", args)
        for path in ["/mcp/rag", "/mcp/agent"]:
            assert tool(path, "recall", {"scope": scope, "query": "SQLite"})[0]["memory"]["id"] == saved["id"]
        view = tool("/mcp/agent", "export_memory_repo", {"scope": scope})
        assert view["files"][f"content/{saved['id']}.txt"] == args["content"]
        for format in ["uglify-json", "none", "toon"]:
            result = tool("/mcp/agent", "read_memory_file", {"scope": scope, "path": "MEMORY.md"}, format)
            assert result if format == "toon" else "## Index" in result["text"]
        assert tool("/mcp/agent", "validate_memory_repo", view)["accepted"]
        assert not tool("/mcp/agent", "validate_memory_repo", {"files": {"MEMORY.md": "## Index\n- [[missing]]\n"}})["accepted"]
        assert "error" in rpc("/mcp/rag", "tools/call", {"name": "read_memory_file", "arguments": {"scope": scope, "path": "MEMORY.md"}})
        for path in ["../access.token", f"notes/{saved['id']}.md"]:
            assert "error" in rpc("/mcp/agent", "tools/call", {"name": "read_memory_file", "arguments": {"scope": "repo:other", "path": path}})
        assert request("GET", "/api/memory-repo?scope=" + scope, auth=False)[0] == 401
        exported = state / "export"
        cli = [str(root / "scripts/enfour"), "repo"]
        url = f"http://127.0.0.1:{port}/mcp/agent"
        subprocess.run([*cli, "export", str(exported), "--scope", scope, "--url", url, "--token-file", str(token_file)], check=True, stdout=subprocess.DEVNULL)
        subprocess.run([*cli, "check", str(exported), "--binary", binary], check=True, stdout=subprocess.DEVNULL)
        subprocess.run([*cli, "import", str(exported), "--scope", scope, "--id", saved["id"], "--expected-revision", "1", "--url", url, "--token-file", str(token_file)], check=True, stdout=subprocess.DEVNULL)
        assert {record["revision"] for record in tool("/mcp/rag", "inspect", {"scope": scope, "id": saved["id"]})} == {1, 2}
        for _ in range(100):
            status = request("GET", "/api/status")[2]["agent_git"]
            if status["applied_event"] == status["queued_event"] and status["error"] is None:
                break
            time.sleep(.05)
        else:
            raise AssertionError(status)
        git_root = state / ".agent-memory" / hashlib.sha256(scope.encode()).hexdigest()
        assert (git_root / ".git").is_dir()
        assert (git_root / "content" / f"{saved['id']}.txt").read_text() == args["content"]
        count = subprocess.check_output(["git", "-C", str(git_root), "rev-list", "--count", "HEAD"], text=True)
        assert count.strip() == "2", count
        print("PASS: both routes, shared writes, output formats, format checks, scope isolation, Python exchange, and hidden Git commits")
    finally:
        server.terminate()
        server.wait(timeout=15)
