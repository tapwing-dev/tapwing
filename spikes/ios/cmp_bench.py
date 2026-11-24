import json, os, statistics, sys
S = sys.argv[1]
med = statistics.median
mruns = []
for i in (1, 2, 3):
    for root, _, files in os.walk(f'{S}/maestro-{i}'):
        for f in files:
            if f.startswith('commands') and f.endswith('.json'):
                c = json.load(open(os.path.join(root, f)))
                mruns.append([((x['metadata'].get('duration') or 0), x['metadata'].get('status')) for x in c])
kruns, names = [], None
for i in (1, 2, 3):
    r = json.load(open(f'{S}/tapwing-{i}.json'))
    kruns.append([s['ms'] for s in r['flows'][0]['steps']])
    names = [s['description'] for s in r['flows'][0]['steps']]
print('maestro runs', len(mruns), [len(r) for r in mruns], [r[-1][1] for r in mruns])
m = [med(d for d, _ in c) for c in zip(*mruns)]
k = [med(c) for c in zip(*kruns)]
print('%-40s %8s %8s %7s' % ('command', 'maestro', 'tapwing', 'x'))
for n, a, b in zip(names, m, k):
    print('%-40s %8.0f %8.1f %7.1f' % (n[:40], a, b, a / b if b else 0))
print('sum %.1f s vs %.1f s (%.1fx); without launchApp %.1f s vs %.1f s (%.1fx)' % (sum(m) / 1000, sum(k) / 1000, sum(m) / sum(k), sum(m[1:]) / 1000, sum(k[1:]) / 1000, sum(m[1:]) / sum(k[1:])))
sp = sorted(a / b for a, b in zip(m[1:], k[1:]) if b)
print('median per-command %.1fx' % med(sp))
