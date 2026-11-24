"""iOS spike: drive the Tapwing driver on the iOS simulator over NDJSON RPC.

usage: python3 spike_client.py <udid> <bundle-id> <out-dir> [steps.json]
Launches the app (terminating it first), accepts the driver's dial-out on
127.0.0.1:7781 (the simulator shares the host loopback, no port forwarding),
dumps a snapshot and optionally runs RPC steps, timing each.
"""
import json
import socket
import subprocess
import sys
import time

udid, bundle, out = sys.argv[1], sys.argv[2], sys.argv[3]
steps = json.load(open(sys.argv[4])) if len(sys.argv) > 4 else []

subprocess.run(["xcrun", "simctl", "terminate", udid, bundle], capture_output=True)
srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 0))
srv.listen(4)
srv.settimeout(30)

port = srv.getsockname()[1]
data = subprocess.run(["xcrun", "simctl", "get_app_container", udid, bundle, "data"], capture_output=True, text=True).stdout.strip()
import os
os.makedirs(data + "/tmp", exist_ok=True)
open(data + "/tmp/tapwing_port", "w").write(str(port))
t0 = time.time()
launched = subprocess.run(["xcrun", "simctl", "launch", udid, bundle], check=True, capture_output=True, text=True)
pid = int(launched.stdout.strip().rsplit(":", 1)[1])
# Accept until the hello comes from the process we just launched: a dying
# previous instance may still redial (same issue as on Android).
while True:
    conn, _ = srv.accept()
    conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    f = conn.makefile("rwb")
    line = f.readline()
    if not line:
        continue
    hello = json.loads(line)
    if hello["params"].get("pid") == pid:
        break
    print("ignored hello from stale pid", hello["params"].get("pid"))
    conn.close()
print("hello after %.0f ms (pid %d): %s" % ((time.time() - t0) * 1000, pid, json.dumps(hello["params"])[:160]))

seq = 0


def call(method, params=None):
    global seq
    seq += 1
    f.write((json.dumps({"id": seq, "method": method, "params": params or {}}) + "\n").encode())
    f.flush()
    while True:
        msg = json.loads(f.readline())
        if msg.get("id") == seq:
            return msg


ts = time.time()
r = call("waitIdle", {"timeoutMs": 5000})
print("waitIdle %.0f ms -> %s" % ((time.time() - ts) * 1000, r.get("result")))
ts = time.time()
snap = call("snapshot")
print("snapshot %.1f ms" % ((time.time() - ts) * 1000))
json.dump(snap.get("result"), open(f"{out}/snapshot-0.json", "w"), indent=1)

for i, (method, params) in enumerate(steps, 1):
    ts = time.time()
    r = call(method, params)
    ms = (time.time() - ts) * 1000
    status = "ERR " + json.dumps(r["error"])[:160] if "error" in r else "ok"
    extra = ""
    if "result" in r and isinstance(r["result"], dict):
        extra = " settle=%s" % r["result"].get("settle")
    print("%2d %-18s %7.1f ms %s%s  %s" % (i, method, ms, status, extra, json.dumps(params)[:70]))
    if method == "snapshot":
        json.dump(r.get("result"), open(f"{out}/snapshot-{i}.json", "w"), indent=1)
conn.close()
