//! Embedded PostgreSQL process supervisor — task 2.1 in
//! `odoo-orchestrator-task-breakdown.md`. Per
//! `odoo-orchestrator-runtime-architecture.md`, running a Postgres cluster
//! from a self-contained, app-owned data directory with no service
//! registration and no admin rights is the normal supported path on all
//! three desktop OSes; this wraps exactly that (`initdb`/`pg_ctl`), owns the
//! started/stopped/crashed state machine, and does real crash detection via
//! periodic health checks rather than just trusting the last command's exit
//! code.
//!
//! **What this deliberately does *not* do yet:** wire into `Core`, the event
//! bus, or the API — those need a product decision this task alone
//! shouldn't make (per `odoo-orchestrator-runtime-architecture.md`,
//! Postgres is meant to be a single private instance the whole app shares
//! across every `OdooServer`'s databases, which is a schema/lifecycle
//! question that belongs with task 2.2's Odoo runtime supervisor, not
//! smuggled in here). This module is deliberately standalone and fully
//! testable on its own, matching how `manifest.rs`/`modules.rs` were built
//! for workstream 1.
//!
//! **Why there's a `run_as` privilege-drop feature at all:** Postgres
//! refuses outright to run as root, with no override — which a real desktop
//! install never hits (the app runs as the logged-in user, same as
//! Postgres.app/DBngin/Docker Desktop), but this sandbox's build/test
//! environment does run as root. Rather than let that surface as a cryptic
//! Postgres-side `FATAL: cannot run as root` — a black box in exactly the
//! spot this project is trying not to have black boxes — `run_as` is a real,
//! Unix-only, tested feature: drop the child process to a specific
//! (uid, gid) before exec. Production code simply never sets it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::sleep;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PgState {
    Stopped,
    Starting,
    Running { pid: u32 },
    Stopping,
    Crashed { exit_code: Option<i32> },
}

#[derive(Debug, thiserror::Error)]
pub enum PgError {
    #[error("postgres is already running or starting")]
    AlreadyRunning,
    #[error("postgres is not running")]
    NotRunning,
    #[error("couldn't run {command}: {source}")]
    Spawn { command: String, source: std::io::Error },
    #[error("{command} exited with {exit_code:?}: {stderr}")]
    CommandFailed { command: String, exit_code: Option<i32>, stderr: String },
    #[error("postgres didn't become ready within {timeout_secs}s — log tail:\n{log_tail}")]
    StartupTimeout { timeout_secs: u64, log_tail: String },
}

/// Where to find the `initdb`/`postgres`/`pg_ctl`/`pg_isready` binaries and
/// how to run them. In the shipped app `bin_dir` always points at the
/// bundled runtime (per the embedded-binaries decision in
/// `odoo-orchestrator-runtime-architecture.md`); `discover_system_binaries`
/// below is a dev/test convenience only, not a production code path.
#[derive(Debug, Clone)]
pub struct PgConfig {
    pub bin_dir: PathBuf,
    pub data_dir: PathBuf,
    pub port: u16,
    /// Drop the child process to this (uid, gid) before exec. Unix-only —
    /// ignored (with a warning) on other platforms, where it should never be
    /// needed anyway. See the module doc comment for why this exists.
    pub run_as: Option<(u32, u32)>,
    pub startup_timeout: Duration,
    pub shutdown_timeout: Duration,
    /// How often the background watcher checks liveness once `Running`.
    pub health_check_interval: Duration,
}

impl PgConfig {
    pub fn new(bin_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>, port: u16) -> Self {
        Self {
            bin_dir: bin_dir.into(),
            data_dir: data_dir.into(),
            port,
            run_as: None,
            startup_timeout: Duration::from_secs(15),
            shutdown_timeout: Duration::from_secs(15),
            health_check_interval: Duration::from_secs(3),
        }
    }
}

struct Inner {
    state: PgState,
    /// Bumped on every `start()`/`stop()` call; a background health-check
    /// loop started by an earlier `start()` compares this before acting, so
    /// a stale watcher from a previous run can never clobber a newer one's
    /// state (e.g. after stop() → start() in quick succession).
    generation: u64,
}

type StateListener = Arc<dyn Fn(PgState) + Send + Sync>;

