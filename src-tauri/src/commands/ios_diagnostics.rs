use super::{blocking_api, ios_ssh};
use crate::{
    models::{
        ApiError, ApiResult, AppResult, IosDeviceAction, IosDeviceActionConfirmation,
        IosDeviceActionRequest, IosDeviceActionResult, IosDeviceActionTarget, IosDiagnosticKind,
        IosDiagnosticRequest, IosDiagnosticResult, IosDiagnosticToolStatus,
        PrepareIosDeviceActionRequest,
    },
    state::{AppState, IosSshConnection, ManagedIosActionConfirmation},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use std::time::{Duration, Instant};
use tauri::State;

const DIAGNOSTIC_MARKER: &str = "MOBIUS_IOS_DIAGNOSTIC";
const ACTION_MARKER: &str = "MOBIUS_IOS_ACTION_ACCEPTED";
const TOOL_MARKER: &str = "MOBIUS_TOOL";
const LOG_SOURCE_MARKER: &str = "MOBIUS_LOG_SOURCE";
const DEFAULT_SYSLOG_LINES: u16 = 120;
const MIN_SYSLOG_LINES: u16 = 20;
const MAX_SYSLOG_LINES: u16 = 400;
const TEXT_LIMIT: usize = 192 * 1024;
const TOOL_TEXT_LIMIT: usize = 32 * 1024;
const ERROR_TEXT_LIMIT: usize = 8 * 1024;
const ACTION_CONFIRMATION_SECONDS: u64 = 30;
const MAX_ACTION_CONFIRMATIONS: usize = 32;
const SSH_TIMEOUT: Duration = Duration::from_secs(20);
const SYSLOG_TIMEOUT: Duration = Duration::from_secs(30);

#[tauri::command]
pub async fn get_ios_runtime_snapshot(
    request: IosDiagnosticRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosDiagnosticResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || get_ios_runtime_snapshot_inner(&state, request)).await)
}

#[tauri::command]
pub async fn prepare_ios_device_action(
    request: PrepareIosDeviceActionRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosDeviceActionConfirmation>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || prepare_ios_device_action_inner(&state, request)).await)
}

#[tauri::command]
pub async fn run_ios_device_action(
    request: IosDeviceActionRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosDeviceActionResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || run_ios_device_action_inner(&state, request)).await)
}

fn get_ios_runtime_snapshot_inner(
    state: &AppState,
    request: IosDiagnosticRequest,
) -> AppResult<IosDiagnosticResult> {
    let (connection, _) = ios_ssh::session_snapshot(state, &request.session_id)?;
    let kind = request.kind;
    let (command, timeout) = match kind {
        IosDiagnosticKind::Overview => (overview_command().to_string(), SSH_TIMEOUT),
        IosDiagnosticKind::Processes => (process_command().to_string(), SSH_TIMEOUT),
        IosDiagnosticKind::Tools => (tools_command().to_string(), SSH_TIMEOUT),
        IosDiagnosticKind::Syslog => (
            syslog_command(clamp_syslog_lines(request.syslog_lines)),
            SYSLOG_TIMEOUT,
        ),
    };
    let response = ios_ssh::run_ssh_command(&connection, &command, timeout)
        .map_err(sanitize_diagnostic_error)?;
    let mut warnings = Vec::new();

    let (title, source, output, tools, truncated) = match kind {
        IosDiagnosticKind::Overview => {
            let body = remove_marker(&response.stdout, DIAGNOSTIC_MARKER);
            let (output, truncated) = sanitize_and_limit(&body, TEXT_LIMIT);
            (
                "设备概览".to_string(),
                "SSH 固定只读诊断".to_string(),
                output,
                Vec::new(),
                truncated || response.truncated,
            )
        }
        IosDiagnosticKind::Processes => {
            let body = remove_marker(&response.stdout, DIAGNOSTIC_MARKER);
            let (output, truncated) = sanitize_and_limit(&body, TEXT_LIMIT);
            (
                "进程快照".to_string(),
                "ps / launchd 只读快照（最多 1200 行）".to_string(),
                output,
                Vec::new(),
                truncated || response.truncated,
            )
        }
        IosDiagnosticKind::Tools => {
            let tools = parse_tool_statuses(&response.stdout);
            let available = tools.iter().filter(|tool| tool.available).count();
            let summary = format!("已检测 {} 项，{} 项可用。", tools.len(), available);
            let (output, truncated) = sanitize_and_limit(&summary, TOOL_TEXT_LIMIT);
            (
                "调试工具状态".to_string(),
                "固定绝对路径清单（不启动工具）".to_string(),
                output,
                tools,
                truncated || response.truncated,
            )
        }
        IosDiagnosticKind::Syslog => {
            let (source, body) = parse_log_source(&response.stdout);
            let (output, truncated) = sanitize_and_limit(&body, TEXT_LIMIT);
            if source == "不可用" {
                warnings.push("设备未提供可用的统一日志查询或 /var/log/syslog。".to_string());
            } else if output.trim().is_empty() {
                warnings.push("指定时间范围内没有读取到日志。".to_string());
            }
            (
                "最近系统日志".to_string(),
                source,
                output,
                Vec::new(),
                truncated || response.truncated,
            )
        }
    };
    if truncated {
        warnings.push("输出已在安全上限处截断；可再次刷新获取最新快照。".to_string());
    }
    if !response.stderr.trim().is_empty() {
        let (stderr, _) = sanitize_and_limit(&response.stderr, 4 * 1024);
        warnings.push(format!("设备返回提示：{}", stderr.trim()));
    }

    Ok(IosDiagnosticResult {
        success: true,
        kind,
        title,
        output,
        truncated,
        source,
        tools,
        warnings,
    })
}

