# Issue titles: 102 real closed issues

Does pasting a bug report's title into `wn ask` point at the file the real fix changed? And how does
that compare with grepping the identifiers in the report? Run on September 29, 2026.

## Protocol

- **Issues.** Recently closed GitHub issues from kubernetes, redis and react whose closing pull
  request was merged, fetched with `gh issue view` and `gh pr view --json files,mergeCommit`.
- **No leakage.** Each repository was indexed at a commit from **before the earliest fix** in its set,
  so neither the index nor the git-history adapter had seen any of the fixes. Issues fixed before that
  commit were dropped.

  | Repository | Indexed commit | Date | Issues |
  |---|---|---|---|
  | kubernetes/kubernetes | `94c1367` | 2026-08-08 | 42 |
  | redis/redis | `9302d27` | 2026-05-11 | 32 |
  | facebook/react | `f4e0d4e` | 2026-04-29 | 28 |

- **Gold.** The non-test, non-doc files the fixing pull request changed that exist at the indexed
  commit. Tests, fixtures, `*.md`, `*.json`, `go.mod`/`go.sum` and generated files are excluded.
- **Score.** hit@3: at least one gold file is in the top 3.
- **Queries.** "Title" is only the issue title, what you would type. "Full" is the title plus the body,
  capped at 2,000 characters.
- **wn.** `wn 0.0.1 (93729a1)`, model `gemma-xl1`, default flags (adapter on, default abstain),
  `wn ask --json --no-log`. "No adapter" adds `--no-adapter`.
- **grep baseline (rg-src).** Automated, so it can be rerun: pull the code-like tokens out of the text
  (backticked spans, snake_case, camelCase, CamelCase, dotted names, UPPER_CASE; at most 12), run
  `rg -c -F` for each, and rank files by the number of distinct tokens matched, then by total matches.
  Tests, fixtures, docs and JSON are excluded from the search. This mimics a developer grepping the
  identifiers in a report.

## Results

| Repository (issues) | wn, title | wn, title, no adapter | wn, full | rg-src, title | rg-src, full | wn or rg-src, full |
|---|---|---|---|---|---|---|
| kubernetes (42) | **23 (55%)** | 23 | **25 (60%)** | 7 (17%) | 21 (50%) | 29 (69%) |
| redis (32) | 17 (53%) | 14 | 24 (75%) | 14 (44%) | **26 (81%)** | 29 (91%) |
| react (28) | 10 (36%) | 4 | 7 (25%) | 11 (39%) | **14 (50%)** | 16 (57%) |
| **all (102)** | **50 (49%)** | 41 (40%) | 56 (55%) | 32 (31%) | **61 (60%)** | 74 (73%) |

- **Title only:** wn 49% vs 31% for grepping the identifiers in the title; on kubernetes 55% vs 17%.
- **Full report:** grep is ahead, 60% vs 55%.
- **Together:** a hit from either wn or rg-src on the full report reaches 73%.
- **Top 1, title only:** kubernetes 18/42, redis 13/32, react 2/28.
- **The adapter** adds 9 hits overall, mostly on react (4 → 10) and redis (14 → 17), and none on
  kubernetes. The kubernetes checkout was shallow, so its adapter saw only 200 commits.

### Latency

- wn, title, warm: median 48 ms (kubernetes), 54 ms (react), 59 ms (redis).
- wn, full body, warm: 100–180 ms median. The cold first call takes 4–8 s.
- rg-src (about 12 greps per issue): median 4.1 s on kubernetes. A single `rg -li` over kubernetes
  takes 0.36–0.48 s.

### Where wn wins

A plain-English title with no unique identifier, in a large repository. On kubernetes, title only,
wn hit and rg-src missed on 20 issues; the reverse happened on 4. Examples, each ranked #1 at the
pre-fix commit:

- "Succeeded pods cause incorrect HPA calculation" → `pkg/controller/podautoscaler/replica_calculator.go`
  (`rg -li hpa` matches 532 files).
- "apimachinery yaml: last line silently dropped when its length is a multiple of the buffer size" →
  `staging/.../util/yaml/decoder.go` (`rg -li yaml` matches 1,226 files).
- "kube-proxy (winkernel) panics with "index out of range [0]" when an HNS load balancer has no ports"
  → `pkg/proxy/winkernel/hns.go`.
