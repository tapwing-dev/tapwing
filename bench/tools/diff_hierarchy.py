"""Compare Maestro's `maestro hierarchy` JSON with `tapwing hierarchy` JSON.

Only nodes carrying text/accessibilityText/hintText/resource-id are compared,
keyed by (attribute values), and bounds are compared per key.
"""
import json, re, sys

def maestro_nodes(path):
    out = []
    def walk(n):
        a = n.get('attributes', {})
        b = a.get('bounds')
        key = (a.get('text', ''), a.get('accessibilityText', ''), a.get('hintText', ''), a.get('resource-id', ''))
        if b and any(key) and not key[3].startswith(('android:', 'com.android.')):
            m = re.match(r'\[(\d+),(\d+)\]\[(\d+),(\d+)\]', b)
            out.append((key, tuple(int(x) for x in m.groups()), a.get('clickable') == 'true',
                        a.get('checked') == 'true', a.get('enabled', 'true') == 'true'))
        for c in n.get('children', []):
            walk(c)
    walk(json.load(open(path)))
    return out

def tapwing_nodes(path):
    out = []
    def walk(n):
        key = (n.get('text', ''), n.get('accessibilityText', ''), n.get('hintText', ''), n.get('resource-id', ''))
        if any(key):
            out.append((key, tuple(n['bounds']), bool(n.get('clickable')), bool(n.get('checked')), n.get('enabled', True)))
        for c in n.get('children', []):
            walk(c)
    walk(json.load(open(path))['root'])
    return out

m, k = maestro_nodes(sys.argv[1]), tapwing_nodes(sys.argv[2])
mk = {x[0]: x for x in m}
kk = {x[0]: x for x in k}
same = mismatch = 0
for key in sorted(set(mk) | set(kk), key=str):
    a, b = mk.get(key), kk.get(key)
    if a and b and a[1:] == b[1:]:
        same += 1
    else:
        mismatch += 1
        print('DIFF', key, '\n   maestro:', a and a[1:], '\n   tapwing:', b and b[1:])
print(f'{same} identical, {mismatch} different (maestro {len(m)} nodes, tapwing {len(k)} nodes)')
