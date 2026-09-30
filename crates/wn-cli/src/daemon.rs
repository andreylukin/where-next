//! The per-user background daemon: keeps models and repository indexes warm so `wn ask` and
//! `wn status` answer in milliseconds instead of reloading the model on every call.
//!
//! - One daemon per cache home (`$WHERE_NEXT_HOME`, else `~/.cache/where-next`), listening on a
//!   Unix socket there (`daemon.sock`, mode 0600). Tests and separate homes never share one.
//! - Started on first use by `wn ask` / `wn status` (detached; logs to `daemon.log`), exits after
//!   `WN_DAEMON_IDLE_SECS` (default 900) without requests.
//! - Every request carries the client's binary id; a daemon from another build is asked to stop
//!   and replaced once ([`wn_daemon::connect`]).
//! - Anything that goes wrong falls back to answering in-process, with identical output.
//!   `--no-daemon` or `WN_NO_DAEMON=1` skip the daemon entirely.
//!
//! Protocol: newline-delimited JSON over the socket, one [`Request`] and one [`Response`] per
//! line. Windows has no daemon yet: commands always answer in-process there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Subcommand;
use serde::{Deserialize, Serialize};

use crate::{AskArgs, Cli, Command};

/// Bumped when requests or responses change shape.
pub const PROTOCOL: u32 = 1;

/// Default idle timeout.
pub const DEFAULT_IDLE_SECS: u64 = 900;

/// `wn daemon …`
#[derive(Debug, Clone, Subcommand)]
pub enum DaemonAction {
    /// Start the daemon now (normally started on first use).
    Start,
    /// Stop the daemon.
    Stop,
    /// Show whether the daemon runs and what it holds.
    Status,
    /// Run the daemon in the foreground (used by the auto-start).
    #[command(hide = true)]
    Serve {
        /// Exit after this many seconds without requests.
        #[arg(long)]
        idle_secs: Option<u64>,
    },
}

/// What a request asks for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Version handshake.
    Hello,
    /// `wn ask`.
    Ask { args: AskArgs, context: String },
    /// `wn status`.
    Status,
    /// Daemon statistics for `wn daemon status`.
    Stats,
    /// Stop the daemon.
    Stop,
}

/// One request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub proto: u32,
    /// The client's [`binary_id`].
    pub client: String,
    /// Repository root (absolute).
    #[serde(default)]
    pub repo: Option<String>,
    /// Resolved model directory; `None` means no model is installed (lexical fallback).
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub json: bool,
    #[serde(flatten)]
    pub op: Op,
}

/// Daemon statistics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub pid: u32,
    pub binary: String,
    pub uptime_secs: u64,
    pub idle_secs: u64,
    pub idle_timeout_secs: u64,
    pub requests: u64,
    pub models: Vec<String>,
    pub repos: usize,
    /// Repositories a request is working on right now (for example a first index build).
    #[serde(default)]
    pub busy: Vec<String>,
}

/// One response line.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    /// Text the command prints (exactly what the in-process path would print).
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub code: i32,
    /// `mismatch`, `draining`, `bad_request`, or another failure description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The daemon's binary id.
    #[serde(default)]
    pub binary: String,
    #[serde(default)]
    pub proto: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    /// An interim progress line (stderr) sent before the final response of a long request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
}

/// Identifies this build: version plus the executable's size and modification time, so a
/// rebuilt or updated binary is always detected. `WN_DAEMON_BINARY_ID` overrides it (tests).
pub fn binary_id() -> String {
    if let Some(id) = std::env::var_os("WN_DAEMON_BINARY_ID") {
        return id.to_string_lossy().into_owned();
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok());
    let stamp = exe
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{:x}-{:x}", m.len(), mtime)
        })
        .unwrap_or_else(|| "unknown".into());
    format!("{}+{stamp}", env!("CARGO_PKG_VERSION"))
}

/// Socket path for a cache home. Unix socket paths are limited to about 104 bytes, so long homes
/// use a hashed name under `/tmp`.
pub fn socket_path(home: &Path) -> PathBuf {
    let direct = home.join("daemon.sock");
    if direct.as_os_str().len() <= 100 {
        return direct;
    }
    let mut h: u64 = 1469598103934665603;
    for b in home.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    PathBuf::from(format!("/tmp/wn-{h:016x}.sock"))
}