/// A handle to one supervised Postgres cluster. Cheap to clone (Arc inside);
/// every clone shares the same underlying state and background watcher.
#[derive(Clone)]
pub struct PgSupervisor {
    config: Arc<PgConfig>,
    inner: Arc<Mutex<Inner>>,
    on_state_change: StateListener,
    /// Stops the cluster when the **last** handle to this supervisor goes
    /// away. Held as its own `Arc` so the `Drop` fires once, when every
    /// clone is gone — not on each clone. See `StopOnDrop`.
    _stop_on_drop: Arc<StopOnDrop>,
}

/// Nobody should be able to lose a running PostgreSQL by dropping a Rust
/// value. Without this, a `Core` that goes away — the app quitting, a test
/// panicking before its explicit `stop()` — leaves a live postmaster
/// holding its port and its data directory, and the next start collides
/// with a process nothing is tracking any more.
///
/// This is deliberately best-effort and silent: it runs `pg_ctl stop -m
/// fast` synchronously (a `Drop` cannot await), gives it a short deadline,
/// and ignores the outcome. A failure here has nowhere useful to go — the
/// value is already being destroyed — and `start()`'s adoption path below
/// is the real safety net for the case this can't cover: a hard kill of
/// the app, where no `Drop` runs at all.
struct StopOnDrop {
    config: Arc<PgConfig>,
    inner: Arc<Mutex<Inner>>,
}

/// What the background health watcher holds: the shared state and config,
/// and nothing that keeps the cluster alive. Carries the same
/// `set_state_if_current`/`is_ready` behaviour the watcher needs, so the
/// loop reads identically to when it held a full `PgSupervisor`.
#[derive(Clone)]
struct WatcherHandle {
    config: Arc<PgConfig>,
    inner: Arc<Mutex<Inner>>,
    on_state_change: StateListener,
}

impl WatcherHandle {
    fn set_state_if_current(&self, generation: u64, new_state: PgState) {
        let applied = {
            let mut inner = self.inner.lock().expect("pg supervisor mutex poisoned");
            if inner.generation == generation {
                inner.state = new_state;
                true
            } else {
                false
            }
        };
        if applied {
            (self.on_state_change)(new_state);
        }
    }

    async fn is_ready(&self) -> bool {
        is_ready_at(&self.config).await
    }
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let (running, pid) = {
            let Ok(inner) = self.inner.lock() else { return };
            match inner.state {
                PgState::Running { pid } => (true, Some(pid)),
                PgState::Starting => (true, None),
                _ => (false, None),
            }
        };
        if !running {
            return;
        }

        // The polite way first: `pg_ctl stop -m fast` lets the postmaster
        // shut its clusters down properly.
        let mut cmd = Command::new(self.config.bin_dir.join("pg_ctl"));
        cmd.arg("-D").arg(&self.config.data_dir).arg("-m").arg("fast").arg("-w").arg("-t").arg("10").arg("stop");
        apply_run_as(&mut cmd, self.config.run_as);
        let stopped = cmd.output().map(|o| o.status.success()).unwrap_or(false);
        if stopped {
            return;
        }

        // `pg_ctl` can only stop a cluster whose data directory it can
        // still read — it finds the postmaster *through* `postmaster.pid`
        // in that folder. So if the folder has gone (a deleted temp dir, an
        // unmounted volume, a user tidying up while the app is running),
        // the polite path fails and the postmaster runs on forever holding
        // its port, which is the exact leak this guard exists to prevent.
        //
        // We know the pid ourselves. Signal it directly rather than giving
        // up: TERM (Postgres treats SIGTERM as "smart shutdown"), a bounded
        // grace period, then KILL. Same escalation `odoo.rs` uses.
        let Some(pid) = pid else { return };
        send_signal(pid, "-TERM");
        for _ in 0..40 {
            if !process_is_alive(pid) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        send_signal(pid, "-KILL");
    }
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: &str) {
    let _ = Command::new("kill").arg(signal).arg(pid.to_string()).status();
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill").arg("-0").arg(pid.to_string()).status().map(|s| s.success()).unwrap_or(false)
}

/// Windows has no signals; `taskkill` is the platform equivalent. Same
/// "written from the documented behaviour, never run on Windows" caveat as
/// `odoo.rs`'s matching pair.
#[cfg(not(unix))]
fn send_signal(pid: u32, signal: &str) {
    let mut cmd = Command::new("taskkill");
    if signal == "-KILL" {
        cmd.arg("/F");
    }
    let _ = cmd.arg("/PID").arg(pid.to_string()).status();
}

