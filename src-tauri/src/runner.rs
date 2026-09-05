use crate::{
    models::{ApiError, AppResult, OperationResult},
    toolchain,
};
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(500);
const CAPTURE_CHANNEL_DEPTH: usize = 32;
const MAX_DRAIN_EVENTS_PER_POLL: usize = 64;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProcessOutput {
    pub program: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub truncated: bool,
    pub duration_ms: u64,
}

impl ProcessOutput {
    pub fn into_operation(self, message: impl Into<String>) -> OperationResult {
        OperationResult {
            success: true,
            message: message.into(),
            stdout: (!self.stdout.is_empty()).then_some(self.stdout),
            stderr: (!self.stderr.is_empty()).then_some(self.stderr),
            pid: None,
            exit_code: self.exit_code,
            timed_out: self.timed_out,
        }
    }
}

pub(crate) fn run_checked(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> AppResult<ProcessOutput> {
    run_checked_redacted(program, args, timeout, &[])
}

pub(crate) fn run_checked_redacted(
    program: &str,
    args: &[String],
    timeout: Duration,
    secrets: &[&str],
) -> AppResult<ProcessOutput> {
    let output = run_process(program, args, timeout, secrets)?;
    if output.timed_out {
        return Err(ApiError::new(
            "process_timeout",
            format!("{program} did not finish within {} ms", timeout.as_millis()),
        )
        .with_details(serde_json::to_value(&output).unwrap_or_default()));
    }
    if output.exit_code != Some(0) {
        let summary = if output.stderr.trim().is_empty() {
            output.stdout.trim()
        } else {
            output.stderr.trim()
        };
        let message = if summary.is_empty() {
            format!("{program} exited with status {:?}", output.exit_code)
        } else {
            format!("{program}: {summary}")
        };
        return Err(ApiError::new("process_failed", message)
            .with_details(serde_json::to_value(&output).unwrap_or_default()));
    }
    Ok(output)
}

pub(crate) fn run_checked_with_env(
    program: &str,
    args: &[String],
    timeout: Duration,
    secrets: &[&str],
    environment: &[(String, String)],
) -> AppResult<ProcessOutput> {
    let environment = environment
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect::<Vec<_>>();
    let output = run_process_inner(program, args, timeout, secrets, None, &environment)?;
    if output.timed_out {
        return Err(ApiError::new(
            "process_timeout",
            format!("{program} did not finish within {} ms", timeout.as_millis()),
        )
        .with_details(serde_json::to_value(&output).unwrap_or_default()));
    }
    if output.exit_code != Some(0) {
        let summary = if output.stderr.trim().is_empty() {
            output.stdout.trim()
        } else {
            output.stderr.trim()
        };
        let message = if summary.is_empty() {
            format!("{program} exited with status {:?}", output.exit_code)
        } else {
            format!("{program}: {summary}")
        };
        return Err(ApiError::new("process_failed", message)
            .with_details(serde_json::to_value(&output).unwrap_or_default()));
    }
    Ok(output)
}

pub(crate) fn run_checked_with_stdin(
    program: &str,
    args: &[String],
    timeout: Duration,
    stdin: &[u8],
) -> AppResult<ProcessOutput> {
    let output = run_process_inner(program, args, timeout, &[], Some(stdin), &[])?;
    if output.timed_out {
        return Err(ApiError::new(
            "process_timeout",
            format!("{program} did not finish within {} ms", timeout.as_millis()),
        )
        .with_details(serde_json::to_value(&output).unwrap_or_default()));
    }
    if output.exit_code != Some(0) {
        let summary = if output.stderr.trim().is_empty() {
            output.stdout.trim()
        } else {
            output.stderr.trim()
        };
        return Err(ApiError::new(
            "process_failed",
            if summary.is_empty() {
                format!("{program} exited with status {:?}", output.exit_code)
            } else {
                format!("{program}: {summary}")
            },
        )
        .with_details(serde_json::to_value(&output).unwrap_or_default()));
    }
    Ok(output)
}

pub(crate) fn run_process(
    program: &str,
    args: &[String],
    timeout: Duration,
    secrets: &[&str],
) -> AppResult<ProcessOutput> {
    run_process_inner(program, args, timeout, secrets, None, &[])
}

fn run_process_inner(
    program: &str,
    args: &[String],
    timeout: Duration,
    secrets: &[&str],
    stdin_bytes: Option<&[u8]>,
    environment: &[(OsString, OsString)],
) -> AppResult<ProcessOutput> {
    let executable = resolve_tool(program)?;
    run_process_at_inner(
        program,
        &executable,
        args,
        timeout,
        secrets,
        stdin_bytes,
        environment,
    )
}

pub(crate) fn run_process_at(
    program: &str,
    executable: &Path,
    args: &[String],
    timeout: Duration,
    secrets: &[&str],
) -> AppResult<ProcessOutput> {
    run_process_at_inner(program, executable, args, timeout, secrets, None, &[])
}

pub(crate) fn run_process_at_with_env(
    program: &str,
    executable: &Path,
    args: &[String],
    timeout: Duration,
    secrets: &[&str],
    environment: &[(OsString, OsString)],
) -> AppResult<ProcessOutput> {
    run_process_at_inner(
        program,
        executable,
        args,
        timeout,
        secrets,
        None,
        environment,
    )
}

fn run_process_at_inner(
    program: &str,
    executable: &Path,
    args: &[String],
    timeout: Duration,
    secrets: &[&str],
    stdin_bytes: Option<&[u8]>,
    environment: &[(OsString, OsString)],
) -> AppResult<ProcessOutput> {
    let started = Instant::now();
    let mut command = background_command(executable);
    command
        .args(args)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.envs(environment.iter().map(|(key, value)| (key, value)));
    let mut child = command
        .spawn()
        .map_err(|error| spawn_error(program, error))?;

    if let Some(bytes) = stdin_bytes {
        let write_result = child
            .stdin
            .take()
            .ok_or_else(|| ApiError::new("process_io_error", "Unable to open process stdin"))?
            .write_all(bytes);
        if let Err(error) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ApiError::new(
                "process_io_error",
                format!("Unable to write process stdin: {error}"),
            ));
        }
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::new("process_io_error", "Unable to capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ApiError::new("process_io_error", "Unable to capture stderr"))?;
    let stdout_reader = capture_stream(stdout);
    let stderr_reader = capture_stream(stderr);
    let mut stdout_capture = Capture::default();
    let mut stderr_capture = Capture::default();

    let mut timed_out = false;
    let mut process_finished_at = None;
    let mut status = None;
    loop {
        drain_capture(&stdout_reader, &mut stdout_capture);
        drain_capture(&stderr_reader, &mut stderr_capture);

        if status.is_some()
            && (stdout_capture.done && stderr_capture.done
                || process_finished_at
                    .is_some_and(|finished: Instant| finished.elapsed() >= PIPE_DRAIN_GRACE))
        {
            if !(stdout_capture.done && stderr_capture.done) {
                stdout_capture.truncated |= !stdout_capture.done;
                stderr_capture.truncated |= !stderr_capture.done;
            }
            break;
        }

        match child.try_wait() {
            Ok(Some(exit_status)) => {
                status = Some(exit_status);
                process_finished_at.get_or_insert_with(Instant::now);
                thread::sleep(POLL_INTERVAL);
            }
            Ok(None) if started.elapsed() < timeout => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                status = Some(child.wait().map_err(|error| {
                    ApiError::new(
                        "process_wait_error",
                        format!("Unable to wait for {program}: {error}"),
                    )
                })?);
                process_finished_at = Some(Instant::now());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApiError::new(
                    "process_wait_error",
                    format!("Unable to query {program}: {error}"),
                ));
            }
        }
    }

    if let Some(error) = stdout_capture.error.or(stderr_capture.error) {
        return Err(ApiError::new("process_io_error", error));
    }

    let mut stdout = String::from_utf8_lossy(&stdout_capture.bytes)
        .trim_end()
        .to_owned();
    let mut stderr = String::from_utf8_lossy(&stderr_capture.bytes)
        .trim_end()
        .to_owned();
    for secret in secrets.iter().filter(|value| !value.is_empty()) {
        stdout = stdout.replace(secret, "******");
        stderr = stderr.replace(secret, "******");
    }

    Ok(ProcessOutput {
        program: program.to_string(),
        exit_code: status.and_then(|value| value.code()),
        stdout,
        stderr,
        timed_out,
        truncated: stdout_capture.truncated || stderr_capture.truncated,
        duration_ms: elapsed_ms(started),
    })
}

