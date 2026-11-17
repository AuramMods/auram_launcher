"""Exercise signed macOS updates only in explicit, isolated QA app copies."""
import argparse
import functools
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument("old", type=Path)
parser.add_argument("new", type=Path)
parser.add_argument("session", type=Path)
args = parser.parse_args()
old, new, session = args.old.resolve(), args.new.resolve(), args.session.resolve()
old_info = json.loads((old / "build.json").read_text())
new_info = json.loads((new / "build.json").read_text())
if old_info["commit"] == new_info["commit"] or old_info["build"] >= new_info["build"]:
    raise RuntimeError("Use artifacts from two increasing, distinct real commits")
install = session / "isolated-install"
install.mkdir(parents=True, exist_ok=True)
app = install / "Auram.app"
if app.exists():
    shutil.rmtree(app)
shutil.copytree(old / "Auram.app", app)
data = session / "isolated-update-data"
data.mkdir(exist_ok=True)
log = data / "qa.log"
log.write_text("")
mode = {"valid": False}

class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/latest.json":
            signature = ((new if mode["valid"] else old) / "Auram.app.tar.gz.sig").read_text().strip()
            manifest = {"version": new_info["version"], "platforms": {"darwin-aarch64": {
                "url": f"http://127.0.0.1:{self.server.server_port}/Auram.app.tar.gz", "signature": signature}}}
            body = json.dumps(manifest).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            super().do_GET()

    def log_message(self, *_):
        pass

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(new)))
threading.Thread(target=server.serve_forever, daemon=True).start()
env = os.environ.copy()
env["AURAM_QA_DATA_DIR"] = str(data)
env["AURAM_QA_UPDATE_URL"] = f"http://127.0.0.1:{server.server_port}/latest.json"

def events():
    return [json.loads(line[7:]) for line in log.read_text().splitlines() if line.startswith("QA_EVT ")]

def wait_for(predicate, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        entries = events()
        match = next((event for event in entries if predicate(event)), None)
        if match:
            return match
        time.sleep(0.25)
    raise RuntimeError("QA condition timed out: " + log.read_text())

def start():
    return subprocess.Popen([str(app / "Contents/MacOS/auram-launcher")], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)

def stop(process):
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)

process = None
try:
    process = start()
    failure = wait_for(lambda event: event["event"] == "updater_result" and event["status"] == "fail")
    if not any(word in failure["details"].lower() for word in ["signature", "verify", "verification"]):
        raise RuntimeError("Update failed before the signature check: " + failure["details"])
    stop(process)
    print('QA_EVT ' + json.dumps({"event": "invalid_signature_rejected", "status": "pass", "details": failure["details"]}), flush=True)
    mode["valid"] = True
    process = start()
    boot = wait_for(lambda event: event["event"] == "native_boot" and event["context"]["commit"] == new_info["commit"], seconds=60)
    print('QA_EVT ' + json.dumps({"event": "signed_update_restart", "status": "pass", "details": f"Build {old_info['build']} -> {new_info['build']}", "context": boot["context"]}), flush=True)
finally:
    if process:
        stop(process)
    server.shutdown()
