//! The person's terminals: a shell in a PTY on the machine the hub runs on, for the GUI to show.
//! They run as the system user, outside any agent's sandbox, and what they print never reaches a
//! model: nothing but `api_terminal` reads it.
//!
//! A terminal outlives its window. Closing one or losing the connection only detaches; only an
//! explicit close ends it, or the hub stopping, or being left alone and idle for
//! [`Terminals::idle`] (never while something runs in it). A window coming back gets the screen as
//! it is now, from a `vt100` copy of it: a tail of raw bytes could start inside an escape sequence
//! and would replay modes long since left. The newest connection is the one that types; others
//! watch until they take over.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use crossagents_api_types::{CreateTerminalRequest, TerminalFrame, TerminalInfo};
use crossagents_core::CoreError;
use crossagents_core::process::Job;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// How many running terminals the hub keeps at once. Finished ones do not count.
pub const MAX_TERMINALS: usize = 16;
pub const MAX_COLS: u16 = 500;
pub const MAX_ROWS: u16 = 200;
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
const SCROLLBACK_ROWS: usize = 1000;
/// Output chunks a slow connection may fall behind by before it gets the screen anew.
const OUTPUT_BACKLOG: usize = 1024;
/// How long a shell's exit waits for the last of its output.
const DRAIN: Duration = Duration::from_secs(2);
pub const DEFAULT_IDLE: Duration = Duration::from_secs(2 * 60 * 60);

/// Environment that is the hub's own and has no business in a person's shell.
const HUB_ENV_PREFIX: &str = "CROSSAGENTS_";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn not_found(id: &str) -> CoreError {
    CoreError::NotFound {
        kind: "terminal",
        id: id.into(),
    }
}

/// What a connection reads first, and how it stays in step: anything `live` delivers is not in
/// `snapshot`.
pub struct Attached {
    pub terminal: Arc<Terminal>,
    pub snapshot: TerminalFrame,
    pub live: broadcast::Receiver<TerminalFrame>,
    /// Set if the shell has already finished: the connection ends after the snapshot.
    pub exit: Option<TerminalFrame>,
    pub guard: AttachGuard,
}

/// Counts a connection for as long as it lasts.
pub struct AttachGuard(Arc<Terminal>);

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.0.attached.fetch_sub(1, Ordering::SeqCst);
        *lock(&self.0.touched) = Instant::now();
    }
}

struct Screen {
    parser: vt100::Parser,
    /// `Some` once the shell has finished; its code if it had one.
    exit: Option<Option<i32>>,
}

struct Input {
    writer: Option<Box<dyn Write + Send>>,
    /// The connection that types.
    owner: Option<String>,
    /// The last batch it sent, so a batch sent twice is typed once.
    last_seq: u32,
}

pub struct Terminal {
    id: String,
    project: Option<String>,
    cwd: PathBuf,
    shell: String,
    created_at: DateTime<Utc>,
    #[cfg_attr(windows, allow(dead_code))]
    pid: Option<u32>,
    screen: Mutex<Screen>,
    tx: broadcast::Sender<TerminalFrame>,
    input: Mutex<Input>,
    /// Dropped when the shell finishes, which closes the PTY.
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// The shell's process tree where the system has jobs: it ends with the hub, however that goes.
    job: Option<Job>,
    attached: AtomicUsize,
    /// The last output, input or detach.
    touched: Mutex<Instant>,
}

impl Terminal {
    fn info(&self) -> TerminalInfo {
        let screen = lock(&self.screen);
        let (rows, cols) = screen.parser.screen().size();
        TerminalInfo {
            id: self.id.clone(),
            project: self.project.clone(),
            cwd: self.cwd.display().to_string(),
            shell: self.shell.clone(),
            cols: cols.get(),
            rows: rows.get(),
            created_at: self.created_at,
            exited: screen.exit.is_some(),
            exit_code: screen.exit.flatten(),
            connections: u32::try_from(self.attached.load(Ordering::SeqCst)).unwrap_or(u32::MAX),
            node: None,
        }
    }

    fn finished(&self) -> bool {
        lock(&self.screen).exit.is_some()
    }

