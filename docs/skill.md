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

## `wn skill sync`

```sh
wn skill sync                    # detected agents, your home directory; shows the plan and asks
wn skill sync --dry-run          # show what would change (with a diff for outdated copies)
wn skill sync --yes              # apply without asking (required when there is no terminal)
wn skill sync --agent codex      # claude | codex | cursor | all (repeatable)
wn skill sync --project          # install into this repository instead (commit it for your team)
wn skill sync --uninstall        # remove what sync installed
wn skill show                    # print the skill
```

| Agent | Detected by | User install | `--project` install |
|---|---|---|---|
| Claude Code | `~/.claude` | `~/.claude/skills/where-next/SKILL.md` | `.claude/skills/where-next/SKILL.md` |
| Codex | `~/.codex` or `~/.agents` | `~/.agents/skills/where-next/SKILL.md` | `.agents/skills/where-next/SKILL.md` |
| Cursor | `~/.cursor` | `~/.cursor/skills/where-next/SKILL.md` | `.cursor/skills/where-next/SKILL.md` |

With no agent detected, Claude Code is the default. Cursor also reads `.claude` and `.agents` skill
directories, so installing for several agents can show the skill twice there; sync only Cursor
(`--agent cursor`) if that bothers you.

Guarantees:

- **Idempotent.** Each installed file carries a marker line with the skill version; re-running
  changes nothing when it is current, and shows a diff when it is not.
- **Never clobbers your files.** A `SKILL.md` without the marker is reported and left alone, for
  both sync and `--uninstall`.
- **Asks first.** Nothing is written without `--yes` or a `y` at the prompt.
- **Kept current.** Installs are recorded in `$WHERE_NEXT_HOME/skills.json`; `wn update` re-syncs
  them (`wn skill sync --yes --from-state`).

### Optional Claude Code hook

`wn skill sync --with-hook` also adds a `UserPromptSubmit` hook to `~/.claude/settings.json`
(`.claude/settings.json` with `--project`) that runs `wn hook claude-prompt`. On the first prompt
of each session it runs `wn ask --start --json` in the session's directory and, when the repository
is indexed, has 3,000+ files and the answer is confident, adds the top files to Claude's context.
Otherwise it prints nothing. It always exits 0, so it never blocks a prompt. The entry is merged
next to your other hooks, added once, and removed by `--uninstall`; an unparsable settings file is
left alone. Off by default.

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
