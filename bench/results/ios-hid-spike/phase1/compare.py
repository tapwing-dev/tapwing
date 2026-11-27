"""Phase 1, experiment 3: compare the XCUITest runner's raw /viewHierarchy with the AXPTranslator tree.
Both sides are mapped to the Maestro attributes that selectors use, following
crates/tapwing/src/ios_native_lane/hierarchy.rs (runner side) and the field table in summary.md
(probe side). Usage: python3 -I compare.py <exp3-dir> <screen> [--dump]"""
import json, sys, collections

CHECKABLE = {12, 40, 41}
ALERT, KEYBOARD = 7, 19
W, H = 440, 956

def trunc(f, landscape=False):
    x, y, w, h = f
    return (int(x), int(y), int(x + w), int(y + h))

def ne(s):
    return s if s else ""

def runner_nodes(path):
    d = json.load(open(path))
    out = []
    def walk(n, path):
        f = n.get("frame") or {}
        fr = (f.get("X", 0), f.get("Y", 0), f.get("Width", 0), f.get("Height", 0))
        text = ne(n.get("title")) or ne(n.get("value") if isinstance(n.get("value"), str) else None)
        out.append(dict(
            src="runner", etype=n["elementType"], acc=ne(n.get("label")), text=text, hint=ne(n.get("placeholderValue")),
            id=ne(n.get("identifier")), enabled=n["enabled"], selected=n["selected"], focused=n.get("hasFocus", False),
            checked=n["elementType"] in CHECKABLE and n.get("value") == "1", frame=fr, bounds=trunc(fr),
            value=n.get("value"), title=n.get("title"), path=path))
        for c in n["children"]:
            walk(c, path + [n["elementType"]])
    walk(d["axElement"], [])
    return out

def probe_nodes(path):
    d = json.load(open(path))
    out = []
    def walk(n, path):
        fr = tuple(n["frame"])
        val = n.get("value")
        text = ne(n.get("title")) or ne(val)
        out.append(dict(
            src="probe", role=n["role"], acc=ne(n.get("label")), text=text, hint=ne(n.get("placeholder")),
            id=ne(n.get("identifier")), enabled=n["enabled"], selected=n["selected"], focused=n["focused"],
            checked=n["role"] in ("AXSwitch", "AXCheckBox", "AXToggle") and val == "1", frame=fr, bounds=trunc(fr),
            value=val, title=n.get("title"), path=path, pid=n.get("pid")))
        for c in n["children"]:
            walk(c, path + [n["role"]])
    for r in d["roots"]:
        walk(r, [])
    return out

def keyed(nodes):
    return [n for n in nodes if n["acc"] or n["text"] or n["hint"] or n["id"]]

def kb_runner(nodes):
    # drop elements below the screen edge (off-screen keyboard parts) and keyboard subtree like the bench diff tool
    return nodes

def main():
    base, screen = sys.argv[1], sys.argv[2]
    r = runner_nodes(f"{base}/{screen}.runner1.json")
    p = probe_nodes(f"{base}/{screen}.probe1.json")
    rk, pk = keyed(r), keyed(p)
    print(f"== {screen}: runner nodes {len(r)} (with a selector attribute {len(rk)}), probe nodes {len(p)} (with {len(pk)})")
    def key(n): return (n["acc"], n["text"], n["hint"], n["id"])
    def full(n): return key(n) + (n["bounds"], n["checked"], n["enabled"], n["selected"], n["focused"])
    for name, fn in (("exact (all selector attributes + bounds + states)", full), ("attributes only", key)):
        rc, pc = collections.Counter(map(fn, rk)), collections.Counter(map(fn, pk))
        both = sum((rc & pc).values())
        print(f"  {name}: matched {both}, only runner {sum((rc - pc).values())}, only probe {sum((pc - rc).values())}")
    # Per-label comparison on the accessibility text, which is what tapOn "text" matches.
    def by_acc(nodes):
        m = collections.defaultdict(list)
        for n in nodes:
            if n["acc"]: m[n["acc"]].append(n)
        return m
    ra, pa = by_acc(rk), by_acc(pk)
    print("  accessibilityText only in runner:", sorted(set(ra) - set(pa)))
    print("  accessibilityText only in probe :", sorted(set(pa) - set(ra)))
    diffs = collections.Counter()
    for label in sorted(set(ra) & set(pa)):
        a, b = ra[label][0], pa[label][0]
        if len(ra[label]) != len(pa[label]): diffs["count differs"] += 1; print(f"   count differs for {label!r}: runner {len(ra[label])}, probe {len(pa[label])}")
        for f in ("text", "hint", "id", "bounds", "enabled", "selected", "focused", "checked"):
            if a[f] != b[f]:
                diffs[f] += 1
                print(f"   {label!r}: {f}: runner={a[f]!r} probe={b[f]!r}")
    print("  attribute difference counts on shared labels:", dict(diffs))
    if "--dump" in sys.argv:
        for n in rk: print("R", n["etype"], key(n), n["bounds"], n["enabled"], n["selected"], n["focused"])
        for n in pk: print("P", n["role"], key(n), n["bounds"], n["enabled"], n["selected"], n["focused"], n.get("value"))
if __name__ == "__main__":
    main()