    fn kill(&self) {
        if let Err(err) = lock(&self.killer).kill() {
            tracing::debug!(error = %err, terminal = self.id, "cannot kill the shell; it may have finished");
        }
        if let Some(job) = &self.job
            && let Err(err) = job.terminate()
        {
            tracing::debug!(error = %err, terminal = self.id, "cannot end the shell's job");
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // The PTY made the shell a group leader: this takes what it started along.
            crossagents_core::process::kill_group(pid);
        }
    }

    /// Whether something other than the shell itself is running. When it cannot be told, yes:
    /// wrongly keeping a terminal costs little, wrongly ending a build does not.
    fn busy(&self) -> bool {
        #[cfg(windows)]
        {
            self.job
                .as_ref()
                .is_none_or(|job| job.active_processes().map_or(true, |n| n > 1))
        }
        #[cfg(unix)]
        {
            let master = lock(&self.master);
            match (
                master.as_ref().and_then(|m| m.process_group_leader()),
                self.pid,
            ) {
                (Some(leader), Some(pid)) => u32::try_from(leader).ok() != Some(pid),
                _ => true,
            }
        }
    }

    fn reclaimable(&self, idle: Duration) -> bool {
        if self.attached.load(Ordering::SeqCst) > 0 || lock(&self.touched).elapsed() < idle {
            return false;
        }
        self.finished() || !self.busy()
    }

    /// Reads the PTY into the screen and out to every connection, until it closes. `replies` gets
    /// what the screen itself answers.
    fn pump(&self, mut reader: Box<dyn Read + Send>, replies: &std::sync::mpsc::Sender<String>) {
        let mut buf = vec![0u8; 16 * 1024];
        let mut held = Vec::new();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut screen = lock(&self.screen);
            for piece in split_cursor_queries(&mut held, &buf[..n]) {
                match piece {
                    Piece::Output(bytes) => {
                        screen.parser.process(&bytes);
                        // No receivers is fine: nobody is watching.
                        let _ = self.tx.send(TerminalFrame::Output {
                            data: STANDARD.encode(&bytes),
                        });
                    }
                    Piece::CursorQuery => {
                        let (row, col) = screen.parser.screen().cursor_position();
                        let _ = replies.send(format!("\x1b[{};{}R", row + 1, col + 1));
                    }
                }
            }
            drop(screen);
            *lock(&self.touched) = Instant::now();
        }
    }

    /// Answers on its own thread: `pump` must keep reading whatever typing is stuck on, or a shell
    /// blocked printing would never get back to reading what is typed.
    fn answer(&self, replies: &std::sync::mpsc::Receiver<String>) {
        while let Ok(reply) = replies.recv() {
            let mut input = lock(&self.input);
            let Some(writer) = input.writer.as_mut() else {
                return;
            };
            if writer
                .write_all(reply.as_bytes())
                .and_then(|()| writer.flush())
                .is_err()
            {
                return;
            }
        }
    }

    /// The shell finished: closes the PTY, lets the output drain and tells everyone.
    fn finish(&self, code: Option<i32>, drained: &std::sync::mpsc::Receiver<()>) {
        // Whatever it left behind goes too; a window keeps what was printed.
        if let Some(job) = &self.job {
            let _ = job.terminate();
        }
        drop(lock(&self.master).take());
        lock(&self.input).writer = None;
        let _ = drained.recv_timeout(DRAIN);
        let mut screen = lock(&self.screen);
        screen.exit = Some(code);
        let _ = self.tx.send(TerminalFrame::Exit { code });
    }
}

/// The terminals of this hub.
pub struct Terminals {
    map: Mutex<HashMap<String, Arc<Terminal>>>,
    shell: Option<Vec<String>>,
    idle: Duration,
}

impl Terminals {
    /// `shell`: the command to start (`[hub] terminal_shell`), else the system's usual one.
    pub fn new(shell: Option<Vec<String>>, idle: Duration) -> Arc<Self> {
        Arc::new(Self {
            map: Mutex::new(HashMap::new()),
            shell,
            idle,
        })
    }

