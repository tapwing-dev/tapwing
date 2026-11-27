"""Shared parsing of tapwing run logs and reports (phase 3)."""
import json
import re
import statistics as st
from pathlib import Path

STEP_RE = re.compile(r"^\s+[✓✗]\s+([\d.]+) ms\s+(.*)$")
EV_RE = re.compile(r"^ios-native-timing event=(\w+) t_ms=(\d+) ms=(\d+)\s*(.*)$")


def kv(s):
    return dict(p.split("=", 1) for p in s.split() if "=" in p)


def load_run(d: Path, name: str):
    """Returns dict(rc, wall, xcb, status, total_ms, steps=[{desc, ms, status, events}]) or None when no files."""
    d = Path(d)
    if not (d / f"{name}.wall").exists():
        return None
    rc, wall = (d / f"{name}.wall").read_text().split()
    xcb = int((d / f"{name}.xcb").read_text().strip() or 0) if (d / f"{name}.xcb").exists() else 0
    run = {"name": name, "rc": int(rc), "wall": float(wall) * 1000, "xcb": xcb, "steps": [], "status": "no-report",
           "total_ms": None, "log": d / f"{name}.log"}
    jp = d / f"{name}.json"
    if jp.exists():
        rep = json.load(open(jp))
        flow = rep["flows"][0]
        run["status"] = flow["status"]
        run["total_ms"] = flow["totalMs"]
        run["steps"] = [{"desc": s["description"], "ms": s["ms"], "status": s["status"], "events": []}
                        for s in flow["steps"]]
    pending = []
    idx = 0
    for line in open(d / f"{name}.log", encoding="utf-8"):
        line = line.rstrip("\n")
        m = EV_RE.match(line)
        if m:
            pending.append(dict(event=m[1], t=int(m[2]), ms=int(m[3]), **kv(m[4])))
            continue
        if STEP_RE.match(line):
            if idx < len(run["steps"]):
                run["steps"][idx]["events"] = pending
            pending = []
            idx += 1
    run["tail_events"] = pending
    return run


def med(xs):
    return st.median(xs) if xs else float("nan")


def mmm(xs, digits=0):
    if not xs:
        return "-"
    f = f"{{:.{digits}f}}"
    return f"{f.format(med(xs))} ({f.format(min(xs))}-{f.format(max(xs))})"
