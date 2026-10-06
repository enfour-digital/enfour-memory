#!/usr/bin/env python3
"""Real stdio and HTTP MCP exchanges; run inside the builder, offline."""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request
import urllib.error

binary = str(Path(os.environ.get("ENFOUR_BINARY", "target/debug/enfour-memory")).resolve())
def decoded(response):
    assert "error" not in response, response
    result = response["result"]
    assert not result.get("isError", False), result
    return json.loads(result["content"][0]["text"]) if "content" in result else result

with tempfile.TemporaryDirectory() as state:
    db = str(Path(state) / "memory.db")
    base = [binary, "--db", db, "--lexical-only"]
    p = subprocess.Popen(base + ["stdio"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    seq = 0
    def rpc(method, params):
        global seq
        seq += 1
        p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": seq, "method": method, "params": params}) + "\n")
        p.stdin.flush()
        while line := p.stdout.readline():
            response = json.loads(line)
            if response.get("id") == seq: return response
        raise AssertionError("server closed without response")
    try:
        decoded(rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "enfour-e2e", "version": "1"}}))
        p.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n'); p.stdin.flush()
        tools = decoded(rpc("tools/list", {}))["tools"]
        assert {t["name"] for t in tools} == {"remember", "recall", "inspect", "forget", "relate", "graph"}
        def call(name, arguments): return decoded(rpc("tools/call", {"name": name, "arguments": arguments}))
        note = {"scope": "repo:test", "key": "database", "title": "Storage", "content": "Use SQLite WAL", "kind": "decision", "source": "test://user/1", "expected_revision": 0}
        saved = call("remember", note)
        assert call("recall", {"scope": "repo:test", "query": "SQLite"})[0]["memory"]["id"] == saved["id"]
        assert call("recall", {"scope": "repo:other", "query": "SQLite"}) == []
        conflict = rpc("tools/call", {"name": "remember", "arguments": note})
        assert "error" in conflict or conflict.get("result", {}).get("isError"), conflict
        assert len(call("inspect", {"scope": "repo:test", "id": saved["id"]})) == 1
        call("forget", {"scope": "repo:test", "id": saved["id"], "expected_revision": 1})
        assert call("graph", {"scope": "repo:test"})["nodes"] == []
    finally:
        p.stdin.close()
        try: p.wait(timeout=10)
        except subprocess.TimeoutExpired: p.kill(); p.wait(); raise
    token_file = str(Path(state) / "access.token")
    subprocess.run(base + ["init", "--token-file", token_file], check=True, stdout=subprocess.DEVNULL)
    token = Path(token_file).read_text().strip()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0)); port = sock.getsockname()[1]
    p = subprocess.Popen(base + ["serve", "--bind", f"127.0.0.1:{port}", "--token-file", token_file], stderr=subprocess.PIPE)
    def http(path, headers=None, data=None):
        request = urllib.request.Request(f"http://127.0.0.1:{port}{path}", headers=headers or {}, data=data)
        try:
            with urllib.request.urlopen(request, timeout=10) as r: return r.status, r.read()
        except urllib.error.HTTPError as e: return e.code, e.read()
    try:
        for _ in range(100):
            try:
                if http("/healthz")[0] == 200: break
            except OSError: pass
            time.sleep(.05)
        else: raise AssertionError("HTTP server not ready")
        auth = {"Authorization": "Bearer " + token}
        assert http("/api/status")[0] == 401
        assert http("/api/status", auth)[0] == 200
        assert http("/api/status", dict(auth, Origin="http://evil.invalid"))[0] == 403
        assert http("/api/status", dict(auth, Host="evil.invalid"))[0] == 403
        assert json.loads(http("/api/graph?scope=repo:test", auth)[1])["nodes"] == []
        headers = dict(auth, **{"Content-Type": "application/json", "Accept": "application/json, text/event-stream"})
        payload = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2026-07-28", "capabilities": {}, "clientInfo": {"name": "http-test", "version": "1"}}}
        code, body = http("/mcp", headers, json.dumps(payload).encode())
        assert code == 200, (code, body)
        if not body.lstrip().startswith(b"{"):
            body = next(line[6:] for line in body.splitlines() if line.startswith(b"data: ") and line[6:].strip().startswith(b"{"))
        decoded(json.loads(body))
        headers["MCP-Protocol-Version"] = "2026-07-28"
        headers["Mcp-Method"] = "tools/list"
        payload = {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}
        code, body = http("/mcp",headers,json.dumps(payload).encode())
        assert code == 200, (code,body)
        if not body.lstrip().startswith(b"{"):
            body = next(line[6:] for line in body.splitlines() if line.startswith(b"data: ") and line[6:].strip().startswith(b"{"))
        assert len(decoded(json.loads(body))["tools"]) == 6
        assert http("/mcp", headers, b"x" * 70000)[0] == 413
        connector = subprocess.Popen([binary,"connect","--url",f"http://127.0.0.1:{port}/mcp","--token-file",token_file],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
        try:
            def bridge_rpc(method, params, ident):
                connector.stdin.write(json.dumps({"jsonrpc":"2.0","id":ident,"method":method,"params":params})+"\n")
                connector.stdin.flush()
                while line := connector.stdout.readline():
                    response=json.loads(line)
                    if response.get("id")==ident: return decoded(response)
                raise AssertionError("connector closed without response")
            bridge_rpc("initialize",{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"bridge-test","version":"1"}},1)
            connector.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');connector.stdin.flush()
            assert len(bridge_rpc("tools/list",{},2)["tools"])==6
            assert bridge_rpc("tools/call",{"name":"graph","arguments":{"scope":"repo:test"}},3)["nodes"]==[]
            # Keep this connector alive across a server restart. The SDK must
            # recover its transport without forcing an agent to reconnect.
            p.send_signal(signal.SIGTERM)
            p.wait(timeout=10)
            p = subprocess.Popen(base + ["serve", "--bind", f"127.0.0.1:{port}", "--token-file", token_file], stderr=subprocess.PIPE)
            for _ in range(100):
                try:
                    if http("/healthz")[0] == 200: break
                except OSError: pass
                time.sleep(.05)
            else: raise AssertionError("restarted HTTP server not ready")
            assert len(bridge_rpc("tools/list",{},4)["tools"])==6
            assert bridge_rpc("tools/call",{"name":"graph","arguments":{"scope":"repo:test"}},5)["nodes"]==[]
        finally:
            connector.stdin.close()
            try: connector.wait(timeout=10)
            except subprocess.TimeoutExpired: connector.kill();connector.wait();raise
        assert connector.returncode==0
    finally:
        p.send_signal(signal.SIGTERM)
        try: p.wait(timeout=10)
        except subprocess.TimeoutExpired: p.kill(); p.wait(); raise
    print("PASS: stdio tools, HTTP initialize/list, SDK connector and restart recovery, scope isolation, stale writes, deletion, graph, authentication, host/origin and body limits")
