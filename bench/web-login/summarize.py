"""Per-step medians of one browser-login benchmark run (bench/web-login/run_bench.sh).

Usage: python3 -I bench/web-login/summarize.py <results-dir>

Reads runs.tsv, the Tapwing JSON reports, Maestro's debug output and the login
server's log from the results directory and prints Markdown. Warm-up runs are
left out. Medians are taken over the runs that passed, the pass count is printed
next to them, every failed run is listed with the step it failed on, and so is
every passed run with a step far above its median.
"""
import json
import os
import re
import statistics
import sys

# Maestro records these, Tapwing's report does not.
MAESTRO_SKIP = {"applyConfigurationCommand", "defineVariablesCommand"}
RUNNERS = ("tapwing", "maestro")


def load_runs(results):
    runs = []
    with open(os.path.join(results, "runs.tsv"), encoding="utf-8") as f:
        for line in f:
            runner, variant, label, status, wall, load = line.rstrip("\n").split("\t")
            runs.append({"runner": runner, "variant": variant, "label": label, "status": status,
                         "wall": float(wall), "load": float(load)})
    return runs


def tapwing_run(results, variant, label):
    """Steps of a Tapwing report, or None when the run wrote no flow."""
    path = os.path.join(results, f"tapwing-{variant}-{label}.json")
    try:
        with open(path, encoding="utf-8") as f:
            flow = json.load(f)["flows"][0]
    except (OSError, ValueError, LookupError):
        return None
    steps = []
    for s in flow.get("steps", []):
        driver = s.get("driver")
        lane = None
        if s["command"] == "launchApp":
            lane = "host"
        elif isinstance(driver, dict):
            # Native operations name their lane. A Flutter result carries the driver's own data.
            lane = driver.get("lane") or "flutter"
        steps.append({"desc": s["description"], "command": s["command"], "ms": s["ms"], "lane": lane,
                      "failed": s.get("status") != "passed", "error": s.get("error")})
    return steps


def maestro_run(results, variant, label):
    """Steps of Maestro's `commands-*.json`, or None when there is none."""
    run_dir = os.path.join(results, f"maestro-{variant}-{label}")
    # Maestro writes into a hidden `.maestro/` folder, which glob skips.
    files = sorted(os.path.join(d, f) for d, _, fs in os.walk(run_dir) for f in fs
                   if f.startswith("commands") and f.endswith(".json"))
    if not files:
        return None
    with open(files[0], encoding="utf-8") as f:
        commands = json.load(f)
    steps = []
    for c in commands:
        name = next(iter(c["command"]))
        if name in MAESTRO_SKIP:
            continue
        meta = c.get("metadata", {})
        error = meta.get("error")
        steps.append({"desc": name, "command": name, "ms": float(meta.get("duration") or 0), "lane": None,
                      "failed": meta.get("status") != "COMPLETED",
                      "error": error.get("message") if isinstance(error, dict) else error})
    return steps


def browser_region(descs):
    """Indices of the steps outside Flutter: after the tap on a `browser_login_*`
    button in the app, up to the step before the final assertion."""
    opens = [i for i, d in enumerate(descs) if re.search(r"id: browser_login_(plain|two_step|controlled)", d)]
    if not opens:
        return range(0)
    return range(opens[-1] + 1, len(descs) - 1)


def typed_text(desc):
    m = re.match(r'inputText "(.*)"$', desc)
    return m.group(1) if m else None


def fmt_ms(values):
    if not values:
        return "n/a"
    med = statistics.median(values)
    return f"{med:.0f}" if med >= 10 else f"{med:.1f}"


def fmt_s(values):
    return f"{statistics.median(values) / 1000:.2f} s" if values else "n/a"


def server_events(results):
    """Event counts the controlled page posted, per (runner, run label)."""
    events, rejected, current = {}, {}, None
    try:
        with open(os.path.join(results, "server.log"), encoding="utf-8") as f:
            for line in f:
                m = re.match(r"run runner=(\S+) variant=(\S+) run=(\S+)", line)
                if m:
                    current = m.groups()
                elif current and line.startswith("controlled-events "):
                    events.setdefault(current, []).append(dict(kv.split("=") for kv in line.split()[1:]))
                elif current and line.startswith("login-rejected "):
                    rejected.setdefault(current, []).append(line.strip())
    except OSError:
        pass
    return events, rejected