#[cfg(not(unix))]
fn process_is_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .arg("/FI")
        .arg(format!("PID eq {pid}"))
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

impl PgSupervisor {
    pub fn new(config: PgConfig) -> Self {
        Self::with_listener(config, |_| {})
    }

    /// Like `new`, but `listener` is called (off the lock) every time this
    /// supervisor's state actually changes — including transitions the
    /// background health watcher makes on its own, like `Crashed`. This is
    /// how a caller (e.g. `Core`, once Postgres instances are wired in) finds
    /// out about a crash without polling `state()` itself.
    pub fn with_listener(config: PgConfig, listener: impl Fn(PgState) + Send + Sync + 'static) -> Self {
        let config = Arc::new(config);
        let inner = Arc::new(Mutex::new(Inner { state: PgState::Stopped, generation: 0 }));
        Self {
            _stop_on_drop: Arc::new(StopOnDrop { config: config.clone(), inner: inner.clone() }),
            config,
            inner,
            on_state_change: Arc::new(listener),
        }
    }

    /// A handle sharing this supervisor's state and config but **not** its
    /// stop-on-drop guard — for the background watcher, which must not keep
    /// the cluster alive merely by watching it.
    fn clone_without_guard(&self) -> WatcherHandle {
        WatcherHandle {
            config: self.config.clone(),
            inner: self.inner.clone(),
            on_state_change: self.on_state_change.clone(),
        }
    }

    pub fn state(&self) -> PgState {
        self.inner.lock().expect("pg supervisor mutex poisoned").state
    }

    /// Sets state and fires the listener, but only if `generation` still
    /// matches the current one — the same staleness guard `start()`/`stop()`
    /// use inline for their own transitions, factored out so every
    /// post-hoc transition (health watcher, delayed command results) goes
    /// through one path that never forgets to fire the listener.
    fn set_state_if_current(&self, generation: u64, new_state: PgState) {
        let applied = {
            let mut inner = self.inner.lock().expect("pg supervisor mutex poisoned");
            if inner.generation == generation {
                inner.state = new_state;
                true
            } else {
                false
            }
        };
        if applied {
            (self.on_state_change)(new_state);
        }
    }

    /// Whether a postmaster is already serving this data directory.
    ///
    /// `pg_ctl status` is the portable answer — it exits 0 when the server
    /// is running, 3 when it isn't, and 4 when the directory isn't a
    /// cluster at all. Reading `postmaster.pid` and checking liveness by
    /// hand would mean platform-specific process probing, which this
    /// module deliberately avoids everywhere else too.
    async fn cluster_is_already_running(&self) -> bool {
        if !self.config.data_dir.join("postmaster.pid").is_file() {
            return false;
        }
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(config.bin_dir.join("pg_ctl"));
            cmd.arg("-D").arg(&config.data_dir).arg("status");
            apply_run_as(&mut cmd, config.run_as);
            cmd.output().map(|o| o.status.success()).unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    }

