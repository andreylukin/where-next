# Troubleshooting

| Symptom | What to do |
|---|---|
| `wn: command not found` right after installing | The installer says so when `~/.local/bin` isn't on your `PATH`: add it (or open a new terminal). Source builds go to `~/.cargo/bin` (`source ~/.cargo/env` after a fresh Rust install). |
| Installer: "prebuilt binaries need glibc >= 2.35" or "Intel Macs aren't supported yet" | Your platform isn't supported yet; see [install.md](install.md#platforms). `WN_FROM=source` lets you try a source build on old Linux anyway, but it needs a compatible ONNX Runtime shared library. |
| Answers look like keyword matches | `wn status`: `lexical fallback` means no model is installed; run `wn model pull`, then `wn init` again in repositories you already indexed. |
| `wn init` seems slow | The first index of a big repository takes minutes (it prints progress). Later runs only re-embed changed files. |
| Files you just added or changed are missing | Normally picked up in the background; `wn status` shows the index state, and `wn init` re-indexes now. |
| Something about the daemon seems off | `wn daemon status`; `wn daemon stop` (it restarts on the next call); `--no-daemon` or `WN_NO_DAEMON=1` answers in-process. Log: `~/.cache/where-next/daemon.log`. |
| You don't want queries logged locally | `wn ask --no-log`, or set `WN_NO_LOG=1` (the log feeds `wn stats` and `wn report`; query text is never stored). |
| Update, reinstall or remove | `wn update` (or re-run the installer); uninstall with `curl -fsSL …/install.sh \| sh -s -- --uninstall`. |
| Model checksum mismatch | `wn model remove <name>`, then `wn model pull` it again. |
| "not inside a git repository" or "refusing to index" your home directory | `wn` only indexes git repositories, and never `~` or `/` by default: run it in the project (or `--path <repo>`); `--any-dir` indexes the directory anyway. |

Answer states such as `stale_index` and `empty_index` are explained in
[quickstart.md](quickstart.md#if-something-is-wrong). More questions: [FAQ](faq.md).
