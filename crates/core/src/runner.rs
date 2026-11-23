//! Subprocess execution with line streaming, log tee and a stall watchdog.
//!
//! Every installer command (wine, winetricks, msiexec, 7z, downloads) goes
//! through here so the GUI gets one unified log and hangs are detected.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

const TAIL_LINES: usize = 200;
const MAX_LINE_BYTES: usize = 64 * 1024;
const EXIT_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Kill the child when it produces no output for this long.
    pub stall_timeout: Option<Duration>,
    /// Tee every line into this file (created or truncated unless `log_append`).
    pub log_path: Option<PathBuf>,
    /// Append to an existing log instead of truncating it.
    pub log_append: bool,
    pub envs: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// Forward lines to the callback.
    pub echo: bool,
    /// Let the child use the terminal's stdin (needed for sudo password prompts).
    pub inherit_stdin: bool,
    /// Optional cancellation flag; when set, the child is killed promptly.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            stall_timeout: None,
            log_path: None,
            log_append: false,
            envs: Vec::new(),
            cwd: None,
            echo: true,
            inherit_stdin: false,
            cancel: None,
        }
    }
}

impl RunOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stall(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    pub fn log(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_path = Some(path.into());
        self
    }

    /// Keep existing log contents and append new lines to the same file.
    pub fn log_append(mut self) -> Self {
        self.log_append = true;
        self
    }

    /// Inherit stdin, so interactive prompts (sudo) reach the terminal.
    pub fn inherit_stdin(mut self) -> Self {
        self.inherit_stdin = true;
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    pub fn quiet(mut self) -> Self {
        self.echo = false;
        self
    }

    pub fn cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }
}

#[derive(Debug)]
pub struct RunResult {
    pub status: ExitStatus,
    pub stalled: bool,
    pub cancelled: bool,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
}

impl RunResult {
    pub fn success(&self) -> bool {
        self.status.success() && !self.stalled && !self.cancelled
    }

    /// Last lines of stderr (falling back to stdout) for error messages.
    pub fn tail_text(&self, max_lines: usize) -> String {
        let mut lines: Vec<&String> = self.stderr_tail.iter().collect();
        if lines.is_empty() {
            lines = self.stdout_tail.iter().collect();
        }
        let start = lines.len().saturating_sub(max_lines);
        lines[start..]
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Spawn `cmd`, stream output, and wait for completion.
///
/// The callback runs on the calling thread, so the caller controls UI updates.
pub fn run<F>(mut cmd: Command, opts: RunOptions, mut on_line: F) -> Result<RunResult>
where
    F: FnMut(Stream, &str),
{
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    for (key, value) in &opts.envs {
        cmd.env(key, value);
    }
    let stdin = if opts.inherit_stdin {
        Stdio::inherit()
    } else {
        Stdio::null()
    };
    cmd.stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let program = cmd.get_program().to_string_lossy().to_string();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn {program}"))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = mpsc::channel::<(Stream, String)>();
    spawn_reader(stdout, Stream::Stdout, tx.clone());
    spawn_reader(stderr, Stream::Stderr, tx.clone());
    drop(tx);

    let mut log = match &opts.log_path {
        Some(path) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let mut options = OpenOptions::new();
            options.create(true).write(true);
            if opts.log_append {
                options.append(true);
            } else {
                options.truncate(true);
            }
            Some(
                options
                    .open(path)
                    .with_context(|| format!("opening {}", path.display()))?,
            )
        }
        None => None,
    };

    let mut stdout_tail = VecDeque::new();
    let mut stderr_tail = VecDeque::new();
    let mut last_output = Instant::now();
    let mut stalled = false;
    let mut cancelled = false;
    let mut status: Option<ExitStatus> = None;
    let mut finished_at: Option<Instant> = None;
    let mut disconnected = false;

    while !disconnected {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok((stream, line)) => {
                last_output = Instant::now();
                emit_line(
                    stream,
                    &line,
                    opts.echo,
                    &mut log,
                    &mut on_line,
                    &mut stdout_tail,
                    &mut stderr_tail,
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => disconnected = true,
        }

        if status.is_none()
            && let Some(exit) = child.try_wait()?
        {
            status = Some(exit);
            finished_at = Some(Instant::now());
        }

        if status.is_none()
            && !cancelled
            && opts
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            cancelled = true;
            let _ = child.kill();
        }

        if status.is_none()
            && !stalled
            && !cancelled
            && let Some(limit) = opts.stall_timeout
            && last_output.elapsed() >= limit
        {
            stalled = true;
            let _ = child.kill();
        }

        if let Some(at) = finished_at
            && at.elapsed() >= EXIT_GRACE
        {
            break;
        }
    }

    if status.is_none() {
        status = Some(child.wait().context("waiting for child process")?);
    }

    // Drain lines that arrived while we were exiting.
    while let Ok((stream, line)) = rx.try_recv() {
        emit_line(
            stream,
            &line,
            opts.echo,
            &mut log,
            &mut on_line,
            &mut stdout_tail,
            &mut stderr_tail,
        );
    }

    Ok(RunResult {
        status: status.expect("status set above"),
        stalled,
        cancelled,
        stdout_tail: stdout_tail.into(),
        stderr_tail: stderr_tail.into(),
    })
}

fn spawn_reader<R>(reader: R, stream: Stream, tx: Sender<(Stream, String)>)
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        let mut last_was_cr = false;

