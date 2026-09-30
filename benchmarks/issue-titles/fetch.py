import json, subprocess, sys
repo, out = sys.argv[1], sys.argv[2]
nums = sys.argv[3:]
res = []
for n in nums:
    iss = json.loads(subprocess.check_output(["gh","issue","view",n,"-R",repo,"--json","number,title,body,closedByPullRequestsReferences,closedAt"]))
    prs = [p for p in iss.get("closedByPullRequestsReferences") or [] if ("/%s/pull/"%repo) in p.get("url","")]
    if not prs:
        print("no PR", n, file=sys.stderr); continue
    pr = prs[-1]["number"]
    try: pj = json.loads(subprocess.check_output(["gh","pr","view",str(pr),"-R",repo,"--json","number,files,mergeCommit,state,baseRefName"]))
    except Exception: print("pr fail",n,file=sys.stderr); continue
    if pj["state"] != "MERGED":
        print("not merged", n, pr, file=sys.stderr); continue
    res.append(dict(issue=iss["number"], title=iss["title"], body=iss["body"], pr=pr,
                    merge=pj["mergeCommit"]["oid"] if pj["mergeCommit"] else None, base=pj["baseRefName"],
                    files=[f["path"] for f in pj["files"]]))
    print(n, pr, pj["baseRefName"], [f["path"] for f in pj["files"]][:8], file=sys.stderr)
json.dump(res, open(out,"w"), indent=1)
