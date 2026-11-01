//! Running a formatter over buffer text.

use std::ffi::OsString;
use std::io::{self, Read, Write as _};
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;
use std::{fmt, fs, thread};

use command::blocking::Command;
use instant::Instant;

use crate::line_ending::LineEnding;
use crate::plan::Invocation;

/// How often a running formatter is checked for having exited.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// How long a formatter's pipes may stay open after it exits before the run is given up on. A
/// formatter that leaves a background process holding its stdout would otherwise never finish.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// The most stderr kept from a failed run.
const MAX_STDERR_BYTES: usize = 64 * 1024;

const MAX_MESSAGE_CHARS: usize = 200;

const MIN_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_TIMEOUT: Duration = Duration::from_secs(60);

/// Bounds on a formatter run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub timeout: Duration,
    /// Buffers larger than this are not formatted.
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
}

impl Limits {
    /// The default limits with `timeout`, kept within bounds that can neither cut off every
    /// formatter nor hold up a save indefinitely.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout: timeout.clamp(MIN_TIMEOUT, MAX_TIMEOUT),
            ..Self::default()
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_input_bytes: 4 * 1024 * 1024,
            max_output_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Why a formatter run produced nothing usable.
#[derive(Debug)]
pub enum RunError {
    InputTooLarge {
        bytes: usize,
    },
    Spawn(io::Error),
    Io(io::Error),
    TimedOut(Duration),
    Exited {
        status: ExitStatus,
        /// What the formatter printed about the failure.
        detail: String,
    },
    OutputTooLarge,
    OutputLeftOpen,
    InvalidUtf8,
    EmptyOutput,
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { bytes } => {
                write!(f, "the file is too large to format ({bytes} bytes)")
            }
            Self::Spawn(error) => write!(f, "could not be started: {error}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::TimedOut(timeout) => write!(f, "timed out after {timeout:?}"),
            Self::Exited { status, detail } => match first_line(detail) {
                Some(line) => f.write_str(&line),
                None => write!(f, "failed ({status})"),
            },
            Self::OutputTooLarge => f.write_str("produced more output than expected"),
            Self::OutputLeftOpen => f.write_str("left its output open after exiting"),
            Self::InvalidUtf8 => f.write_str("produced output that is not valid UTF-8"),
            Self::EmptyOutput => f.write_str("produced no output"),
        }
    }
}

/// Formats `input` with `invocation` and returns the formatted text, using the line endings of
/// `input`. `path_env` becomes the child's `PATH`; the rest of its environment is inherited.
pub fn run(
    invocation: &Invocation,
    input: &str,
    path_env: Option<&str>,
    limits: &Limits,
) -> Result<String, RunError> {
    if input.len() > limits.max_input_bytes {
        return Err(RunError::InputTooLarge { bytes: input.len() });
    }
    let output = if invocation.uses_temp_copy() {
        run_on_temp_copy(invocation, input, path_env, limits)?
    } else {
        let args = invocation.rendered_args(Path::new(""));
        execute(
            invocation,
            &args,
            Some(input.as_bytes().to_vec()),
            path_env,
            limits,
        )?
    };
    let output = String::from_utf8(output).map_err(|_| RunError::InvalidUtf8)?;
    if output.trim().is_empty() && !input.trim().is_empty() {
        return Err(RunError::EmptyOutput);
    }
    Ok(LineEnding::detect(input).apply(output))
}

/// Runs a tool that rewrites files in place on a copy of the buffer, so the user's file is never
/// the one being rewritten.
fn run_on_temp_copy(
    invocation: &Invocation,
    input: &str,
    path_env: Option<&str>,
    limits: &Limits,
) -> Result<Vec<u8>, RunError> {
    let dir = tempfile::Builder::new()
        .prefix("warp-format-")
        .tempdir()
        .map_err(RunError::Io)?;
    // The copy keeps the file's name because these tools pick their rules from it.
    let file_name = invocation.file.file_name().unwrap_or_default();
    let copy = dir.path().join(file_name);
    fs::write(&copy, input).map_err(RunError::Io)?;

    execute(
        invocation,
        &invocation.rendered_args(&copy),
        None,
        path_env,
        limits,
    )?;

    let size = fs::metadata(&copy).map_err(RunError::Io)?.len();
    if size > limits.max_output_bytes as u64 {
        return Err(RunError::OutputTooLarge);
    }
    fs::read(&copy).map_err(RunError::Io)
}

