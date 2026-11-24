"""Compare `maestro hierarchy` (iOS simulator) with `tapwing hierarchy` (iOS).

Usage: python3 -I bench/tools/diff_hierarchy_ios.py maestro.json tapwing.json

Maestro's iOS dump (`IOSDriver.mapViewHierarchy`) is
`root -> [application element, status bars, ...]`. Only the application
subtree is compared; the system keyboard (which XCUITest reports inside the
app window, on- or off-screen) is pruned. Every element carrying
accessibilityText / text / hintText / resource-id is compared as an ordered
multiset on (accessibilityText, text, hintText, resource-id, bounds, checked,
enabled, selected, focused), so duplicates and exact point bounds count.
"""
import collections
import json
import re
import sys

# Subtree roots of the system keyboard / input assistant in XCUITest dumps.
KEYBOARD_IDS = {"inputView", "SystemInputAssistantView", "CenterPageView", "UIKeyboardLayoutStar Preview"}


def parse_bounds(b):
    m = re.match(r"\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]", b or "")
    return tuple(int(x) for x in m.groups()) if m else None


def maestro_nodes(path):
    root = json.load(open(path))
    app = (root.get("children") or [root])[0]
    app_bounds = parse_bounds(app["attributes"].get("bounds"))
    screen_h = app_bounds[3] if app_bounds else 10**9
    out, pruned = [], 0

    def walk(n):
        nonlocal pruned
        a = n.get("attributes", {})
        b = parse_bounds(a.get("bounds"))
        if a.get("resource-id") in KEYBOARD_IDS or (b and b[1] >= screen_h):
            pruned += 1
            return
        key = (a.get("accessibilityText", ""), a.get("text", ""), a.get("hintText", ""), a.get("resource-id", ""))
        if any(key):
            out.append(key + (b, a.get("checked") == "true", a.get("enabled", "true") == "true",
                              a.get("selected") == "true", a.get("focused") == "true"))
        for c in n.get("children", []):
            walk(c)

    walk(app)
    return out, pruned


def tapwing_nodes(path):
    out = []

    def walk(n):
        key = (n.get("accessibilityText", ""), n.get("text", ""), n.get("hintText", ""), n.get("resource-id", ""))
        if any(key):
            out.append(key + (tuple(n["bounds"]), bool(n.get("checked")), n.get("enabled", True),
                              bool(n.get("selected")), bool(n.get("focused"))))
        for c in n.get("children", []):
            walk(c)

    walk(json.load(open(path))["root"])
    return out


def main():
    m, pruned = maestro_nodes(sys.argv[1])
    k = tapwing_nodes(sys.argv[2])
    cm, ck = collections.Counter(m), collections.Counter(k)
    only_m, only_k = cm - ck, ck - cm
    fmt = lambda x: f"{x[:4]} bounds={x[4]} checked={x[5]} enabled={x[6]} selected={x[7]} focused={x[8]}"
    for x in only_m.elements():
        print("ONLY MAESTRO", fmt(x))
    for x in only_k.elements():
        print("ONLY TAPWING", fmt(x))
    same = sum((cm & ck).values())
    order = "same order" if [x for x in m if x in ck] == [x for x in k if x in cm] else "DIFFERENT order"
    print(f"{same} identical, {sum(only_m.values())} only-maestro, {sum(only_k.values())} only-tapwing "
          f"(maestro {len(m)} app nodes, {pruned} keyboard/offscreen subtrees pruned; tapwing {len(k)}; {order})")
    return 0 if not only_m and not only_k else 1


if __name__ == "__main__":
    sys.exit(main())