        loop {
            let chunk = match reader.fill_buf() {
                Ok([]) => break,
                Ok(buf) => buf,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let len = chunk.len();
            let mut stopped = false;

            for &byte in chunk {
                match byte {
                    b'\r' => {
                        if send_line(&tx, stream, &mut line).is_err() {
                            stopped = true;
                            break;
                        }
                        last_was_cr = true;
                    }
                    b'\n' => {
                        if last_was_cr && line.is_empty() {
                            last_was_cr = false;
                            continue;
                        }
                        if send_line(&tx, stream, &mut line).is_err() {
                            stopped = true;
                            break;
                        }
                        last_was_cr = false;
                    }
                    _ => {
                        last_was_cr = false;
                        line.push(byte);
                        if line.len() >= MAX_LINE_BYTES
                            && send_line(&tx, stream, &mut line).is_err()
                        {
                            stopped = true;
                            break;
                        }
                    }
                }
            }

            reader.consume(len);
            if stopped {
                return;
            }
        }

        if !line.is_empty() {
            let _ = send_line(&tx, stream, &mut line);
        }
    });
}

fn send_line(
    tx: &Sender<(Stream, String)>,
    stream: Stream,
    buf: &mut Vec<u8>,
) -> std::result::Result<(), ()> {
    let text = String::from_utf8_lossy(buf).into_owned();
    buf.clear();
    tx.send((stream, text)).map_err(|_| ())
}

fn emit_line<F>(
    stream: Stream,
    line: &str,
    echo: bool,
    log: &mut Option<File>,
    on_line: &mut F,
    stdout_tail: &mut VecDeque<String>,
    stderr_tail: &mut VecDeque<String>,
) where
    F: FnMut(Stream, &str),
{
    if let Some(file) = log {
        let _ = writeln!(file, "{line}");
    }
    let tail = match stream {
        Stream::Stdout => stdout_tail,
        Stream::Stderr => stderr_tail,
    };
    if tail.len() == TAIL_LINES {
        tail.pop_front();
    }
    tail.push_back(line.to_string());
    if echo {
        on_line(stream, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[test]
    fn streams_stdout_and_stderr() {
        let mut lines = Vec::new();
        let result = run(
            sh("echo out1; echo err1 >&2; echo out2"),
            RunOptions::default(),
            |stream, line| lines.push((stream, line.to_string())),
        )
        .unwrap();

        assert!(result.status.success());
        assert!(!result.stalled);
        assert!(
            lines
                .iter()
                .any(|(s, l)| *s == Stream::Stdout && l == "out1")
        );
        assert!(
            lines
                .iter()
                .any(|(s, l)| *s == Stream::Stderr && l == "err1")
        );
        assert!(result.stdout_tail.contains(&"out2".to_string()));
    }

    #[test]
    fn tee_writes_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/run.log");
        let result = run(
            sh("echo hello"),
            RunOptions::default().quiet().log(&path),
            |_, _| {},
        )
        .unwrap();
        assert!(result.status.success());
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.contains("hello"));
    }

    #[test]
    fn stall_watchdog_kills_silent_child() {
        let started = Instant::now();
        let result = run(
            sh("sleep 10"),
            RunOptions::default()
                .quiet()
                .stall(Duration::from_millis(300)),
            |_, _| {},
        )
        .unwrap();
        assert!(result.stalled);
        assert!(started.elapsed() < Duration::from_secs(6));
    }

    #[test]
    fn reports_failure_status() {
        let result = run(sh("exit 3"), RunOptions::default().quiet(), |_, _| {}).unwrap();
        assert!(!result.success());
        assert_eq!(result.status.code(), Some(3));
    }

    #[test]
    fn handles_non_utf8_output() {
        let mut lines = Vec::new();
        let result = run(
            sh("printf 'bad\\377byte\\nafter\\n'"),
            RunOptions::default(),
            |_, line| lines.push(line.to_string()),
        )
        .unwrap();
        assert!(result.success());
        assert!(lines.iter().any(|line| line.contains("bad")));
        assert!(lines.iter().any(|line| line == "after"));
    }

    #[test]
    fn splits_carriage_return_progress_lines() {
        let mut lines = Vec::new();
        let result = run(
            sh("printf '10%%\\r50%%\\r100%%\\r\\ndone\\n'"),
            RunOptions::default(),
            |_, line| lines.push(line.to_string()),
        )
        .unwrap();
        assert!(result.success());
        assert_eq!(lines, vec!["10%", "50%", "100%", "done"]);
    }

    #[test]
    fn cancel_flag_stops_child() {
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            trigger.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = run(
            sh("sleep 10"),
            RunOptions::default().quiet().cancel(cancel),
            |_, _| {},
        )
        .unwrap();
        assert!(result.cancelled);
        assert!(!result.success());
        assert!(started.elapsed() < Duration::from_secs(6));
    }
}
