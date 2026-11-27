"""Task 3: per-step event timelines. Usage: explain.py <flow-mode dir> <step prefix> [runs...]"""
import sys
from pathlib import Path
from lib_runs import load_run, med

ROOT = Path(__file__).resolve().parent / "bench"


def summarize(dirname, prefix, hid, names):
    rows = []
    for n in names:
        r = load_run(ROOT / dirname, f"{n}-{hid}")
        if not r or r["rc"] != 0:
            continue
        for s in r["steps"]:
            if not s["desc"].startswith(prefix):
                continue
            ev = [e for e in s["events"] if not (e["event"] == "http" and e.get("route") == "status")]
            touch_i = next((i for i, e in enumerate(ev) if (e["event"] == "hid" and e.get("route") == "touch") or (e["event"] == "http" and e.get("route") == "touch")), None)
            def cnt(evs):
                fr = sum(1 for e in evs if e["event"] == "hid" and e.get("route") == "frame")
                ss = sum(1 for e in evs if e["event"] == "http" and e.get("route") == "isScreenStatic")
                hv = sum(1 for e in evs if e["event"] == "http" and e.get("route") == "viewHierarchy")
                return fr, ss, hv
            pre = ev[:touch_i] if touch_i is not None else ev
            post = ev[touch_i + 1:] if touch_i is not None else []
            t_touch = ev[touch_i]["ms"] if touch_i is not None else None
            t0 = None
            rows.append(dict(run=n, ms=s["ms"], touch_ms=t_touch, pre=cnt(pre), post=cnt(post),
                             t_end=ev[-1]["t"] if ev else None, t_touch=ev[touch_i]["t"] if touch_i is not None else None))
    return rows


if __name__ == "__main__":
    d, pref = sys.argv[1], sys.argv[2]
    names = sys.argv[3:] or [f"run-{i}" for i in range(1, 6)]
    for hid in ("off", "on"):
        print(hid)
        for r in summarize(d, pref, hid, names):
            post_len = (r["t_end"] - r["t_touch"]) if r["t_touch"] and r["t_end"] else None
            print(" ", r["run"], f"step={r['ms']:.0f}", f"touch={r['touch_ms']}", f"pre(frames,static,hier)={r['pre']}", f"post={r['post']}", f"touch_end->step_end_ms={post_len}")
