"""Phase 1, experiment 2: how long must the finger stay down for a tap to register?
Flutter: login screen 'F2 Playground' button, success when the playground screen shows.
UIKit: Settings root, a row opens its detail page.
Usage: python3 -I hold_test.py <udid> <tries>"""
import json, subprocess, sys, time
U, N = sys.argv[1], int(sys.argv[2])
SETTLE = float(sys.argv[4]) if len(sys.argv) > 4 else 0.0  # seconds to wait after the target shows
P = "bench/ios/hid-probe/build/hid-probe"

def snap():
    r = subprocess.run([P, "snapshot", U], capture_output=True, text=True, timeout=30)
    return json.loads(r.stdout)

def walk(n):
    yield n
    for c in n["children"]:
        yield from walk(c)

def find(d, label):
    for r in d["roots"][:1]:
        for n in walk(r):
            if n["label"] and n["label"].startswith(label):
                return n
def center(n):
    f = n["frame"]; return f[0] + f[2] / 2, f[1] + f[3] / 2

def tap(x, y, hold):
    r = subprocess.run([P, "tap", U, str(x), str(y), "--hold", str(hold)], capture_output=True, text=True)
    return r.stderr.strip()

def wait_for(label, secs=2.5):
    end = time.time() + secs
    while time.time() < end:
        n = find(snap(), label)
        if n: return n
        time.sleep(0.1)

def run(name, setup, target, check, back, holds):
    for hold in holds:
        ok = 0; ms = []
        for _ in range(N):
            setup()
            n = wait_for(target, 8)
            if not n: print(name, hold, "setup failed"); continue
            time.sleep(SETTLE)
            msg = tap(*center(n), hold)
            ms.append(float(msg.split(": ")[-1].split(" ")[0]))
            if wait_for(check, 2.0): ok += 1
            back()
        print(f"{name} hold={hold}ms registered {ok}/{N} tap_call_ms={ms}", flush=True)

def flutter_setup():
    subprocess.run(["xcrun","simctl","terminate",U,"dev.tapwing.sampleApp"],capture_output=True)
    subprocess.run(["xcrun","simctl","launch",U,"dev.tapwing.sampleApp"],capture_output=True)
    wait_for("F2 Playground", 8)
def noop(): pass
def settings_setup():
    subprocess.run(["xcrun","simctl","terminate",U,"com.apple.Preferences"],capture_output=True)
    subprocess.run(["xcrun","simctl","launch",U,"com.apple.Preferences"],capture_output=True)

which = sys.argv[3] if len(sys.argv) > 3 else "both"
holds = [0, 5, 10, 20, 50]
if which in ("both", "flutter"):
    run("flutter", flutter_setup, "F2 Playground", "Selectors & Taps", noop, holds)
if which in ("both", "uikit"):
    run("uikit", settings_setup, "Obecné", "Informace", noop, holds)