fn prepare_ios_device_action_inner(
    state: &AppState,
    request: PrepareIosDeviceActionRequest,
) -> AppResult<IosDeviceActionConfirmation> {
    let (connection, _) = ios_ssh::session_snapshot(state, &request.session_id)?;
    require_root_session(&connection)?;
    let target = action_target(&connection);
    require_expected_target(
        &target,
        &request.expected_ssh_host,
        request.expected_ssh_port,
        &request.expected_username,
        request.expected_server_system.as_deref(),
        None,
    )?;

    let confirmation_id = new_confirmation_id()?;
    let expires_at = Instant::now() + Duration::from_secs(ACTION_CONFIRMATION_SECONDS);
    let ticket = ManagedIosActionConfirmation {
        confirmation_id: confirmation_id.clone(),
        session_id: request.session_id.clone(),
        action: request.action,
        ssh_host: target.ssh_host.clone(),
        ssh_port: target.ssh_port,
        username: target.username.clone(),
        server_system: target.server_system.clone(),
        host_key_identity: target.host_key_identity.clone(),
        expires_at,
    };
    let mut confirmations = state
        .ios_action_confirmations
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS action confirmation lock was poisoned"))?;
    let now = Instant::now();
    confirmations.retain(|_, pending| pending.expires_at > now);
    confirmations.retain(|_, pending| {
        pending.session_id != request.session_id || pending.action != request.action
    });
    if confirmations.len() >= MAX_ACTION_CONFIRMATIONS {
        return Err(ApiError::new(
            "ios_action_confirmation_limit",
            "Too many device confirmations are pending; wait briefly and try again",
        ));
    }
    confirmations.insert(confirmation_id.clone(), ticket);

    Ok(IosDeviceActionConfirmation {
        success: true,
        confirmation_id,
        session_id: request.session_id,
        action: request.action,
        target,
        expires_in_seconds: ACTION_CONFIRMATION_SECONDS,
    })
}

fn run_ios_device_action_inner(
    state: &AppState,
    request: IosDeviceActionRequest,
) -> AppResult<IosDeviceActionResult> {
    validate_confirmation_id(&request.confirmation_id)?;
    let ticket = consume_action_confirmation(state, &request.confirmation_id)?;
    require_confirmation_match(&ticket, &request)?;

    let (connection, _) = ios_ssh::session_snapshot(state, &request.session_id)?;
    require_root_session(&connection)?;
    let target = action_target(&connection);
    require_expected_target(
        &target,
        &request.expected_ssh_host,
        request.expected_ssh_port,
        &request.expected_username,
        request.expected_server_system.as_deref(),
        Some(&request.expected_host_key_identity),
    )?;
    let output = ios_ssh::run_ssh_command(&connection, action_command(request.action), SSH_TIMEOUT)
        .map_err(sanitize_diagnostic_error)?;
    let accepted = output
        .stdout
        .lines()
        .any(|line| line.trim() == ACTION_MARKER);
    if !accepted {
        return Err(ApiError::new(
            "ios_action_not_accepted",
            "The device did not acknowledge the requested fixed action",
        ));
    }
    let message = match request.action {
        IosDeviceAction::Respring => "Respring 已调度，SpringBoard 将重新载入。",
        IosDeviceAction::Reboot => "设备重启已调度，SSH 连接将暂时中断。",
    };
    Ok(IosDeviceActionResult {
        success: true,
        action: request.action,
        message: message.to_string(),
        accepted,
    })
}

