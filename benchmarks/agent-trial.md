# Agent trial protocol

The only benchmark that answers "does where-next save an agent work?". Offline retrieval gains do
not prove this on their own.

## Arms

1. The agent with its normal search tools.
2. The same agent, plus where-next hints (fine-tuned model).
3. The same agent, plus where-next hints with the per-repo adapter, frozen per task and fitted only
   on history before the task's base commit.

The agent calls where-next as a tool when it chooses to (no oracle timing). Hints are capped at 3
paths and 250 tokens. Each arm runs in its own container, and no arm's observations train another.

## Tasks

Fresh issue-style tasks with executable verification, from repositories held out from training, at
least 10 repositories. A 50-task pilot first, then a confirmatory trial sized from the pilot's
variance.

## Measures

Success (independent verifier), billed tokens and cost, cost per resolved task, wall time, search and
read calls, time to the first useful file, and wrong-hint detours. Failures and timeouts are
included; savings are never reported only over successful runs.

## Pre-declared gate

At least 15% lower cost per resolved task, with the confidence interval excluding no improvement, and
no more than 2 points of success-rate harm. A pilot may be too small to establish this; the report
says so.