/// Idle timeout from `WN_DAEMON_IDLE_SECS`.
pub fn idle_timeout() -> Duration {
    let secs = std::env::var("WN_DAEMON_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_IDLE_SECS);
    Duration::from_secs(secs.max(1))
}

/// Whether this command must not use the daemon.
pub fn disabled(cli: &Cli) -> bool {
    cli.no_daemon
        || std::env::var("WN_NO_DAEMON")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
}

/// Which daemon operation serves a command (`None`: the command always runs in-process).
pub fn op_for(command: &Command) -> Option<OpKind> {
    match command {
        Command::Ask { .. } => Some(OpKind::Ask),
        Command::Status => Some(OpKind::Status),
        _ => None,
    }
}

/// Commands the daemon can serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Ask,
    Status,
}

/// Builds the request for a command (context already read by the caller).
pub fn request(cli: &Cli, kind: OpKind, context: &str) -> Option<Request> {
    let root = wn_git::repo_root(&cli.path);
    let root = root.canonicalize().unwrap_or(root);
    let model = crate::resolve_model(cli.model.as_deref())
        .map(|m| m.canonicalize().unwrap_or(m).to_string_lossy().into_owned());
    let op = match (kind, &cli.command) {
        (
            OpKind::Ask,
            Command::Ask {
                query,
                functions,
                k,
                no_adapter,
                strict,
                no_abstain,
                start,
                start_min_files,
                no_log,
                ..
            },
        ) => Op::Ask {
            args: AskArgs {
                query: query.clone(),
                functions: *functions,
                k: *k,
                no_adapter: *no_adapter,
                strict: *strict,
                no_abstain: *no_abstain,
                start: *start,
                start_min_files: *start_min_files,
                // The daemon outlives this command's environment, so forward `WN_NO_LOG`.
                no_log: *no_log || !wn_daemon::usage::enabled(),
            },
            context: context.to_string(),
        },
        (OpKind::Status, Command::Status) => Op::Status,
        _ => return None,
    };
    Some(Request {
        proto: PROTOCOL,
        client: binary_id(),
        repo: Some(root.to_string_lossy().into_owned()),
        model,
        json: cli.json,
        op,
    })
}

/// A report for `wn daemon status`.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonReport {
    pub running: bool,
    pub socket: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn render_report(r: &DaemonReport) -> String {
    match (&r.stats, &r.message) {
        (Some(s), _) => format!(
            "daemon: running (pid {}, up {}s, idle {}s of {}s, {} requests, {} repos, models: {}){}\nsocket: {}",
            s.pid,
            s.uptime_secs,
            s.idle_secs,
            s.idle_timeout_secs,
            s.requests,
            s.repos,
            if s.models.is_empty() {
                "none loaded".to_string()
            } else {
                s.models.join(", ")
            },
            if s.busy.is_empty() {
                String::new()
            } else {
                format!("\nbusy with: {}", s.busy.join(", "))
            },
            r.socket
        ),
        (None, Some(m)) => format!("daemon: {m}\nsocket: {}", r.socket),
        (None, None) => format!("daemon: not running\nsocket: {}", r.socket),
    }
}

/// `wn daemon start | stop | status | serve`.
pub fn run_action(action: &DaemonAction, cli: &Cli) -> (String, i32) {
    let home = crate::home();
    let socket = socket_path(&home);
    let report = |r: DaemonReport, code: i32| {
        let text = if cli.json {
            serde_json::to_string_pretty(&r).unwrap_or_default()
        } else {
            render_report(&r)
        };
        (text, code)
    };
    match action {
        DaemonAction::Serve { idle_secs } => {
            let idle = idle_secs
                .map(Duration::from_secs)
                .unwrap_or_else(idle_timeout);
            match server::serve(&home, idle) {
                Ok(()) => (String::new(), 0),
                Err(e) => (format!("wn daemon: {e}"), 1),
            }
        }
        DaemonAction::Status => {
            let (running, stats) = match client::probe(&home) {
                client::Probe::Stats(s) => (true, Some(*s)),
                client::Probe::Silent => (true, None),
                client::Probe::Absent => (false, None),
            };
            let message = (running && stats.is_none())
                .then(|| "running, but busy (it did not answer in time)".to_string());
            report(
                DaemonReport {
                    running,
                    socket: socket.display().to_string(),
                    stats,
                    message,
                },
                0,
            )
        }
        DaemonAction::Stop => {
            let stopped = client::stop(&home);
            report(
                DaemonReport {
                    running: false,
                    socket: socket.display().to_string(),
                    stats: None,
                    message: Some(if stopped {
                        "stopped".into()
                    } else {
                        "not running".into()
                    }),
                },
                0,
            )
        }
        DaemonAction::Start => match client::ensure_started(&home) {
            Ok(stats) => report(
                DaemonReport {
                    running: true,
                    socket: socket.display().to_string(),
                    stats: Some(stats),
                    message: None,
                },
                0,
            ),
            Err(e) => report(
                DaemonReport {
                    running: false,
                    socket: socket.display().to_string(),
                    stats: None,
                    message: Some(format!("could not start: {e}")),
                },
                1,
            ),
        },
    }
}