/// Runs the formatter to completion and returns its stdout.
fn execute(
    invocation: &Invocation,
    args: &[OsString],
    stdin: Option<Vec<u8>>,
    path_env: Option<&str>,
    limits: &Limits,
) -> Result<Vec<u8>, RunError> {
    let mut command = Command::new(&invocation.program);
    command
        .args(args)
        .current_dir(&invocation.cwd)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path_env) = path_env {
        command.env("PATH", path_env);
    }
    // The formatter leads its own process group so that a timeout can take down everything it
    // started, such as the interpreter behind a wrapper script.
    //
    // SAFETY: `setpgid` is async-signal-safe, as `pre_exec` requires.
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = RunningChild {
        child: command.spawn().map_err(RunError::Spawn)?,
        exited: false,
    };
    #[cfg(windows)]
    let _job = {
        use std::os::windows::io::AsRawHandle as _;
        let job = win32job::Job::create().map_err(|error| RunError::Io(io::Error::other(error)))?;
        let mut info = job
            .query_extended_limit_info()
            .map_err(|error| RunError::Io(io::Error::other(error)))?;
        info.limit_kill_on_job_close();
        job.set_extended_limit_info(&info)
            .map_err(|error| RunError::Io(io::Error::other(error)))?;
        job.assign_process(child.child.as_raw_handle() as isize)
            .map_err(|error| RunError::Io(io::Error::other(error)))?;
        job
    };
    let deadline = Instant::now() + limits.timeout;

    if let (Some(input), Some(mut pipe)) = (stdin, child.child.stdin.take()) {
        // A formatter that exits without reading its input closes the pipe; that failure is
        // reported through its exit status instead.
        thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let stdout = capture(child.child.stdout.take(), limits.max_output_bytes);
    let stderr = capture(child.child.stderr.take(), MAX_STDERR_BYTES);

    let mut status = None;
    let mut output = None;
    let mut exited_at = None;
    let (status, output) = loop {
        if status.is_none() {
            status = child.try_wait().map_err(RunError::Io)?;
            if status.is_some() {
                exited_at = Some(Instant::now());
            }
        }
        if output.is_none() {
            match stdout.recv_timeout(POLL_INTERVAL) {
                Ok(captured) => output = Some(captured),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => output = Some(Captured::default()),
            }
        } else if status.is_none() {
            thread::sleep(POLL_INTERVAL);
        }

        if output.as_ref().is_some_and(|captured| captured.truncated) {
            return Err(RunError::OutputTooLarge);
        }
        if let Some(status) = status
            && let Some(output) = output.take()
        {
            break (status, output);
        }
        if exited_at.is_some_and(|exited_at| exited_at.elapsed() > DRAIN_GRACE) {
            return Err(RunError::OutputLeftOpen);
        }
        if Instant::now() >= deadline {
            return Err(RunError::TimedOut(limits.timeout));
        }
    };

    if status.success() {
        return Ok(output.bytes);
    }
    let stderr = stderr.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    // Some tools report their errors on stdout.
    let detail = if stderr.bytes.trim_ascii().is_empty() {
        output.bytes
    } else {
        stderr.bytes
    };
    Err(RunError::Exited {
        status,
        detail: String::from_utf8_lossy(&detail).into_owned(),
    })
}

/// A spawned formatter. Unless it has already exited, dropping it kills its whole process group
/// and reaps it, so an abandoned run leaves nothing behind.
struct RunningChild {
    child: Child,
    exited: bool,
}

impl RunningChild {
    #[cfg(unix)]
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use std::os::unix::process::ExitStatusExt as _;
        // Keep the leader unreaped until Drop so its process-group ID cannot be reused.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: info is writable and waitid only observes our owned child without reaping it.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: waitid initialized info, and its child fields are valid when si_pid is nonzero.
        let info = unsafe { info.assume_init() };
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let status = unsafe { info.si_status() };
        let raw = if info.si_code == libc::CLD_EXITED {
            status << 8
        } else {
            status
        };
        Ok(Some(ExitStatus::from_raw(raw)))
    }

    #[cfg(not(unix))]
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        self.exited |= status.is_some();
        Ok(status)
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        // Once the formatter has been reaped its process group id may belong to something else.
        if self.exited {
            return;
        }
        // SAFETY: `killpg` only sends a signal, and the unreaped child keeps the group id ours.
        #[cfg(unix)]
        unsafe {
            libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    /// Whether the stream held more than the capture limit.
    truncated: bool,
}

/// Reads `pipe` to its end on another thread, keeping at most `limit` bytes.
fn capture(pipe: Option<impl Read + Send + 'static>, limit: usize) -> Receiver<Captured> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.by_ref().take(limit as u64 + 1).read_to_end(&mut bytes);
            let truncated = bytes.len() > limit;
            bytes.truncate(limit);
            let _ = sender.send(Captured { bytes, truncated });
            // Keep reading so a formatter that prints more than is kept does not block on a full
            // pipe.
            let _ = io::copy(&mut pipe, &mut io::sink());
        });
    }
    receiver
}

/// The first line of a formatter's error output that says something, without terminal styling.
fn first_line(stderr: &str) -> Option<String> {
    let plain = strip_ansi_escapes(stderr);
    let line = plain.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(line.chars().take(MAX_MESSAGE_CHARS).collect())
}

fn strip_ansi_escapes(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            plain.push(c);
            continue;
        }
        // A CSI sequence runs from `ESC [` to its first byte in `@..=~`.
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    plain
}

// The tests drive stand-in formatters written as shell scripts.
#[cfg(all(test, unix))]
#[path = "run_tests.rs"]
mod tests;