pub(crate) fn resolve_tool(program: &str) -> AppResult<PathBuf> {
    toolchain::resolve_tool(program).map(|tool| tool.path)
}

/// Creates a host-side CLI process without opening a transient console window
/// when the desktop application runs as a Windows GUI executable. This flag
/// only suppresses the console; tools that create their own GUI (scrcpy, for
/// example) continue to do so normally.
pub(crate) fn background_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new(program);
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(program)
    }
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
    done: bool,
    error: Option<String>,
}

enum CaptureEvent {
    Chunk(Vec<u8>),
    Truncated,
    Done,
    Error(io::Error),
}

fn capture_stream(mut stream: impl Read + Send + 'static) -> Receiver<CaptureEvent> {
    let (sender, receiver) = mpsc::sync_channel(CAPTURE_CHANNEL_DEPTH);
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        let mut retained = 0_usize;
        let mut reported_truncation = false;
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(CaptureEvent::Done);
                    break;
                }
                Ok(count) => {
                    let keep = MAX_CAPTURE_BYTES.saturating_sub(retained).min(count);
                    if keep > 0 {
                        if sender
                            .send(CaptureEvent::Chunk(buffer[..keep].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                        retained += keep;
                    }
                    if keep < count && !reported_truncation {
                        if sender.send(CaptureEvent::Truncated).is_err() {
                            break;
                        }
                        reported_truncation = true;
                    }
                }
                Err(error) => {
                    let _ = sender.send(CaptureEvent::Error(error));
                    break;
                }
            }
        }
    });
    receiver
}