    /// Reclaims what was left alone, until `shutdown`.
    pub fn start_reclaiming(self: &Arc<Self>, shutdown: CancellationToken) {
        let terminals = self.clone();
        let every = (self.idle / 4).clamp(Duration::from_millis(100), Duration::from_secs(60));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(every) => {}
                }
                let all = terminals.clone();
                let swept = tokio::task::spawn_blocking(move || all.reclaim()).await;
                if let Err(err) = swept {
                    tracing::warn!(error = %err, "terminal reclaim failed");
                }
            }
        });
    }

    fn reclaim(&self) {
        let gone: Vec<_> = {
            let mut map = lock(&self.map);
            let ids: Vec<_> = map
                .values()
                .filter(|t| t.reclaimable(self.idle))
                .map(|t| t.id.clone())
                .collect();
            ids.iter().filter_map(|id| map.remove(id)).collect()
        };
        for terminal in gone {
            tracing::info!(terminal = terminal.id, "reclaiming a terminal left alone");
            terminal.kill();
        }
    }

    pub fn list(&self) -> Vec<TerminalInfo> {
        let mut all: Vec<_> = lock(&self.map).values().map(|t| t.info()).collect();
        all.sort_by_key(|t| t.created_at);
        all
    }

    fn get(&self, id: &str) -> Result<Arc<Terminal>, CoreError> {
        lock(&self.map)
            .get(id)
            .cloned()
            .ok_or_else(|| not_found(id))
    }

    /// Starts a shell. Blocking: this may search the `PATH` and start a process.
    pub fn create(&self, req: &CreateTerminalRequest) -> Result<TerminalInfo, CoreError> {
        // Reserved for cross-node terminals (docs/MULTI-NODE.md): this hub is the only node yet.
        if req.node.is_some() {
            return Err(CoreError::Unsupported(
                "this hub has no other nodes yet".into(),
            ));
        }
        check_size(req.cols, req.rows)?;
        let cwd = Path::new(&req.cwd);
        if !cwd.is_dir() {
            return Err(CoreError::Unsupported(format!(
                "not a directory: {}",
                req.cwd
            )));
        }
        let running = lock(&self.map).values().filter(|t| !t.finished()).count();
        if running >= MAX_TERMINALS {
            return Err(CoreError::Conflict(format!(
                "{MAX_TERMINALS} terminals are already running; close one first"
            )));
        }
        let command = self.shell.clone().unwrap_or_else(default_shell);
        let terminal = spawn(&command, cwd, req).map_err(|err| {
            CoreError::Agent(format!("cannot start {}: {err:#}", command.join(" ")))
        })?;
        let info = terminal.info();
        lock(&self.map).insert(terminal.id.clone(), terminal);
        Ok(info)
    }

    /// A new connection reads this terminal. It types from now on, unless it already did.
    /// Blocking: a batch being typed holds the input.
    pub fn attach(&self, id: &str, connection: &str) -> Result<Attached, CoreError> {
        let terminal = self.get(id)?;
        terminal.attached.fetch_add(1, Ordering::SeqCst);
        let guard = AttachGuard(terminal.clone());
        self.take_input(&terminal, connection);
        // Under the screen's lock nothing is parsed: what `live` delivers follows the snapshot.
        let mut screen = lock(&terminal.screen);
        let snapshot = snapshot_of(&mut screen.parser);
        let live = terminal.tx.subscribe();
        let exit = screen.exit.map(|code| TerminalFrame::Exit { code });
        drop(screen);
        Ok(Attached {
            terminal: terminal.clone(),
            snapshot,
            live,
            exit,
            guard,
        })
    }

    /// The screen anew, for a connection that fell behind.
    pub fn resnapshot(terminal: &Terminal) -> TerminalFrame {
        snapshot_of(&mut lock(&terminal.screen).parser)
    }

    fn take_input(&self, terminal: &Terminal, connection: &str) {
        let mut input = lock(&terminal.input);
        if input.owner.as_deref() == Some(connection) {
            return;
        }
        input.owner = Some(connection.into());
        input.last_seq = 0;
        let _ = terminal.tx.send(TerminalFrame::Writer {
            connection: connection.into(),
        });
    }

    /// "接管输入": `connection` types from now on. Blocking, like `attach`.
    pub fn takeover(&self, id: &str, connection: &str) -> Result<(), CoreError> {
        let terminal = self.get(id)?;
        self.take_input(&terminal, connection);
        Ok(())
    }

    /// Types a batch. A batch whose `seq` was already typed is not typed again; one from a
    /// connection that no longer types is refused, and stays untyped for good: the person saw it
    /// was not accepted, and replaying keys later could run something they no longer mean.
    /// Blocking: a shell not reading fills the PTY's buffer.
    pub fn input(
        &self,
        id: &str,
        connection: &str,
        seq: u32,
        data: &str,
    ) -> Result<u32, CoreError> {
        if data.len() > MAX_INPUT_BYTES {
            return Err(CoreError::Unsupported(format!(
                "input over {MAX_INPUT_BYTES} bytes"
            )));
        }
        let terminal = self.get(id)?;
        let mut input = lock(&terminal.input);
        if input.owner.as_deref() != Some(connection) {
            return Err(CoreError::Conflict(
                "another window is typing in this terminal".into(),
            ));
        }
        if seq <= input.last_seq {
            return Ok(input.last_seq);
        }
        let Some(writer) = input.writer.as_mut() else {
            return Err(CoreError::Conflict("the shell has finished".into()));
        };
        writer
            .write_all(data.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(|err| CoreError::Conflict(format!("cannot write to the shell: {err}")))?;
        input.last_seq = seq;
        drop(input);
        *lock(&terminal.touched) = Instant::now();
        Ok(seq)
    }

    /// Only the connection that types sets the size: its window is the one being used.
    pub fn resize(
        &self,
        id: &str,
        connection: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), CoreError> {
        check_size(cols, rows)?;
        let terminal = self.get(id)?;
        if lock(&terminal.input).owner.as_deref() != Some(connection) {
            return Err(CoreError::Conflict(
                "another window is typing in this terminal".into(),
            ));
        }
        if let Some(master) = lock(&terminal.master).as_ref() {
            master
                .resize(pty_size(cols, rows))
                .map_err(|err| CoreError::Conflict(format!("cannot resize: {err}")))?;
        }
        let (Some(rows), Some(cols)) = (NonZeroU16::new(rows), NonZeroU16::new(cols)) else {
            return Ok(());
        };
        lock(&terminal.screen).parser.set_size(rows, cols);
        Ok(())
    }

    /// Ends a terminal and forgets it: the × on its tab.
    pub fn close(&self, id: &str) -> Result<(), CoreError> {
        let terminal = lock(&self.map).remove(id).ok_or_else(|| not_found(id))?;
        terminal.kill();
        Ok(())
    }

    /// The hub is stopping: terminals do not outlive it.
    pub fn stop_all(&self) {
        let all: Vec<_> = lock(&self.map).drain().map(|(_, t)| t).collect();
        for terminal in all {
            terminal.kill();
        }
    }
}

