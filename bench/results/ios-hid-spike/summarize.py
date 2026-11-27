#!/usr/bin/env python3
"""Builds step0/summary.md from the run logs and JSON reports of run_matrix.sh.

Usage: summarize.py   (run from anywhere; reads step0/, writes step0/summary.md)

Every number is measured, except where a line says "derived". Derived values
place a step on the event clock (`t_ms`, ms since process start): the log has
no step start events, so the end of a step is taken as the `t_ms` of its last
runner event and its start as end minus the step duration from the report.
"""
import json
import re
import statistics as st
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent / "step0"
CONFIGS = [
    ("f5_cs", "auto"),
    ("f5_cs", "native"),
    ("f5_cs_lazy", "auto"),
    ("f5_cs_lazy", "native"),
]
STEP_RE = re.compile(r"^\s+[✓✗]\s+([\d.]+) ms\s+(.*)$")
EV_RE = re.compile(r"^ios-native-timing event=(\w+) t_ms=(\d+) ms=(\d+)\s*(.*)$")
ALERT_MARK = "Povolit"


def med(xs):
    return st.median(xs) if xs else 0.0


def fmt(x):
    return f"{x:.0f}"


def mmm(xs):
    """median (min-max)"""
    if not xs:
        return "-"
    return f"{fmt(med(xs))} ({fmt(min(xs))}-{fmt(max(xs))})"


def kv(s):
    return dict(p.split("=", 1) for p in s.split() if "=" in p)


def parse_run(d: Path, name: str):
    rep = json.load(open(d / f"{name}.json"))
    flow = rep["flows"][0]
    steps = [{"desc": s["description"], "ms": s["ms"], "events": [], "settle": (s.get("driver") or {}).get("settle")}
             for s in flow["steps"]]
    runner = {}  # runner_start event
    waits = []  # (step index, ms)
    pending = []
    idx = 0
    for line in open(d / f"{name}.log", encoding="utf-8"):
        line = line.rstrip("\n")
        m = EV_RE.match(line)
        if m:
            ev = dict(event=m[1], t=int(m[2]), ms=int(m[3]), **kv(m[4]))
            if ev["event"] == "runner_start":
                runner = ev
            else:
                pending.append(ev)
            continue
        m = STEP_RE.match(line)
        if m:
            steps[idx]["events"] = pending
            pending = []
            idx += 1
    # Derived step windows on the event clock.
    end = [None] * len(steps)
    for i, s in enumerate(steps):
        anchors = [e["t"] for e in s["events"] if not (e["event"] == "http" and e.get("route") == "status")]
        if anchors:
            end[i] = max(anchors)
    nxt = None
    for i in range(len(steps) - 1, -1, -1):
        if end[i] is None and nxt is not None:
            end[i] = nxt[0] - sum(steps[j]["ms"] for j in range(i + 1, nxt[1] + 1)) + steps[nxt[1]]["ms"] * 0
            # end of step i = start of step i+1 = end[nxt] - sum(ms of steps i+1..nxt)
        if end[i] is not None:
            nxt = (end[i], i)
    for i, s in enumerate(steps):
        s["t_end"] = end[i]
        s["t_start"] = None if end[i] is None else end[i] - s["ms"]
    wall = open(d / f"{name}.wall").read().split()
    return dict(
        status=flow["status"], total=flow["totalMs"], steps=steps, runner=runner,
        wall=float(wall[1]) * 1000, rc=int(wall[0]),
    )


def analyse(run):
    steps = run["steps"]
    r = run["runner"]
    res = {"calls": defaultdict(list)}
    t_ready = r.get("t")
    session_start = t_ready - int(r["total_ms"]) if r else None
    spawn_ms = r.get("ms")
    flow_start = steps[0]["t_start"] if steps[0]["t_start"] is not None else 0
    eager = session_start is not None and session_start - flow_start < 1000
    res["eager"] = eager
    res["runner_ms"] = spawn_ms
    res["runner_total_ms"] = int(r["total_ms"]) if r else None
    per_step = []
    for i, s in enumerate(steps):
        calls = [e for e in s["events"] if e["event"] == "http" and e.get("route") != "status"]
        waits = [e["ms"] for e in s["events"] if e["event"] == "warmup_wait"]
        blocked = sum(waits)
        paid = blocked
        probe_delay = 0
        if r and not eager and s["t_start"] is not None and s["t_start"] <= session_start <= s["t_end"]:
            paid = t_ready - session_start  # lazy: step waited for the runner it just started
            probe_delay = session_start - s["t_start"]
        per_step.append(dict(calls=calls, blocked=blocked, paid=paid, probe_delay=probe_delay, ms=s["ms"], desc=s["desc"]))
    res["steps"] = per_step
    return res


