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
tasks from repositories with at least 3,000 source files. Trials 4 and 5 reuse the confirmatory tasks with
a different design (forced hints and an oracle arm), described in their own sections.

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

## Trial 4: oracle headroom, Codex (September 30, 2026)

Trials 1–3 asked whether wn's hints save cost. Trial 4 asks a prior question: would even a **perfect**
hint help? If not, navigation isn't the agent's bottleneck and no hint tool can help it.

**Pre-declared** (written before any run, index build or hint computation):

- **Gate 1, headroom.** The oracle arm resolves at least 8 points more than no hint, or its median wall
  time is at most 0.8× no hint's (point estimates; intervals reported alongside). If neither holds, the
  verdict is "no headroom: navigation isn't this agent's bottleneck", and wn's arm is reported
  descriptively only.
- **Gate 2, wn's share of the gap** (only if gate 1 passes): (W − A) / (O − A) for success, and the
  matching ratio for wall time.

**Setup:**

- **Tasks:** the 58 confirmatory tasks from trial 3 (17 repositories, each with at least 3,000 files).
  None excluded.
- **Agent:** Codex CLI (`codex exec`) with `gpt-6-luna`, reasoning effort medium, on Harbor/Modal, one
  attempt per task per arm.
- **Cap:** 600 s of agent wall time. On a timeout the tests still run on the final state, so the
  outcome is "resolved within 600 s".
- **Arms:**
  - **A:** no hint, the task text unchanged.
  - **W:** wn's top 3 files (`wn ask --start`, `gemma-xl1` with its shipped calibration and the
    per-repo adapter fitted only on ancestors of the task's base commit), computed off the container at
    the base commit. If wn abstained, W gets A's prompt; it didn't abstain on any task.
  - **O (oracle):** the files the gold patch edits that exist at the base commit, top 3 by lines
    changed.
- **Hint delivery:** prepended to the task prompt, with identical wording in W and O (only the paths
  differ, no scores):

  ```
  Hint: these files are likely relevant to this task (verify before relying on them):
  - <path 1>
  - <path 2>
  - <path 3>
  ```

  The hint is forced, so this measures what a hint does when it arrives, separate from whether an agent
  would ask for one.

**Results.** All 58 tasks have a valid trial in every arm.

| Arm | Resolved | Median wall time | Median API calls | Median tokens | Cost per resolved task | Median time / tokens to first gold file named |
|---|---|---|---|---|---|---|
| A: no hint | 32/58 (55.2%) | 110 s | 19 | 488k | $0.0299 | 6.1 s / 12.8k |
| W: wn hint | 34/58 (58.6%) | 101 s | 17 | 554k | $0.0314 | 4.3 s / 12.4k |
| O: oracle hint | 34/58 (58.6%) | 98.5 s | 15 | 342k | $0.0246 | 3.9 s / 0 |

Paired over the same tasks (95% bootstrap intervals, 4,000 resamples):

| Comparison | Success difference, by task | By repository | Median wall-time ratio, by task | Cost-per-resolved ratio |
|---|---|---|---|---|
| O − A | +3.4 pts [−5.2, +12.1] | [−3.6, +12.0] | 0.90 [0.67, 1.12] | 0.83 [0.66, 1.01] |
| W − A | +3.4 pts [−3.4, +10.3] | [−2.6, +17.2] | 0.92 [0.72, 1.18] | 1.05 [0.81, 1.38] |
| O − W | 0.0 pts [−8.6, +8.6] | [−10.5, +5.0] | 0.98 [0.80, 1.14] | 0.79 [0.60, 1.01] |

### Verdict

**Gate 1 failed: no headroom. Navigation isn't this agent's bottleneck.** A perfect hint raised success
by 3.4 points (threshold +8) and cut median wall time to 0.90× (threshold 0.8×). Gate 2 was not
evaluated, as pre-declared.

- **Why:** without any hint, the agent named a gold file in 57 of 58 tasks, within a median of about 6 s
  and 13k tokens. The median run took 110 s, far under the cap (3–4 timeouts per arm). Finding the file
  is not the constraint; understanding and editing are.
- **The one oracle effect is cost:** fewer tokens, 0.83× cost per resolved task, with an interval that
  just reaches no change. The wn hint didn't share it (1.05×; W used more tokens than A).
- **Descriptive only:** wn's hint contained a gold file in 31 of 58 tasks, the same count as trial 3.

**Deviations:** 12 A/O trials (6 tasks × 2 arms) failed at agent install from a Harbor dependency bug
and were rerun once after a fix, as the plan allowed; all succeeded. A full disk on the hint-building
machine corrupted two hint entries, which were recomputed before W ran. W ran about 45 minutes after
A and O, a declared limitation for its wall-time comparison.

**Spend:** LLM $2.86 (recomputed from token usage at list price, not invoice-verified); Modal at most
$5.72 (that billing window also includes some other jobs).