/// Serves one request against the daemon's warm state. Shared with tests.
///
/// Locking: the map of workspaces is locked only to look a repository up; each repository has
/// its own lock, held while a request for it runs (including a first index build). A request for
/// one repository therefore never waits for another repository's indexing, and statistics never
/// wait for any repository.
pub mod handler {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, TryLockError};
    use std::time::{Duration, Instant, SystemTime};

    use wn_core::index::IndexedFile;
    use wn_git::Coverage;

    use super::{Op, Request, Response, PROTOCOL};
    use crate::progress::Sink;
    use crate::{
        ask_command_with, encoder_for, status_command, EncoderInfo, SharedEncoder, StatusKind,
        Workspace,
    };

    /// Loads the encoder for a resolved model directory (replaceable in tests).
    pub type Loader = dyn Fn(Option<&Path>) -> (SharedEncoder, EncoderInfo) + Send + Sync;

    type Key = (PathBuf, String);

    /// A model directory and its manifest's modification time.
    type ModelKey = (Option<PathBuf>, Option<SystemTime>);

    /// One repository (for one model): its workspace, opened on first use under its own lock.
    struct Slot {
        root: PathBuf,
        encoder: SharedEncoder,
        info: EncoderInfo,
        ws: Mutex<Option<Workspace>>,
        /// When the background rescanner may start refreshing it (after the first request).
        scanned: Mutex<Option<Instant>>,
    }

    /// Loaded encoders and open workspaces.
    pub struct Warm {
        load: Box<Loader>,
        encoders: Mutex<HashMap<ModelKey, (SharedEncoder, EncoderInfo)>>,
        slots: Mutex<HashMap<Key, Arc<Slot>>>,
    }

    impl Default for Warm {
        fn default() -> Self {
            Warm::with_loader(Box::new(encoder_for))
        }
    }

    /// Minimum pause between background rescans of warm workspaces (`WN_DAEMON_RESCAN_MS`,
    /// default 2000). The rescanner also waits at least ten times its last scan's duration, so a
    /// large repository never keeps a core busy. Between rescans, answers use the current index.
    pub fn rescan_every() -> Duration {
        let ms = std::env::var("WN_DAEMON_RESCAN_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        Duration::from_millis(ms)
    }

    /// Pause before the next background rescan, given how long the last one took.
    pub fn next_pause(last_scan: Duration) -> Duration {
        rescan_every().max(last_scan * 10)
    }

    impl Warm {
        pub fn with_loader(load: Box<Loader>) -> Self {
            Warm {
                load,
                encoders: Mutex::default(),
                slots: Mutex::default(),
            }
        }

        /// Model names loaded (or `lexical` for the fallback).
        pub fn models(&self) -> Vec<String> {
            let Ok(encoders) = self.encoders.lock() else {
                return Vec::new();
            };
            let mut names: Vec<String> = encoders
                .values()
                .map(|(_, info)| info.model.clone().unwrap_or_else(|| "lexical".into()))
                .collect();
            names.sort();
            names.dedup();
            names
        }

        pub fn repos(&self) -> usize {
            self.slots.lock().map(|s| s.len()).unwrap_or(0)
        }

        /// Names of repositories a request is working on right now (indexing or answering).
        pub fn busy(&self) -> Vec<String> {
            let slots: Vec<Arc<Slot>> = match self.slots.lock() {
                Ok(s) => s.values().cloned().collect(),
                Err(_) => return Vec::new(),
            };
            let mut names: Vec<String> = slots
                .iter()
                .filter(|slot| matches!(slot.ws.try_lock(), Err(TryLockError::WouldBlock)))
                .map(|slot| {
                    slot.root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| slot.root.display().to_string())
                })
                .collect();
            names.sort();
            names
        }

        fn encoder(&self, model: Option<&Path>) -> (SharedEncoder, EncoderInfo) {
            // Key on the manifest's modification time too, so a replaced model reloads.
            let stamp = model
                .and_then(|m| std::fs::metadata(m.join("wn-model.json")).ok())
                .and_then(|m| m.modified().ok());
            let key = (model.map(Path::to_path_buf), stamp);
            // Held while a model loads (once per model, a few seconds at most).
            let mut encoders = self.encoders.lock().unwrap_or_else(|e| e.into_inner());
            encoders
                .entry(key)
                .or_insert_with(|| (self.load)(model))
                .clone()
        }

        fn slot(&self, repo: &Path, model: Option<&Path>) -> Arc<Slot> {
            let (encoder, info) = self.encoder(model);
            let key = (repo.to_path_buf(), info.fingerprint.clone());
            let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            slots
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(Slot {
                        root: repo.to_path_buf(),
                        encoder,
                        info,
                        ws: Mutex::new(None),
                        scanned: Mutex::new(None),
                    })
                })
                .clone()
        }

        /// Workspaces the background rescanner should refresh: key and repository root.
        pub fn targets(&self) -> Vec<(Key, PathBuf)> {
            let Ok(slots) = self.slots.lock() else {
                return Vec::new();
            };
            slots
                .iter()
                .filter(|(_, slot)| slot.scanned.lock().is_ok_and(|s| s.is_some()))
                .map(|(k, slot)| (k.clone(), slot.root.clone()))
                .collect()
        }

        /// Applies a scan taken without any lock (only changed files are embedded). Skipped when
        /// a request is using the repository or another process is indexing it.
        pub fn apply(&self, key: &Key, scan: (Vec<IndexedFile>, Coverage)) {
            let slot = match self.slots.lock() {
                Ok(s) => s.get(key).cloned(),
                Err(_) => return,
            };
            let Some(slot) = slot else { return };
            let Ok(mut guard) = slot.ws.try_lock() else {
                return;
            };
            if let Some(ws) = guard.as_mut() {
                if let Ok(Some(_)) = ws.try_apply_scan(scan.0, scan.1, false) {
                    if let Ok(mut s) = slot.scanned.lock() {
                        *s = Some(Instant::now());
                    }
                }
            }
        }

        /// Answers `ask` and `status`; other ops are handled by the server loop. Progress of a
        /// long index build (or of waiting for one) goes to `progress`.
        pub fn serve(&self, req: &Request, progress: Option<Arc<dyn Sink>>) -> Response {
            let Some(repo) = req.repo.as_deref() else {
                return failure("bad_request: no repository");
            };
            let model = req.model.as_deref().map(Path::new);
            let loading = progress
                .clone()
                .map(|sink| crate::progress::Ticker::step("loading the model", sink));
            let slot = self.slot(Path::new(repo), model);
            drop(loading);
            let mut guard = match slot.ws.try_lock() {
                Ok(g) => g,
                Err(TryLockError::WouldBlock) => {
                    if let Some(p) = &progress {
                        let name = slot
                            .root
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        p.update(&format!(
                            "wn: {name}: waiting for the daemon to finish indexing it"
                        ));
                    }
                    slot.ws.lock().unwrap_or_else(|e| e.into_inner())
                }
                Err(TryLockError::Poisoned(e)) => e.into_inner(),
            };
            let ws = guard.get_or_insert_with(|| {
                Workspace::open_with(&slot.root, slot.encoder.clone(), slot.info.clone())
            });
            // The first request for a repository scans synchronously; after that the background
            // rescanner keeps the index current and requests answer from it directly.
            let first = {
                let mut scanned = slot.scanned.lock().unwrap_or_else(|e| e.into_inner());
                let first = scanned.is_none();
                if first {
                    *scanned = Some(Instant::now());
                }
                first
            };
            ws.progress = progress;
            let (text, code) = match &req.op {
                Op::Ask { args, context } => {
                    ask_command_with(ws, args, context, req.json, first || args.functions)
                }
                Op::Status => status_command(ws, StatusKind::Status, req.json),
                _ => {
                    ws.progress = None;
                    return failure("bad_request: not a repository operation");
                }
            };
            ws.progress = None;
            Response {
                ok: true,
                text,
                code,
                proto: PROTOCOL,
                ..Response::default()
            }
        }
    }

    pub fn failure(error: &str) -> Response {
        Response {
            ok: false,
            error: Some(error.to_string()),
            proto: PROTOCOL,
            ..Response::default()
        }
    }
}

