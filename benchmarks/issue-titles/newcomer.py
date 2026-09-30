import json, subprocess, time, os, sys
S = "/private/tmp/claude-501/-Users-andrey/fa95657f-fe28-42fa-84cd-3edf5bf845ed/scratchpad/edge"
R = {"flask": S+"/repos/flask", "redis": S+"/repos/redis", "react": S+"/repos/react", "k8s": "/private/tmp/wnbench/kubernetes"}
Q = [
 ("flask","where is the session cookie signed","session",["src/flask/sessions.py"]),
 ("flask","how does `flask run` find my app","find_app|locate_app",["src/flask/cli.py"]),
 ("flask","where are 404 and 500 error handlers looked up","errorhandler",["src/flask/app.py","src/flask/sansio/app.py","src/flask/sansio/scaffold.py"]),
 ("flask","where are static files served from","static",["src/flask/app.py","src/flask/sansio/app.py","src/flask/helpers.py","src/flask/sansio/scaffold.py"]),
 ("flask","how are JSON responses serialized","json",["src/flask/json/provider.py","src/flask/json/__init__.py"]),
 ("redis","where is the LRU/LFU eviction policy implemented","eviction",["src/evict.c"]),
 ("redis","how does redis write RDB snapshots in the background","bgsave",["src/rdb.c"]),
 ("redis","where are expired keys actively deleted","expire",["src/expire.c","src/db.c"]),
 ("redis","how does the AOF rewrite work","rewrite",["src/aof.c"]),
 ("redis","where is the RESP protocol parsed from client sockets","multibulk",["src/networking.c"]),
 ("redis","where are cluster failovers handled","failover",["src/cluster_legacy.c"]),
 ("redis","how does the event loop wait for sockets","epoll|kqueue",["src/ae.c","src/ae_epoll.c","src/ae_kqueue.c"]),
 ("redis","where are sorted sets implemented","zset",["src/t_zset.c"]),
 ("react","where is useEffect implemented","useEffect",["packages/react-reconciler/src/ReactFiberHooks.js"]),
 ("react","how does reconciliation diff children with keys","reconcileChildren",["packages/react-reconciler/src/ReactChildFiber.js"]),
 ("react","where are browser events dispatched to React handlers","dispatchEvent",["packages/react-dom-bindings/src/events/DOMPluginEventSystem.js","packages/react-dom-bindings/src/events/ReactDOMEventListener.js"]),
 ("react","where is a hydration mismatch detected","hydration",["packages/react-reconciler/src/ReactFiberHydrationContext.js"]),
 ("react","where is the scheduler's task queue","scheduler",["packages/scheduler/src/forks/Scheduler.js","packages/scheduler/src/SchedulerMinHeap.js"]),
 ("react","where does streaming server rendering start","renderToPipeableStream|renderToReadableStream",["packages/react-server/src/ReactFizzServer.js","packages/react-dom/src/server/ReactDOMFizzServerNode.js","packages/react-dom/src/server/ReactDOMFizzServerBrowser.js"]),
 ("k8s","where does kubelet evict pods under memory pressure","eviction",["pkg/kubelet/eviction/eviction_manager.go","pkg/kubelet/eviction/helpers.go"]),
 ("k8s","how does kube-proxy program iptables rules","iptables",["pkg/proxy/iptables/proxier.go"]),
 ("k8s","where are pod specs validated","ValidatePod",["pkg/apis/core/validation/validation.go"]),
 ("k8s","where does the scheduler pick a node for a pod","schedulePod|SchedulePod",["pkg/scheduler/schedule_one.go"]),
 ("k8s","how does a Deployment do a rolling update","rolling",["pkg/controller/deployment/rolling.go"]),
 ("k8s","how does the garbage collector cascade deletes via ownerReferences","ownerReferences",["pkg/controller/garbagecollector/garbagecollector.go","pkg/controller/garbagecollector/graph_builder.go"]),
 ("k8s","where does the HPA compute desired replicas","desiredReplicas|DesiredReplicas",["pkg/controller/podautoscaler/replica_calculator.go","pkg/controller/podautoscaler/horizontal.go"]),
 ("k8s","where is kubectl apply's three-way merge","apply",["staging/src/k8s.io/kubectl/pkg/cmd/apply/apply.go","staging/src/k8s.io/apimachinery/pkg/util/strategicpatch/patch.go"]),
]
rows = []
for repo, q, kw, gold in Q:
    t = time.time(); p = subprocess.run(["wn","ask","--json","--no-log","--path",R[repo],q], capture_output=True, text=True); dt = time.time()-t
    d = json.loads(p.stdout); files = [f["path"] for f in d.get("files",[])]
    t = time.time(); o = subprocess.run(["rg","-c","-i","--no-messages","-e",kw,"."], cwd=R[repo], stdin=subprocess.DEVNULL, capture_output=True, text=True).stdout; rdt = time.time()-t
    cnt = sorted(((int(l.rpartition(":")[2]), l.rpartition(":")[0].removeprefix("./")) for l in o.splitlines()), reverse=True)
    rgfiles = [f for _, f in cnt]
    grank = min([rgfiles.index(g)+1 for g in gold if g in rgfiles] or [None], key=lambda x: x or 10**9)
    wrank = min([files.index(g)+1 for g in gold if g in files] or [None], key=lambda x: x or 10**9)
    rows.append(dict(repo=repo, q=q, kw=kw, gold=gold, wn=files, wn_state=d.get("state"), wn_rank=wrank, wn_ms=round(dt*1000), rg_n=len(rgfiles), rg_top3=rgfiles[:3], rg_rank=grank, rg_ms=round(rdt*1000)))
    print(f"{repo:6} wn@{wrank} rg@{grank}/{len(rgfiles)} {round(dt*1000)}ms | {q}", flush=True)
json.dump(rows, open(S+"/eval/newcomer.res.json","w"), indent=1)
print("wn top3", sum(1 for r in rows if r['wn_rank']), "/", len(rows), " rg top3", sum(1 for r in rows if r['rg_rank'] and r['rg_rank']<=3), " rg top10", sum(1 for r in rows if r['rg_rank'] and r['rg_rank']<=10), " rg median files", sorted(r['rg_n'] for r in rows)[len(rows)//2])