fn check_size(cols: u16, rows: u16) -> Result<(), CoreError> {
    if cols == 0 || rows == 0 || cols > MAX_COLS || rows > MAX_ROWS {
        return Err(CoreError::Unsupported(format!(
            "terminal size {cols}x{rows} is outside 1x1 to {MAX_COLS}x{MAX_ROWS}"
        )));
    }
    Ok(())
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn spawn(
    command: &[String],
    cwd: &Path,
    req: &CreateTerminalRequest,
) -> anyhow::Result<Arc<Terminal>> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("the shell command is empty"))?;
    let pair = native_pty_system().openpty(pty_size(req.cols, req.rows))?;
    let mut builder = CommandBuilder::new(program);
    builder.args(args);
    builder.cwd(cwd);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with(HUB_ENV_PREFIX) {
            builder.env_remove(&name);
        }
    }
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(builder)?;
    // The shell holds the only slave that should stay open: end-of-file then means it went.
    drop(pair.slave);
    let pid = child.process_id();
    let job = pid.and_then(|pid| {
        Job::new()
            .and_then(|job| job.assign(pid).map(|()| job))
            .map_err(|err| tracing::debug!(error = %err, "the terminal runs without a job"))
            .ok()
    });
    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let (rows, cols) = (
        NonZeroU16::new(req.rows).ok_or_else(|| anyhow::anyhow!("no rows"))?,
        NonZeroU16::new(req.cols).ok_or_else(|| anyhow::anyhow!("no columns"))?,
    );
    let (tx, _) = broadcast::channel(OUTPUT_BACKLOG);
    let terminal = Arc::new(Terminal {
        id: uuid::Uuid::new_v4().to_string(),
        project: req.project.clone(),
        cwd: cwd.to_path_buf(),
        shell: program.clone(),
        created_at: Utc::now(),
        pid,
        screen: Mutex::new(Screen {
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_ROWS),
            exit: None,
        }),
        tx,
        input: Mutex::new(Input {
            writer: Some(writer),
            owner: None,
            last_seq: 0,
        }),
        master: Mutex::new(Some(pair.master)),
        killer: Mutex::new(child.clone_killer()),
        job,
        attached: AtomicUsize::new(0),
        touched: Mutex::new(Instant::now()),
    });

    let (drained_tx, drained) = std::sync::mpsc::channel();
    let (replies, to_answer) = std::sync::mpsc::channel();
    let pumping = terminal.clone();
    std::thread::Builder::new()
        .name("terminal-read".into())
        .spawn(move || {
            pumping.pump(reader, &replies);
            let _ = drained_tx.send(());
        })?;
    let answering = terminal.clone();
    std::thread::Builder::new()
        .name("terminal-answer".into())
        .spawn(move || answering.answer(&to_answer))?;
    let waiting = terminal.clone();
    std::thread::Builder::new()
        .name("terminal-wait".into())
        .spawn(move || {
            let code = child
                .wait()
                .ok()
                .map(|status| i32::try_from(status.exit_code()).unwrap_or(i32::MAX));
            waiting.finish(code, &drained);
        })?;
    Ok(terminal)
}

