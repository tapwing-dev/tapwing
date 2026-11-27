"""Task 2 table: medians (min-max) for the 8 configurations. Usage: summarize_bench.py (prints markdown)."""
from pathlib import Path
from lib_runs import load_run, med, mmm

ROOT = Path(__file__).resolve().parent / "bench"
CONFIGS = [("f5_cs", "auto"), ("f5_cs", "native"), ("f5_cs_lazy", "auto"), ("f5_cs_lazy", "native")]
ALERT = [("assertVisible", "assertVisible \"Povolit"), ("tapOn", "tapOn \"Povolit"), ("assertNotVisible", "assertNotVisible \"Povolit")]


def runs_of(d, hid):
    out = []
    for p in sorted(d.glob(f"run-*-{hid}.wall")):
        r = load_run(d, p.stem)
        if r:
            out.append(r)
    return out


def step_ms(r, prefix):
    for s in r["steps"]:
        if s["desc"].startswith(prefix):
            return s["ms"]
    return None


def table():
    lines = []
    lines.append("| flow | mode | flag | passed | flow ms | wall ms | alert assertVisible ms | alert tapOn ms | alert assertNotVisible ms | xcodebuild spawned |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|")
    failures = []
    for flow, mode in CONFIGS:
        d = ROOT / f"{flow}-{mode}"
        for hid in ("off", "on"):
            runs = runs_of(d, hid)
            ok = [r for r in runs if r["rc"] == 0 and r["status"] == "passed"]
            for r in runs:
                if r not in ok:
                    failures.append((flow, mode, hid, r))
            cells = [mmm([r["total_ms"] for r in ok]), mmm([r["wall"] for r in ok])]
            for _, pref in ALERT:
                cells.append(mmm([x for x in (step_ms(r, pref) for r in ok) if x is not None]))
            xcb = f"{sum(1 for r in ok if r['xcb'] > 0)}/{len(ok)}"
            lines.append(f"| {flow} | {mode} | {hid} | {len(ok)}/{len(runs)} | " + " | ".join(cells) + f" | {xcb} |")
    return lines, failures


def deltas():
    lines = ["| flow | mode | flow ms off | flow ms on | gain ms | gain % | wall gain ms |", "|---|---|---|---|---|---|---|"]
    for flow, mode in CONFIGS:
        d = ROOT / f"{flow}-{mode}"
        off = [r for r in runs_of(d, "off") if r["rc"] == 0]
        on = [r for r in runs_of(d, "on") if r["rc"] == 0]
        a, b = med([r["total_ms"] for r in off]), med([r["total_ms"] for r in on])
        wa, wb = med([r["wall"] for r in off]), med([r["wall"] for r in on])
        lines.append(f"| {flow} | {mode} | {a:.0f} | {b:.0f} | {a-b:.0f} | {100*(a-b)/a:.1f} | {wa-wb:.0f} |")
    return lines


def per_step():
    """Median ms per step for every configuration, flag off against on."""
    lines = []
    for flow, mode in CONFIGS:
        d = ROOT / f"{flow}-{mode}"
        off = [r for r in runs_of(d, "off") if r["rc"] == 0]
        on = [r for r in runs_of(d, "on") if r["rc"] == 0]
        if not off or not on:
            continue
        lines += [f"### {flow}, {mode}", "", "| step | off median | on median | gain ms |", "|---|---|---|---|"]
        for i, s in enumerate(off[0]["steps"]):
            a = med([r["steps"][i]["ms"] for r in off if len(r["steps"]) > i])
            b = med([r["steps"][i]["ms"] for r in on if len(r["steps"]) > i])
            lines.append(f"| {s['desc'][:60]} | {a:.0f} | {b:.0f} | {a-b:.0f} |")
        lines.append("")
    return lines


if __name__ == "__main__":
    t, f = table()
    print("\n".join(t))
    print()
    for flow, mode, hid, r in f:
        print("FAILED", flow, mode, hid, r["name"], r["rc"], r["status"])
    print("\n".join(deltas()))
    print("\n".join(per_step()))