def comp(ps):
    """component ms of a set of per-step dicts"""
    calls = [c for p in ps for c in p["calls"]]
    by = lambda r: sum(c["ms"] for c in calls if c["route"] == r)
    vh, touch = by("viewHierarchy"), by("touch")
    other_calls = sum(c["ms"] for c in calls) - vh - touch
    paid = sum(p["paid"] for p in ps)
    probe = sum(p["probe_delay"] for p in ps)
    total = sum(p["ms"] for p in ps)
    return dict(total=total, paid=paid, vh=vh, touch=touch, other_calls=other_calls, probe=probe,
                rest=total - paid - vh - touch - other_calls - probe, ncalls=len(calls))


def main():
    out = []
    P = out.append
    P("# Step 0 summary: native steps of f5_permission_dialog (Czech copy)\n")
    P("Generated by `bench/results/ios-hid-spike/summarize.py` from the logs and reports in this directory.")
    P("Cells are median (min-max) over the 5 measured runs, in ms, unless noted. All runs measured except where a row says derived.\n")
    data = {}
    for flow, mode in CONFIGS:
        d = ROOT / f"{flow}-{mode}"
        data[(flow, mode)] = [(i, parse_run(d, f"run-{i}")) for i in range(1, 6)]
    for (flow, mode), runs in data.items():
        key = f"{flow} / {mode}"
        P(f"## {key}\n")
        P("Runs: " + ", ".join(f"run-{i} {r['status']} (exit {r['rc']})" for i, r in runs) + "\n")
        an = [analyse(r) for _, r in runs]
        P(f"- Flow total (report `totalMs`): {mmm([r['total'] for _, r in runs])}")
        P(f"- Wall time of `tapwing test` incl. process start and exit: {mmm([r['wall'] for _, r in runs])}")
        P(f"- Runner start, `xcodebuild` spawn until `/status` has our session id: {mmm([a['runner_ms'] for a in an])}")
        P(f"- Runner start incl. locate/sweep before the spawn (`start_session` total): {mmm([a['runner_total_ms'] for a in an])}")
        P(f"- Runner spawned at flow start (eager warm-up): {'yes' if all(a['eager'] for a in an) else 'no, on demand'}\n")
        nsteps = len(an[0]["steps"])
        P("| # | step | ms | blocked in `get_or_wait` | startup paid | runner calls (n) | runner call ms |")
        P("|---|---|---|---|---|---|---|")
        for i in range(nsteps):
            ps = [a["steps"][i] for a in an]
            cs = [comp([p]) for p in ps]
            P(f"| {i} | {ps[0]['desc']} | {mmm([p['ms'] for p in ps])} | {mmm([p['blocked'] for p in ps])} "
              f"| {mmm([p['paid'] for p in ps])} | {fmt(med([c['ncalls'] for c in cs]))} "
              f"| {mmm([c['vh'] + c['touch'] + c['other_calls'] for c in cs])} |")
        P("")
        blocked_steps = Counter()
        for a in an:
            for i, p in enumerate(a["steps"]):
                if p["blocked"] > 0 or p["paid"] > 0:
                    blocked_steps[i] += 1
        if blocked_steps:
            P("Steps that paid for the runner start (steps with blocked or lazy-wait time > 0, runs out of 5): "
              + ", ".join(f"step {i} ({n}/5)" for i, n in sorted(blocked_steps.items())) + "\n")
        else:
            P("No step blocked on the runner start.\n")
        lazyp = [p["probe_delay"] for a in an for p in a["steps"] if p["probe_delay"]]
        if lazyp:
            P(f"Derived: the step that started the runner on demand first looked in the Flutter tree for {mmm(lazyp)} ms before the spawn (IOS_LAZY_PROBE_MS = 2000).\n")
        # Alert steps.
        alert_idx = [i for i, p in enumerate(an[0]["steps"]) if ALERT_MARK in p["desc"]]
        P("Alert steps, runner calls per step (median count over runs; ms are medians):\n")
        P("| step | calls by route | first viewHierarchy | later viewHierarchy (median) | touch | calls total | step ms | not calls |")
        P("|---|---|---|---|---|---|---|---|")
        for i in alert_idx:
            ps = [a["steps"][i] for a in an]
            routes = sorted({c["route"] for p in ps for c in p["calls"]})
            counts = ", ".join(f"{r} x{fmt(med([sum(1 for c in p['calls'] if c['route'] == r) for p in ps]))}" for r in routes)
            first = [next((c["ms"] for c in p["calls"] if c["route"] == "viewHierarchy"), None) for p in ps]
            later = [med([c["ms"] for c in p["calls"] if c["route"] == "viewHierarchy"][1:]) for p in ps
                     if len([c for c in p["calls"] if c["route"] == "viewHierarchy"]) > 1]
            touch = [sum(c["ms"] for c in p["calls"] if c["route"] == "touch") for p in ps]
            tot = [sum(c["ms"] for c in p["calls"]) for p in ps]
            P(f"| {ps[0]['desc']} | {counts} | {mmm([x for x in first if x is not None])} | {mmm(later)} "
              f"| {mmm([t for t in touch if t])} | {mmm(tot)} | {mmm([p['ms'] for p in ps])} "
              f"| {mmm([p['ms'] - sum(c['ms'] for c in p['calls']) for p in ps])} |")
        P("")
        P("Where the time goes (median ms per component, share of the median total; startup = time the step waited for the runner to come up, derived for on-demand starts):\n")
        P("| scope | total | runner startup paid | viewHierarchy | touch | other runner calls | lazy probe before spawn | rest (Flutter lookup, settle, sleeps, host) |")
        P("|---|---|---|---|---|---|---|---|")
        scopes = [(an[0]["steps"][i]["desc"], [i]) for i in alert_idx]
        scopes.append(("all alert steps", alert_idx))
        scopes.append(("whole flow (sum of steps)", list(range(nsteps))))
        for label, idxs in scopes:
            cs = [comp([a["steps"][i] for i in idxs]) for a in an]
            tot = med([c["total"] for c in cs])
            def cell(k):
                v = med([c[k] for c in cs])
                return f"{fmt(v)} ({100 * v / tot:.0f}%)" if tot else "-"
            P(f"| {label} | {fmt(tot)} | {cell('paid')} | {cell('vh')} | {cell('touch')} | {cell('other_calls')} | {cell('probe')} | {cell('rest')} |")
        P("")
        cs = [comp(a["steps"]) for a in an]
        att = [c["paid"] + c["vh"] + c["touch"] + c["other_calls"] for c in cs]
        P(f"**Attributable to the XCUITest runner** (startup paid by steps + sum of runner calls): {mmm(att)} ms, "
          f"{100 * med(att) / med([r['total'] for _, r in runs]):.0f}% of the median flow total.\n")
        # Flutter-side settle stall.
        stalls = [(a_i, p) for a_i, a in enumerate(an) for p in a["steps"]]
        settle = Counter()
        for _, r in runs:
            for s in r["steps"]:
                if s["settle"]:
                    settle[(s["desc"], s["settle"])] += 1
        if settle:
            P("Flutter driver settle result per step (runs out of 5): " + "; ".join(f"{d} -> {v} x{n}" for (d, v), n in settle.items()) + "\n")
    notes = Path(__file__).resolve().parent / "summary_notes.md"
    if notes.exists():
        P(notes.read_text())
    (ROOT / "summary.md").write_text("\n".join(out) + "\n")
    print("\n".join(out))


if __name__ == "__main__":
    main()