    /// Idempotent: returns `true` if this call actually ran `initdb`,
    /// `false` if the data directory already looked initialized.
    pub async fn ensure_initdb(&self) -> Result<bool, PgError> {
        let data_dir = self.config.data_dir.clone();
        if data_dir.join("PG_VERSION").is_file() {
            return Ok(false);
        }
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&data_dir)
                .map_err(|e| PgError::Spawn { command: "mkdir data_dir".into(), source: e })?;
            let mut cmd = Command::new(config.bin_dir.join("initdb"));
            cmd.arg("-D").arg(&data_dir).arg("-U").arg("postgres").arg("--auth=trust").arg("--no-instructions");
            // **Odoo cannot use a non-UTF8 cluster, and this is not
            // optional.** `initdb` with no `--encoding` inherits the
            // machine's locale, and on any machine where that locale isn't
            // UTF-8 it silently produces a `SQL_ASCII` cluster. Odoo then
            // fails to initialize *any* database on it — its very first
            // insert into `ir_module_module` carries a `®` (the
            // `delivery_mondialrelay` module's summary), and SQL_ASCII
            // can't represent it:
            //
            //     psycopg2.errors.UntranslatableCharacter:
            //     unsupported Unicode escape sequence
            //
            // The failure is environment-dependent, which is the worst
            // kind: it works on a typical Mac (LANG=en_US.UTF-8) and fails
            // on a machine with an unset or C locale, so it would ship
            // looking fine and break on somebody else's laptop. Forcing
            // both the encoding and a UTF-8 collation here makes the
            // cluster identical everywhere, independent of who's logged in
            // and what their shell exports.
            cmd.arg("--encoding=UTF8").arg("--locale=C.UTF-8");
            apply_run_as(&mut cmd, config.run_as);
            let output = run(cmd, "initdb")?;
            check_success(&output, "initdb")
        })
        .await
        .expect("initdb blocking task panicked")?;
        Ok(true)
    }

    /// Starts Postgres via `pg_ctl start -w` (which does its own readiness
    /// wait internally), then hands off to a background task that polls
    /// `pg_isready` every `health_check_interval` to detect a later crash —
    /// the actual "recovery from a mid-session crash" requirement from task
    /// 2.1/0.7, done portably (no raw signals, no platform-specific PID
    /// liveness checks) since `pg_ctl`/`pg_isready` exist on every target OS.
    pub async fn start(&self) -> Result<(), PgError> {
        let generation = {
            let mut inner = self.inner.lock().expect("pg supervisor mutex poisoned");
            if matches!(inner.state, PgState::Running { .. } | PgState::Starting) {
                return Err(PgError::AlreadyRunning);
            }
            inner.state = PgState::Starting;
            inner.generation += 1;
            inner.generation
        };
        (self.on_state_change)(PgState::Starting);

        // A postmaster left over from a previous run of this app — killed
        // rather than quit, so no `Drop` ever ran — is still *this*
        // cluster, serving this data directory on this port. Starting a
        // second one would fail with a cryptic "address already in use",
        // and telling the user to go hunt a process down would be exactly
        // the black box this project is trying not to have. Adopt it
        // instead: it is already the thing we were about to create.
        if self.cluster_is_already_running().await {
            let pid = self.read_pid().await.unwrap_or(0);
            self.set_state_if_current(generation, PgState::Running { pid });
            self.spawn_health_watcher(generation);
            return Ok(());
        }

        self.ensure_initdb().await?;

        let config = self.config.clone();
        let result = tokio::task::spawn_blocking(move || {
            let log_path = config.data_dir.join("server.log");
            let mut cmd = Command::new(config.bin_dir.join("pg_ctl"));
            cmd.arg("-D")
                .arg(&config.data_dir)
                .arg("-o")
                .arg(format!("-p {} -k {} -c listen_addresses=127.0.0.1", config.port, config.data_dir.display()))
                .arg("-l")
                .arg(&log_path)
                .arg("-w")
                .arg("-t")
                .arg(config.startup_timeout.as_secs().to_string())
                .arg("start");
            apply_run_as(&mut cmd, config.run_as);
            let output = run(cmd, "pg_ctl start")?;
            if output.status.success() {
                Ok(())
            } else {
                let log_tail = read_log_tail(&log_path, 40);
                Err(PgError::StartupTimeout { timeout_secs: config.startup_timeout.as_secs(), log_tail })
            }
        })
        .await
        .expect("pg_ctl start blocking task panicked");

        if let Err(err) = result {
            self.set_state_if_current(generation, PgState::Stopped);
            return Err(err);
        }

        let pid = self.read_pid().await.unwrap_or(0);
        self.set_state_if_current(generation, PgState::Running { pid });

        self.spawn_health_watcher(generation);
        Ok(())
    }

    /// Stops via `pg_ctl stop -m fast -w` — "Fast Shutdown": rolls back
    /// in-progress transactions and disconnects clients immediately rather
    /// than waiting for them, which is the right default for a dev tool with
    /// no long-lived client sessions to protect (see `pg_ctl(1)`).
    pub async fn stop(&self) -> Result<(), PgError> {
        let generation = {
            let mut inner = self.inner.lock().expect("pg supervisor mutex poisoned");
            match inner.state {
                PgState::Stopped => return Ok(()),
                // Nothing to actually stop — the process is already gone,
                // pg_ctl would just fail with "server is not running". Bump
                // the generation anyway so a lingering health watcher from
                // that run can never act again.
                PgState::Crashed { .. } => {
                    inner.state = PgState::Stopped;
                    inner.generation += 1;
                    drop(inner);
                    (self.on_state_change)(PgState::Stopped);
                    return Ok(());
                }
                PgState::Running { .. } => {
                    inner.state = PgState::Stopping;
                    inner.generation += 1;
                    inner.generation
                }
                PgState::Starting | PgState::Stopping => {
                    inner.generation += 1;
                    inner.generation
                }
            }
        };
        (self.on_state_change)(PgState::Stopping);

        let config = self.config.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(config.bin_dir.join("pg_ctl"));
            cmd.arg("-D")
                .arg(&config.data_dir)
                .arg("-m")
                .arg("fast")
                .arg("-w")
                .arg("-t")
                .arg(config.shutdown_timeout.as_secs().to_string())
                .arg("stop");
            apply_run_as(&mut cmd, config.run_as);
            let output = run(cmd, "pg_ctl stop")?;
            check_success(&output, "pg_ctl stop")
        })
        .await
        .expect("pg_ctl stop blocking task panicked");

        self.set_state_if_current(generation, PgState::Stopped);
        result
    }

    /// One-shot liveness check via `pg_isready` — used both to gate
    /// `ensure_initdb`-adjacent logic in tests and by the background watcher.
    pub async fn is_ready(&self) -> bool {
        is_ready_at(&self.config).await
    }

    async fn read_pid(&self) -> Option<u32> {
        let path = self.config.data_dir.join("postmaster.pid");
        // A tiny, already-flushed-by-the-time-we-get-here file (pg_ctl -w
        // only returns after Postgres is accepting connections, well after
        // it writes this) — reading it synchronously isn't worth a tokio::fs
        // dependency for one call site.
        let contents = std::fs::read_to_string(&path).ok()?;
        contents.lines().next()?.trim().parse().ok()
    }

    /// Polls health every `health_check_interval` while this generation is
    /// still the current one and state is `Running`; the first time a health
    /// check fails, transitions to `Crashed` and stops polling. Guarding on
    /// `generation` (rather than just checking `state == Running`) is what
    /// stops a watcher from a previous `start()` clobbering the result of a
    /// `stop()` immediately followed by a fresh `start()`.
    fn spawn_health_watcher(&self, generation: u64) {
        // A **weak** handle, deliberately. Cloning `self` into this
        // long-lived task would keep the supervisor alive forever, which
        // means the last real handle could never be "last" — and the
        // stop-on-drop guard above would be dead code, in the app as well
        // as in tests. Holding it weakly lets the supervisor be dropped
        // (and its cluster stopped), and this loop simply ends the next
        // time it looks.
        let weak = Arc::downgrade(&self._stop_on_drop);
        let this = self.clone_without_guard();
        tokio::spawn(async move {
            loop {
                sleep(this.config.health_check_interval).await;
                if weak.upgrade().is_none() {
                    return; // the supervisor is gone; so is anything to watch
                }
                {
                    let inner = this.inner.lock().expect("pg supervisor mutex poisoned");
                    if inner.generation != generation || !matches!(inner.state, PgState::Running { .. }) {
                        return; // superseded by a stop()/start() — nothing to do
                    }
                }
                if this.is_ready().await {
                    continue;
                }
                // Confirm rather than trust a single flaky check.
                sleep(Duration::from_millis(300)).await;
                if this.is_ready().await {
                    continue;
                }
                let should_apply = {
                    let inner = this.inner.lock().expect("pg supervisor mutex poisoned");
                    inner.generation == generation && matches!(inner.state, PgState::Running { .. })
                };
                if should_apply {
                    this.set_state_if_current(generation, PgState::Crashed { exit_code: None });
                }
                return;
            }
        });
    }
}

