"""Builds phase3/summary.md: generated tables (tasks 2, 3, 4, 5) from the logs, plus the prose of prose/*.md.
Usage: make_summary.py (writes summary.md next to this script)."""
import csv
import io
import contextlib
from pathlib import Path
from lib_runs import load_run, med, mmm
import summarize_bench as sb
import explain2

HERE = Path(__file__).resolve().parent


def prose(name):
    p = HERE / "prose" / name
    return p.read_text().rstrip() + "\n" if p.exists() else f"(missing {name})\n"


def bench_section():
    t, fails = sb.table()
    out = ["Medians with min and max in parentheses, milliseconds, over the passed runs. `flow ms` is `totalMs` of the report, `wall` is the process wall time.", ""]
    out += t + [""]
    out += ["Runs that did not pass:", ""]
    for flow, mode, hid, r in fails:
        out.append(f"- {flow} {mode} flag {hid}, {r['name']}: exit {r['rc']}, status {r['status']}. See the log for the step.")
    out += ["", "Flow gain with the flag on (medians of passed runs):", ""] + sb.deltas() + [""]
    out += ["Per step medians:", ""] + sb.per_step()
    return out


def timeline_section():
    out = ["Event clock times in `--input native`, medians over the passed runs (ms). `req->alert touch` runs from the start of the Request touch to the start of the alert touch. `settle before touch` is the time from the end of the previous step to the start of the alert touch. `touch->step end` runs from the start of the alert touch to the end of the step.", "",
           "| flow | flag | req->alert touch | settle before touch | touch->step end | req->step end |", "|---|---|---|---|---|---|"]
    for d in ("f5_cs-native", "f5_cs_lazy-native"):
        for hid in ("off", "on"):
            rows = explain2.analyse(d, hid, [f"run-{i}" for i in range(1, 7)])
            m = lambda k: f"{med([r[k] for r in rows]):.0f}"
            out.append(f"| {d} | {hid} | {m('req_to_alert_touch')} | {m('pre_settle')} | {m('post')} | {m('req_to_end')} |")
    return out


def reliability_section():
    out = []
    def count(path):
        c = {}
        n = 0
        for row in csv.reader(open(path)):
            if row:
                n += 1
                k = row[4].split(":")[0]
                c[k] = c.get(k, 0) + 1
        return n, c
    rel = HERE / "reliability"
    n, c = count(rel / "main_120.csv")
    out += ["Main simulator, one process and one HID client, hold 10 ms, Flutter switch toggled and read back from the framebuffer (green track):", "",
            "| target | taps sent | delivered in 700 ms | delivered late | lost | other |", "|---|---|---|---|---|---|",
            f"| main simulator | {n} | {c.get('ok',0)} | {c.get('slow',0)} | {c.get('lost',0)} | {c.get('unreadable',0)+c.get('error',0)} |", ""]
    out += ["Temporary simulator, one boot per row:", "", "| boot | taps sent | delivered | delivered late | lost | other |", "|---|---|---|---|---|---|"]
    tot = tot_ok = 0
    for p in sorted(rel.glob("boot*.csv")):
        n, c = count(p)
        tot += n; tot_ok += c.get("ok", 0) + c.get("slow", 0)
        out.append(f"| {p.stem} | {n} | {c.get('ok',0)} | {c.get('slow',0)} | {c.get('lost',0)} | {c.get('unreadable',0)+c.get('error',0)} |")
    out += ["", f"All boots: {tot_ok} of {tot} delivered.", ""]
    runs = [load_run(rel / "lazy20", f"run-{i}") for i in range(1, 21)]
    ok = [r for r in runs if r and r["rc"] == 0 and r["status"] == "passed"]
    touches = [sum(1 for s in r["steps"] for e in s["events"] if e["event"] == "hid" and e.get("route") == "touch" and e.get("ok") == "1") for r in runs if r]
    out += [f"Tapwing, `f5_cs_lazy` in `auto`, flag on, 20 consecutive runs: {len(ok)} of {len(runs)} passed. xcodebuild spawned in {sum(1 for r in runs if r['xcb']>0)} runs. "
            f"Delivered HID touches per run: {sorted(set(touches))}. Flow ms {mmm([r['total_ms'] for r in ok])}, wall ms {mmm([r['wall'] for r in ok])}.", ""]
    return out


def regression_section():
    d = HERE / "regression" / "pure"
    out = ["Pure Flutter flows, `auto`, three interleaved runs per flag state (no warm-up, first pair includes cold caches).", "",
           "| flow | flag | passed | flow ms | wall ms | xcodebuild spawned | HID events in log |", "|---|---|---|---|---|---|---|"]
    for f in ("f7_app_health", "f2_endless_animation"):
        for hid in ("off", "on"):
            runs = [load_run(d, f"{f}-{i}-{hid}") for i in (1, 2, 3)]
            ok = [r for r in runs if r["rc"] == 0]
            ev = sum(sum(1 for line in open(r["log"]) if "event=hid" in line) for r in runs)
            out.append(f"| {f} | {hid} | {len(ok)}/3 | {mmm([r['total_ms'] for r in ok])} | {mmm([r['wall'] for r in ok])} | {sum(1 for r in runs if r['xcb']>0)}/3 | {ev} |")
    out += ["", "Forced failures, `f5_cs_lazy`, `auto`, flag on, one run each:", "",
            "| TAPWING_IOS_HID_FAIL | passed | flow ms | xcodebuild spawned | delivered HID touches | runner touches | failed HID touch calls |", "|---|---|---|---|---|---|---|"]
    fd = HERE / "regression" / "fail"
    for p in ("open", "snapshot", "touch", "frame", None):
        name = f"fail-{p}" if p else "flagoff"
        r = load_run(fd, name)
        lines = open(r["log"]).read().splitlines()
        hid_ok = sum(1 for l in lines if "event=hid " in l and "route=touch ok=1" in l)
        hid_bad = sum(1 for l in lines if "event=hid " in l and "route=touch ok=0" in l)
        http = sum(1 for l in lines if "route=touch status" in l)
        out.append(f"| {p or 'none (flag off)'} | {r['status']} | {r['total_ms']:.0f} | {'yes' if r['xcb'] else 'no'} | {hid_ok} | {http} | {hid_bad} |")
    return out


def main():
    parts = ["# Phase 3: independent verification of the iOS HID backend (issue 30)", "",
             prose("00_intro.md"),
             "## 1. Build and tests", "", prose("01_tests.md"),
             "## 2. Benchmark", ""] + bench_section() + ["",
             "## 3. The two smoke anomalies", "", prose("03a_tap.md")] + timeline_section() + ["", prose("03b_granted.md"),
             "## 4. Tap delivery", ""] + reliability_section() + [prose("04_notes.md"),
             "## 5. Regression checks", ""] + regression_section() + ["", prose("05_notes.md"),
             "## 6. Code review", "", prose("06_review.md"),
             "## Recommendation", "", prose("07_recommendation.md")]
    (HERE / "summary.md").write_text("\n".join(parts) + "\n")


if __name__ == "__main__":
    main()
