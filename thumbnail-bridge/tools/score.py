#!/usr/bin/env python3
"""Compare plotmill reports with the sample ground truth. usage: score.py <out_dir> <samples_dir>"""
import json, sys, math
from pathlib import Path
out, smp = Path(sys.argv[1]), Path(sys.argv[2])
def rgb(h): return [int(h[i:i+2], 16) for i in (1, 3, 5)]
def cd(a, b): return math.dist(rgb(a), rgb(b))
tot = dict(lines=0, text=0, font=0, weight=0, size=0, fill=0, stroke=0, shadow=0)
for rp in sorted(out.glob('*.report.json')):
    r = json.loads(rp.read_text()); t = json.loads((smp / rp.name.replace('.report.json', '.truth.json')).read_text())
    print(f"--- {rp.stem}: person={r['person']} decorations={[d['kind'] for d in r['decorations']]}")
    for x in t['texts']:
        tot['lines'] += 1
        # match by text overlap of boxes
        bx = x['bbox']
        best = None
        for y in r['texts']:
            fb = y['fillBox']
            ix = max(0, min(bx[2], fb[2]) - max(bx[0], fb[0])) * max(0, min(bx[3], fb[3]) - max(bx[1], fb[1]))
            if ix > 0 and (best is None or ix > best[0]): best = (ix, y)
        if not best:
            print(f"  MISSING {x['text']}"); continue
        y = best[1]
        ok = lambda c: 'OK ' if c else 'NG '
        ct = y['text'] == x['text']; cf = y['font'] == x['family']; cw = abs(y['weight'] - x['weight']) < 150
        cs = abs(y['sizePx'] - x['size_px']) <= max(2, 0.04 * x['size_px']); cfill = cd(y['fill'], x['fill']) < 30
        cst = (x['stroke'] is None) == (y['stroke'] is None) and (x['stroke'] is None or (abs(x['stroke']['width'] - y['stroke']['width']) <= 2 and cd(x['stroke']['color'], y['stroke']['color']) < 40))
        csh = (x['shadow'] is None) == (y['shadow'] is None)
        for k, v in [('text', ct), ('font', cf), ('weight', cw), ('size', cs), ('fill', cfill), ('stroke', cst), ('shadow', csh)]:
            tot[k] += v
        print(f"  {ok(ct)}text {y['text']!r} (truth {x['text']!r}) | {ok(cf and cw)}font {y['font']} {y['weight']} (truth {x['family']} {x['weight']}) | {ok(cs)}size {y['sizePx']:.1f} ({x['size_px']}) | {ok(cfill)}fill {y['fill']} ({x['fill']}) | {ok(cst)}stroke {y['stroke']} ({x['stroke']}) | {ok(csh)}shadow {y['shadow']} ({x['shadow']})")
n = tot.pop('lines')
print('SUMMARY', {k: f'{v}/{n}' for k, v in tot.items()})