fn drain_capture(receiver: &Receiver<CaptureEvent>, capture: &mut Capture) {
    for _ in 0..MAX_DRAIN_EVENTS_PER_POLL {
        match receiver.try_recv() {
            Ok(CaptureEvent::Chunk(chunk)) => {
                let remaining = MAX_CAPTURE_BYTES.saturating_sub(capture.bytes.len());
                let keep = remaining.min(chunk.len());
                capture.bytes.extend_from_slice(&chunk[..keep]);
                capture.truncated |= keep < chunk.len();
            }
            Ok(CaptureEvent::Truncated) => capture.truncated = true,
            Ok(CaptureEvent::Done) => capture.done = true,
            Ok(CaptureEvent::Error(error)) => {
                capture.error = Some(format!("Unable to read process output: {error}"));
                capture.done = true;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                capture.done = true;
                break;
            }
        }
    }
}

fn spawn_error(program: &str, error: io::Error) -> ApiError {
    let code = if error.kind() == io::ErrorKind::NotFound {
        "tool_not_found"
    } else {
        "process_spawn_error"
    };
    ApiError::new(code, format!("Unable to start {program}: {error}"))
}

pub(crate) fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_is_bounded_and_reports_truncation() {
        let receiver = capture_stream(std::io::Cursor::new(vec![b'x'; MAX_CAPTURE_BYTES * 2]));
        let mut capture = Capture::default();
        for _ in 0..1_000 {
            drain_capture(&receiver, &mut capture);
            if capture.done {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(capture.done);
        assert!(capture.truncated);
        assert_eq!(capture.bytes.len(), MAX_CAPTURE_BYTES);
    }
}