- "CPU Manager incorrectly spans sockets with distribute-cpus-across-cores and align-by-socket" →
  `pkg/kubelet/cm/cpumanager/cpu_assignment.go`.
- react: "Controlled number input does not update defaultValue when value prop has changed" →
  `ReactDOMInput.js`.

### Where grep wins

- **The report names a unique identifier:** `handleClientsBlockedOnKey()` use-after-free,
  `PodsByCreationTime`, `onAllReady called after onShellError`, `ToggleEvent is missing source property`,
  `FragmentInstance.compareDocumentPosition`.
- **Long bodies full of identifiers and stack traces:** redis full 81% vs 75%, react 50% vs 25%. In 20
  of the 32 redis issues and 22 of the 42 kubernetes issues, the report itself already names a changed
  file.
- **React is wn's weakest repository.** Long repro-code bodies hurt the full query (25%), and wn ranks
  `fixtures/` and devtools demo files highly.

So use both: wn for a description, `rg` for a name.

## Newcomer "where is X" questions

27 hand-written questions over flask, redis, react and kubernetes, with the gold files judged by one
person. The grep side used one natural keyword per question (for example `eviction`, `iptables`,
`useEffect`, `failover`), with files ranked by match count. Some keywords were expert picks
(`multibulk`, `epoll|kqueue`, `reconcileChildren`), which favors grep.

| | Top-3 hit | Notes |
|---|---|---|
| wn | **19/27 (70%)** | 30–50 ms warm; always at most 3 lines |
| rg, one keyword, by count | 15/27 top 3, 22/27 top 10 | median 64 matching files to scan (up to 3,055 for `apply` in kubernetes) |

- wn wins: "how does a Deployment do a rolling update" (wn #1; rg ranks `rolling.go` 115th of 420),
  "where does kubelet evict pods under memory pressure" (wn #1, rg #5 of 617), "kubectl apply's
  three-way merge" (wn #1, rg #7 of 3,055), and all 5 flask questions.
- wn misses: "where is useEffect implemented" (react; returns devtools/ART files), redis RDB background
  save, RESP parsing, the event loop, "sorted sets" (returns `t_set.c` instead of `t_zset.c`), and the
  react scheduler queue, event dispatch and Fizz entry. Most of these live in very large core files
  (`ReactFiberHooks.js` 176 KB, `rdb.c` 222 KB, `networking.c` 250 KB).

## Limitations

- **One run, one model:** `gemma-xl1` as of 2026-09-29, a single pass with no repeats and no
  confidence intervals. With 102 issues, a difference of a few points is noise.
- **Three repositories** for the issue set, each with a few dozen issues.
- **The title-only grep baseline is a floor.** When a title contains no identifier, rg-src returns
  nothing; a person would guess a keyword instead. So the title-only grep numbers understate what grep
  can do by hand. The newcomer table covers that side with hand-picked keywords.
- **Gold is the files one pull request changed**, which is not the only reasonable answer.
- **The newcomer set is small and hand-judged,** and its checkouts' commits were not recorded
  separately.

## Reproducing

The scripts are in [issue-titles/](issue-titles/), copied as they ran (set `WN_EVAL_DIR` to the directory holding the repository clones; other paths
may need editing). They They need `gh`, `rg`, Python 3 and `wn` on `PATH`.

- [`fetch.py`](issue-titles/fetch.py) `<owner/repo> <out.json> <issue numbers…>`: fetches each issue
  and its merged closing pull request's changed files.
- [`run_issues.py`](issue-titles/run_issues.py) `<checkout> <issues.json> <out.res.json>`: runs wn
  (title, full, title without adapter) and the unfiltered grep baseline per issue, and scores hit@3.
- [`rgbase.py`](issue-titles/rgbase.py): the grep baseline (token extraction and ranking).
- [`addrgsrc.py`](issue-titles/addrgsrc.py) `<checkout> <res.json> <issues.json>`: adds the rg-src
  (source-only) baseline quoted above.
- [`summ.py`](issue-titles/summ.py) `<res.json> <issues.json>`: summary counts and latency medians.
- [`newcomer.py`](issue-titles/newcomer.py): the 27 newcomer questions, keywords and gold files.
- [`issues.json`](issue-titles/issues.json): the 102 scored issues (repository, issue, fixing pull
  request, gold files, and each method's hit), so the table above can be recounted without rerunning.
