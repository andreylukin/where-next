import json, os, sys
res, sel = sys.argv[1], sys.argv[2]
its = {x['issue']: x for x in json.load(open(sel))}
rows = json.load(open(res))
grp = {True: [], False: []}
for r in rows:
    it = its[r['issue']]; txt = (it['title'] or '') + (it['body'] or '')
    m = any(os.path.basename(g) in txt for g in r['gold'])
    grp[m].append(r)
keys = ("wn_title_hit","wn_title_noadapt_hit","wn_full_hit","rg_hit","rg_title_hit")
for m in (False, True):
    g = grp[m]; print("names-gold-file" if m else "no-file-named", len(g), {k: sum(r[k] for r in g) for k in keys})
print("all", len(rows), {k: sum(r[k] for r in rows) for k in keys})
g = grp[False]
print("no-file-named: wn_full only", sum(r['wn_full_hit'] and not r['rg_hit'] for r in g), "rg only", sum(r['rg_hit'] and not r['wn_full_hit'] for r in g), "either", sum(r['rg_hit'] or r['wn_full_hit'] for r in g))
print("all: wn_title only-vs-rg_title", sum(r['wn_title_hit'] and not r['rg_title_hit'] for r in rows), sum(r['rg_title_hit'] and not r['wn_title_hit'] for r in rows))
ms = sorted(r['wn_title_ms'] for r in rows[1:]); print("warm title ms median", ms[len(ms)//2], "max", ms[-1]); ms = sorted(r['wn_full_ms'] for r in rows); print("full ms median", ms[len(ms)//2])
ms = sorted(r['rg_ms'] for r in rows); print("rg (multi-term) ms median", ms[len(ms)//2])
