import json, sys
mf, kf = sys.argv[1], sys.argv[2]
def m(n, out):
    a = n.get('attributes', {})
    v = {k: a[k] for k in ('accessibilityText', 'title', 'value', 'text', 'hintText', 'resource-id', 'bounds', 'focused', 'checked', 'selected') if a.get(k) not in (None, '', 'false')}
    if any(a.get(k) for k in ('accessibilityText', 'value', 'text', 'hintText', 'resource-id')) and not any(w in str(v) for w in ('battery', 'Wi-Fi', 'Mobile Service', 'No signal')):
        out.append(v)
    for c in n.get('children', []): m(c, out)
def k(n, out):
    v = {x: n[x] for x in ('text', 'hintText', 'accessibilityText', 'resource-id', 'bounds', 'focused', 'checked', 'selected') if n.get(x) not in (None, '', False)}
    if any(n.get(x) for x in ('text', 'hintText', 'accessibilityText', 'resource-id')): out.append(v)
    for c in n.get('children', []): k(c, out)
mo = []; m(json.load(open(mf)), mo)
ko = []; k(json.load(open(kf))['root'], ko)
print('MAESTRO (iOS)'); [print('  ', x) for x in mo]
print('TAPWING (px)'); [print('  ', x) for x in ko]