/// `pg_isready` against one cluster. A free function so both the
/// supervisor and its watcher can use it without either owning the other.
async fn is_ready_at(config: &Arc<PgConfig>) -> bool {
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(config.bin_dir.join("pg_isready"));
        cmd.arg("-h").arg(&config.data_dir).arg("-p").arg(config.port.to_string());
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        cmd.status().map(|s| s.success()).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

#[cfg(unix)]
fn apply_run_as(cmd: &mut Command, run_as: Option<(u32, u32)>) {
    if let Some((uid, gid)) = run_as {
        use std::os::unix::process::CommandExt;
        cmd.uid(uid);
        cmd.gid(gid);
    }
}

#[cfg(not(unix))]
fn apply_run_as(_cmd: &mut Command, run_as: Option<(u32, u32)>) {
    if run_as.is_some() {
        tracing::warn!("PgConfig::run_as is set but this platform doesn't support privilege dropping — ignoring");
    }
}

fn run(mut cmd: Command, name: &str) -> Result<Output, PgError> {
    cmd.output().map_err(|e| PgError::Spawn { command: name.to_string(), source: e })
}

fn check_success(output: &Output, name: &str) -> Result<(), PgError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(PgError::CommandFailed {
            command: name.to_string(),
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

fn read_log_tail(path: &Path, lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let all: Vec<&str> = contents.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(_) => "(no log file)".to_string(),
    }
}

/// Dev/test convenience: find a system-installed Postgres bin directory
/// (Debian/Ubuntu ships it at `/usr/lib/postgresql/<version>/bin`, not on
/// `PATH`). **Not used in production** — the shipped app always gets
/// `bin_dir` from its own bundled runtime per
/// `odoo-orchestrator-runtime-architecture.md`.
pub fn discover_system_bin_dir() -> Option<PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join("pg_ctl").is_file() {
                return Some(dir);
            }
        }
    }
    let debian_root = Path::new("/usr/lib/postgresql");
    let mut versions: Vec<_> = std::fs::read_dir(debian_root).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    versions.sort();
    versions.reverse(); // prefer the newest installed major version
    versions.into_iter().map(|v| v.join("bin")).find(|bin| bin.join("pg_ctl").is_file())
}

