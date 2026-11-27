"""Lists the elements (with a selector attribute) that only one side has. Usage: python3 -I unmatched.py <dir> <screen>..."""
import collections, importlib.util, sys
spec = importlib.util.spec_from_file_location("cmp", __file__.replace("unmatched.py", "compare.py"))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
base = sys.argv[1]
for screen in sys.argv[2:]:
    r = m.keyed(m.runner_nodes(f"{base}/{screen}.runner1.json")); p = m.keyed(m.probe_nodes(f"{base}/{screen}.probe1.json"))
    key = lambda n: (n["acc"], n["text"], n["hint"], n["id"], n["bounds"])
    rc = collections.Counter(map(key, r)); pc = collections.Counter(map(key, p))
    print("==", screen)
    for n in r:
        if rc[key(n)] > pc[key(n)]: print(" only runner", n["etype"], key(n), n["value"] or "")
    for n in p:
        if pc[key(n)] > rc[key(n)]: print(" only probe ", n["role"], key(n), n["value"] or "")