def summarize_variant(results, variant, runs, events, rejected):
    measured = [r for r in runs if r["variant"] == variant and r["label"] != "warmup"]
    runners = [r for r in RUNNERS if any(m["runner"] == r for m in measured)]
    loaders = {"tapwing": tapwing_run, "maestro": maestro_run}
    data, failures = {}, []
    for runner in runners:
        mine = [m for m in measured if m["runner"] == runner]
        passed = []
        for m in mine:
            steps = loaders[runner](results, variant, m["label"])
            if m["status"] == "passed" and steps and not any(s["failed"] for s in steps):
                passed.append((m, steps))
                continue
            bad = next((s for s in steps or [] if s["failed"]), None)
            where = f"step {steps.index(bad) + 1} `{bad['desc']}`" if bad else "no step recorded as failed"
            detail = f": {str(bad['error']).splitlines()[0]}" if bad and bad.get("error") else ""
            failures.append(f"- {runner} `{variant}` run {m['label']} failed at {where}{detail}")
            for line in rejected.get((runner, variant, m["label"]), []):
                failures.append(f"  - server: `{line}`")
        data[runner] = {"total": len(mine), "passed": passed}

    print(f"### `{variant}`\n")
    for runner in runners:
        d = data[runner]
        walls = [m["wall"] * 1000 for m, _ in d["passed"]]
        loads = [m["load"] for m in measured if m["runner"] == runner]
        print(f"- {runner}: passed {len(d['passed'])}/{d['total']}, wall clock {fmt_s(walls)}, "
              f"host load average {statistics.median(loads):.0f}")
    print()

    reference = next((data[r]["passed"][0][1] for r in runners if data[r]["passed"]), None)
    if reference is None:
        print("No run passed.\n")
        if failures:
            print("\n".join(failures) + "\n")
        return
    # Tapwing's descriptions are the readable ones. Maestro has the same steps in the same order.
    descs = [s["desc"] for s in (data["tapwing"]["passed"][0][1] if data.get("tapwing", {}).get("passed")
                                 else reference)]
    region = browser_region(descs)
    lanes = []
    for i in range(len(descs)):
        seen = {steps[i]["lane"] for _, steps in data.get("tapwing", {}).get("passed", []) if i < len(steps)}
        seen.discard(None)
        if len(seen) == 1:
            lanes.append(seen.pop())
        elif seen:
            lanes.append("mixed")
        elif "tapwing" in data:
            # The report has no lane for this step. Marked by its place in the flow.
            lanes.append("native*" if i in region else "flutter*")
        else:
            lanes.append("")

    header = "| # | step | lane | " + " | ".join(f"{r} ms" for r in runners) + " |"
    print(header)
    print("|---|---|---|" + "---:|" * len(runners))
    for i, desc in enumerate(descs):
        cells = []
        for runner in runners:
            cells.append(fmt_ms([steps[i]["ms"] for _, steps in data[runner]["passed"] if i < len(steps)]))
        mark = " (browser)" if i in region else ""
        print(f"| {i + 1} | `{desc}`{mark} | {lanes[i]} | " + " | ".join(cells) + " |")
    print()

    # Derived numbers, computed per run and then taken as a median over the runs.
    def per_run(fn):
        out = []
        for runner in runners:
            out.append([v for v in (fn(steps) for _, steps in data[runner]["passed"]) if v is not None])
        return out

    type_steps = [i for i in region if typed_text(descs[i]) is not None]
    chars = sum(len(typed_text(descs[i])) for i in type_steps)
    lookups = [i for i in region if descs[i].startswith(("tapOn", "assertVisible"))]
    rows = []
    if chars:
        rows.append((f"`inputText` in the browser per character ({chars} characters)",
                     per_run(lambda s: sum(s[i]["ms"] for i in type_steps) / chars), fmt_ms, " ms"))
    rows.append((f"`tapOn` and `assertVisible` in the browser, sum of {len(lookups)} steps",
                 per_run(lambda s: sum(s[i]["ms"] for i in lookups)), fmt_s, ""))
    rows.append(("submit tap until the first Flutter step passed",
                 per_run(lambda s: s[-2]["ms"] + s[-1]["ms"]), fmt_s, ""))
    rows.append(("outside Flutter, time", per_run(lambda s: sum(s[i]["ms"] for i in region)), fmt_s, ""))
    rows.append(("outside Flutter, share of the flow",
                 per_run(lambda s: 100 * sum(s[i]["ms"] for i in region) / sum(x["ms"] for x in s)),
                 lambda v: f"{statistics.median(v):.0f} %" if v else "n/a", ""))
    rows.append(("outside Flutter, share of the flow without `launchApp`",
                 per_run(lambda s: 100 * sum(s[i]["ms"] for i in region) / sum(x["ms"] for x in s[1:])),
                 lambda v: f"{statistics.median(v):.0f} %" if v else "n/a", ""))
    rows.append(("whole flow, sum of steps", per_run(lambda s: sum(x["ms"] for x in s)), fmt_s, ""))
    print("| | " + " | ".join(runners) + " |")
    print("|---|" + "---:|" * len(runners))
    for name, values, fmt, unit in rows:
        print(f"| {name} | " + " | ".join(fmt(v) + (unit if v else "") for v in values) + " |")
    print()

    if variant == "controlled":
        for runner in runners:
            counts = [e for m, _ in data[runner]["passed"] for e in events.get((runner, variant, m["label"]), [])]
            if counts:
                seen = sorted({(c["keydown"], c["input"], c["change"]) for c in counts})
                text = "; ".join(f"keydown {k}, input {i}, change {c}" for k, i, c in seen)
                print(f"- {runner}: events the page received before submit: {text}")
        print()
    # A median hides a run that waited out a timeout and still passed.
    outliers = []
    for runner in runners:
        for i, desc in enumerate(descs):
            values = [(m["label"], steps[i]["ms"]) for m, steps in data[runner]["passed"] if i < len(steps)]
            med = statistics.median(v for _, v in values)
            for label, v in values:
                if v >= 3 * med and v - med >= 3000:
                    outliers.append(f"- {runner} `{variant}` run {label}: step {i + 1} `{desc}` took {v:.0f} ms "
                                    f"(median {med:.0f} ms)")
    if outliers:
        print("Passed runs with a step far above its median:\n")
        print("\n".join(outliers) + "\n")
    if failures:
        print("\n".join(failures) + "\n")


def main(results):
    runs = load_runs(results)
    events, rejected = server_events(results)
    variants = []
    for r in runs:
        if r["variant"] not in variants:
            variants.append(r["variant"])
    print("Medians over the runs that passed. Lanes marked `*` are not in the JSON report "
          "(it has none for `assertVisible`) and follow from the step's place in the flow.\n")
    for variant in variants:
        if any(r["variant"] == variant and r["label"] != "warmup" for r in runs):
            summarize_variant(results, variant, runs, events, rejected)


if __name__ == "__main__":
    main(sys.argv[1])