/// Dev/sandbox convenience — **not** a production code path (see the
/// `run_as` doc comment above). Postgres refuses outright to run as root; a
/// real desktop install never hits that (the app runs as the logged-in
/// user), but a root-only CI/sandbox build does. If the current process is
/// root *and* this environment happens to provide an unprivileged `claude`
/// account (the one this project's own sandbox ships), returns its
/// `(uid, gid)` so a caller can chown a data dir to it and set
/// `PgConfig::run_as` accordingly. Returns `None` everywhere else —
/// including on every real desktop install, where the root check alone
/// already short-circuits it.
pub fn sandbox_root_workaround_user() -> Option<(u32, u32)> {
    if !is_running_as_root() {
        return None;
    }
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 4 && fields[0] == "claude" {
            return Some((fields[2].parse().ok()?, fields[3].parse().ok()?));
        }
    }
    None
}

// Avoid a `libc` crate dependency for one syscall: read it from /proc
// instead, which is exactly as reliable on the Linux CI/sandbox this needs
// to run on (and simply reports `false` — i.e. "not root" — anywhere else,
// including every real desktop OS, since there's no /proc there either).
fn is_running_as_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).map(|l| l.to_string()))
        .and_then(|line| line.split_whitespace().nth(1).map(|s| s.parse::<u32>().unwrap_or(u32::MAX)))
        .map(|uid| uid == 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(data_dir: &Path, port: u16) -> PgConfig {
        let bin_dir = discover_system_bin_dir().expect(
            "these tests need a system Postgres installed (Debian/Ubuntu: apt install postgresql) — \
             they exercise the real supervisor against a real cluster, not a mock",
        );
        let mut config = PgConfig::new(bin_dir, data_dir, port);
        // This test process runs as root in CI/sandbox environments (Postgres
        // refuses that outright); drop to the sandbox's unprivileged account
        // and grant it ownership of the data dir first. See
        // `sandbox_root_workaround_user`'s doc comment — a no-op on any real
        // desktop install, where the process is never root to begin with.
        if let Some((uid, gid)) = sandbox_root_workaround_user() {
            std::os::unix::fs::chown(data_dir, Some(uid), Some(gid)).expect("chown temp data dir for postgres test");
            config.run_as = Some((uid, gid));
        }
        config
    }

    #[tokio::test]
    async fn full_lifecycle_start_ready_stop() {
        let dir = TempDir::new().unwrap();
        let sup = PgSupervisor::new(test_config(dir.path(), 55401));

        assert_eq!(sup.state(), PgState::Stopped);
        sup.start().await.expect("postgres should start cleanly");
        assert!(matches!(sup.state(), PgState::Running { .. }));
        assert!(sup.is_ready().await, "pg_isready should report ready once Running");

        sup.stop().await.expect("postgres should stop cleanly");
        assert_eq!(sup.state(), PgState::Stopped);
        assert!(!sup.is_ready().await, "shouldn't be reachable after a clean stop");
    }

    #[tokio::test]
    async fn starting_twice_is_rejected_not_a_second_instance() {
        let dir = TempDir::new().unwrap();
        let sup = PgSupervisor::new(test_config(dir.path(), 55402));

        sup.start().await.unwrap();
        let err = sup.start().await.unwrap_err();
        assert!(matches!(err, PgError::AlreadyRunning));

        sup.stop().await.unwrap();
    }

    #[tokio::test]
    async fn stop_without_start_is_a_harmless_no_op() {
        let dir = TempDir::new().unwrap();
        let sup = PgSupervisor::new(test_config(dir.path(), 55403));
        sup.stop().await.expect("stopping an already-stopped supervisor should be Ok, not an error");
    }

    #[tokio::test]
    async fn state_listener_observes_every_transition_including_a_crash() {
        let dir = TempDir::new().unwrap();
        let seen: Arc<Mutex<Vec<PgState>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_for_listener = seen.clone();
        let mut config = test_config(dir.path(), 55407);
        config.health_check_interval = Duration::from_millis(200);
        let sup = PgSupervisor::with_listener(config, move |state| {
            seen_for_listener.lock().unwrap().push(state);
        });

        sup.start().await.unwrap();
        let pid = match sup.state() {
            PgState::Running { pid } => pid,
            other => panic!("expected Running, got {other:?}"),
        };
        std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status().unwrap();

        let mut crashed = false;
        for _ in 0..20 {
            sleep(Duration::from_millis(200)).await;
            if matches!(sup.state(), PgState::Crashed { .. }) {
                crashed = true;
                break;
            }
        }
        assert!(crashed, "should have crashed by now");

        let observed = seen.lock().unwrap().clone();
        assert!(matches!(observed[0], PgState::Starting), "first event should be Starting, got {observed:?}");
        assert!(observed.iter().any(|s| matches!(s, PgState::Running { .. })), "should have observed Running: {observed:?}");
        assert!(matches!(observed.last(), Some(PgState::Crashed { .. })), "last event should be the crash: {observed:?}");
    }

    /// Dropping the last handle must not leave a live PostgreSQL behind.
    ///
    /// This is the bug that made the test suite itself unreliable: a test
    /// that panicked before its explicit `stop()` leaked a postmaster on
    /// a fixed port, and every later run collided with it. The same thing
    /// happens to a user whose app quits without a clean shutdown.
    #[tokio::test]
    async fn dropping_the_last_handle_stops_the_cluster() {
        let dir = TempDir::new().unwrap();
        let config = test_config(dir.path(), 55410);
        {
            let sup = PgSupervisor::new(config.clone());
            sup.start().await.unwrap();
            assert!(matches!(sup.state(), PgState::Running { .. }));
            // A clone going out of scope must NOT stop it — only the last
            // handle should.
            {
                let clone = sup.clone();
                assert!(matches!(clone.state(), PgState::Running { .. }));
            }
            assert!(is_running(&config).await, "dropping one clone of a shared handle must leave the cluster alone");
        }
        // Every handle is gone now.
        assert!(!is_running(&config).await, "the cluster must be stopped once the last handle is dropped");
    }

    /// The case a `Drop` can never cover: the app was killed, so nothing
    /// ran, and a postmaster is still serving this data directory. Starting
    /// again must adopt it rather than fail with "address already in use".
    #[tokio::test]
    async fn starting_over_a_cluster_left_running_by_a_killed_app_adopts_it() {
        let dir = TempDir::new().unwrap();
        let config = test_config(dir.path(), 55411);

        // Start one and deliberately forget it the way a kill -9 would:
        // mark it stopped so its own guard won't reap it, then drop it.
        let abandoned = PgSupervisor::new(config.clone());
        abandoned.start().await.unwrap();
        let original_pid = match abandoned.state() {
            PgState::Running { pid } => pid,
            other => panic!("expected Running, got {other:?}"),
        };
        abandoned.inner.lock().unwrap().state = PgState::Stopped;
        drop(abandoned);
        assert!(is_running(&config).await, "the abandoned cluster should still be up — that's the situation under test");

        let fresh = PgSupervisor::new(config.clone());
        fresh.start().await.expect("starting over a live cluster must adopt it, not collide with it");
        match fresh.state() {
            PgState::Running { pid } => assert_eq!(pid, original_pid, "it should adopt the existing process, not start a second one"),
            other => panic!("expected Running, got {other:?}"),
        }

        fresh.stop().await.unwrap();
    }

    /// The leak this guard could not previously prevent.
    ///
    /// `pg_ctl stop` finds the postmaster *through* `postmaster.pid` inside
    /// the data directory. Take the directory away — a deleted temp dir, an
    /// unmounted volume, someone tidying up while the app runs — and the
    /// polite stop fails and the cluster runs on forever holding its port.
    /// Found for real: a panicking test dropped its `TempDir` before its
    /// `Core`, and left a postmaster behind that blocked every later run.
    #[tokio::test]
    async fn a_cluster_whose_data_directory_vanished_is_still_stopped() {
        if discover_system_bin_dir().is_none() {
            eprintln!("skipping: no system postgres installed in this environment");
            return;
        }
        let dir = TempDir::new().unwrap();
        let config = test_config(dir.path(), 55412);

        let supervisor = PgSupervisor::new(config.clone());
        supervisor.start().await.unwrap();
        let pid = match supervisor.state() {
            PgState::Running { pid } => pid,
            other => panic!("expected Running, got {other:?}"),
        };

        // Pull the directory out from under it, then drop the supervisor.
        std::fs::remove_dir_all(dir.path()).unwrap();
        drop(supervisor);

        // Asserted against the OS, not against anything this app believes.
        assert!(!process_is_alive(pid), "the postmaster survived a drop it could not pg_ctl its way out of");
    }

    /// `pg_ctl status` against the real cluster — the tests' own check,
    /// deliberately not going through `PgSupervisor::state()`, which only
    /// knows what this process believes.
    async fn is_running(config: &PgConfig) -> bool {
        let config = config.clone();
        tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(config.bin_dir.join("pg_ctl"));
            cmd.arg("-D").arg(&config.data_dir).arg("status");
            apply_run_as(&mut cmd, config.run_as);
            cmd.output().map(|o| o.status.success()).unwrap_or(false)
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn ensure_initdb_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let config = test_config(dir.path(), 55404);
        let sup = PgSupervisor::new(config);

        assert!(sup.ensure_initdb().await.unwrap(), "first call should actually run initdb");
        assert!(!sup.ensure_initdb().await.unwrap(), "second call should recognize the existing cluster and skip");
    }

    #[tokio::test]
    async fn detects_a_real_crash_via_health_watcher() {
        let dir = TempDir::new().unwrap();
        let mut config = test_config(dir.path(), 55405);
        config.health_check_interval = Duration::from_millis(200);
        let sup = PgSupervisor::new(config.clone());

        sup.start().await.unwrap();
        assert!(matches!(sup.state(), PgState::Running { .. }));

        // Simulate a real crash out-of-band — exactly what the health
        // watcher (not a graceful stop()) is supposed to catch — by killing
        // it the same rough way an OOM killer or a user's `kill -9` would.
        let pid = match sup.state() {
            PgState::Running { pid } => pid,
            other => panic!("expected Running, got {other:?}"),
        };
        let killed = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
        assert!(killed.map(|s| s.success()).unwrap_or(false), "test setup: failed to kill the postgres pid directly");

        // Give the health watcher a few polling cycles to notice.
        let mut noticed = false;
        for _ in 0..20 {
            sleep(Duration::from_millis(200)).await;
            if matches!(sup.state(), PgState::Crashed { .. }) {
                noticed = true;
                break;
            }
        }
        assert!(noticed, "health watcher should have transitioned to Crashed after the process was killed out-of-band");
    }

    #[tokio::test]
    async fn generation_guard_prevents_a_stale_watcher_from_clobbering_a_fresh_start() {
        let dir = TempDir::new().unwrap();
        let sup = PgSupervisor::new(test_config(dir.path(), 55406));

        sup.start().await.unwrap();
        sup.stop().await.unwrap();
        sup.start().await.unwrap();
        assert!(matches!(sup.state(), PgState::Running { .. }));

        // The first start()'s watcher is (or was) still polling on its own
        // schedule; make sure enough time passes for it to have fired at
        // least once, then confirm the second start()'s state survived.
        sleep(Duration::from_millis(400)).await;
        assert!(matches!(sup.state(), PgState::Running { .. }), "a superseded watcher must not overwrite current state");

        sup.stop().await.unwrap();
    }
}
