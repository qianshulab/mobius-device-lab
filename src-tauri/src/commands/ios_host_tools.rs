use super::blocking_api;
use crate::{
    models::{
        ApiError, ApiResult, AppResult, IosHostDiagnosticKind, IosHostDiagnosticRequest,
        IosHostDiagnosticResult,
    },
    runner::run_process,
    validation,
};
use std::time::Duration;

const DEFAULT_DIAGNOSTIC_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_SYSLOG_TIMEOUT_MS: u64 = 5_000;
const MIN_TIMEOUT_MS: u64 = 250;
const MAX_TIMEOUT_MS: u64 = 30_000;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const MAX_ERROR_BYTES: usize = 8 * 1024;

#[tauri::command]
pub async fn run_ios_host_diagnostic(
    request: IosHostDiagnosticRequest,
) -> ApiResult<IosHostDiagnosticResult> {
    blocking_api(move || run_ios_host_diagnostic_inner(request)).await
}

fn run_ios_host_diagnostic_inner(
    request: IosHostDiagnosticRequest,
) -> AppResult<IosHostDiagnosticResult> {
    validation::serial(&request.udid)?;
    let timeout = diagnostic_timeout(request.kind, request.timeout_ms)?;
    let (tool, args) = diagnostic_invocation(request.kind, &request.udid, request.network);
    // run_process is intentionally used instead of run_checked: a syslog capture
    // is successful when the bounded collection window expires and the child is
    // stopped by the runner.
    let process = run_process(tool, &args, timeout, &[])?;
    let (output, output_truncated) = sanitize_and_limit(&process.stdout, MAX_OUTPUT_BYTES);
    let (stderr, stderr_truncated) = sanitize_and_limit(&process.stderr, MAX_STDERR_BYTES);

    if process.timed_out && request.kind != IosHostDiagnosticKind::Syslog {
        return Err(ApiError::new(
            "ios_host_diagnostic_timeout",
            format!("{tool} did not finish within {} ms", timeout.as_millis()),
        ));
    }
    if !process.timed_out && process.exit_code != Some(0) {
        let summary = if stderr.trim().is_empty() {
            output.trim()
        } else {
            stderr.trim()
        };
        let summary = sanitize_and_limit(summary, MAX_ERROR_BYTES).0;
        return Err(ApiError::new(
            "ios_host_diagnostic_failed",
            if summary.is_empty() {
                format!("{tool} exited with status {:?}", process.exit_code)
            } else {
                format!("{tool}: {summary}")
            },
        ));
    }

    let (title, source) = diagnostic_metadata(request.kind, timeout);
    let mut warnings = Vec::new();
    if process.truncated || output_truncated || stderr_truncated {
        warnings.push("Output reached the bounded capture limit and was truncated".into());
    }

    Ok(IosHostDiagnosticResult {
        success: true,
        kind: request.kind,
        title: title.into(),
        source,
        udid: request.udid,
        network: request.network,
        tool: tool.to_string(),
        output,
        stderr: (!stderr.is_empty()).then_some(stderr),
        exit_code: process.exit_code,
        timed_out: process.timed_out,
        truncated: process.truncated || output_truncated || stderr_truncated,
        duration_ms: process.duration_ms,
        warnings,
    })
}

fn diagnostic_metadata(kind: IosHostDiagnosticKind, timeout: Duration) -> (&'static str, String) {
    match kind {
        IosHostDiagnosticKind::DeviceInfo => ("libimobiledevice 设备信息", "ideviceinfo".into()),
        IosHostDiagnosticKind::Pairing => ("配对状态", "idevicepair validate".into()),
        IosHostDiagnosticKind::Apps => ("已安装应用", "ideviceinstaller list --all".into()),
        IosHostDiagnosticKind::Syslog => (
            "设备实时日志采样",
            format!("idevicesyslog · {} ms 采样", timeout.as_millis()),
        ),
    }
}

