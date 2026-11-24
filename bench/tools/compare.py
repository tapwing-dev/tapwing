"""Compare per-command timings: Maestro debug output vs Tapwing JSON report.

Usage: python3 -I bench/tools/compare.py bench/results
Prints a Markdown table of medians.
"""
import glob
import json
import os
import re
import statistics
import sys

SKIP = {"applyConfigurationCommand", "defineVariablesCommand"}


def maestro_run(run_dir):
    # Maestro writes into a hidden `.maestro/` folder, which glob skips.
    files = [os.path.join(d, f) for d, _, fs in os.walk(run_dir) for f in fs
             if f.startswith("commands") and f.endswith(".json")]
    if not files:
        return None
    cmds = json.load(open(files[0]))
    out = []
    for c in cmds:
        name = next(iter(c["command"]))
        if name in SKIP:
            continue
        out.append(c["metadata"].get("duration", 0))
    return out


def tapwing_run(path):
    r = json.load(open(path))
    f = r["flows"][0]
    return [s["ms"] for s in f["steps"]], [s["description"] for s in f["steps"]], f["totalMs"]


def wall(results, prefix):
    vals = []
    for line in open(os.path.join(results, "wall.txt")):
        m = re.match(rf"{prefix} run \d+: ([\d.]+) s", line)
        if m:
            vals.append(float(m.group(1)))
    return vals


def main(results):
    m_runs = [r for r in (maestro_run(d) for d in sorted(glob.glob(os.path.join(results, "maestro-[0-9]*")))
              if os.path.isdir(d)) if r]
    k_files = sorted(glob.glob(os.path.join(results, "tapwing-*.json")))
    k_runs = [tapwing_run(p) for p in k_files]
    if not m_runs or not k_runs:
        print("missing runs")
        return
    descs = k_runs[0][1]
    n = min(len(descs), *(len(r) for r in m_runs))
    print(f"Runs: Maestro {len(m_runs)}x, Tapwing {len(k_runs)}x (medians, ms)\n")
    print("| # | command | Maestro | Tapwing | speedup |")
    print("|---|---|---:|---:|---:|")
    m_tot = k_tot = 0.0
    m_tot_nl = k_tot_nl = 0.0
    for i in range(n):
        m = statistics.median(r[i] for r in m_runs)
        k = statistics.median(r[0][i] for r in k_runs)
        m_tot += m
        k_tot += k
        if not descs[i].startswith("launchApp"):
            m_tot_nl += m
            k_tot_nl += k
        sp = f"{m / k:.0f}×" if k > 0.5 else "—"
        print(f"| {i + 1} | `{descs[i]}` | {m:.0f} | {k:.1f} | {sp} |")
    print(f"| | **sum of commands** | **{m_tot:.0f}** | **{k_tot:.0f}** | **{m_tot / k_tot:.1f}×** |")
    print(f"| | **without launchApp** | **{m_tot_nl:.0f}** | **{k_tot_nl:.0f}** | **{m_tot_nl / k_tot_nl:.1f}×** |")
    mw, kw = wall(results, "maestro"), wall(results, "tapwing")
    if mw and kw:
        print(f"| | **wall clock incl. CLI start** | **{statistics.median(mw) * 1000:.0f}** | "
              f"**{statistics.median(kw) * 1000:.0f}** | **{statistics.median(mw) / statistics.median(kw):.1f}×** |")


if __name__ == "__main__":
    main(sys.argv[1])
