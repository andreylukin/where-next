import json, subprocess, sys, time, os, re
sys.path.insert(0, os.path.dirname(__file__))
import rgbase
repo_dir, issues_json, out = sys.argv[1], sys.argv[2], sys.argv[3]
env = dict(os.environ)
def is_src(f):
    if re.search(r'(^|/)(tests?|testdata|__tests__|fixtures?)(/|$)', f) or re.search(r'_test\.go$|\.test\.js$|-test\.js$|\.tcl$', f): return False
    if re.search(r'\.(md|rst|txt|json)$', f) or f.startswith('docs/') or 'CHANGELOG' in f or 'CHANGES' in f or re.search(r'(^|/)(go\.(mod|sum|work\.sum)|\.import-restrictions)$|zz_generated|generated\.proto|\.pb\.go$', f): return False
    return os.path.exists(os.path.join(repo_dir, f))
def wn(q, extra=()):
    t = time.time()
    p = subprocess.run(["wn","ask","--json","--no-log","--path",repo_dir,*extra,q], capture_output=True, text=True, env=env)
    dt = time.time()-t
    try: d = json.loads(p.stdout)
    except Exception: d = {"state":"badjson","raw":p.stdout[:300]+p.stderr[:300]}
    return d, dt
rows = []
for it in json.load(open(issues_json)):
    gold = [f for f in it["files"] if is_src(f)]
    if not gold: continue
    body = re.sub(r'<!--.*?-->', '', it["body"] or '', flags=re.S)
    full = (it["title"] + "\n\n" + body)[:2000]
    r = dict(issue=it["issue"], title=it["title"], gold=gold)
    for name, q in (("title", it["title"]), ("full", full)):
        d, dt = wn(q)
        files = [f["path"] for f in d.get("files", [])]
        r[f"wn_{name}"] = files; r[f"wn_{name}_state"] = d.get("state"); r[f"wn_{name}_ms"] = round(dt*1000)
        r[f"wn_{name}_hit"] = any(f in gold for f in files[:3])
        if name == "title":
            d2, _ = wn(q, ("--no-adapter",)); f2 = [f["path"] for f in d2.get("files", [])]
            r["wn_title_noadapt_hit"] = any(f in gold for f in f2[:3])
    t = time.time(); terms, rgf = rgbase.rank(repo_dir, full); r["rg_ms"] = round((time.time()-t)*1000)
    r["rg_terms"] = terms; r["rg"] = rgf; r["rg_hit"] = any(f in gold for f in rgf[:3])
    tt, rgt = rgbase.rank(repo_dir, it["title"]); r["rg_title"] = rgt; r["rg_title_terms"] = tt; r["rg_title_hit"] = any(f in gold for f in rgt[:3])
    rows.append(r)
    print(r["issue"], "wn_title", r["wn_title_hit"], "wn_full", r["wn_full_hit"], "rg", r["rg_hit"], "rg_title", r["rg_title_hit"], r["wn_title_ms"], file=sys.stderr, flush=True)
json.dump(rows, open(out, "w"), indent=1)
n = len(rows)
for k in ("wn_title_hit","wn_title_noadapt_hit","wn_full_hit","rg_hit","rg_title_hit"):
    print(k, sum(r[k] for r in rows), "/", n)
