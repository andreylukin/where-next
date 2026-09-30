import json, sys, os, re
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__))); import rgbase
repo, res, sel = sys.argv[1:4]
its = {x['issue']: x for x in json.load(open(sel))}
rows = json.load(open(res))
for r in rows:
    it = its[r['issue']]; body = re.sub(r'<!--.*?-->', '', it['body'] or '', flags=re.S)
    full = (it['title'] + "\n\n" + body)[:2000]
    _, f = rgbase.rank(repo, full, src_only=True); r['rgsrc'] = f; r['rgsrc_hit'] = any(x in r['gold'] for x in f)
    _, f = rgbase.rank(repo, it['title'], src_only=True); r['rgsrc_title'] = f; r['rgsrc_title_hit'] = any(x in r['gold'] for x in f)
json.dump(rows, open(res, 'w'), indent=1)
print(res, 'rgsrc', sum(r['rgsrc_hit'] for r in rows), 'rgsrc_title', sum(r['rgsrc_title_hit'] for r in rows), '/', len(rows))