/// "Where is the cursor?" `ConPTY` asks once at the start and waits for the answer before the shell
/// runs at all, whether or not a window is connected, so the hub answers from its copy of the
/// screen. Windows do not get the question: they would answer as well, and the second answer would
/// land in the shell as typed text.
const CURSOR_QUERY: &[u8] = b"\x1b[6n";

#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Output(Vec<u8>),
    CursorQuery,
}

/// `chunk` with the cursor queries taken out. `held` keeps what may be the start of one until the
/// next chunk says: the PTY may split it.
fn split_cursor_queries(held: &mut Vec<u8>, chunk: &[u8]) -> Vec<Piece> {
    held.extend_from_slice(chunk);
    let mut pieces = Vec::new();
    let mut start = 0;
    while let Some(at) = held[start..]
        .windows(CURSOR_QUERY.len())
        .position(|w| w == CURSOR_QUERY)
    {
        if at > 0 {
            pieces.push(Piece::Output(held[start..start + at].to_vec()));
        }
        pieces.push(Piece::CursorQuery);
        start += at + CURSOR_QUERY.len();
    }
    let rest = &held[start..];
    let keep = (1..CURSOR_QUERY.len())
        .rev()
        .find(|&n| rest.ends_with(&CURSOR_QUERY[..n]))
        .unwrap_or(0);
    if rest.len() > keep {
        pieces.push(Piece::Output(rest[..rest.len() - keep].to_vec()));
    }
    held.drain(..held.len() - keep);
    pieces
}

/// Bytes that bring a cleared xterm to this screen: history first, pushed up out of the way, then
/// the screen and its modes. A full-screen program's alternate screen has no history of its own,
/// and the normal one's is not reachable while it is up, so that comes back without it.
fn snapshot_of(parser: &mut vt100::Parser) -> TerminalFrame {
    let screen = parser.screen_mut();
    let (rows, cols) = screen.size();
    let mut out = String::new();
    if screen.alternate_screen() {
        out.push_str("\x1b[?1049h");
    } else {
        screen.set_scrollback(usize::MAX);
        let mut left = screen.scrollback();
        while left > 0 {
            screen.set_scrollback(left);
            let page = left.min(usize::from(rows.get()));
            for (i, row) in screen.rows_formatted(0, cols.get()).take(page).enumerate() {
                out.push_str("\x1b[m");
                out.push_str(&row);
                let wrapped = u16::try_from(i).is_ok_and(|i| screen.row_wrapped(i));
                if !wrapped {
                    out.push_str("\r\n");
                }
            }
            left -= page;
        }
        screen.set_scrollback(0);
        out.push_str("\x1b[m");
        out.push_str(&"\r\n".repeat(usize::from(rows.get()) - 1));
    }
    out.push_str(&screen.state_formatted());
    TerminalFrame::Snapshot {
        data: STANDARD.encode(out.as_bytes()),
        cols: cols.get(),
        rows: rows.get(),
    }
}

