#!/usr/bin/env python3
"""Check dashboard behavior with Chromium and a local fixture. No service or models needed."""
import argparse
import base64
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parent.parent


@contextmanager
def browser_pipe(binary, profile):
    incoming, requests = os.pipe()
    responses, outgoing = os.pipe()
    # A short Python child sets Chrome's required fd numbers without preexec_fn.
    launcher = ("import os,sys; a,b=map(int,sys.argv[1:3]); "
                "os.dup2(a,3,inheritable=True); os.dup2(b,4,inheritable=True); "
                "os.execvp(sys.argv[3],sys.argv[3:])")
    try:
        process = subprocess.Popen(
            [sys.executable, "-c", launcher, str(incoming), str(outgoing), binary,
             "--headless", "--password-store=basic", "--use-mock-keychain", "--disable-gpu",
             "--disable-background-networking", "--disable-component-update", "--no-first-run",
             "--disable-sync", "--no-proxy-server", "--remote-debugging-pipe",
             f"--user-data-dir={profile}", "about:blank"],
            pass_fds=(incoming, outgoing), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL)
    except BaseException:
        for fd in (incoming, requests, responses, outgoing):
            os.close(fd)
        raise
    os.close(incoming)
    os.close(outgoing)
    sequence, buffer = 0, b""

    def rpc(method, params=None, session=None):
        nonlocal sequence, buffer
        sequence += 1
        message = {"id": sequence, "method": method, "params": params or {}}
        if session:
            message["sessionId"] = session
        wire = memoryview(json.dumps(message).encode() + b"\0")
        while wire:
            wire = wire[os.write(requests, wire):]
        deadline = time.monotonic() + 30
        while True:
            while b"\0" in buffer:
                raw, buffer = buffer.split(b"\0", 1)
                result = json.loads(raw)
                if result.get("id") == sequence:
                    if "error" in result:
                        raise RuntimeError(f"Browser command failed: {method}: {result['error']}")
                    return result["result"]
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([responses], [], [], remaining)[0]:
                raise TimeoutError(f"Browser command timed out: {method}")
            block = os.read(responses, 65536)
            if not block:
                raise RuntimeError("Chromium closed its command pipe.")
            buffer += block

    try:
        yield rpc
    finally:
        os.close(requests)
        os.close(responses)
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        if self.path == "/":
            content, mime = (ROOT / "assets/index.html").read_bytes(), "text/html"
        elif self.headers.get("Authorization") != "Bearer fixture-token":
            self.send_error(401)
            return
        elif self.path == "/api/status":
            content = json.dumps({"scopes": ["repo:dashboard-test"], "active_memories": 2, "mode": "fixture"}).encode()
            mime = "application/json"
        elif self.path.startswith("/api/recall?"):
            time.sleep(0.1)
            content = json.dumps([{"memory": {"title": title, "kind": "fact", "revision": 1, "source": "fixture"},
                                   "excerpt": "Exact source evidence."}
                                  for title in ["SQLite is the single memory store", "Local model files"]]).encode()
            mime = "application/json"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        self.wfile.write(content)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--browser", default=os.environ.get("ENFOUR_BROWSER", "chromium"))
    parser.add_argument("--screenshot", type=Path, default=ROOT / "artifacts/dashboard.png")
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="enfour-browser-") as profile, browser_pipe(args.browser, profile) as rpc:
            target = rpc("Target.createTarget", {"url": "about:blank"})["targetId"]
            session = rpc("Target.attachToTarget", {"targetId": target, "flatten": True})["sessionId"]

            def page(method, params=None):
                return rpc(method, params, session)

            def evaluate(expression):
                result = page("Runtime.evaluate", {"expression": expression, "awaitPromise": True, "returnByValue": True})
                if "exceptionDetails" in result:
                    raise RuntimeError(str(result["exceptionDetails"]))
                return result["result"].get("value")

            page("Emulation.setDeviceMetricsOverride", {"width": 1040, "height": 1100, "deviceScaleFactor": 1, "mobile": False})
            page("Page.enable")
            page("Page.navigate", {"url": f"http://127.0.0.1:{server.server_port}"})
            for _ in range(100):
                if evaluate("Boolean(document.getElementById('token'))"):
                    break
                time.sleep(0.05)
            else:
                raise RuntimeError("Dashboard did not load.")
            result = evaluate("""(async()=>{
                document.getElementById('token').value='fixture-token';
                await document.getElementById('token').onchange();
                document.getElementById('query').value='Why did we choose SQLite?';
                const form=document.getElementById('search'),original=window.fetch;
                let requests=0;
                window.fetch=(...args)=>{if(String(args[0]).startsWith('/api/recall'))requests++;return original(...args);};
                const pending=form.onsubmit({preventDefault(){}});
                const blocked=[...form.elements].every(e=>e.disabled)&&form.getAttribute('aria-busy')==='true';
                await form.onsubmit({preventDefault(){}});await pending;window.fetch=original;
                return {blocked,requests,released:[...form.elements].every(e=>!e.disabled)&&form.getAttribute('aria-busy')==='false',
                    articles:document.querySelectorAll('article').length,title:document.querySelector('article h2')?.textContent};
            })()""")
            assert result == {"blocked": True, "requests": 1, "released": True, "articles": 2,
                              "title": "SQLite is the single memory store"}, result
            evaluate("document.getElementById('token').value='';document.getElementById('token').placeholder='Connected for this session';")
            screenshot = page("Page.captureScreenshot", {"format": "png"})
            args.screenshot.parent.mkdir(parents=True, exist_ok=True)
            args.screenshot.write_bytes(base64.b64decode(screenshot["data"]))
            print(json.dumps({"result": "PASS", **result, "screenshot": str(args.screenshot)}))
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
