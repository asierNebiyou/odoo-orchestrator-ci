//! Supervises a real Odoo server process (`odoo-bin`) — task 2.2. Mirrors
//! `postgres.rs`'s shape deliberately (the same `Stopped → Starting →
//! Running → Stopping → Crashed` state machine, the same generation guard,
//! the same listener mechanism) because the "app is a process supervisor"
//! idea from the technical-design doc applies identically here: Odoo is
//! just another local process on a port to start, watch, and stop.
//!
//! **What's genuinely different from Postgres, not just copy-pasted:**
//! `pg_ctl` daemonizes and hands back a pidfile — `PgSupervisor` never
//! holds the actual child process, only a command it ran. `odoo-bin` runs
//! in the foreground, so this module keeps the live `tokio::process::Child`
//! for the whole time it's supervised: its stdout/stderr are read
//! continuously and forwarded line-by-line (the process log this project's
//! event system exists to surface), and its exit is observed directly via
//! `child.wait()` rather than inferred from a periodic `pg_isready`-style
//! poll — so crash detection here is immediate, not bounded by a health-
//! check interval.
//!
//! **Known environment limitation, stated up front rather than glossed
//! over:** this sandbox's egress policy blocks `github.com`,
//! `codeload.github.com`, and `api.github.com` — and real Odoo's source is
//! only distributed from GitHub (there is no legitimate full "odoo" package
//! on PyPI; the name is squatted — `pypi.org/pypi/odoo/json` returns an
//! unrelated placeholder project). So this can't be verified end-to-end
//! against a real `odoo-bin` *in this sandbox*, the same class of gap as
//! "no Tauri window" documented in the repo README. What **is** verified
//! here, against a real subprocess (not a mock): spawning, live
//! stdout-and-stderr streaming, graceful SIGTERM-then-escalate stop, and
//! out-of-band crash detection — all the process-supervision behavior that
//! has nothing to do with Odoo specifically. `python_runtime.rs` (used to
//! provision the interpreter this module would launch `odoo-bin` with) *is*
//! verified against real `uv` and real PyPI. Once a real Odoo checkout
//! exists on a target machine, this supervisor should need zero code
//! changes to run it — it already takes an arbitrary interpreter path,
//! working directory, argv, and environment, and only *assumes* the
//! process serves HTTP on `config.port` once ready (true of `odoo-bin`,
//! not Odoo-specific in how it's checked).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum OdooState {
    Stopped,
    Starting,
    Running { pid: u32 },
    Stopping,
    Crashed { exit_code: Option<i32> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// What a listener is notified of — either a state transition (see
/// `OdooState`) or one line of the process's own output. Log lines are
/// delivered as they're printed, not batched, so a live "Activity" view can
/// tail a running server the same way a terminal would.
#[derive(Debug, Clone)]
pub enum OdooNotification {
    State(OdooState),
    Log { stream: LogStream, line: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OdooError {
    #[error("odoo server is already running or stopping")]
    AlreadyRunning,
    #[error("couldn't spawn {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("process exited before becoming ready (exit code: {exit_code:?})")]
    ExitedDuringStartup { exit_code: Option<i32> },
    #[error("odoo server didn't start serving on port {port} within {timeout_secs}s")]
    StartupTimeout { port: u16, timeout_secs: u64 },
    /// Something is already answering on this port. Named as its own case
    /// rather than left to surface as a startup timeout, because the two
    /// have completely different fixes — one is "wait longer or read the
    /// log", the other is "something else has your port".
    #[error("something is already serving port {port} — if that's an Odoo this app lost track of, stop it and try again")]
    PortInUse { port: u16 },
}

/// Everything needed to spawn and supervise one Odoo server process.
/// Deliberately generic over "run this interpreter with these args in this
/// directory" — see the module doc comment for why that's not a cop-out.
#[derive(Debug, Clone)]
pub struct OdooConfig {
    /// The Python interpreter to run — normally a venv's `python` from
    /// `python_runtime.rs`, e.g. `.../venv/bin/python`.
    pub python_bin: PathBuf,
    /// `odoo-bin`'s own working directory (the Odoo source checkout).
    pub working_dir: PathBuf,
    /// Full argv passed to `python_bin`, e.g.
    /// `["odoo-bin", "-c", "/path/to/odoo.conf"]`.
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The HTTP port Odoo will serve on once ready — polled to detect
    /// readiness, exactly like `pg_isready` does for Postgres, just over
    /// raw HTTP instead of a Postgres-specific command (no HTTP client
    /// dependency needed for a bare "does something HTTP-shaped answer").
    pub port: u16,
    pub startup_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub health_check_interval: Duration,
}

impl OdooConfig {
    pub fn new(python_bin: impl Into<PathBuf>, working_dir: impl Into<PathBuf>, args: Vec<String>, port: u16) -> Self {
        Self {
            python_bin: python_bin.into(),
            working_dir: working_dir.into(),
            args,
            env: Vec::new(),
            port,
            startup_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(15),
            health_check_interval: Duration::from_millis(300),
        }
    }
}

struct Inner {
    state: OdooState,
    /// The currently-spawned process's pid, if any — kept separately from
    /// `OdooState::Running`'s own `pid` field because it also needs to be
    /// known while `Starting` (to signal it) and while `Stopping` (ditto).
    current_pid: Option<u32>,
    /// Bumped only by a fresh `start()` — see the module doc comment on why
    /// `stop()` deliberately does *not* bump this the way `PgSupervisor`'s
    /// does: the wait-task that observes this process's real exit needs to
    /// still match this generation in order to apply the final `Stopped`/
    /// `Crashed` transition itself.
    generation: u64,
}

type NotifyListener = Arc<dyn Fn(OdooNotification) + Send + Sync>;

/// A handle to one supervised Odoo process. Cheap to clone (Arc inside);
/// every clone shares the same underlying state and background tasks.
#[derive(Clone)]
pub struct OdooSupervisor {
    config: Arc<OdooConfig>,
    inner: Arc<Mutex<Inner>>,
    on_notify: NotifyListener,
    /// Stops the Odoo process when the **last** handle goes away. Its own
    /// `Arc` so the `Drop` fires once, when every clone is gone. Exactly
    /// the same shape as `PgSupervisor`'s guard, and for the same reason —
    /// see `StopOnDrop` below.
    _stop_on_drop: Arc<StopOnDrop>,
}

/// Quitting the app must not leave `odoo-bin` running.
///
/// Without this, a `Core` that goes away — the app closing, a test
/// panicking before its `stop()` — leaves a live Odoo holding its HTTP
/// port, and the next start fails with a port already in use by a process
/// nothing is tracking. The identical bug existed in `PgSupervisor` and
/// was fixed there first; this is the same fix for the other half of the
/// pair, because "everything is a supervised local process" has to mean
/// both of them.
///
/// Best-effort and synchronous — a `Drop` cannot await. TERM first, a
/// short grace period, then KILL, so a normal shutdown is still clean.
struct StopOnDrop {
    inner: Arc<Mutex<Inner>>,
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let pid = {
            let Ok(inner) = self.inner.lock() else { return };
            match inner.state {
                OdooState::Running { .. } | OdooState::Starting | OdooState::Stopping => inner.current_pid,
                _ => None,
            }
        };
        let Some(pid) = pid else { return };
        send_signal(pid, "-TERM");
        // A short, bounded wait: long enough for Odoo to close its own
        // connections, short enough that quitting the app never feels hung.
        for _ in 0..20 {
            if !process_is_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        send_signal(pid, "-KILL");
    }
}

/// What the background tasks hold: the shared state and its listener, and
/// **not** the stop-on-drop guard. A task that watched the process while
/// keeping it alive would make the guard unreachable — the mistake caught
/// in `PgSupervisor`, where the health watcher's own clone meant the last
/// handle could never be last.
#[derive(Clone)]
struct TaskHandle {
    inner: Arc<Mutex<Inner>>,
    on_notify: NotifyListener,
}

impl TaskHandle {
    fn notify_log_if_current(&self, generation: u64, stream: LogStream, line: String) {
        let current = self.inner.lock().expect("odoo supervisor mutex poisoned").generation;
        if current == generation {
            (self.on_notify)(OdooNotification::Log { stream, line });
        }
    }
}

impl OdooSupervisor {
    pub fn new(config: OdooConfig) -> Self {
        Self::with_listener(config, |_| {})
    }

    pub fn with_listener(config: OdooConfig, listener: impl Fn(OdooNotification) + Send + Sync + 'static) -> Self {
        let inner = Arc::new(Mutex::new(Inner { state: OdooState::Stopped, current_pid: None, generation: 0 }));
        Self {
            config: Arc::new(config),
            _stop_on_drop: Arc::new(StopOnDrop { inner: inner.clone() }),
            inner,
            on_notify: Arc::new(listener),
        }
    }

    /// A handle for a background task: shares this supervisor's state
    /// without keeping its process alive.
    fn task_handle(&self) -> TaskHandle {
        TaskHandle { inner: self.inner.clone(), on_notify: self.on_notify.clone() }
    }

    pub fn state(&self) -> OdooState {
        self.inner.lock().expect("odoo supervisor mutex poisoned").state
    }

    /// Real memory and CPU for this Odoo, or `None` when it isn't running.
    pub fn usage(&self) -> Option<ProcessUsage> {
        let pid = self.inner.lock().expect("odoo supervisor mutex poisoned").current_pid?;
        process_usage(pid)
    }

    fn set_state_if_current(&self, generation: u64, new_state: OdooState) {
        let applied = {
            let mut inner = self.inner.lock().expect("odoo supervisor mutex poisoned");
            if inner.generation == generation {
                inner.state = new_state;
                if matches!(new_state, OdooState::Stopped | OdooState::Crashed { .. }) {
                    inner.current_pid = None;
                }
                true
            } else {
                false
            }
        };
        if applied {
            (self.on_notify)(OdooNotification::State(new_state));
        }
    }

    // `notify_log_if_current` lives on `TaskHandle` now — the log reader
    // is the only caller, and it must not hold the supervisor itself.

    /// Spawns the configured process and returns immediately once it's
    /// actually serving HTTP on `config.port` (or errors out — startup
    /// timeout, or the process exiting before it got there).
    pub async fn start(&self) -> Result<(), OdooError> {
        let generation = {
            let mut inner = self.inner.lock().expect("odoo supervisor mutex poisoned");
            if matches!(inner.state, OdooState::Running { .. } | OdooState::Starting | OdooState::Stopping) {
                return Err(OdooError::AlreadyRunning);
            }
            inner.generation += 1;
            inner.state = OdooState::Starting;
            inner.current_pid = None;
            inner.generation
        };
        (self.on_notify)(OdooNotification::State(OdooState::Starting));

        // Something is already serving this port. Deliberately *not*
        // adopted the way `PgSupervisor` adopts a leftover cluster: a
        // PostgreSQL data directory identifies its own postmaster, so
        // adoption there is provably reclaiming our own process. A TCP
        // port proves nothing — the listener could be an Odoo this app
        // forgot, another copy of this app, or something else entirely,
        // and claiming a stranger's process as ours would be worse than
        // failing. So this reports exactly what it found and stops, rather
        // than spawning a process that will die on bind and surface as a
        // confusing startup timeout.
        if tcp_http_ready(self.config.port).await {
            self.set_state_if_current(generation, OdooState::Stopped);
            return Err(OdooError::PortInUse { port: self.config.port });
        }

        let config = self.config.clone();
        let mut cmd = Command::new(&config.python_bin);
        cmd.args(&config.args)
            .current_dir(&config.working_dir)
            .envs(config.env.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.set_state_if_current(generation, OdooState::Stopped);
                return Err(OdooError::Spawn { command: config.args.join(" "), source: e });
            }
        };
        let pid = child.id().unwrap_or(0);
        {
            let mut inner = self.inner.lock().expect("odoo supervisor mutex poisoned");
            if inner.generation == generation {
                inner.current_pid = Some(pid);
            }
        }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        if let Some(stdout) = stdout {
            self.spawn_log_reader(generation, LogStream::Stdout, stdout);
        }
        if let Some(stderr) = stderr {
            self.spawn_log_reader(generation, LogStream::Stderr, stderr);
        }
        self.spawn_wait_task(generation, child);

        let deadline = Instant::now() + config.startup_timeout;
        loop {
            {
                let inner = self.inner.lock().expect("odoo supervisor mutex poisoned");
                if inner.generation != generation {
                    // Superseded by a stop()/start() that raced with us —
                    // whatever it decided wins, not this startup poll.
                    return Ok(());
                }
                if let OdooState::Crashed { exit_code } = inner.state {
                    return Err(OdooError::ExitedDuringStartup { exit_code });
                }
            }
            if tcp_http_ready(config.port).await {
                self.set_state_if_current(generation, OdooState::Running { pid });
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(config.health_check_interval).await;
        }

        send_signal(pid, "-KILL");
        self.set_state_if_current(generation, OdooState::Stopped);
        Err(OdooError::StartupTimeout { port: config.port, timeout_secs: config.startup_timeout.as_secs() })
    }

    /// Stops the process gracefully (`SIGTERM`, per the same "fast, don't
    /// babysit clients" posture `postgres.rs` uses), escalating to `SIGKILL`
    /// if it hasn't exited within `shutdown_timeout`.
    pub async fn stop(&self) -> Result<(), OdooError> {
        let (generation, pid) = {
            let mut inner = self.inner.lock().expect("odoo supervisor mutex poisoned");
            match inner.state {
                OdooState::Stopped | OdooState::Stopping => return Ok(()),
                OdooState::Crashed { .. } => {
                    inner.state = OdooState::Stopped;
                    inner.generation += 1;
                    inner.current_pid = None;
                    drop(inner);
                    (self.on_notify)(OdooNotification::State(OdooState::Stopped));
                    return Ok(());
                }
                OdooState::Starting | OdooState::Running { .. } => {
                    inner.state = OdooState::Stopping;
                    (inner.generation, inner.current_pid)
                }
            }
        };
        (self.on_notify)(OdooNotification::State(OdooState::Stopping));

        let Some(pid) = pid else {
            self.set_state_if_current(generation, OdooState::Stopped);
            return Ok(());
        };

        send_signal(pid, "-TERM");
        let deadline = Instant::now() + self.config.shutdown_timeout;
        while Instant::now() < deadline {
            if !matches!(self.state(), OdooState::Stopping) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        // Didn't exit in time on its own — escalate.
        send_signal(pid, "-KILL");
        for _ in 0..30 {
            if !matches!(self.state(), OdooState::Stopping) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    fn spawn_log_reader(&self, generation: u64, stream: LogStream, pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static) {
        let this = self.task_handle();
        tokio::spawn(async move {
            let mut lines = BufReader::new(pipe).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => this.notify_log_if_current(generation, stream, line),
                    _ => return,
                }
            }
        });
    }

    fn spawn_wait_task(&self, generation: u64, mut child: tokio::process::Child) {
        // Deliberately a task handle, not a clone: this task lives until
        // the process exits, and holding the supervisor would mean the
        // guard above could never run. It still reaps the child either way.
        let this = self.task_handle();
        tokio::spawn(async move {
            let status = child.wait().await;
            let mut inner = this.inner.lock().expect("odoo supervisor mutex poisoned");
            if inner.generation != generation {
                return; // superseded — a newer start() or an explicit reset already decided the state
            }
            let new_state = match inner.state {
                // We asked it to stop and it did — expected, not a crash.
                OdooState::Stopping => OdooState::Stopped,
                // It exited on its own while we thought it was up (or still
                // coming up) — that's the actual crash case.
                _ => OdooState::Crashed { exit_code: status.ok().and_then(|s| exit_code_of(&s)) },
            };
            inner.state = new_state;
            inner.current_pid = None;
            drop(inner);
            (this.on_notify)(OdooNotification::State(new_state));
        });
    }
}

#[cfg(unix)]
fn exit_code_of(status: &std::process::ExitStatus) -> Option<i32> {
    status.code() // None here means it died to a signal, e.g. our own SIGKILL
}

#[cfg(not(unix))]
fn exit_code_of(status: &std::process::ExitStatus) -> Option<i32> {
    status.code()
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: &str) {
    // Shelling out to `kill` rather than adding a `libc`/`nix` dependency —
    // the same tradeoff `postgres.rs`'s own tests already make to simulate
    // a crash, reused here as an actual production code path.
    let _ = std::process::Command::new("kill").arg(signal).arg(pid.to_string()).status();
}

/// Real resident memory and CPU for one process, read from the OS.
///
/// Measured, never estimated: the Stats screen deliberately showed uptime
/// and nothing else while this didn't exist, because a made-up memory
/// figure for a process touching client data is worse than an honest gap.
///
/// `ps` rather than a `sysinfo`-style dependency: it's on every Unix
/// target this app ships to, it's the same tradeoff `send_signal` above
/// already makes, and it keeps this to one small function instead of a
/// crate that samples the whole system to answer about one pid.
///
/// `None` means the process wasn't there to measure — already exited, or
/// a platform where this isn't implemented — not "zero".
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProcessUsage {
    /// Resident set size: memory actually held in RAM right now.
    pub memory_bytes: u64,
    /// Percent of one CPU, as the OS reports it. Averaged over the
    /// process's life on Linux rather than instantaneous, which is worth
    /// knowing before reading too much into a single number.
    pub cpu_percent: f32,
}

#[cfg(unix)]
pub fn process_usage(pid: u32) -> Option<ProcessUsage> {
    let output = std::process::Command::new("ps")
        .arg("-o")
        .arg("rss=,pcpu=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.split_whitespace();
    // `rss` is in kilobytes on both Linux and macOS.
    let rss_kb: u64 = fields.next()?.parse().ok()?;
    let cpu_percent: f32 = fields.next()?.parse().ok()?;
    Some(ProcessUsage { memory_bytes: rss_kb * 1024, cpu_percent })
}

#[cfg(not(unix))]
pub fn process_usage(_pid: u32) -> Option<ProcessUsage> {
    None
}

/// Whether a pid is still around — `kill -0`, the standard "does this
/// process exist" probe, which signals nothing and just reports.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Windows: `tasklist` filtered to one pid prints a header and nothing
/// else when the process is gone, so the pid appearing in the output is
/// the check. Same "untested on Windows" caveat as `send_signal`.
#[cfg(not(unix))]
fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .arg("/FI")
        .arg(format!("PID eq {pid}"))
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Windows has no signals. `taskkill` is the platform's own equivalent:
/// without `/F` it posts WM_CLOSE and lets the process shut down on its
/// own, with `/F` it terminates outright — which maps exactly onto the
/// TERM-then-KILL escalation the Unix path uses, so `stop()` and the
/// drop guard need no platform-specific logic of their own.
///
/// **Untested on Windows.** Nothing in this project has run there yet, so
/// this is written from the documented behaviour rather than observed —
/// worth knowing before trusting it. It replaces a path that previously
/// just logged "not implemented" and killed nothing at all, which meant a
/// Windows user's Odoo processes survived every stop.
#[cfg(not(unix))]
fn send_signal(pid: u32, signal: &str) {
    let mut cmd = std::process::Command::new("taskkill");
    cmd.arg("/PID").arg(pid.to_string()).arg("/T");
    if signal == "-KILL" {
        cmd.arg("/F");
    }
    let _ = cmd.status();
}

/// Readiness probe: connect over TCP and send a minimal HTTP/1.0 request,
/// then check the response starts with `HTTP/` — proving something
/// HTTP-shaped is actually listening (not just that *a* process opened the
/// port), without pulling in an HTTP client dependency for it.
async fn tcp_http_ready(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    let stream = match tokio::time::timeout(Duration::from_millis(500), TcpStream::connect(&addr)).await {
        Ok(Ok(stream)) => stream,
        _ => return false,
    };
    let mut stream = stream;
    if stream.write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").await.is_err() {
        return false;
    }
    let mut buf = [0u8; 16];
    match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => buf[..n].starts_with(b"HTTP/"),
        _ => false,
    }
}

/// Real output of a completed one-shot command (see `run_one_shot`).
#[derive(Debug, Clone)]
pub struct OneShotOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `python_bin args...` to completion in `working_dir` and captures its
/// output — task 2.4's module install/upgrade/uninstall commands are
/// one-shot (`odoo-bin -i/-u ... --stop-after-init`, or `odoo-bin shell`
/// fed a script on stdin), not a long-running server, so they don't need
/// `OdooSupervisor`'s readiness-polling/crash-detection state machine at
/// all — this is the simpler, complete-and-report primitive underneath
/// them, kept here since it's exactly as Odoo-agnostic as the rest of this
/// module (see the module doc comment).
pub async fn run_one_shot(
    python_bin: &std::path::Path,
    working_dir: &std::path::Path,
    args: &[String],
    env: &[(String, String)],
    stdin: Option<&str>,
) -> Result<OneShotOutput, OdooError> {
    let mut cmd = Command::new(python_bin);
    cmd.args(args)
        .current_dir(working_dir)
        .envs(env.iter().cloned())
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| OdooError::Spawn { command: args.join(" "), source: e })?;
    if let Some(stdin) = stdin {
        // The child's own stdout/stderr are being captured too (piped), so
        // writing stdin before `wait_with_output` (which drains those pipes
        // concurrently) risks a deadlock if the child writes enough output
        // to fill its pipe buffer before reading all of stdin — write on a
        // separate task so both directions make progress at once.
        let mut pipe = child.stdin.take().expect("stdin was requested as piped");
        let stdin = stdin.to_string();
        tokio::spawn(async move {
            let _ = pipe.write_all(stdin.as_bytes()).await;
            // Dropping `pipe` here closes the write end, which is what
            // signals EOF to the child (e.g. `odoo-bin shell` reading a
            // script from stdin).
        });
    }

    let output = child.wait_with_output().await.map_err(|e| OdooError::Spawn { command: args.join(" "), source: e })?;
    Ok(OneShotOutput {
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;

    /// A real, minimal HTTP server standing in for `odoo-bin` — this module
    /// tests process supervision (spawn, log streaming, stop, crash
    /// detection), which has nothing to do with Odoo specifically. See the
    /// module doc comment for why real `odoo-bin` isn't reachable here.
    /// Prints a stdout line and a stderr line on startup (so log-streaming
    /// tests can assert against a known, real line from each stream), then
    /// serves real HTTP responses until it's killed.
    const STAND_IN_SERVER: &str = r#"
import socketserver, http.server, sys

port = int(sys.argv[1])
print("stand-in server starting on port %d" % port, flush=True)
print("stand-in server stderr line", file=sys.stderr, flush=True)

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, *args):
        pass

with socketserver.TCPServer(("127.0.0.1", port), Handler) as httpd:
    print("ready", flush=True)
    httpd.serve_forever()
"#;

    /// A stand-in that deliberately never opens its port — for the startup-
    /// timeout test.
    const NEVER_READY_SERVER: &str = r#"
import time
print("never-ready server: sleeping instead of binding a port", flush=True)
time.sleep(60)
"#;

    fn write_script(dir: &std::path::Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn free_port() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    }

    /// Any Python 3 will do — this is a stand-in HTTP server, not Odoo
    /// itself. Windows has no `python3` on `PATH` even when Python 3 is
    /// installed (`actions/setup-python` on `windows-latest` only provides
    /// `python.exe`), so every plausible name is tried rather than assuming
    /// the Unix convention.
    fn python3() -> PathBuf {
        let candidates: &[&str] = if cfg!(windows) {
            &["python3.exe", "python.exe", "python3", "python"]
        } else {
            &["python3", "python"]
        };
        for dir in std::env::split_paths(&std::env::var("PATH").unwrap_or_default()) {
            for name in candidates {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        panic!("these tests need a system python 3 to run the stand-in HTTP server");
    }

    fn stand_in_config(dir: &std::path::Path, port: u16) -> OdooConfig {
        let script = write_script(dir, "stand_in.py", STAND_IN_SERVER);
        let mut config = OdooConfig::new(python3(), dir, vec!["-u".into(), script.display().to_string(), port.to_string()], port);
        config.startup_timeout = Duration::from_secs(10);
        config.shutdown_timeout = Duration::from_secs(5);
        config.health_check_interval = Duration::from_millis(100);
        config
    }

    /// Quitting must not leave `odoo-bin` running. The same guarantee
    /// `PgSupervisor` makes — and the bug that existed here until the
    /// PostgreSQL fix was carried across.
    #[tokio::test]
    async fn dropping_the_last_handle_stops_the_process() {
        let dir = TempDir::new().unwrap();
        let port = free_port();
        {
            let sup = OdooSupervisor::new(stand_in_config(dir.path(), port));
            sup.start().await.expect("stand-in server should start");
            assert!(matches!(sup.state(), OdooState::Running { .. }));

            // A clone going out of scope must not stop it — only the last
            // handle should.
            {
                let clone = sup.clone();
                assert!(matches!(clone.state(), OdooState::Running { .. }));
            }
            assert!(tcp_http_ready(port).await, "dropping one clone of a shared handle must leave the process alone");
        }
        // Every handle is gone. The guard runs synchronously on drop, so
        // the port is already free by the time we look.
        assert!(!tcp_http_ready(port).await, "the process must be stopped once the last handle is dropped");
    }

    /// A port already in use gets its own error, rather than being left to
    /// surface as a startup timeout — the two have completely different
    /// fixes, and a wrong one sends someone reading logs for a process
    /// that never started.
    #[tokio::test]
    async fn starting_on_a_port_something_else_holds_says_so_plainly() {
        let dir = TempDir::new().unwrap();
        let port = free_port();

        let squatter = OdooSupervisor::new(stand_in_config(dir.path(), port));
        squatter.start().await.expect("the first one should start");

        let second = OdooSupervisor::new(stand_in_config(dir.path(), port));
        let err = second.start().await.expect_err("a second server on the same port must not appear to succeed");
        assert!(matches!(err, OdooError::PortInUse { port: p } if p == port), "got {err:?}");
        assert_eq!(second.state(), OdooState::Stopped, "a refused start must not leave the state stuck at Starting");

        squatter.stop().await.unwrap();
    }

    /// Memory and CPU are measured from the OS, or absent. The Stats
    /// screen showed uptime and nothing else while this didn't exist,
    /// because inventing a memory figure for a process touching client
    /// data is worse than an honest gap.
    #[tokio::test]
    async fn usage_reports_real_numbers_while_running_and_nothing_when_not() {
        let dir = TempDir::new().unwrap();
        let port = free_port();
        let sup = OdooSupervisor::new(stand_in_config(dir.path(), port));

        assert!(sup.usage().is_none(), "a process that was never started has no usage to report — not zero");

        sup.start().await.expect("stand-in server should start");
        let usage = sup.usage().expect("a running process must be measurable");
        assert!(usage.memory_bytes > 0, "a real python process holds real memory: {usage:?}");
        assert!(usage.cpu_percent >= 0.0, "cpu must be a real reading: {usage:?}");

        sup.stop().await.unwrap();
        assert!(sup.usage().is_none(), "a stopped process reports nothing, rather than its last known figures");
    }

    #[tokio::test]
    async fn full_lifecycle_start_ready_stop() {
        let dir = TempDir::new().unwrap();
        let port = free_port();
        let sup = OdooSupervisor::new(stand_in_config(dir.path(), port));

        assert_eq!(sup.state(), OdooState::Stopped);
        sup.start().await.expect("stand-in server should start and become ready");
        assert!(matches!(sup.state(), OdooState::Running { .. }));

        sup.stop().await.expect("should stop cleanly");
        assert_eq!(sup.state(), OdooState::Stopped);
        assert!(!tcp_http_ready(port).await, "shouldn't be reachable after a clean stop");
    }

    #[tokio::test]
    async fn starting_twice_is_rejected() {
        let dir = TempDir::new().unwrap();
        let port = free_port();
        let sup = OdooSupervisor::new(stand_in_config(dir.path(), port));

        sup.start().await.unwrap();
        let err = sup.start().await.unwrap_err();
        assert!(matches!(err, OdooError::AlreadyRunning));

        sup.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stop_without_start_is_a_harmless_no_op() {
        let dir = TempDir::new().unwrap();
        let sup = OdooSupervisor::new(stand_in_config(dir.path(), free_port()));
        sup.stop().await.expect("stopping an already-stopped supervisor should be Ok, not an error");
    }

    #[tokio::test]
    async fn log_lines_are_streamed_from_both_stdout_and_stderr() {
        let dir = TempDir::new().unwrap();
        let port = free_port();
        let seen: Arc<StdMutex<Vec<OdooNotification>>> = Arc::new(StdMutex::new(Vec::new()));
        let seen_for_listener = seen.clone();
        let sup = OdooSupervisor::with_listener(stand_in_config(dir.path(), port), move |n| {
            seen_for_listener.lock().unwrap().push(n);
        });

        sup.start().await.unwrap();
        sup.stop().await.unwrap();

        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|n| match n {
                OdooNotification::Log { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect();
        assert!(lines.iter().any(|l| l.contains("stand-in server starting on port")), "missing stdout line: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("stand-in server stderr line")), "missing stderr line: {lines:?}");
        assert!(lines.iter().any(|l| l == "ready"), "missing the second stdout line: {lines:?}");
    }

    #[tokio::test]
    async fn detects_a_real_crash_via_process_exit() {
        let dir = TempDir::new().unwrap();
        let port = free_port();
        let sup = OdooSupervisor::new(stand_in_config(dir.path(), port));

        sup.start().await.unwrap();
        let pid = match sup.state() {
            OdooState::Running { pid } => pid,
            other => panic!("expected Running, got {other:?}"),
        };

        // Kill it out-of-band — exactly what an OOM killer or a user's own
        // `kill` would do, not something going through our own stop().
        let killed = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
        assert!(killed.map(|s| s.success()).unwrap_or(false), "test setup: failed to kill the stand-in server");

        let mut noticed = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if matches!(sup.state(), OdooState::Crashed { .. }) {
                noticed = true;
                break;
            }
        }
        assert!(noticed, "wait-task should have observed the process exit and transitioned to Crashed");
    }

    #[tokio::test]
    async fn startup_timeout_kills_the_process_and_reports_the_error() {
        let dir = TempDir::new().unwrap();
        let port = free_port();
        let script = write_script(dir.path(), "never_ready.py", NEVER_READY_SERVER);
        let mut config = OdooConfig::new(python3(), dir.path(), vec!["-u".into(), script.display().to_string()], port);
        config.startup_timeout = Duration::from_millis(800);
        config.health_check_interval = Duration::from_millis(100);
        let sup = OdooSupervisor::new(config);

        let err = sup.start().await.unwrap_err();
        assert!(matches!(err, OdooError::StartupTimeout { .. }), "expected StartupTimeout, got {err:?}");
        assert_eq!(sup.state(), OdooState::Stopped, "should reset to Stopped after killing the never-ready process");
    }

    // --- run_one_shot (task 2.4's underlying primitive) ---------------------

    #[tokio::test]
    async fn run_one_shot_captures_real_output_and_exit_code() {
        let dir = TempDir::new().unwrap();
        let script = write_script(dir.path(), "one_shot.py", "import sys\nprint('hello stdout')\nprint('hello stderr', file=sys.stderr)\nsys.exit(3)\n");

        let output = run_one_shot(&python3(), dir.path(), &[script.display().to_string()], &[], None).await.unwrap();

        assert!(!output.success);
        assert_eq!(output.exit_code, Some(3));
        assert!(output.stdout.contains("hello stdout"));
        assert!(output.stderr.contains("hello stderr"));
    }

    #[tokio::test]
    async fn run_one_shot_pipes_real_stdin_to_the_child() {
        let dir = TempDir::new().unwrap();
        let script = write_script(dir.path(), "echo_stdin.py", "import sys\nprint(sys.stdin.read().strip())\n");

        let output = run_one_shot(&python3(), dir.path(), &[script.display().to_string()], &[], Some("real input from the parent"))
            .await
            .unwrap();

        assert!(output.success);
        assert_eq!(output.stdout.trim(), "real input from the parent");
    }
}