## Trial 5: weak model, preliminary (September 30, 2026)

Trial 4 found no headroom for a capable agent. Trial 5 asks whether a weak model, which plausibly
navigates poorly, has some. Same gates, hint wording, hint delivery, cap and analysis as trial 4.

**Setup:**

- **Model, chosen by a pre-declared smoke rule** (3 tasks, no-hint arm, first candidate to resolve at
  least 1 of 3 wins): `gpt-oss-20b` resolved 0/3 with malformed tool calls; `qwen3-coder-30b-a3b`
  resolved 1/3 and was used.
- **Agent:** Claude Code pointed at the model through OpenRouter. Codex could not drive either model
  through OpenRouter, so the harness differs from trial 4 and the two trials aren't directly comparable.
- **Tasks:** the 58 minus the 7 teleport tasks (their verifier took about 17 minutes per trial, too
  costly), then a seeded random 36 of the remaining 51 (`random.Random(1).sample`), decided before any
  main-run result to fit the budget.
- **Arms:** A, O and, since gate 1 passed, W using trial 4's wn hints unchanged.
- **Cap:** 600 s agent wall time.

**Results, A vs O** (36 tasks):

| Arm | Resolved | Median wall time | Median API calls | Median tokens | Cost per resolved task | Gold file named in |
|---|---|---|---|---|---|---|
| A: no hint | 1/36 (2.8%) | 27.5 s | 2 | 87k | $0.614 | 15/36 |
| O: oracle hint | 5/36 (13.9%) | 102.9 s | 6 | 259k | $0.179 | 30/36 |

O − A success: **+11.1 points** [+2.8, +22.2] by task, [0.0, +17.8] by repository. Wall time doesn't
apply as a criterion: O runs are longer (median ratio 3.74), because A often stops almost at once.

**Results, A vs W vs O** (the 25 tasks where all three arms completed):

| Arm | Resolved | Median wall time | Median API calls | Median tokens | Cost per resolved task | Gold file named in |
|---|---|---|---|---|---|---|
| A: no hint | 1/25 | 29 s | 2 | 87k | $0.363 | 11/25 |
| W: wn hint | 6/25 | 238 s | 17 | 598k | $0.213 | 19/25 |
| O: oracle hint | 4/25 | 119 s | 7 | 273k | $0.151 | 21/25 |

W − A: +20 points [0, +40] by task, [−5.6, +40] by repository. O − A: +12 points [0, +24]. O − W: −8
points [−28, +12]. Gate 2 (W's share of the gap): 1.67 [0.0, 6.0]. All 6 of W's solves were on the 17
tasks where wn's hint contained a gold file; on the other 8 it solved none.

### Verdict

**Gate 1 passed on success, but this is not a claim.** It is the first setting with measurable
headroom, and wn's hint arm did well (W 6/25 vs A 1/25), but:

- **Confound: the hint may just make the model start working.** Without a hint, the model's median run
  is 2 API calls and 28 s: it often replies with plain text ("I'll explore the codebase…") and no tool
  call, and the session ends. Any concrete file list at the top of the prompt may push it into using
  tools, whether or not the list is right. W's solves all being on correct-hint tasks points the other
  way, but separating the two needs a **placebo arm** (the same block with plausible wrong files),
  which was not pre-declared and not run.
- **W is incomplete:** 11 of 36 W trials were cancelled by the spend guard, and they were the
  longest-running ones, so the 25-task comparison may be biased in an unknown direction.
- **Near the floor:** the counts are 1, 4 and 6 solves, and the intervals are wide.

**Deviations** (logged as plan amendments before or during the runs): the task subset for budget; a
first run routed to the cheapest provider returned empty responses in 44 of 70 trials and was voided
and rerun with default routing; one launch was killed by a spend guard reading a shared API key and
voided; the W job was stopped by the guard, as above.

**Spend:** LLM $3.49 recomputed at list price, including the voided runs (default routing may have
used pricier providers, so the bill could be higher). Modal about $4.4, **over the $3 budget**, mostly
from about 110 wasted trials in the voided and killed runs.

A real test would pre-declare A, a placebo, W and O on the 51 tasks, with a harness that doesn't end
on the model's first text-only turn.

## Conclusion across the trials

For a cheap, capable agent, automatic start hints don't save cost or improve success at any repository
size tested (trials 1–3). Trial 4 shows why: **capable agents don't need navigation hints; even perfect
ones barely help**, because the agent finds a right file within seconds on its own. Weak models might
benefit (trial 5), but that is untested: the only signal so far is confounded and was not checked
against a placebo. where-next stays an **opt-in** tool for agents (the agent called it on its own in 23
of 58 tasks in trial 3), with the start hint off by default, and is positioned as fast local navigation
for people. Not tested: more expensive agents, a placebo-controlled weak-model trial, and people
navigating by hand.