#[cfg(unix)]
pub mod server {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use wn_daemon::resident::{ResidentEvent, ResidentLifecycle};

    use super::handler::{failure, Warm};
    use super::{binary_id, socket_path, Op, Request, Response, Stats, PROTOCOL};
    use crate::progress::Sink;

    /// Process-wide bookkeeping. Locked only briefly, never while a request is served.
    struct Shared {
        life: ResidentLifecycle,
        requests: u64,
        in_flight: usize,
        last: Instant,
    }

    /// Everything the connection threads share.
    struct Daemon {
        shared: Mutex<Shared>,
        warm: Warm,
    }

    /// Runs the daemon until idle timeout or a stop request.
    pub fn serve(home: &Path, idle: Duration) -> Result<(), String> {
        std::fs::create_dir_all(home).map_err(|e| e.to_string())?;
        let socket = socket_path(home);
        let mut life = ResidentLifecycle::default();
        // Another live daemon owns the socket: nothing to do.
        if UnixStream::connect(&socket).is_ok() {
            let _ = life.handle(ResidentEvent::BindFailed);
            return Err("another daemon is already listening".into());
        }
        let _ = std::fs::remove_file(&socket);
        let listener = match UnixListener::bind(&socket) {
            Ok(l) => l,
            Err(e) => {
                let _ = life.handle(ResidentEvent::BindFailed);
                return Err(format!("cannot bind {}: {e}", socket.display()));
            }
        };
        let _ = std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600));
        let _ = life.handle(ResidentEvent::Bound);
        let binary = binary_id();
        let started = Instant::now();
        eprintln!(
            "where-next daemon {binary}: listening on {} (pid {}, idle timeout {}s)",
            socket.display(),
            std::process::id(),
            idle.as_secs()
        );
        let daemon = Arc::new(Daemon {
            shared: Mutex::new(Shared {
                life,
                requests: 0,
                in_flight: 0,
                last: Instant::now(),
            }),
            warm: Warm::default(),
        });

        // Idle watchdog: drains and exits once nothing arrived for `idle`.
        {
            let daemon = daemon.clone();
            let socket = socket.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                let Ok(mut s) = daemon.shared.lock() else {
                    return;
                };
                if s.life.state().accepts() && s.in_flight == 0 && s.last.elapsed() >= idle {
                    let _ = s.life.handle(ResidentEvent::IdleTimeout);
                    finish(&mut s, &socket, "idle timeout");
                }
            });
        }

        // Background rescanner: keeps warm indexes current without scanning on the request path.
        // The scan (git listing, stat calls) runs without any lock; only changes are applied, and
        // a repository busy with a request or another indexer is skipped this round.
        {
            let daemon = daemon.clone();
            std::thread::spawn(move || {
                let mut pause = super::handler::rescan_every();
                loop {
                    std::thread::sleep(pause);
                    let started = Instant::now();
                    for (key, root) in daemon.warm.targets() {
                        let scan = crate::scan_repo(&root);
                        daemon.warm.apply(&key, scan);
                    }
                    pause = super::handler::next_pause(started.elapsed());
                }
            });
        }

        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let daemon = daemon.clone();
            let socket = socket.clone();
            let binary = binary.clone();
            std::thread::spawn(move || handle(stream, &daemon, &socket, &binary, idle, started));
        }
        Ok(())
    }

    fn finish(s: &mut Shared, socket: &Path, why: &str) {
        let _ = std::fs::remove_file(socket);
        let _ = s.life.handle(ResidentEvent::Drained);
        eprintln!("where-next daemon: stopping ({why})");
        std::process::exit(0);
    }

    fn write_line(stream: &mut UnixStream, reply: &Response) -> bool {
        let mut text = serde_json::to_string(reply).unwrap_or_default();
        text.push('\n');
        stream.write_all(text.as_bytes()).is_ok() && stream.flush().is_ok()
    }

    /// Forwards progress lines to the client as interim responses.
    struct ToClient(Mutex<UnixStream>);

    impl Sink for ToClient {
        fn update(&self, line: &str) {
            if let Ok(mut s) = self.0.lock() {
                let _ = write_line(
                    &mut s,
                    &Response {
                        ok: true,
                        progress: Some(line.to_string()),
                        proto: PROTOCOL,
                        ..Response::default()
                    },
                );
            }
        }
    }

    fn handle(
        stream: UnixStream,
        daemon: &Arc<Daemon>,
        socket: &Path,
        binary: &str,
        idle: Duration,
        started: Instant,
    ) {
        let Ok(read) = stream.try_clone() else { return };
        let mut writer = stream;
        for line in BufReader::new(read).lines() {
            let Ok(line) = line else { return };
            if line.trim().is_empty() {
                continue;
            }
            let progress = writer
                .try_clone()
                .ok()
                .map(|s| Arc::new(ToClient(Mutex::new(s))) as Arc<dyn Sink>);
            let reply = match serde_json::from_str::<Request>(&line) {
                Err(e) => failure(&format!("bad_request: {e}")),
                Ok(req) => respond(req, daemon, binary, idle, started, progress),
            };
            if !write_line(&mut writer, &reply) {
                return;
            }
            if reply.error.as_deref() == Some("stopping") {
                if let Ok(mut s) = daemon.shared.lock() {
                    finish(&mut s, socket, "stop requested");
                }
            }
        }
    }

    fn respond(
        req: Request,
        daemon: &Daemon,
        binary: &str,
        idle: Duration,
        started: Instant,
        progress: Option<Arc<dyn Sink>>,
    ) -> Response {
        let base = Response {
            binary: binary.to_string(),
            proto: PROTOCOL,
            ..Response::default()
        };
        match &req.op {
            Op::Hello => Response {
                ok: req.proto == PROTOCOL && req.client == binary,
                error: (req.proto != PROTOCOL || req.client != binary).then(|| "mismatch".into()),
                ..base
            },
            Op::Stop => {
                let Ok(mut s) = daemon.shared.lock() else {
                    return failure("poisoned");
                };
                let _ = s.life.handle(ResidentEvent::Shutdown);
                Response {
                    ok: true,
                    error: Some("stopping".into()),
                    ..base
                }
            }
            Op::Stats => {
                let (requests, idle_secs) = match daemon.shared.lock() {
                    Ok(s) => (s.requests, s.last.elapsed().as_secs()),
                    Err(_) => return failure("poisoned"),
                };
                Response {
                    ok: true,
                    stats: Some(Stats {
                        pid: std::process::id(),
                        binary: binary.to_string(),
                        uptime_secs: started.elapsed().as_secs(),
                        idle_secs,
                        idle_timeout_secs: idle.as_secs(),
                        requests,
                        models: daemon.warm.models(),
                        repos: daemon.warm.repos(),
                        busy: daemon.warm.busy(),
                    }),
                    ..base
                }
            }
            Op::Ask { .. } | Op::Status => {
                if req.proto != PROTOCOL || req.client != binary {
                    return Response {
                        error: Some("mismatch".into()),
                        ..base
                    };
                }
                {
                    let Ok(mut s) = daemon.shared.lock() else {
                        return failure("poisoned");
                    };
                    if s.life.handle(ResidentEvent::Request).is_err() {
                        return Response {
                            error: Some("draining".into()),
                            ..base
                        };
                    }
                    s.in_flight += 1;
                    s.requests += 1;
                }
                // Served without the process-wide lock: only this repository's lock is held.
                let mut reply = daemon.warm.serve(&req, progress);
                if let Ok(mut s) = daemon.shared.lock() {
                    s.in_flight -= 1;
                    s.last = Instant::now();
                }
                reply.binary = binary.to_string();
                reply
            }
        }
    }
}

