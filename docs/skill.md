# Agent skill and daemon

Agents use `wn` as a command: `wn ask --json "<query>"`. A short skill tells them when that helps,
and a background daemon keeps it fast. MCP (`wn mcp`) remains available for clients without skills.

## The skill

[`skills/where-next/SKILL.md`](../skills/where-next/SKILL.md) is a standard `SKILL.md` (name and
description frontmatter, a page of instructions): ask "where does X live" questions and paste
errors, use `rg` instead for exact names and strings, and read the JSON `state` (`ok`, `abstain`,
`stale_index`, …) before trusting the hints. `wn ask` always exits 0 when it could run; the `state`
field says whether there are hints. `--start` (a task-start hint, skipped in repositories under
3,000 source files) is opt-in: in agent trials it did not lower cost, so the skill doesn't tell
agents to use it.

## `wn setup`

```sh
wn setup                    # detected agents, your home directory; shows every file it writes, asks once
wn setup --dry-run          # show what would change (diffs), write nothing
wn setup --yes              # apply without asking (required when there is no terminal)
wn setup --agent codex      # claude | codex | cursor | bough | all (repeatable)
wn setup --no-hooks         # only the skill
wn setup --project          # install into this repository instead (commit it for your team)
wn setup --uninstall        # remove everything setup added
wn skill show               # print the skill
```

`wn skill sync` is the same command under its old name.

| Agent | Detected by | Skill | Hooks |
|---|---|---|---|
| Claude Code | `~/.claude` | `~/.claude/skills/where-next/SKILL.md` | `~/.claude/settings.json` |
| Codex | `~/.codex` or `~/.agents` | `~/.agents/skills/where-next/SKILL.md` | `~/.codex/hooks.json` |
| Cursor | `~/.cursor` | `~/.cursor/skills/where-next/SKILL.md` | `~/.cursor/hooks.json` |
| bough | `~/.bough` | `~/.bough/skills/where-next/SKILL.md` | `~/.bough/hooks/<event>/where-next.js` |

With `--project` the same paths are used under the repository (`.claude/`, `.agents/`, `.codex/`,
`.cursor/`, `.bough/`). With no agent detected, Claude Code is the default. Cursor also reads
`.claude` and `.agents` skill directories, so installing for several agents can show the skill
twice there; set up only Cursor (`--agent cursor`) if that bothers you. bough reads a repository's
skills from `.claude/skills` only, so with `--project` it gets the skill from a Claude Code install
there (`--agent claude`). bough's copy of the skill says `manual: true`: bough injects a skill into
the turn whenever its name appears in the prompt, and every prompt hint names where-next. It stays
in bough's skill list for the model to read.

Guarantees:

- **Shows everything, asks once.** The plan lists every file with a diff of what changes. Nothing
  is written without `--yes` or a `y` at the prompt.
- **Idempotent.** Each skill file carries a marker line with the skill version; hook entries are
  recognised by their command (`… wn hook <agent>-<moment>`). Re-running changes nothing when both
  are current.
- **Merges, never clobbers.** Hooks are inserted as text next to your other settings and hooks;
  the rest of the file keeps its bytes. A symlinked settings file is written through the link,
  permissions are kept, and a file edited after the plan was shown is not overwritten. wn keeps
  no copy of your settings (`skills.json` records only which files it touched, mode 0600). A settings file that is not valid JSON
  is reported and left alone, and so is a `SKILL.md` without the marker. Codex: when
  `~/.codex/config.toml` defines hooks inline, `hooks.json` is left alone (Codex warns when a layer
  has both). Setup finishes with a `Codex: hooks NOT connected` notice and entries to paste into
  that `config.toml`.