fn consume_action_confirmation(
    state: &AppState,
    confirmation_id: &str,
) -> AppResult<ManagedIosActionConfirmation> {
    let ticket = state
        .ios_action_confirmations
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS action confirmation lock was poisoned"))?
        .remove(confirmation_id)
        .ok_or_else(|| {
            ApiError::new(
                "ios_action_confirmation_invalid",
                "The device confirmation is missing, expired, or was already used",
            )
        })?;
    if ticket.expires_at <= Instant::now() {
        return Err(ApiError::new(
            "ios_action_confirmation_expired",
            "The device confirmation expired; review the SSH target again",
        ));
    }
    Ok(ticket)
}

fn require_root_session(connection: &IosSshConnection) -> AppResult<()> {
    if connection.remote_uid == Some(0) {
        Ok(())
    } else {
        Err(ApiError::new(
            "ios_root_required",
            "This iOS device action requires a verified root SSH session",
        ))
    }
}

fn action_target(connection: &IosSshConnection) -> IosDeviceActionTarget {
    IosDeviceActionTarget {
        ssh_host: connection.ssh_host.clone(),
        ssh_port: connection.ssh_port,
        username: connection.username.clone(),
        server_system: connection.server_system.clone(),
        host_key_identity: connection
            .host_key_alias
            .clone()
            .unwrap_or_else(|| format!("[{}]:{}", connection.ssh_host, connection.ssh_port)),
    }
}

fn require_expected_target(
    target: &IosDeviceActionTarget,
    expected_host: &str,
    expected_port: u16,
    expected_username: &str,
    expected_server_system: Option<&str>,
    expected_host_key_identity: Option<&str>,
) -> AppResult<()> {
    if target.ssh_host != expected_host
        || target.ssh_port != expected_port
        || target.username != expected_username
        || target.server_system.as_deref() != expected_server_system
        || expected_host_key_identity.is_some_and(|identity| identity != target.host_key_identity)
    {
        return Err(ApiError::new(
            "ios_action_target_changed",
            "The active SSH target changed; review the current endpoint before continuing",
        ));
    }
    Ok(())
}

fn require_confirmation_match(
    ticket: &ManagedIosActionConfirmation,
    request: &IosDeviceActionRequest,
) -> AppResult<()> {
    if ticket.confirmation_id != request.confirmation_id
        || ticket.session_id != request.session_id
        || ticket.action != request.action
        || ticket.ssh_host != request.expected_ssh_host
        || ticket.ssh_port != request.expected_ssh_port
        || ticket.username != request.expected_username
        || ticket.server_system != request.expected_server_system
        || ticket.host_key_identity != request.expected_host_key_identity
    {
        return Err(ApiError::new(
            "ios_action_confirmation_mismatch",
            "The device confirmation does not match the current SSH target and action",
        ));
    }
    Ok(())
}

fn new_confirmation_id() -> AppResult<String> {
    let mut random = [0_u8; 24];
    getrandom::getrandom(&mut random).map_err(|_| {
        ApiError::new(
            "ios_action_confirmation_unavailable",
            "Unable to create a secure device confirmation",
        )
    })?;
    Ok(URL_SAFE_NO_PAD.encode(random))
}

fn validate_confirmation_id(value: &str) -> AppResult<()> {
    if value.len() == 32
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        Ok(())
    } else {
        Err(ApiError::new(
            "invalid_ios_action_confirmation",
            "Invalid iOS device confirmation identifier",
        ))
    }
}

fn sanitize_diagnostic_error(error: ApiError) -> ApiError {
    let (message, _) = sanitize_and_limit(&error.message, ERROR_TEXT_LIMIT);
    ApiError {
        code: sanitize_field(&error.code, 96),
        message,
        details: None,
    }
}

fn clamp_syslog_lines(lines: Option<u16>) -> u16 {
    lines
        .unwrap_or(DEFAULT_SYSLOG_LINES)
        .clamp(MIN_SYSLOG_LINES, MAX_SYSLOG_LINES)
}