#[cfg(not(unix))]
pub mod server {
    use std::path::Path;
    use std::time::Duration;

    pub fn serve(_home: &Path, _idle: Duration) -> Result<(), String> {
        Err("the background daemon is not supported on this platform yet".into())
    }
}

#[cfg(unix)]
pub mod client {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use wn_daemon::connect::{ConnectEvent as E, ConnectState as S, Connection};

    use super::{
        binary_id, disabled, request, socket_path, Op, OpKind, Request, Response, Stats, PROTOCOL,
    };
    use crate::progress::Sink as _;
    use crate::Cli;

    const CONNECT_WAIT: Duration = Duration::from_secs(5);
    const HANDSHAKE_WAIT: Duration = Duration::from_secs(5);

    fn connect(socket: &Path) -> std::io::Result<UnixStream> {
        UnixStream::connect(socket)
    }

    fn exchange(
        stream: &mut UnixStream,
        req: &Request,
        timeout: Option<Duration>,
    ) -> Option<Response> {
        exchange_with(stream, req, timeout, &mut |_| {})
    }

    /// Sends one request and reads its response, passing interim progress lines to `progress`.
    fn exchange_with(
        stream: &mut UnixStream,
        req: &Request,
        timeout: Option<Duration>,
        progress: &mut dyn FnMut(&str),
    ) -> Option<Response> {
        stream.set_read_timeout(timeout).ok()?;
        let mut line = serde_json::to_string(req).ok()?;
        line.push('\n');
        stream.write_all(line.as_bytes()).ok()?;
        stream.flush().ok()?;
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        loop {
            let mut reply = String::new();
            if reader.read_line(&mut reply).ok()? == 0 {
                return None;
            }
            let reply: Response = serde_json::from_str(&reply).ok()?;
            match &reply.progress {
                Some(p) => progress(p),
                None => return Some(reply),
            }
        }
    }

