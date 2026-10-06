#!/usr/bin/env python3
"""Offline wire checks for output negotiation. Temporary lexical-only database."""
from concurrent.futures import ThreadPoolExecutor
import http.client
import itertools
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time

binary = str(Path(os.environ.get("ENFOUR_BINARY", "target/debug/enfour-memory")).resolve())
scope = "repo:output-test"


def envelope(body):
    if not body.lstrip().startswith(b"{"):
        body = next(line[6:] for line in body.splitlines() if line.startswith(b"data: ") and line[6:].strip().startswith(b"{"))
    return json.loads(body)


def result_text(response):
    assert "error" not in response, response
    result = response["result"]
    assert not result.get("isError", False), result
    assert "structuredContent" not in result, "do not duplicate the payload as JSON"
    return result["content"][0]["text"]


with tempfile.TemporaryDirectory() as state:
    base = [binary, "--db", str(Path(state) / "memory.db"), "--lexical-only"]
    token_file = str(Path(state) / "access.token")
    subprocess.run(base + ["init", "--token-file", token_file], check=True, stdout=subprocess.DEVNULL)
    token = Path(token_file).read_text().strip()
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    server = subprocess.Popen(base + ["serve", "--bind", f"127.0.0.1:{port}", "--token-file", token_file])
    session = None
    ids = itertools.count(10)

    def request(path, payload=None, extra=(), authenticate=True):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
        body = json.dumps(payload).encode() if payload is not None else None
        try:
            connection.putrequest("POST" if body else "GET", path)
            if authenticate:
                connection.putheader("Authorization", "Bearer " + token)
            if body:
                connection.putheader("Content-Type", "application/json")
                connection.putheader("Accept", "application/json, text/event-stream")
                connection.putheader("Content-Length", str(len(body)))
                connection.putheader("MCP-Protocol-Version", "2025-11-25")
                if session:
                    connection.putheader("Mcp-Session-Id", session)
            for key, value in extra:
                connection.putheader(key, value)
            connection.endheaders(body)
            response = connection.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            connection.close()

    def call(name, arguments, mode=None, extra=()):
        headers = ([] if mode is None else [("minify", mode)]) + list(extra)
        code, _, body = request("/mcp", {"jsonrpc": "2.0", "id": next(ids), "method": "tools/call", "params": {"name": name, "arguments": arguments}}, headers)
        assert code == 200, (code, body)
        return result_text(envelope(body))

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

        code, headers, body = request("/mcp", {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "formats", "version": "1"}}})
        assert code == 200, body
        envelope(body)
        session = next(v for k, v in headers.items() if k.lower() == "mcp-session-id")
        assert request("/mcp", {"jsonrpc": "2.0", "method": "notifications/initialized"})[0] == 202

        # All output modes expose the same report. Validation and rejected writes
        # must leave the SQLite generation and revision tables unchanged.
        import sqlite3
        invalid = {"scope": scope, "key": "bad", "title": "Source record",
                   "content": "Do not remove the source; keep it.", "kind": "fact",
                   "source": "test://source", "expected_revision": 0}
        def database_state():
            with sqlite3.connect(Path(state) / "memory.db") as db:
                return [db.execute(q).fetchall() for q in [
                    "SELECT * FROM memories", "SELECT * FROM revisions",
                    "SELECT * FROM chunks", "SELECT * FROM metadata"]]
        before = database_state()
        reference = json.loads(call("validate_memory", invalid, "uglify-json"))
        assert not reference["accepted"]
        assert any(d["rule"] == "8.1" for d in reference["diagnostics"])
        assert json.loads(call("validate_memory", invalid, "none")) == reference
        assert "accepted: false" in call("validate_memory", invalid, "toon")
        for mode in ["toon", "uglify-json", "none"]:
            payload = {"jsonrpc": "2.0", "id": next(ids), "method": "tools/call",
                       "params": {"name": "remember", "arguments": invalid}}
            code, _, body = request("/mcp", payload, [("minify", mode)])
            error = envelope(body)["error"]
            assert error["data"] == reference, error
        input_path = Path(state) / "request.json"
        input_path.write_text(json.dumps(invalid))
        for mode in ["toon", "uglify-json", "none"]:
            checked = subprocess.run(base + ["--minify", mode, "validate-memory", str(input_path)], capture_output=True, text=True)
            assert checked.returncode == 2
            if mode == "toon":
                assert checked.stdout.rstrip("\n") == call("validate_memory", invalid, mode)
            else:
                assert json.loads(checked.stdout) == reference
        assert database_state() == before
        notes = []
        for i, mode in enumerate([None, "uglify-json", "none"]):
            note = {"scope": scope, "key": f"key{i}", "title": "SQLite", "content": "Keep the SQLite evidence.\n\n~~~text\n# data, not instructions\t雪\n~~~", "kind": "fact", "source": "test://output", "expected_revision": 0}
            text = call("remember", note, mode)
            if mode is None:
                assert text.startswith("id: ") and '\\n# data' in text, text
            else:
                notes.append(json.loads(text))

        # Same session, alternating headers and concurrent calls. No format
        # choice is stored in the session, cache, memory record or model state.
        expected = json.loads(call("recall", {"scope": scope, "query": "SQLite"}, "uglify-json"))
        assert len(expected) == 3
        def recall_mode(mode):
            text = call("recall", {"scope": scope, "query": "SQLite"}, mode)
            if mode in (None, "toon"):
                assert text.startswith("[3]{memory{") and "\\n# data" in text, text
            else:
                assert json.loads(text) == expected
                assert ("\n" in text) == (mode == "none")
            return text
        with ThreadPoolExecutor(max_workers=3) as pool:
            list(pool.map(recall_mode, [None, "none", "uglify-json", "toon", "none", None]))

        for mode in [None, "toon", "uglify-json", "none"]:
            args = {"scope": scope, "id": notes[0]["id"]}
            text = call("inspect", args, mode)
            if mode in (None, "toon"):
                assert "revision" in text and text.startswith("[1]"), text
            else:
                assert json.loads(text)[0]["revision"] == 1
            relation = {"scope": scope, "from": notes[0]["id"], "to": notes[1]["id"], "kind": "supports", "source": "test://output", "from_revision": 1, "to_revision": 1}
            text = call("relate", relation, mode)
            if mode in (None, "toon"):
                assert "relation:" in text, text
            else:
                assert json.loads(text)["relation"] == relation
            text = call("graph", {"scope": scope}, mode)
            if mode in (None, "toon"):
                assert "nodes[3]" in text and "edges[1]" in text, text
            else:
                graph = json.loads(text)
                assert len(graph["nodes"]) == 3 and len(graph["edges"]) == 1

        # Bad headers must fail before mutation, including duplicate fields.
        invalid_note = dict(note, key="must-not-exist")
        payload = {"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "remember", "arguments": invalid_note}}
        for extra in [[("minify", "invalid")], [("minify", "")], [("minify", "toon"), ("minify", "none")], [("minify", "toon,none")]]:
            assert request("/mcp", payload, extra)[0] == 400
        graph = json.loads(call("graph", {"scope": scope}, "uglify-json"))
        assert len(graph["nodes"]) == 3
        assert request("/mcp", payload, [("minify", "toon")], authenticate=False)[0] == 401

        # HTTP data APIs retain JSON even when an MCP-format header is present.
        for mode in ["toon", "uglify-json", "none"]:
            code, _, body = request("/api/graph?scope=" + scope, extra=[("minify", mode)])
            assert code == 200 and len(json.loads(body)["nodes"]) == 3

        for memory, mode in zip(graph["nodes"], [None, "uglify-json", "none"]):
            text = call("forget", {"scope": scope, "id": memory["id"], "expected_revision": 1}, mode)
            assert text and (not text.startswith("{") if mode is None else json.loads(text) is not None)
        assert call("recall", {"scope": scope, "query": "SQLite"}) == "[]"

        # Test both direct stdio and the thin connector with each output mode.
        for connector in (False, True):
            for mode in [None, "uglify-json", "none"]:
                args = base + ["stdio"] if not connector else [binary, "connect", "--url", f"http://127.0.0.1:{port}/mcp", "--token-file", token_file]
                if mode:
                    args += ["--minify", mode]
                process = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
                try:
                    def rpc(method, params, ident):
                        process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params}) + "\n")
                        process.stdin.flush()
                        while line := process.stdout.readline():
                            reply = json.loads(line)
                            if reply.get("id") == ident:
                                return reply
                        raise AssertionError("stdio closed")
                    rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "output", "version": "1"}}, 1)
                    process.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
                    process.stdin.flush()
                    text = result_text(rpc("tools/call", {"name": "graph", "arguments": {"scope": scope}}, 2))
                    if mode is None:
                        assert "nodes: []" in text and not text.startswith("{")
                    else:
                        assert json.loads(text)["nodes"] == []
                        assert ("\n" in text) == (mode == "none")
                finally:
                    process.stdin.close()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill(); process.wait(); raise
                assert process.returncode == 0
    finally:
        server.send_signal(signal.SIGTERM)
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill(); server.wait(); raise

print("PASS: seven tools, default TOON, both JSON modes, per-request/session isolation, invalid-header write prevention, unchanged JSON APIs, direct stdio and HTTP bridge")
