# Agent trial

The only benchmark that answers "does where-next save an agent work?". Offline retrieval gains do
not prove this on their own.

## Protocol

### Arms

1. **A:** the agent with its normal search tools.
2. **B:** the same agent, plus where-next hints from the fine-tuned model, with the tool's abstain rule.
3. **B2:** as B, but the start-of-task hint never abstains.
4. **C:** as B2, plus the per-repo adapter, frozen per task and fitted only on commits that are git
   ancestors of the task's base commit.

In the hint arms the agent gets one hint at the start, built from the task text, and a `where-next`
tool it may call whenever it chooses (no oracle timing). Hints are capped at 3 paths and 250 tokens.
Each arm runs in its own container, gold patches never reach the ranking service, and no arm's
observations train another.

Comparisons: **A vs B2** isolates hints, **B2 vs C** isolates the adapter, **B vs B2** isolates
abstention.

### Tasks

Issue-style tasks with executable verification from repositories held out from training. Three trials
ran: a 50-task pilot, a 50-task trial on large repositories, and a pre-declared confirmatory trial on new
tasks from repositories with at least 3,000 source files.

### Measures

Success (the benchmark's own tests), billed tokens and cost, cost per resolved task, wall time, search
and read calls, the step at which the agent first reads a file the real fix changed, and wrong-hint
detours. Failures and timeouts are included; savings are never reported only over successful runs.

### Pre-declared gate

At least 15% lower cost per resolved task, with the confidence interval excluding no improvement, and
no more than 2 points of success-rate harm.

### Leakage checks

- Every commit the adapter learns from is a git ancestor of the task's base commit.
- No history commit contains the fix (checked by patch-id and by overlap with the fix's added lines).
- The fix applies forward, and not in reverse, at every base commit, so bases are pre-fix.

## Pilot 1: SWE-bench Pro, cheap capable agent (September 2026)

50 tasks from the SWE-bench Pro public subset, 10 repositories × 5 tasks. One repository that appeared
in training data was excluded. The agent was a cheap, capable model in a fixed research harness
(about $0.09 per task, 120 steps, $2 cost cap). The research prototype served hints, at parity with
the tool: same pinned model, query text, adapter fitting, abstain rule and hint budget. The leakage
checks above all passed.

| Arm | Solved /50 | Cost per task | Cost per resolved task | Median wall time | Mean step of first correct-file read | Tasks where the agent called the tool |
|---|---|---|---|---|---|---|
| A: no hints | 32 | $0.085 | $0.133 | 440 s | 7.5 | – |
| B: hints, abstain rule | 32 | $0.095 | $0.149 | 584 s | 5.0 | 6 |
| B2: hints, no abstain | 31 | $0.086 | $0.139 | 451 s | 4.8 | 11 |
| C: hints + adapter | 31 | $0.090 | $0.145 | 453 s | 6.4 | 11 |

The first-correct-file column uses the original count, which missed the harness's own search and read
operations; recomputed, the B2 − A difference is about −0.9 steps (see the verdict).

Paired over the same 50 tasks (95% bootstrap intervals):

| Comparison | Success difference | Cost-per-resolved ratio |
|---|---|---|
| B vs A | 0.00 [−0.10, +0.10] | 1.12 [0.91, 1.38] |
| B2 vs A (hints) | −0.02 [−0.14, +0.10] | 1.05 [0.83, 1.34] |
| C vs A | −0.02 [−0.12, +0.08] | 1.09 [0.88, 1.36] |
| C vs B2 (adapter) | 0.00 [−0.12, +0.12] | 1.05 [0.82, 1.35] |

### Verdict

**The gate was not met by any arm.** Every cost point estimate went the wrong way, though all
intervals include no change.

- **Hints barely changed when the agent first read a correct file.** An early count said 2.6 steps
  sooner, but it missed the harness's own search and read operations; recomputed, the difference is
  about 0.9 steps. Either way, finding the file is not where the cost goes on these tasks: the cost is
  in understanding, editing and testing.
- **The agent rarely asked on its own** (6–11 of 50 tasks). Nearly all exposure came from the start hint.
- **The adapter showed no benefit here,** consistent with offline controls on similar issue-style tasks.
- **Abstention was miscalibrated.** The abstain rule withheld 24 of 50 start hints, and 20 of those would
  have been correct. Thresholds are being recalibrated per model and per query type.
- **The agent read at least one hinted file that wasn't changed by the fix in 28–30 of 50 tasks.** That
  is an upper bound on detours, since such files can still be useful context.

Sizing: detecting a true 15% cost reduction would take about 80 tasks, but the observed effect was +1%
to +12%. Bounding success harm within 2 points would take about 3,500 paired tasks (within 5 points,
about 560).

**What we conclude:** for a cheap, capable agent on SWE-bench Pro, where-next hints do not reduce cost
or time, and do not change success. We do not claim agent savings.

**Not tested in the pilot:** more expensive or slower agents, very large repositories, and people
navigating by hand. Large repositories were tested next.

Caveats: n = 50 with one attempt per arm; the first-correct-file and detour measures are heuristics over
shell commands; arm B2 ran concurrently with the other arms.

## Trial 2: large repositories (September 2026)

50 new tasks from the largest repositories the harness can run that weren't in training (median about
5,400 candidate files; babel, druid, lucene, a large web client, django and others). Arms A, B2 and D
(B2 plus hints injected after failed searches or long read streaks); same agent and settings as the
pilot.

| Comparison | Cost-per-resolved ratio | Solved |
|---|---|---|
| B2 vs A | 0.89 [0.67, 1.19] | 24 vs 25 |
| D vs A | 0.93 [0.68, 1.25] | 26 vs 25 |

B2 used fewer steps and reads, but the interval includes no change. Pooled with the pilot, tasks in
repositories with more than 3,000 files showed a ratio of 0.73 and a weak size trend (Spearman ρ −0.21,
interval −0.40 to −0.01). Hints after failed searches (D) added nothing over the start hint. This was
suggestive only, so it was tested in a confirmatory trial.

## Trial 3: confirmatory, repositories with at least 3,000 files (September 2026)

Pre-declared before any task was selected or run: population, arms (A vs B2), the primary endpoint (cost
per resolved task) and a success guard (no worse than −5 points). Every runnable SWE-style dataset was
screened; 58 new tasks from 17 repositories (3,038–13,649 candidate files, at most 20 per repository)
met the rule, fewer than the 120–150 target, so the trial is underpowered for a 10% effect. All 58 tasks
have a valid trial in both arms; timeouts count as outcomes.

| | A: no hints | B2: start hint |
|---|---|---|
| Solved | 30/58 | 27/58 |
| Cost per resolved task | $0.174 | $0.183 |
| Median tokens before first correct-file read | 18.6k | 10.2k |
| Median steps | 35 | 33 |
| Median wall time | 838 s | 726 s |

- **Cost per resolved task, B2 / A: 1.05** [0.82, 1.37] resampling tasks, [0.82, 1.50] resampling
  repositories.
- **Success, B2 − A: −5.2 points** [−13.8, +3.4]. That is just past the guard, but the interval includes
  no change.
- **The hint worked mechanically:** it contained a correct file in 31 of 58 tasks and roughly halved the
  tokens before the first correct-file read, but that didn't lower cost or raise success.
- **The size trend from trial 2 didn't replicate:** within these repositories, size didn't predict the
  cost change (ρ −0.03, interval −0.30 to +0.26).

## Conclusion across the three trials

For a cheap, capable agent, automatic start hints don't save cost or improve success at any repository
size tested. where-next stays an **opt-in** tool for agents (the agent called it on its own in 23 of 58
tasks in trial 3), with the start hint off by default, and is positioned as fast local navigation for
people. Not tested: more expensive agents, and people navigating by hand.