fn overview_command() -> &'static str {
    r#"printf 'MOBIUS_IOS_DIAGNOSTIC\n'
printf '[Kernel]\n'
if [ -x /usr/bin/uname ]; then /usr/bin/uname -a 2>&1 || true; elif [ -x /bin/uname ]; then /bin/uname -a 2>&1 || true; else printf 'Unavailable\n'; fi
printf '\n[System version]\n'
if [ -x /usr/bin/sw_vers ]; then /usr/bin/sw_vers 2>&1 || true; elif [ -r /System/Library/CoreServices/SystemVersion.plist ] && [ -x /usr/bin/plutil ]; then /usr/bin/plutil -p /System/Library/CoreServices/SystemVersion.plist 2>&1 || true; elif [ -r /System/Library/CoreServices/SystemVersion.plist ] && [ -x /var/jb/usr/bin/plutil ]; then /var/jb/usr/bin/plutil -p /System/Library/CoreServices/SystemVersion.plist 2>&1 || true; else printf 'Unavailable\n'; fi
printf '\n[Identity]\n'
if [ -x /usr/bin/id ]; then /usr/bin/id 2>&1 || true; elif [ -x /bin/id ]; then /bin/id 2>&1 || true; elif [ -x /var/jb/usr/bin/id ]; then /var/jb/usr/bin/id 2>&1 || true; else printf 'Unavailable\n'; fi
printf '\n[Hardware]\n'
if [ -x /usr/sbin/sysctl ]; then /usr/sbin/sysctl -n hw.machine hw.model hw.memsize kern.osversion 2>&1 || true; elif [ -x /var/jb/usr/sbin/sysctl ]; then /var/jb/usr/sbin/sysctl -n hw.machine hw.model hw.memsize kern.osversion 2>&1 || true; else printf 'Unavailable\n'; fi
printf '\n[Uptime]\n'
if [ -x /usr/bin/uptime ]; then /usr/bin/uptime 2>&1 || true; elif [ -x /var/jb/usr/bin/uptime ]; then /var/jb/usr/bin/uptime 2>&1 || true; else printf 'Unavailable\n'; fi
printf '\n[Disk]\n'
if [ -x /bin/df ]; then /bin/df -h / /private/var 2>&1 || /bin/df -h 2>&1 || true; elif [ -x /var/jb/bin/df ]; then /var/jb/bin/df -h / /private/var 2>&1 || /var/jb/bin/df -h 2>&1 || true; else printf 'Unavailable\n'; fi
printf '\n[Memory]\n'
if [ -x /usr/bin/vm_stat ]; then /usr/bin/vm_stat 2>&1 || true; elif [ -x /usr/sbin/sysctl ]; then /usr/sbin/sysctl hw.memsize 2>&1 || true; elif [ -x /var/jb/usr/sbin/sysctl ]; then /var/jb/usr/sbin/sysctl hw.memsize 2>&1 || true; else printf 'Unavailable\n'; fi
true"#
}

fn process_command() -> &'static str {
    r#"printf 'MOBIUS_IOS_DIAGNOSTIC\n'
printf '[Processes]\n'
mobius_ps=''
for mobius_candidate in /bin/ps /usr/bin/ps /var/jb/bin/ps /var/jb/usr/bin/ps; do if [ -x "$mobius_candidate" ]; then mobius_ps="$mobius_candidate"; break; fi; done
mobius_head=''
for mobius_candidate in /usr/bin/head /bin/head /var/jb/usr/bin/head; do if [ -x "$mobius_candidate" ]; then mobius_head="$mobius_candidate"; break; fi; done
if [ -z "$mobius_head" ]; then printf 'Unavailable\n'; elif [ -n "$mobius_ps" ] && "$mobius_ps" auxww >/dev/null 2>&1; then "$mobius_ps" auxww 2>/dev/null | "$mobius_head" -n 1200; elif [ -n "$mobius_ps" ] && "$mobius_ps" -ef >/dev/null 2>&1; then "$mobius_ps" -ef 2>/dev/null | "$mobius_head" -n 1200; elif [ -n "$mobius_ps" ]; then "$mobius_ps" -A 2>/dev/null | "$mobius_head" -n 1200; elif [ -x /bin/launchctl ]; then printf '[ps unavailable; launchd service/process snapshot]\n'; /bin/launchctl print system 2>/dev/null | "$mobius_head" -n 1200; else printf 'Unavailable\n'; fi
true"#
}