- **Undoable.** `wn setup --uninstall` removes our skill files and hook entries (every agent, and
  `--project` installs it recorded) and nothing else, and says what it removed. A settings file
  nothing else changed in gets its original bytes back; a hooks file that `wn setup` created and
  that holds nothing else is deleted. `wn uninstall` does this and removes wn itself
  ([install.md](install.md#uninstall)).
- **Kept current.** Installs are recorded in `$WHERE_NEXT_HOME/skills.json`; `wn update` re-syncs
  them (`wn setup --yes --from-state`), which also moves hooks from older releases to the current
  ones.

## How wn plugs into your agent

Agents rarely decide to call a tool on their own, so `wn setup` also installs hooks: the agent runs
`wn hook …` at the moments where knowing which file to open matters, and wn adds its hints to the
agent's context. The agent never has to remember wn exists.

**What is installed**

| Agent | Event (matcher) | Command |
|---|---|---|
| Claude Code | `SessionStart` | `wn hook claude-start` |
| Claude Code | `UserPromptSubmit` | `wn hook claude-prompt` |
| Claude Code | `PostToolUse` (`Grep\|Glob\|Bash`), `PostToolUseFailure` (`Bash`) | `wn hook claude-search` |
| Codex | `SessionStart` | `wn hook codex-start` |
| Codex | `UserPromptSubmit` | `wn hook codex-prompt` |
| Codex | `PostToolUse` (`Bash`) | `wn hook codex-search` |
| Cursor | `sessionStart` | `wn hook cursor-start` |
| Cursor | `postToolUse` (`Shell\|Grep`) | `wn hook cursor-search` |
| bough | `session-start` | `wn hook bough-start` |
| bough | `user-prompt-submit` | `wn hook bough-prompt` |
| bough | `post-result` (native `bash` calls) | `wn hook bough-search` |

If Codex already defines hooks inline in `~/.codex/config.toml`, append these entries there.
`wn setup` prints the same entries with the command path it selected for your installation;
use that path if `wn` is not on the agent's `PATH`.

```toml
[[hooks.SessionStart]]
[[hooks.SessionStart.hooks]]
type = "command"
command = "wn hook codex-start"
timeout = 5

[[hooks.UserPromptSubmit]]
[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "wn hook codex-prompt"
timeout = 5

[[hooks.PostToolUse]]
matcher = "Bash"
[[hooks.PostToolUse.hooks]]
type = "command"
command = "wn hook codex-search"
timeout = 5
```

Each entry has a 5-second agent-side `timeout`. The command names `wn` when that is the binary on
your `PATH`, else its absolute path. Cursor has no prompt hook that can add context
(`beforeSubmitPrompt` can only allow or block), so it gets the warm-up and search hooks. **Codex runs new
hooks only after you trust them**: open Codex, run `/hooks` and trust the where-next entries.
Claude Code and Cursor pick them up in new sessions.

bough hooks are JavaScript files, not commands: `wn setup` writes one `where-next.js` per event
(marked ``// managed by `wn setup` ``; a file there without the marker is left alone). bough runs
them in its code-mode VM and re-reads them on every event, so running sessions pick them up at
once. Each passes the event, with the session's id and directory from `bough.session()`, to
`wn hook bough-…` through `tools.bash`, and appends what wn prints to the prompt
(`user-prompt-submit` → `input`) or to the search's output (`post-result` → `result`). bough
shows the rewritten prompt to you as "hook user-prompt-submit rewrote your message". The prompt
hook keeps the prompt in a VM global (`whereNextPrompt`), which the search hook sends as its
context. bough's only limit on a hook is its 30 s script timeout; wn's 1.5 s budget bounds it.
Only native `bash` calls count as searches: on the code-mode loop (`loop.plugin=loop`) a
`post-result` covers a whole JavaScript block, so the search hook stays out of it.

**When they fire**

- *Session start* (all four): in an indexed repository, starts a background `wn ask` through the
  daemon so the model and index are loaded before the first prompt, and returns at once with no
  output.
- *Prompt* (Claude Code, Codex, bough): on every prompt, the prompt is asked as a query.
- *Search* (all four): after `rg`, `grep`, `git grep`, `ag`, `ack`, `fd` or `find` in a shell, or
  the Grep/Glob tools, when the search found nothing or more than 30 results. The pattern is asked,
  with the session's latest prompt as context (read from the end of the transcript the agent names
  in the hook payload; not stored). Searches with a handful of results are left alone.
- *Outside a repository* (an agent started in `~` or another directory of repositories): the same
  hooks ask across every repository indexed below that directory (see
  [quickstart](quickstart.md#many-repositories-from-your-home-directory)), with paths relative to
  it. With nothing indexed there they stay silent.

**What they inject**

Only confident answers (state `ok`; an abstain prints nothing): at most 3 file paths, each file at
most once per session, in a few lines that start with "where-next (local index of this
repository) suggests". Claude Code and Codex receive it as `hookSpecificOutput.additionalContext`,
Cursor as `additional_context`, bough's hook file as `context`.

**When they stay silent**

A hook prints nothing and exits 0 when wn abstains, every hinted file was already shown in this
session, no model is installed (the lexical fallback has no calibrated threshold), the directory is
not in a git repository, the repository has no index for the installed model (`wn init` once per
repository), the payload is not one it understands, or `WN_HOOKS=0`. It never loads a model or
builds an index itself: it asks the background daemon (starting it if needed) and gives up after
1.5 s. The session-start warm-up loads the model first; without it (or when the daemon has been
idle for 15 minutes mid-session) the first hook can give up while the model loads. On this repository (101 source files, gemma-xl1, debug build, Apple
Silicon) warm prompt and search hooks took 35–42 ms end to end, including process start, and a
shell command that is not a search 10 ms. The session-start hook returned in 11 ms, and the first
prompt hook a few seconds later answered in 112 ms.

**Local state.** Per session, the paths already injected are kept in
`$WHERE_NEXT_HOME/hook-sessions/` and expire after a day. Each run that asked the daemon is logged
in `$WHERE_NEXT_HOME/hook-log.jsonl` (time, agent, session id, repository, latency, outcome,
injected paths), and each injection in the repository's usage log, like a `wn ask` (both off with
`WN_NO_LOG`, kept 30 days, never sent anywhere). Prompt and query text are never stored.

**Seeing what they did.** `wn stats` shows a *Hooks* row: injections, files and sessions, the median
hook time, how many runs were quiet or timed out, and, from your Claude Code and Codex transcripts
(read locally), after how many injections the agent then opened, ran or edited a hinted file. It
counts what happened; it does not estimate tokens or money saved.

**Turning them off.**

| Setting | Effect |
|---|---|
| `WN_HOOKS=0` | Every hook is a no-op (set it in your shell, or in the agent's environment) |
| `WN_HOOK_MIN_FILES=N` | Hooks answer only in repositories with at least N indexed source files (default 0) |
| `WN_HOOK_TIMEOUT_MS=N` | Time budget of one hook (default 1500) |
| `wn setup --uninstall` | Removes the hooks and the skill |

## The daemon

`wn ask` and `wn status` go through a per-user daemon that keeps models and indexes warm:

- **Automatic.** The first call starts it in the background (`wn daemon serve`), logging to
  `$WHERE_NEXT_HOME/daemon.log` (default `~/.cache/where-next`). It listens on a Unix socket
  (`daemon.sock`, mode 0600) and exits after 15 idle minutes.
- **Same answers.** The daemon runs the same code as the in-process path; output is byte-identical.
  It rescans repositories in the background and never blocks an answer on a rescan after the
  first request for a repository.
- **Version handshake.** A daemon from a different build (after `wn update`) is stopped and
  replaced automatically.
- **Fail-open.** If the daemon can't start or answer, `wn` answers in-process.

```sh
wn daemon status [--json]   # running?, pid, uptime, requests, warm models and repositories
wn daemon start | stop
```

| Setting | Effect |
|---|---|
| `--no-daemon`, `WN_NO_DAEMON=1` | Answer in-process; never start a daemon |
| `WN_DAEMON_IDLE_SECS` | Idle exit (default 900) |
| `WN_DAEMON_RESCAN_MS` | Minimum pause between background rescans (default 2000; grows with scan time) |
| `WHERE_NEXT_HOME` | Each cache home has its own daemon |

Windows has no daemon yet: commands answer in-process.

Latency (Django checkout, 2,976 files, lexical encoder, Apple Silicon under heavy load): the cold
first call, which starts the daemon and indexes, takes 6–10 s. Warm calls take about 80–100 ms p50
and 180–200 ms p95, end to end including process start (about 22 ms). Answering in-process rescans
the tree on every call instead, which takes about 460 ms.