#[cfg(windows)]
fn default_shell() -> Vec<String> {
    let found = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
    };
    let program = if found("pwsh.exe") {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    vec![program.into(), "-NoLogo".into()]
}

#[cfg(not(windows))]
fn default_shell() -> Vec<String> {
    vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_from(frame: &TerminalFrame) -> vt100::Parser {
        let TerminalFrame::Snapshot { data, cols, rows } = frame else {
            panic!("not a snapshot: {frame:?}");
        };
        let mut parser = vt100::Parser::new(
            NonZeroU16::new(*rows).unwrap(),
            NonZeroU16::new(*cols).unwrap(),
            SCROLLBACK_ROWS,
        );
        parser.process(&STANDARD.decode(data).unwrap());
        parser
    }

    fn parser() -> vt100::Parser {
        vt100::Parser::new(
            NonZeroU16::new(5).unwrap(),
            NonZeroU16::new(40).unwrap(),
            SCROLLBACK_ROWS,
        )
    }

    #[test]
    fn a_snapshot_restores_the_screen_and_its_history_in_order() {
        let mut original = parser();
        for n in 0..12 {
            original.process(format!("line-{n}\r\n").as_bytes());
        }
        original.process(b"\x1b[31mred\x1b[m prompt>");
        let restored = screen_from(&snapshot_of(&mut original));
        assert_eq!(
            restored.screen().contents(),
            original.screen().contents(),
            "the screen"
        );
        assert_eq!(
            restored.screen().cursor_position(),
            original.screen().cursor_position()
        );
        assert_eq!(
            restored.screen().cell(4, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1),
            "colours"
        );

        let mut restored = restored;
        restored.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(restored.screen().scrollback(), 8, "exactly the history");
        assert!(restored.screen().contents().starts_with("line-0\nline-1\n"));
        // Taking a snapshot leaves the original looking at its live screen.
        assert_eq!(original.screen().scrollback(), 0);
    }

    #[test]
    fn a_snapshot_joins_a_line_that_wrapped() {
        let mut original = parser();
        let long = "x".repeat(50);
        original.process(long.as_bytes());
        original.process(b"\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n");
        let mut restored = screen_from(&snapshot_of(&mut original));
        restored.screen_mut().set_scrollback(usize::MAX);
        let top = restored.screen().contents();
        assert!(
            top.starts_with(&format!("{}\n", "x".repeat(40))) || top.starts_with(&long),
            "{top}"
        );
        assert!(restored.screen().row_wrapped(0), "still one line: {top}");
    }

    #[test]
    fn a_snapshot_restores_a_full_screen_program() {
        let mut original = parser();
        original.process(b"before\r\n\x1b[?1049h\x1b[?1h\x1b[2;3Hvim here");
        let restored = screen_from(&snapshot_of(&mut original));
        assert!(restored.screen().alternate_screen());
        assert!(restored.screen().application_cursor());
        assert_eq!(restored.screen().contents(), original.screen().contents());
    }

    #[test]
    fn cursor_queries_are_taken_out_even_when_split() {
        let out = |text: &str| Piece::Output(text.as_bytes().to_vec());
        let mut held = Vec::new();
        assert_eq!(
            split_cursor_queries(&mut held, b"a\x1b[6nb\x1b[6n"),
            [out("a"), Piece::CursorQuery, out("b"), Piece::CursorQuery]
        );
        assert!(held.is_empty());
        assert_eq!(split_cursor_queries(&mut held, b"c\x1b["), [out("c")]);
        assert_eq!(held, b"\x1b[");
        assert_eq!(
            split_cursor_queries(&mut held, b"6nd"),
            [Piece::CursorQuery, out("d")]
        );
        // Not one after all: nothing is lost.
        assert_eq!(split_cursor_queries(&mut held, b"\x1b"), []);
        assert_eq!(split_cursor_queries(&mut held, b"[31m"), [out("\x1b[31m")]);
        assert!(held.is_empty());
    }

    #[test]
    fn sizes_outside_the_limits_are_refused() {
        assert!(check_size(80, 24).is_ok());
        assert!(check_size(0, 24).is_err());
        assert!(check_size(80, 0).is_err());
        assert!(check_size(MAX_COLS + 1, 24).is_err());
        assert!(check_size(80, MAX_ROWS + 1).is_err());
    }
}
