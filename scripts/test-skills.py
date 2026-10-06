#!/usr/bin/env python3
"""Offline skills discovery and MCP isolation checks; optional real CLI install."""
import hashlib
import http.client
import itertools
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

binary = str(Path(os.environ.get("ENFOUR_BINARY", "target/debug/enfour-memory")).resolve())
root = "/.well-known/agent-skills"


def decode(body):
    if not body.lstrip().startswith(b"{"):
        body = next(line[6:] for line in body.splitlines() if line.startswith(b"data: ") and line[6:].strip().startswith(b"{"))
    return json.loads(body)


with tempfile.TemporaryDirectory() as temporary:
    state = Path(temporary)
    base = [binary, "--db", str(state / "memory.sqlite"), "--lexical-only"]
    token_file = state / "access.token"
    subprocess.run(base + ["init", "--token-file", str(token_file)], check=True, stdout=subprocess.DEVNULL)
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    origin = f"http://127.0.0.1:{port}"
    server = subprocess.Popen(base + ["serve", "--bind", f"127.0.0.1:{port}", "--token-file", str(token_file)])
    session = None
    ids = itertools.count(1)

    def request(path, payload=None, headers=None):
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        hs = {"Accept": "application/json, text/event-stream", "Content-Type": "application/json", "MCP-Protocol-Version": "2025-11-25"}
        if session:
            hs["Mcp-Session-Id"] = session
        hs.update(headers or {})
        try:
            conn.request("POST" if payload else "GET", path, json.dumps(payload) if payload else None, hs)
            response = conn.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            conn.close()

    def rpc(method, params=None, headers=None):
        return request("/mcp/skills", {"jsonrpc": "2.0", "id": next(ids), "method": method, "params": params or {}}, headers)

    try:
        for _ in range(100):
            try:
                if request("/healthz")[0] == 200:
                    break
            except OSError:
                pass
            time.sleep(.05)
        else:
            raise AssertionError("server not ready")
        status, headers, body = request(root + "/index.json")
        assert status == 200 and headers.get("x-content-type-options") == "nosniff"
        index = json.loads(body)
        assert index["$schema"].endswith("/0.2.0/schema.json")
        texts = {}
        for item in index["skills"]:
            status, _, text = request(root + "/" + item["url"])
            assert status == 200
            assert "sha256:" + hashlib.sha256(text).hexdigest() == item["digest"]
            assert text == (Path("skills") / item["name"] / "SKILL.md").read_bytes()
            texts[item["name"]] = text
        assert len(texts) == 2
        for path in ["/mcp", "/api/status", "/api/graph?scope=test", "/mcp/skills-evil", "/mcp/skills/extra", root + "/unknown/SKILL.md"]:
            assert request(path)[0] == 401, path
        for path in ["/mcp/skills", root + "/index.json"]:
            assert request(path, headers={"Origin": "https://evil.example"})[0] == 403
            assert request(path, headers={"Host": "evil.example"})[0] == 403
        status, headers, body = rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "skills-test", "version": "1"}})
        assert status == 200, body
        assert decode(body)["result"]["serverInfo"]["name"] == "Enfour Memory Skills"
        session = next(v for k, v in headers.items() if k.lower() == "mcp-session-id")
        assert request("/mcp/skills", {"jsonrpc": "2.0", "method": "notifications/initialized"})[0] == 202
        status, _, body = rpc("tools/list")
        assert status == 200
        assert [t["name"] for t in decode(body)["result"]["tools"]] == ["install_skills"]
        expected = None
        for mode in ["uglify-json", "none", "toon"]:
            status, _, body = rpc("tools/call", {"name": "install_skills", "arguments": {"source_url": origin}}, {"minify": mode})
            assert status == 200
            result = decode(body)["result"]
            assert not result.get("isError")
            text = result["content"][0]["text"]
            if mode != "toon":
                value = json.loads(text)
                assert value["skills"] == index["skills"]
                assert origin in value["command"] and "DISABLE_TELEMETRY=1" in value["command"]
                if expected is not None:
                    assert value == expected
                expected = value
            else:
                assert text.startswith("command:")
        assert rpc("tools/list", headers={"minify": "invalid"})[0] == 400
        for source in ["file:///tmp", origin + "/mcp/skills", "https://user:secret@example.com", origin + "?x=1", "https://example.com/#bad"]:
            _, _, body = rpc("tools/call", {"name": "install_skills", "arguments": {"source_url": source}})
            response = decode(body)
            assert "error" in response or response["result"].get("isError"), source
        _, _, body = rpc("tools/call", {"name": "recall", "arguments": {"scope": "test", "query": "secret"}})
        response = decode(body)
        assert "error" in response or response["result"].get("isError")
        # A public session must not bypass bearer authentication on memory routes.
        assert request("/mcp", {"jsonrpc": "2.0", "id": 999, "method": "tools/list"})[0] == 401
        cli = os.environ.get("ENFOUR_SKILLS_CLI")
        if cli:
            project = state / "project"
            project.mkdir()
            env = dict(os.environ, HOME=str(state / "home"), XDG_CONFIG_HOME=str(state / "config"), DISABLE_TELEMETRY="1")
            subprocess.run([cli, "add", origin, "--agent", "codex", "--skill", "*", "--yes"], cwd=project, env=env, check=True, timeout=60)
            for name, text in texts.items():
                assert (project / ".agents/skills" / name / "SKILL.md").read_bytes() == text
            print("Real skills CLI installation: passed")
        print("Skills discovery, digest, formats, URL validation and memory isolation: passed")
    finally:
        server.terminate()
        server.wait(timeout=10)
