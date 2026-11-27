"""Task 3: absolute timeline of the permission block in --input native runs (event clock, ms since process start).
For each run: start of the Request touch, start of the alert touch, end of the alert tapOn step, settle time before and after the alert touch."""
import sys
from pathlib import Path
from lib_runs import load_run, med
ROOT = Path(__file__).resolve().parent / "bench"


def touch_events(r):
    out = []
    for si, s in enumerate(r["steps"]):
        for e in s["events"]:
            if (e["event"] == "hid" and e.get("route") == "touch") or (e["event"] == "http" and e.get("route") == "touch"):
                out.append((si, e["t"] - e["ms"], e["t"]))
    return out


def analyse(dirname, hid, names):
    rows = []
    for n in names:
        r = load_run(ROOT / dirname, f"{n}-{hid}")
        if not r or r["rc"] != 0:
            continue
        idx = {s["desc"][:25]: i for i, s in enumerate(r["steps"])}
        req_i = next(i for i, s in enumerate(r["steps"]) if s["desc"].startswith('tapOn "Request'))
        alert_i = next(i for i, s in enumerate(r["steps"]) if s["desc"].startswith('tapOn "Povolit'))
        tev = {si: (a, b) for si, a, b in touch_events(r)}
        step_end = lambda i: max(e["t"] for e in r["steps"][i]["events"]) if r["steps"][i]["events"] else None
        req_start, req_end = tev[req_i]
        alert_start, alert_end = tev[alert_i]
        a_end = step_end(alert_i)
        prev_end = step_end(alert_i - 1)
        rows.append(dict(run=n, req_to_alert_touch=alert_start - req_start, pre_settle=alert_start - prev_end,
                         post=a_end - alert_start, req_to_end=a_end - req_start))
    return rows


if __name__ == "__main__":
    for d in sys.argv[1:]:
        print("==", d)
        for hid in ("off", "on"):
            rows = analyse(d, hid, [f"run-{i}" for i in range(1, 7)])
            for k in ("req_to_alert_touch", "pre_settle", "post", "req_to_end"):
                print(f"  {hid:3} {k:20} median {med([x[k] for x in rows]):6.0f}  min {min(x[k] for x in rows):5.0f} max {max(x[k] for x in rows):5.0f}  n={len(rows)}")