fn diagnostic_timeout(kind: IosHostDiagnosticKind, requested: Option<u64>) -> AppResult<Duration> {
    let milliseconds = requested.unwrap_or(match kind {
        IosHostDiagnosticKind::Syslog => DEFAULT_SYSLOG_TIMEOUT_MS,
        _ => DEFAULT_DIAGNOSTIC_TIMEOUT_MS,
    });
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&milliseconds) {
        return Err(ApiError::new(
            "invalid_ios_diagnostic_timeout",
            format!(
                "iOS diagnostic timeout must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS} ms"
            ),
        ));
    }
    Ok(Duration::from_millis(milliseconds))
}

fn diagnostic_invocation(
    kind: IosHostDiagnosticKind,
    udid: &str,
    network: bool,
) -> (&'static str, Vec<String>) {
    let (tool, mut args) = match kind {
        IosHostDiagnosticKind::DeviceInfo => ("ideviceinfo", vec!["-u".into(), udid.to_string()]),
        IosHostDiagnosticKind::Pairing => ("idevicepair", vec!["-u".into(), udid.to_string()]),
        IosHostDiagnosticKind::Apps => ("ideviceinstaller", vec!["-u".into(), udid.to_string()]),
        IosHostDiagnosticKind::Syslog => ("idevicesyslog", vec!["-u".into(), udid.to_string()]),
    };
    if network {
        args.push("-n".into());
    }
    match kind {
        IosHostDiagnosticKind::Pairing => args.push("validate".into()),
        IosHostDiagnosticKind::Apps => args.extend(["list".into(), "--all".into()]),
        IosHostDiagnosticKind::Syslog => args.push("--no-colors".into()),
        IosHostDiagnosticKind::DeviceInfo => {}
    }
    (tool, args)
}

fn sanitize_and_limit(value: &str, max_bytes: usize) -> (String, bool) {
    let clean = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>();
    let clean = clean.trim_end();
    if clean.len() <= max_bytes {
        return (clean.to_string(), false);
    }
    const LONG_SUFFIX: &str = "\n…[output truncated]";
    const SHORT_SUFFIX: &str = "…";
    let suffix = if max_bytes >= LONG_SUFFIX.len() {
        LONG_SUFFIX
    } else {
        SHORT_SUFFIX
    };
    let target = max_bytes.saturating_sub(suffix.len());
    let mut boundary = target.min(clean.len());
    while boundary > 0 && !clean.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (format!("{}{}", &clean[..boundary], suffix), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_invocations_bind_the_selected_udid_and_transport() {
        assert_eq!(
            diagnostic_invocation(IosHostDiagnosticKind::DeviceInfo, "device-1", true),
            (
                "ideviceinfo",
                vec!["-u", "device-1", "-n"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            )
        );
        assert_eq!(
            diagnostic_invocation(IosHostDiagnosticKind::Pairing, "device-1", false),
            (
                "idevicepair",
                vec!["-u", "device-1", "validate"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            )
        );
        assert_eq!(
            diagnostic_invocation(IosHostDiagnosticKind::Apps, "device-1", true),
            (
                "ideviceinstaller",
                vec!["-u", "device-1", "-n", "list", "--all"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            )
        );
        assert_eq!(
            diagnostic_invocation(IosHostDiagnosticKind::Syslog, "device-1", false),
            (
                "idevicesyslog",
                vec!["-u", "device-1", "--no-colors"]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            )
        );
    }

    #[test]
    fn timeout_is_strictly_bounded() {
        assert!(diagnostic_timeout(IosHostDiagnosticKind::Syslog, Some(249)).is_err());
        assert!(diagnostic_timeout(IosHostDiagnosticKind::Syslog, Some(30_001)).is_err());
        assert_eq!(
            diagnostic_timeout(IosHostDiagnosticKind::Syslog, None).expect("default"),
            Duration::from_millis(DEFAULT_SYSLOG_TIMEOUT_MS)
        );
    }

    #[test]
    fn output_sanitizer_preserves_utf8_and_enforces_its_limit() {
        let (output, truncated) = sanitize_and_limit("A\u{1b}\u{0}手机日志手机日志", 18);
        assert!(truncated);
        assert!(!output.contains('\u{1b}'));
        assert!(output.is_char_boundary(output.len()));
        assert!(output.len() <= 18);
    }
}