    fn control(op: Op) -> Request {
        Request {
            proto: PROTOCOL,
            client: binary_id(),
            repo: None,
            model: None,
            json: false,
            op,
        }
    }

    fn spawn(home: &Path) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let _ = std::fs::create_dir_all(home);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(home.join("daemon.log"));
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["daemon", "serve"])
            .env("WHERE_NEXT_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(match log {
                Ok(f) => Stdio::from(f),
                Err(_) => Stdio::null(),
            });
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd.spawn().is_ok()
    }

    fn wait_for_socket(socket: &Path) -> Option<UnixStream> {
        let deadline = Instant::now() + CONNECT_WAIT;
        while Instant::now() < deadline {
            if let Ok(s) = connect(socket) {
                return Some(s);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        None
    }

    fn wait_for_gone(socket: &Path) -> bool {
        let deadline = Instant::now() + CONNECT_WAIT;
        while Instant::now() < deadline {
            if connect(socket).is_err() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    /// Drives the [`Connection`] machine to `Ready` (returns the stream) or `InProcess` (`None`).
    fn establish(home: &Path) -> Option<UnixStream> {
        let socket = socket_path(home);
        let mut c = Connection::default();
        let _ = c.handle(E::Connect);
        let mut stream: Option<UnixStream> = None;
        loop {
            let event = match c.state() {
                S::Connecting => match connect(&socket) {
                    Ok(s) => {
                        stream = Some(s);
                        E::Connected
                    }
                    // Nothing usable listens (missing file, refused, or a stale non-socket
                    // file): start a daemon, which replaces the stale path. Only a socket we may
                    // not touch sends the command in-process.
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => E::TimedOut,
                    Err(_) => E::Refused,
                },
                S::Spawning | S::Respawning => {
                    if spawn(home) {
                        E::Spawned
                    } else {
                        E::SpawnFailed
                    }
                }
                S::Waiting | S::Rewaiting => match wait_for_socket(&socket) {
                    Some(s) => {
                        stream = Some(s);
                        E::Connected
                    }
                    None => E::TimedOut,
                },
                S::Handshaking | S::Rehandshaking => {
                    let s = stream.as_mut()?;
                    match exchange(s, &control(Op::Hello), Some(HANDSHAKE_WAIT)) {
                        Some(r) if r.ok => E::Accepted,
                        Some(r) if r.error.as_deref() == Some("mismatch") => E::Mismatch,
                        _ => E::TimedOut,
                    }
                }
                S::Restarting => {
                    if let Some(s) = stream.as_mut() {
                        let _ = exchange(s, &control(Op::Stop), Some(HANDSHAKE_WAIT));
                    }
                    stream = None;
                    if wait_for_gone(&socket) {
                        E::Stopped
                    } else {
                        E::TimedOut
                    }
                }
                S::Ready => return stream,
                S::InProcess | S::Idle => return None,
            };
            if c.handle(event).is_err() {
                return None;
            }
        }
    }

    /// Runs a command through the daemon. `None` means "answer in-process".
    pub fn call(cli: &Cli, kind: OpKind, context: &str) -> Option<(String, i32)> {
        if disabled(cli) {
            return None;
        }
        let home = crate::home();
        let req = request(cli, kind, context)?;
        let mut stream = establish(&home)?;
        // No read timeout: a first query may build a large index (its progress goes to stderr).
        let render = crate::progress::Render::stderr();
        let reply = exchange_with(&mut stream, &req, None, &mut |line| render.update(line));
        render.done();
        match reply {
            Some(r) if r.ok => Some((r.text, r.code)),
            _ => None,
        }
    }

    /// What `wn daemon status` finds.
    pub enum Probe {
        /// A daemon answered with its statistics.
        Stats(Box<Stats>),
        /// A daemon accepted the connection but did not answer in time.
        Silent,
        /// Nothing listens.
        Absent,
    }

    /// Asks a running daemon for statistics (never starts one).
    pub fn probe(home: &Path) -> Probe {
        let Ok(mut s) = connect(&socket_path(home)) else {
            return Probe::Absent;
        };
        match exchange(&mut s, &control(Op::Stats), Some(HANDSHAKE_WAIT)).and_then(|r| r.stats) {
            Some(stats) => Probe::Stats(Box::new(stats)),
            None => Probe::Silent,
        }
    }

    /// Daemon statistics, if one is running and answers (never starts one).
    pub fn stats(home: &Path) -> Option<Stats> {
        match probe(home) {
            Probe::Stats(s) => Some(*s),
            _ => None,
        }
    }

    /// Stops a running daemon. Returns whether one was running.
    pub fn stop(home: &Path) -> bool {
        let socket = socket_path(home);
        let Ok(mut s) = connect(&socket) else {
            return false;
        };
        let _ = exchange(&mut s, &control(Op::Stop), Some(HANDSHAKE_WAIT));
        wait_for_gone(&socket)
    }

    /// Starts the daemon if needed (replacing one from another build) and returns its stats.
    pub fn ensure_started(home: &Path) -> Result<Stats, String> {
        establish(home).ok_or_else(|| "the daemon did not come up (see daemon.log)".to_string())?;
        stats(home).ok_or_else(|| "the daemon did not answer".to_string())
    }
}

#[cfg(not(unix))]
pub mod client {
    use std::path::Path;

    use super::{OpKind, Stats};
    use crate::Cli;

    pub fn call(_cli: &Cli, _kind: OpKind, _context: &str) -> Option<(String, i32)> {
        None
    }

    pub fn stats(_home: &Path) -> Option<Stats> {
        None
    }

    /// What `wn daemon status` finds.
    pub enum Probe {
        Stats(Box<Stats>),
        Silent,
        Absent,
    }

    pub fn probe(_home: &Path) -> Probe {
        Probe::Absent
    }

    pub fn stop(_home: &Path) -> bool {
        false
    }

    pub fn ensure_started(_home: &Path) -> Result<Stats, String> {
        Err("the background daemon is not supported on this platform yet".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescans_back_off_to_ten_times_the_scan_cost() {
        let floor = handler::rescan_every();
        assert_eq!(handler::next_pause(Duration::from_millis(1)), floor);
        assert_eq!(
            handler::next_pause(Duration::from_secs(2)),
            Duration::from_secs(20).max(floor)
        );
    }

    #[test]
    fn long_homes_get_a_short_hashed_socket() {
        let short = Path::new("/tmp/h");
        assert_eq!(socket_path(short), short.join("daemon.sock"));
        let long = PathBuf::from(format!("/tmp/{}", "x".repeat(120)));
        let p = socket_path(&long);
        assert!(p.starts_with("/tmp") && p.as_os_str().len() < 40, "{p:?}");
        assert_eq!(p, socket_path(&long), "stable");
        let other = PathBuf::from(format!("/tmp/{}", "y".repeat(120)));
        assert_ne!(socket_path(&other), p, "distinct homes, distinct sockets");
    }

    #[test]
    fn requests_and_responses_round_trip() {
        let req = Request {
            proto: PROTOCOL,
            client: "b".into(),
            repo: Some("/r".into()),
            model: None,
            json: true,
            op: Op::Ask {
                args: AskArgs {
                    query: "where is auth".into(),
                    functions: false,
                    k: 3,
                    no_adapter: false,
                    strict: true,
                    no_abstain: false,
                    start: true,
                    start_min_files: 3000,
                    no_log: false,
                },
                context: "Traceback".into(),
            },
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(!line.contains('\n'), "one request per line");
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), req);
        let hello: Request =
            serde_json::from_str(r#"{"proto":1,"client":"x","op":"hello"}"#).unwrap();
        assert_eq!(hello.op, Op::Hello);
        let resp = Response {
            ok: true,
            text: "a\nb".into(),
            ..Response::default()
        };
        let line = serde_json::to_string(&resp).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), resp);
    }

    #[test]
    fn binary_id_can_be_pinned_for_tests_and_otherwise_names_the_version() {
        let id = binary_id();
        assert!(
            id.starts_with(env!("CARGO_PKG_VERSION"))
                || std::env::var_os("WN_DAEMON_BINARY_ID").is_some()
        );
    }
}