fn tools_command() -> &'static str {
    r#"printf 'MOBIUS_IOS_DIAGNOSTIC\n'
mobius_find() {
  mobius_path=''
  for mobius_candidate in "$@"; do
    case "$mobius_candidate" in /*) ;; *) continue ;; esac
    if [ -x "$mobius_candidate" ]; then mobius_path="$mobius_candidate"; break; fi
  done
  printf '%s' "$mobius_path"
}
mobius_emit() {
  mobius_id="$1"
  shift
  mobius_path=$(mobius_find "$@")
  printf 'MOBIUS_TOOL|%s|%s||\n' "$mobius_id" "$mobius_path"
}
mobius_emit frida-server /usr/sbin/frida-server /usr/local/bin/frida-server /var/jb/usr/sbin/frida-server
mobius_emit sshd /usr/sbin/sshd /var/jb/usr/sbin/sshd
mobius_emit lldb /usr/bin/lldb /var/jb/usr/bin/lldb
mobius_emit debugserver /usr/bin/debugserver /Developer/usr/bin/debugserver /var/jb/usr/bin/debugserver
mobius_emit dpkg /usr/bin/dpkg /var/jb/usr/bin/dpkg
mobius_emit ldid /usr/bin/ldid /var/jb/usr/bin/ldid
mobius_emit otool /usr/bin/otool /var/jb/usr/bin/otool
mobius_emit log /usr/bin/log
mobius_emit plutil /usr/bin/plutil /var/jb/usr/bin/plutil
true"#
}

fn syslog_command(lines: u16) -> String {
    format!(
        r#"printf 'MOBIUS_IOS_DIAGNOSTIC\n'
mobius_tail=''
for mobius_candidate in /usr/bin/tail /bin/tail /var/jb/usr/bin/tail; do if [ -x "$mobius_candidate" ]; then mobius_tail="$mobius_candidate"; break; fi; done
if [ -x /usr/bin/log ] && [ -n "$mobius_tail" ] && /usr/bin/log show --help >/dev/null 2>&1; then
  printf 'MOBIUS_LOG_SOURCE|Unified log · 最近 5 分钟\n'
  /usr/bin/log show --last 5m --style compact 2>/dev/null | "$mobius_tail" -n {lines}
elif [ -r /var/log/syslog ] && [ -n "$mobius_tail" ]; then
  printf 'MOBIUS_LOG_SOURCE|/var/log/syslog\n'
  "$mobius_tail" -n {lines} /var/log/syslog 2>/dev/null || true
else
  printf 'MOBIUS_LOG_SOURCE|不可用\n'
fi
true"#
    )
}

fn action_command(action: IosDeviceAction) -> &'static str {
    match action {
        IosDeviceAction::Respring => {
            r#"mobius_id=''
for mobius_candidate in /usr/bin/id /bin/id /var/jb/usr/bin/id; do if [ -x "$mobius_candidate" ]; then mobius_id="$mobius_candidate"; break; fi; done
[ -n "$mobius_id" ] && [ "$("$mobius_id" -u 2>/dev/null)" = "0" ] || exit 77
mobius_respring=''
for mobius_candidate in /usr/bin/sbreload /usr/local/bin/sbreload /var/jb/usr/bin/sbreload /var/jb/basebin/sbreload; do if [ -x "$mobius_candidate" ]; then mobius_respring="$mobius_candidate"; break; fi; done
mobius_killall=''
for mobius_candidate in /usr/bin/killall /bin/killall /var/jb/usr/bin/killall; do if [ -x "$mobius_candidate" ]; then mobius_killall="$mobius_candidate"; break; fi; done
mobius_sleep=''
for mobius_candidate in /bin/sleep /usr/bin/sleep /var/jb/bin/sleep; do if [ -x "$mobius_candidate" ]; then mobius_sleep="$mobius_candidate"; break; fi; done
if [ -n "$mobius_sleep" ] && [ -n "$mobius_respring" ]; then printf 'MOBIUS_IOS_ACTION_ACCEPTED\n'; ("$mobius_sleep" 1; "$mobius_respring") >/dev/null 2>&1 & elif [ -n "$mobius_sleep" ] && [ -n "$mobius_killall" ]; then printf 'MOBIUS_IOS_ACTION_ACCEPTED\n'; ("$mobius_sleep" 1; "$mobius_killall" -9 SpringBoard) >/dev/null 2>&1 & else exit 78; fi"#
        }
        IosDeviceAction::Reboot => {
            r#"mobius_id=''
for mobius_candidate in /usr/bin/id /bin/id /var/jb/usr/bin/id; do if [ -x "$mobius_candidate" ]; then mobius_id="$mobius_candidate"; break; fi; done
[ -n "$mobius_id" ] && [ "$("$mobius_id" -u 2>/dev/null)" = "0" ] || exit 77
mobius_reboot=''
for mobius_candidate in /sbin/reboot /usr/sbin/reboot /usr/bin/reboot /var/jb/sbin/reboot /var/jb/usr/sbin/reboot /var/jb/usr/bin/reboot; do if [ -x "$mobius_candidate" ]; then mobius_reboot="$mobius_candidate"; break; fi; done
mobius_sleep=''
for mobius_candidate in /bin/sleep /usr/bin/sleep /var/jb/bin/sleep; do if [ -x "$mobius_candidate" ]; then mobius_sleep="$mobius_candidate"; break; fi; done
[ -n "$mobius_reboot" ] && [ -n "$mobius_sleep" ] || exit 78
printf 'MOBIUS_IOS_ACTION_ACCEPTED\n'
("$mobius_sleep" 1; "$mobius_reboot") >/dev/null 2>&1 &"#
        }
    }
}

fn remove_marker(output: &str, marker: &str) -> String {
    output
        .lines()
        .filter(|line| line.trim() != marker)
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_log_source(output: &str) -> (String, String) {
    let prefix = format!("{LOG_SOURCE_MARKER}|");
    let mut source = "不可用".to_string();
    let mut body_lines = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed == DIAGNOSTIC_MARKER {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            source = sanitize_field(value, 120);
            continue;
        }
        body_lines.push(line);
    }
    let body = body_lines.join("\n");
    (source, body)
}

fn parse_tool_statuses(output: &str) -> Vec<IosDiagnosticToolStatus> {
    let mut statuses: Vec<IosDiagnosticToolStatus> = Vec::with_capacity(9);
    for line in output.lines().take(128) {
        let mut fields = line.trim().splitn(5, '|');
        if fields.next() != Some(TOOL_MARKER) {
            continue;
        }
        let Some(id) = fields.next() else {
            continue;
        };
        let Some((name, purpose)) = tool_metadata(id) else {
            continue;
        };
        if statuses.iter().any(|tool| tool.id == id) {
            continue;
        }
        let path = sanitize_field(fields.next().unwrap_or_default(), 512);
        let available = is_reportable_tool_path(id, &path);
        statuses.push(IosDiagnosticToolStatus {
            id: id.to_string(),
            name: name.to_string(),
            available,
            path: available.then_some(path),
            version: None,
            running: None,
            purpose: purpose.to_string(),
        });
        if statuses.len() == 9 {
            break;
        }
    }
    statuses
}

fn is_reportable_tool_path(id: &str, path: &str) -> bool {
    let candidates: &[&str] = match id {
        "frida-server" => &[
            "/usr/sbin/frida-server",
            "/usr/local/bin/frida-server",
            "/var/jb/usr/sbin/frida-server",
        ],
        "sshd" => &["/usr/sbin/sshd", "/var/jb/usr/sbin/sshd"],
        "lldb" => &["/usr/bin/lldb", "/var/jb/usr/bin/lldb"],
        "debugserver" => &[
            "/usr/bin/debugserver",
            "/Developer/usr/bin/debugserver",
            "/var/jb/usr/bin/debugserver",
        ],
        "dpkg" => &["/usr/bin/dpkg", "/var/jb/usr/bin/dpkg"],
        "ldid" => &["/usr/bin/ldid", "/var/jb/usr/bin/ldid"],
        "otool" => &["/usr/bin/otool", "/var/jb/usr/bin/otool"],
        "log" => &["/usr/bin/log"],
        "plutil" => &["/usr/bin/plutil", "/var/jb/usr/bin/plutil"],
        _ => &[],
    };
    candidates.contains(&path)
}

fn tool_metadata(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "frida-server" => ("Frida Server", "动态插桩服务"),
        "sshd" => ("OpenSSH Server", "SSH 远程服务"),
        "lldb" => ("LLDB", "原生调试器"),
        "debugserver" => ("debugserver", "远程原生调试"),
        "dpkg" => ("dpkg", "越狱软件包查询"),
        "ldid" => ("ldid", "Mach-O 签名信息"),
        "otool" => ("otool", "Mach-O 元数据"),
        "log" => ("Apple log", "统一日志查询"),
        "plutil" => ("plutil", "属性列表读取"),
        _ => return None,
    })
}

fn sanitize_field(value: &str, max_bytes: usize) -> String {
    let clean = value
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    truncate_utf8(clean.trim(), max_bytes).0
}

fn sanitize_and_limit(value: &str, max_bytes: usize) -> (String, bool) {
    let clean = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>();
    truncate_utf8(clean.trim(), max_bytes)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    const SUFFIX: &str = "\n…[输出已截断]";
    let target = max_bytes.saturating_sub(SUFFIX.len());
    let mut boundary = target.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (format!("{}{}", &value[..boundary], SUFFIX), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syslog_line_count_is_strictly_bounded() {
        assert_eq!(clamp_syslog_lines(None), 120);
        assert_eq!(clamp_syslog_lines(Some(1)), 20);
        assert_eq!(clamp_syslog_lines(Some(999)), 400);
        assert!(syslog_command(400).contains("\"$mobius_tail\" -n 400"));
    }

    #[test]
    fn parses_only_known_tool_markers() {
        let parsed = parse_tool_statuses(
            "noise\nMOBIUS_TOOL|frida-server|/usr/sbin/frida-server|true|16.1.4\nMOBIUS_TOOL|unknown|/tmp/x||1\nMOBIUS_TOOL|lldb|||",
        );
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].available);
        assert_eq!(parsed[0].version, None);
        assert_eq!(parsed[0].running, None);
        assert!(!parsed[1].available);
        assert_eq!(parsed[1].path, None);
    }

    #[test]
    fn tool_probe_uses_only_compiled_absolute_paths_without_starting_tools() {
        let command = tools_command();
        assert!(!command.contains("command -v"));
        assert!(!command.contains("--version"));
        assert!(!command.contains("pgrep"));
        assert!(command.contains("/usr/sbin/frida-server"));
        assert!(command.contains("[ -x \"$mobius_candidate\" ]"));
    }

    #[test]
    fn ignores_non_absolute_tool_paths_from_remote_output() {
        let parsed = parse_tool_statuses("MOBIUS_TOOL|frida-server|frida-server|true|16.1.4");
        assert_eq!(parsed.len(), 1);
        assert!(!parsed[0].available);
        assert_eq!(parsed[0].path, None);
        assert_eq!(parsed[0].running, None);
    }

    #[test]
    fn ignores_paths_outside_each_tools_compiled_catalog() {
        let parsed = parse_tool_statuses("MOBIUS_TOOL|frida-server|/tmp/frida-server||");
        assert_eq!(parsed.len(), 1);
        assert!(!parsed[0].available);
        assert_eq!(parsed[0].path, None);
    }

    #[test]
    fn tool_status_result_is_deduplicated_and_catalog_bounded() {
        let repeated = "MOBIUS_TOOL|frida-server|/usr/sbin/frida-server||\n".repeat(500);
        let parsed = parse_tool_statuses(&repeated);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "frida-server");
    }

    #[test]
    fn output_sanitizer_removes_controls_and_keeps_utf8_valid() {
        let (value, truncated) = sanitize_and_limit("A\u{1b}[31m中文内容", 12);
        assert!(truncated);
        assert!(!value.contains('\u{1b}'));
        assert!(value.is_char_boundary(value.len()));
    }

    #[test]
    fn device_actions_are_fixed_and_acknowledged() {
        let respring = action_command(IosDeviceAction::Respring);
        let reboot = action_command(IosDeviceAction::Reboot);
        assert!(respring.contains(ACTION_MARKER));
        assert!(respring.contains("SpringBoard"));
        assert!(!respring.contains("command -v"));
        assert!(reboot.contains(ACTION_MARKER));
        assert!(reboot.contains("reboot"));
        assert!(!reboot.contains("command -v"));
    }

    #[test]
    fn confirmation_ids_are_random_url_safe_tokens() {
        let first = new_confirmation_id().expect("first confirmation");
        let second = new_confirmation_id().expect("second confirmation");
        assert_ne!(first, second);
        assert!(validate_confirmation_id(&first).is_ok());
        assert!(validate_confirmation_id("short").is_err());
    }

    #[test]
    fn confirmation_ticket_is_consumed_exactly_once() {
        let state = AppState::default();
        let confirmation_id = "0123456789abcdef0123456789abcdef".to_string();
        state
            .ios_action_confirmations
            .lock()
            .expect("confirmation registry")
            .insert(
                confirmation_id.clone(),
                ManagedIosActionConfirmation {
                    confirmation_id: confirmation_id.clone(),
                    session_id: "ios-ssh-test-session".into(),
                    action: IosDeviceAction::Respring,
                    ssh_host: "192.0.2.10".into(),
                    ssh_port: 22,
                    username: "root".into(),
                    server_system: Some("Darwin test".into()),
                    host_key_identity: "[192.0.2.10]:22".into(),
                    expires_at: Instant::now() + Duration::from_secs(30),
                },
            );
        assert!(consume_action_confirmation(&state, &confirmation_id).is_ok());
        let replay = consume_action_confirmation(&state, &confirmation_id).expect_err("replay");
        assert_eq!(replay.code, "ios_action_confirmation_invalid");
    }

    #[test]
    fn expired_confirmation_ticket_is_consumed_and_rejected() {
        let state = AppState::default();
        let confirmation_id = "fedcba9876543210fedcba9876543210".to_string();
        state
            .ios_action_confirmations
            .lock()
            .expect("confirmation registry")
            .insert(
                confirmation_id.clone(),
                ManagedIosActionConfirmation {
                    confirmation_id: confirmation_id.clone(),
                    session_id: "ios-ssh-test-session".into(),
                    action: IosDeviceAction::Reboot,
                    ssh_host: "192.0.2.10".into(),
                    ssh_port: 22,
                    username: "root".into(),
                    server_system: Some("Darwin test".into()),
                    host_key_identity: "[192.0.2.10]:22".into(),
                    expires_at: Instant::now() - Duration::from_secs(1),
                },
            );
        let expired =
            consume_action_confirmation(&state, &confirmation_id).expect_err("expired ticket");
        assert_eq!(expired.code, "ios_action_confirmation_expired");
        assert!(consume_action_confirmation(&state, &confirmation_id).is_err());
    }

    #[test]
    fn target_snapshot_must_match_every_endpoint_field() {
        let target = IosDeviceActionTarget {
            ssh_host: "192.0.2.10".into(),
            ssh_port: 22,
            username: "root".into(),
            server_system: Some("Darwin test".into()),
            host_key_identity: "[192.0.2.10]:22".into(),
        };
        assert!(require_expected_target(
            &target,
            "192.0.2.10",
            22,
            "root",
            Some("Darwin test"),
            Some("[192.0.2.10]:22")
        )
        .is_ok());
        assert!(require_expected_target(
            &target,
            "192.0.2.11",
            22,
            "root",
            Some("Darwin test"),
            Some("[192.0.2.10]:22")
        )
        .is_err());
    }

    #[test]
    fn diagnostic_errors_are_sanitized_bounded_and_drop_details() {
        let error = ApiError::new(
            "remote_error",
            format!("bad\u{1b}[31m{}", "x".repeat(10_000)),
        )
        .with_details(serde_json::json!({ "stdout": "remote data" }));
        let sanitized = sanitize_diagnostic_error(error);
        assert!(sanitized.message.len() <= ERROR_TEXT_LIMIT);
        assert!(!sanitized.message.contains('\u{1b}'));
        assert!(sanitized.details.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn fixed_remote_scripts_are_valid_posix_shell() {
        let scripts = [
            overview_command().to_string(),
            process_command().to_string(),
            tools_command().to_string(),
            syslog_command(120),
            action_command(IosDeviceAction::Respring).to_string(),
            action_command(IosDeviceAction::Reboot).to_string(),
        ];
        for script in scripts {
            assert!(!script.contains("command -v"));
            let status = std::process::Command::new("/bin/sh")
                .args(["-n", "-c", &script])
                .status()
                .expect("parse fixed diagnostic script");
            assert!(status.success());
        }
    }

    #[test]
    fn log_source_marker_is_not_exposed_as_output() {
        let (source, output) = parse_log_source(
            "MOBIUS_IOS_DIAGNOSTIC\nMOBIUS_LOG_SOURCE|/var/log/syslog\nline one\nline two",
        );
        assert_eq!(source, "/var/log/syslog");
        assert_eq!(output, "line one\nline two");
    }
}
