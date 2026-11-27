"""Phase 1, experiment 5: which transform maps a landscape point to the HID touch ratio?
The sample app is on the playground screen in LANDSCAPE_LEFT. The Back button centre is (90, 28) in
landscape points. Each of the 8 axis choices is tried until Back disappears (the app pops).
Usage: python3 -I landscape_tap.py <udid>"""
import json, subprocess, sys, time
U = sys.argv[1]; P = "bench/ios/hid-probe/build/hid-probe"
W, H = 440.0, 956.0           # portrait size the shim divides by
lx, ly = 90.0, 28.0           # landscape point of the Back button (frame 956 x 440)
ux, uy = lx / 956.0, ly / 440.0
cands = {
 "ratio=(ux,uy)": (ux, uy), "ratio=(1-ux,uy)": (1-ux, uy), "ratio=(ux,1-uy)": (ux, 1-uy), "ratio=(1-ux,1-uy)": (1-ux, 1-uy),
 "ratio=(uy,ux)": (uy, ux), "ratio=(1-uy,ux)": (1-uy, ux), "ratio=(uy,1-ux)": (uy, 1-ux), "ratio=(1-uy,1-ux)": (1-uy, 1-ux)}
def labels():
    for _ in range(6):
        r = subprocess.run([P, "snapshot", U], capture_output=True, text=True)
        if r.stdout:
            d = json.loads(r.stdout)
            return [c["label"] for c in d["roots"][0]["children"] if c["label"]]
        time.sleep(0.5)
    return None
for name, (rx, ry) in cands.items():
    before = labels()
    subprocess.run([P, "tap", U, f"{rx*W:.1f}", f"{ry*H:.1f}"], capture_output=True)
    time.sleep(1.5)
    after = labels()
    print(name, "tap args", f"({rx*W:.1f},{ry*H:.1f})", "->", "Back still there" if after and "Back" in after else f"CHANGED {after[:3] if after else after}", flush=True)
    if not (after and "Back" in after): break
