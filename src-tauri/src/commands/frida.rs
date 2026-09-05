use super::{blocking_api, files::run_adb_shell, ports};
use crate::{
    models::{
        ApiError, ApiResult, FridaPortMapping, FridaServerResult, MobilePlatform, PortDirection,
        StartFridaRequest, UploadFridaRequest,
    },
    runner::{run_checked, ProcessOutput},
    state::{AppState, ManagedFridaProcess},
    validation,
};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::State;

const DEFAULT_REMOTE_PATH: &str = "/data/local/tmp/mobius-agentd";
const DEFAULT_LISTEN_ADDRESS: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 27_042;
const FRIDA_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_FRIDA_BYTES: u64 = 512 * 1024 * 1024;

#[tauri::command]
pub async fn upload_frida_server(
    request: UploadFridaRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<FridaServerResult>, ApiError> {
    let state = state.inner().clone();
    let registry = Arc::clone(&state.frida_processes);
    Ok(blocking_api(move || {
        require_android(request.platform)?;
        validation::serial(&request.serial)?;
        let registry_guard = registry
            .lock()
            .map_err(|_| ApiError::new("state_error", "Frida registry lock was poisoned"))?;
        if state.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                "app_shutting_down",
                "Mobius is exiting and cannot upload a managed server",
            ));
        }
        if registry_guard.contains_key(&request.serial) {
            return Err(ApiError::new(
                "frida_server_active",
                "Stop the Mobius-managed server on this device before replacing its binary",
            ));
        }
        let source = validate_frida_binary(&request.local_path)?;
        let remote_path = request
            .remote_path
            .unwrap_or_else(|| DEFAULT_REMOTE_PATH.to_string());
        validate_frida_remote_path(&remote_path)?;

        let upload_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let temporary_path = format!(
            "{remote_path}.mobius-upload-{}-{upload_suffix}",
            std::process::id()
        );
        let push = run_checked(
            "adb",
            &[
                "-s".into(),
                request.serial.clone(),
                "push".into(),
                source.to_string_lossy().into_owned(),
                temporary_path.clone(),
            ],
            Duration::from_secs(180),
        )?;
        let install_command = format!(
            "chmod 0755 -- {} && mv -f -- {} {}",
            validation::quote_remote(&temporary_path),
            validation::quote_remote(&temporary_path),
            validation::quote_remote(&remote_path)
        );
        let install = match run_adb_shell(&request.serial, &install_command, FRIDA_TIMEOUT) {
            Ok(output) => output,
            Err(error) => {
                let _ = run_adb_shell(
                    &request.serial,
                    &format!("rm -f -- {}", validation::quote_remote(&temporary_path)),
                    FRIDA_TIMEOUT,
                );
                return Err(error);
            }
        };
        let stdout = join_output(push.stdout, install.stdout);
        let stderr = join_output(push.stderr, install.stderr);
        Ok(FridaServerResult {
            success: true,
            message: format!("Instrumentation server uploaded as {remote_path}"),
            platform: MobilePlatform::Android,
            active: false,
            remote_path,
            pid: None,
            listen_address: None,
            device_port: None,
            host_port: None,
            mapping: None,
            mapping_active: None,
            stdout: (!stdout.is_empty()).then_some(stdout),
            stderr: (!stderr.is_empty()).then_some(stderr),
        })
    })
    .await)
}

#[tauri::command]
pub async fn start_frida_server(
    request: StartFridaRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<FridaServerResult>, ApiError> {
    let state = state.inner().clone();
    let registry = Arc::clone(&state.frida_processes);
    Ok(blocking_api(move || {
        require_android(request.platform)?;
        validation::serial(&request.serial)?;
        let (device_port, host_port) = resolve_ports(&request)?;
        let remote_path = request
            .remote_path
            .unwrap_or_else(|| DEFAULT_REMOTE_PATH.to_string());
        validate_frida_remote_path(&remote_path)?;
        let listen_address = request
            .listen_address
            .unwrap_or_else(|| DEFAULT_LISTEN_ADDRESS.to_string());
        if !matches!(listen_address.as_str(), "127.0.0.1" | "::1") {
            return Err(ApiError::new(
                "unsafe_frida_listen_address",
                "The instrumentation server may only listen on Android loopback; use the managed ADB forward for host access",
            ));
        }
        let mut processes = registry
            .lock()
            .map_err(|_| ApiError::new("state_error", "Frida registry lock was poisoned"))?;
        if state.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                "app_shutting_down",
                "Mobius is exiting and cannot launch a managed server",
            ));
        }
        if processes.contains_key(&request.serial) {
            return Err(ApiError::new(
                "frida_already_managed",
                "Mobius is already managing an instrumentation server for this device",
            ));
        }
        let use_su = requires_su(&request.serial)?;
        let canonical_output = run_frida_shell(
            &request.serial,
            &format!("readlink -f -- {}", validation::quote_remote(&remote_path)),
            use_su,
        )?;
        let canonical_path = canonical_output.stdout.trim().to_string();
        validate_frida_remote_path(&canonical_path)?;
        let listen = if listen_address == "::1" {
            format!("[::1]:{device_port}")
        } else {
            format!("127.0.0.1:{device_port}")
        };
        let quoted_path = validation::quote_remote(&canonical_path);
        let command = format!(
            "test -x {quoted_path} || exit 41; nohup {quoted_path} -l {} </dev/null >/dev/null 2>&1 & pid=$!; echo \"$pid\"",
            validation::quote_remote(&listen)
        );
        let output = run_frida_shell(&request.serial, &command, use_su)?;
        let pid = output
            .stdout
            .lines()
            .rev()
            .find_map(|line| line.trim().parse::<u32>().ok())
            .filter(|pid| *pid > 1)
            .ok_or_else(|| {
                ApiError::new(
                    "frida_start_failed",
                    "Device shell did not return a valid instrumentation server PID",
                )
                .with_details(serde_json::to_value(&output).unwrap_or_default())
            })?;

        thread::sleep(Duration::from_millis(180));
        let identity = query_process_identity(&request.serial, pid, use_su)?.ok_or_else(|| {
            ApiError::new(
                "frida_start_failed",
                "Instrumentation server exited immediately after launch",
            )
        })?;
        if identity.executable != canonical_path {
            let _ = run_frida_shell(&request.serial, &format!("kill -TERM {pid}"), use_su);
            return Err(ApiError::new(
                "frida_identity_mismatch",
                "Started process identity did not match the requested binary",
            ));
        }

        let forward_local_endpoint = format!("tcp:{host_port}");
        let forward_remote_endpoint = format!("tcp:{device_port}");
        let mut managed = ManagedFridaProcess {
            pid,
            remote_path: canonical_path.clone(),
            start_time: identity.start_time,
            use_su,
            listen_address: listen_address.clone(),
            device_port,
            host_port,
            forward_local_endpoint: forward_local_endpoint.clone(),
            forward_remote_endpoint: forward_remote_endpoint.clone(),
            forward_owned: false,
        };
        if let Err(error) = run_checked(
            "adb",
            &[
                "-s".into(),
                request.serial.clone(),
                "forward".into(),
                "--no-rebind".into(),
                forward_local_endpoint.clone(),
                forward_remote_endpoint.clone(),
            ],
            FRIDA_TIMEOUT,
        ) {
            if let Err(rollback_error) = terminate_verified_process(&request.serial, &managed) {
                processes.insert(request.serial.clone(), managed);
                return Err(ApiError::new(
                    "frida_rollback_incomplete",
                    format!(
                        "ADB forward failed ({}), and the started server could not be stopped ({}). It remains recorded for an explicit stop or exit cleanup.",
                        error.message, rollback_error.message
                    ),
                ));
            }
            return Err(ApiError::new(
                "frida_forward_failed",
                format!(
                    "Server started but the managed loopback ADB forward failed: {}",
                    error.message
                ),
            ));
        }
        managed.forward_owned = true;
        if let Err(error) = ports::remember_mapping(
            &state,
            &request.serial,
            PortDirection::Forward,
            &forward_local_endpoint,
            &forward_remote_endpoint,
            "frida",
        ) {
            let forward_cleanup = run_checked(
                "adb",
                &[
                    "-s".into(),
                    request.serial.clone(),
                    "forward".into(),
                    "--remove".into(),
                    forward_local_endpoint.clone(),
                ],
                FRIDA_TIMEOUT,
            );
            if forward_cleanup.is_ok() {
                managed.forward_owned = false;
            }
            let process_cleanup = terminate_verified_process(&request.serial, &managed);
            if forward_cleanup.is_err() || process_cleanup.is_err() {
                let cleanup_details = [
                    forward_cleanup.err().map(|value| value.message),
                    process_cleanup.err().map(|value| value.message),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
                processes.insert(request.serial.clone(), managed);
                return Err(ApiError::new(
                    "frida_rollback_incomplete",
                    format!(
                        "Managed-state registration failed ({}), and rollback was incomplete ({cleanup_details}). The resource remains recorded for an explicit stop or exit cleanup.",
                        error.message
                    ),
                ));
            }
            return Err(error);
        }
        processes.insert(request.serial.clone(), managed);
        Ok(server_result(
            true,
            format!(
                "Instrumentation server is running on device {listen}; host access is 127.0.0.1:{host_port}"
            ),
            ServerResource {
                remote_path: &canonical_path,
                pid: Some(pid),
                listen_address: Some(&listen_address),
                device_port: Some(device_port),
                host_port: Some(host_port),
                endpoints: Some((&forward_local_endpoint, &forward_remote_endpoint)),
                mapping_active: Some(true),
            },
            None,
            (!output.stderr.is_empty()).then_some(output.stderr),
        ))
    })
    .await)
}

#[tauri::command]
pub async fn stop_frida_server(
    serial: String,
    state: State<'_, AppState>,
) -> Result<ApiResult<FridaServerResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        validation::serial(&serial)?;
        let managed = {
            let processes = state
                .frida_processes
                .lock()
                .map_err(|_| ApiError::new("state_error", "Frida registry lock was poisoned"))?;
            processes.get(&serial).cloned().ok_or_else(|| {
                ApiError::new(
                    "frida_not_managed",
                    "No instrumentation server started by this Mobius session is recorded for the device",
                )
            })?
        };
        let result = stop_managed(&state, &serial, managed, false)?;
        let mut processes = state
            .frida_processes
            .lock()
            .map_err(|_| ApiError::new("state_error", "Frida registry lock was poisoned"))?;
        processes.remove(&serial);
        Ok(result)
    })
    .await)
}

pub(crate) fn cleanup_managed_frida(state: &AppState) {
    let processes = match state.frida_processes.lock() {
        Ok(registry) => registry.clone(),
        Err(_) => {
            eprintln!("Mobius cleanup: Frida registry lock was poisoned");
            return;
        }
    };
    for (serial, managed) in processes {
        match stop_managed(state, &serial, managed, true) {
            Ok(_) => match state.frida_processes.lock() {
                Ok(mut registry) => {
                    registry.remove(&serial);
                }
                Err(_) => eprintln!("Mobius cleanup: Frida registry lock was poisoned"),
            },
            Err(error) => {
                eprintln!(
                    "Mobius cleanup: instrumentation server on {serial} was not cleaned up and remains recorded: {}",
                    error.message
                );
            }
        }
    }
}

fn stop_managed(
    state: &AppState,
    serial: &str,
    managed: ManagedFridaProcess,
    exiting: bool,
) -> Result<FridaServerResult, ApiError> {
    let identity = query_process_identity(serial, managed.pid, managed.use_su)?;
    let identity_matches = identity.as_ref().is_some_and(|identity| {
        identity.executable == managed.remote_path && identity.start_time == managed.start_time
    });
    let message = if identity_matches {
        run_frida_shell(
            serial,
            &format!("kill -TERM {}", managed.pid),
            managed.use_su,
        )?;
        let mut stopped = false;
        for _ in 0..15 {
            thread::sleep(Duration::from_millis(100));
            let current = query_process_identity(serial, managed.pid, managed.use_su)?;
            if !current.is_some_and(|identity| {
                identity.executable == managed.remote_path
                    && identity.start_time == managed.start_time
            }) {
                stopped = true;
                break;
            }
        }
        if !stopped {
            run_frida_shell(
                serial,
                &format!("kill -KILL {}", managed.pid),
                managed.use_su,
            )?;
            thread::sleep(Duration::from_millis(100));
            if query_process_identity(serial, managed.pid, managed.use_su)?.is_some_and(
                |identity| {
                    identity.executable == managed.remote_path
                        && identity.start_time == managed.start_time
                },
            ) {
                return Err(ApiError::new(
                    "frida_stop_failed",
                    "Instrumentation server remained alive after SIGKILL",
                ));
            }
        }
        "Instrumentation server stopped".to_string()
    } else if identity.is_some() {
        "Recorded PID was reused by another process and was left untouched".to_string()
    } else {
        "Instrumentation server was already stopped".to_string()
    };
    let (mapping_active, mapping_warning) = if managed.forward_owned {
        remove_forward_if_unchanged(state, serial, &managed)
    } else {
        (Some(false), None)
    };
    if exiting && identity.is_some() && !identity_matches {
        eprintln!(
            "Mobius cleanup: PID {} on {} did not match its recorded identity and was left untouched",
            managed.pid, serial
        );
    }
    Ok(server_result(
        false,
        message,
        ServerResource {
            remote_path: &managed.remote_path,
            pid: None,
            listen_address: Some(&managed.listen_address),
            device_port: Some(managed.device_port),
            host_port: Some(managed.host_port),
            endpoints: managed.forward_owned.then_some((
                managed.forward_local_endpoint.as_str(),
                managed.forward_remote_endpoint.as_str(),
            )),
            mapping_active,
        },
        None,
        mapping_warning,
    ))
}

fn terminate_verified_process(serial: &str, managed: &ManagedFridaProcess) -> Result<(), ApiError> {
    let Some(identity) = query_process_identity(serial, managed.pid, managed.use_su)? else {
        return Ok(());
    };
    if identity.executable != managed.remote_path || identity.start_time != managed.start_time {
        return Err(ApiError::new(
            "frida_rollback_identity_mismatch",
            "Rollback refused to signal a PID whose identity no longer matches",
        ));
    }
    run_frida_shell(
        serial,
        &format!("kill -TERM {}", managed.pid),
        managed.use_su,
    )?;
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(100));
        if !query_process_identity(serial, managed.pid, managed.use_su)?.is_some_and(|current| {
            current.executable == managed.remote_path && current.start_time == managed.start_time
        }) {
            return Ok(());
        }
    }
    run_frida_shell(
        serial,
        &format!("kill -KILL {}", managed.pid),
        managed.use_su,
    )?;
    thread::sleep(Duration::from_millis(100));
    if query_process_identity(serial, managed.pid, managed.use_su)?.is_some_and(|current| {
        current.executable == managed.remote_path && current.start_time == managed.start_time
    }) {
        return Err(ApiError::new(
            "frida_rollback_failed",
            "Started server remained alive after rollback SIGKILL",
        ));
    }
    Ok(())
}

fn remove_forward_if_unchanged(
    state: &AppState,
    serial: &str,
    managed: &ManagedFridaProcess,
) -> (Option<bool>, Option<String>) {
    let listed = run_checked(
        "adb",
        &[
            "-s".into(),
            serial.into(),
            "forward".into(),
            "--list".into(),
        ],
        FRIDA_TIMEOUT,
    );
    let (mapping_active, warning, forget) = match listed {
        Ok(output) => {
            let matches = output.stdout.lines().any(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                matches!(fields.as_slice(), [found_serial, local, remote, ..]
                    if *found_serial == serial
                        && *local == managed.forward_local_endpoint
                        && *remote == managed.forward_remote_endpoint)
                    || matches!(fields.as_slice(), [local, remote]
                        if *local == managed.forward_local_endpoint
                            && *remote == managed.forward_remote_endpoint)
            });
            if matches {
                match run_checked(
                    "adb",
                    &[
                        "-s".into(),
                        serial.into(),
                        "forward".into(),
                        "--remove".into(),
                        managed.forward_local_endpoint.clone(),
                    ],
                    FRIDA_TIMEOUT,
                ) {
                    Ok(_) => (Some(false), None, true),
                    Err(error) => (
                        Some(true),
                        Some(format!("ADB forward cleanup failed: {}", error.message)),
                        false,
                    ),
                }
            } else {
                (
                    Some(false),
                    Some("ADB forward changed externally and was left untouched".into()),
                    true,
                )
            }
        }
        Err(error) => (
            None,
            Some(format!(
                "ADB forward could not be verified: {}",
                error.message
            )),
            false,
        ),
    };
    if forget {
        if let Err(error) = ports::forget_mapping(
            state,
            serial,
            PortDirection::Forward,
            &managed.forward_local_endpoint,
        ) {
            return (
                mapping_active,
                Some(format!("Port registry cleanup failed: {}", error.message)),
            );
        }
    }
    (mapping_active, warning)
}

struct ServerResource<'a> {
    remote_path: &'a str,
    pid: Option<u32>,
    listen_address: Option<&'a str>,
    device_port: Option<u16>,
    host_port: Option<u16>,
    endpoints: Option<(&'a str, &'a str)>,
    mapping_active: Option<bool>,
}

fn server_result(
    active: bool,
    message: String,
    resource: ServerResource<'_>,
    stdout: Option<String>,
    stderr: Option<String>,
) -> FridaServerResult {
    FridaServerResult {
        success: true,
        message,
        platform: MobilePlatform::Android,
        active,
        remote_path: resource.remote_path.into(),
        pid: resource.pid,
        listen_address: resource.listen_address.map(str::to_string),
        device_port: resource.device_port,
        host_port: resource.host_port,
        mapping: resource.endpoints.map(|(local, remote)| FridaPortMapping {
            direction: "forward".into(),
            local: local.into(),
            remote: remote.into(),
        }),
        mapping_active: resource.mapping_active,
        stdout,
        stderr,
    }
}

fn resolve_ports(request: &StartFridaRequest) -> Result<(u16, u16), ApiError> {
    let compatibility = request.port;
    let device_port = request
        .device_port
        .or(compatibility)
        .unwrap_or(DEFAULT_PORT);
    let host_port = request.host_port.or(compatibility).unwrap_or(device_port);
    if device_port == 0 || host_port == 0 {
        return Err(ApiError::new(
            "invalid_frida_port",
            "Device and host ports must each be between 1 and 65535",
        ));
    }
    Ok((device_port, host_port))
}

fn require_android(platform: Option<MobilePlatform>) -> Result<(), ApiError> {
    if platform == Some(MobilePlatform::Ios) {
        return Err(ApiError::new(
            "frida_ios_not_implemented",
            "Managed Frida server lifecycle currently supports Android only; Mobius will not pretend to manage a jailbroken iOS launch",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ProcessIdentity {
    executable: String,
    start_time: String,
}

fn query_process_identity(
    serial: &str,
    pid: u32,
    use_su: bool,
) -> Result<Option<ProcessIdentity>, ApiError> {
    let command = format!(
        "if [ -r /proc/{pid}/stat ]; then readlink -f /proc/{pid}/exe; cat /proc/{pid}/stat; fi"
    );
    let output = run_frida_shell(serial, &command, use_su)?;
    let mut lines = output.stdout.lines();
    let executable = match lines.next().map(str::trim).filter(|line| !line.is_empty()) {
        Some(value) => value.to_string(),
        None => return Ok(None),
    };
    let stat = lines
        .next()
        .ok_or_else(|| ApiError::new("frida_identity_error", "Missing /proc process metadata"))?;
    let after_name = stat
        .rsplit_once(") ")
        .map(|(_, rest)| rest)
        .ok_or_else(|| ApiError::new("frida_identity_error", "Malformed /proc process metadata"))?;
    let start_time = after_name
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| ApiError::new("frida_identity_error", "Missing process start time"))?
        .to_string();
    Ok(Some(ProcessIdentity {
        executable,
        start_time,
    }))
}

fn requires_su(serial: &str) -> Result<bool, ApiError> {
    let uid = run_adb_shell(serial, "id -u", FRIDA_TIMEOUT)?;
    if uid.stdout.trim() == "0" {
        return Ok(false);
    }
    run_adb_shell(serial, "command -v su >/dev/null 2>&1", FRIDA_TIMEOUT).map_err(|_| {
        ApiError::new(
            "frida_root_required",
            "Instrumentation server requires a root adb shell or an available su command",
        )
    })?;
    Ok(true)
}

fn run_frida_shell(serial: &str, command: &str, use_su: bool) -> Result<ProcessOutput, ApiError> {
    if use_su {
        let privileged = format!("su -c {}", validation::quote_remote(command));
        run_adb_shell(serial, &privileged, FRIDA_TIMEOUT)
    } else {
        run_adb_shell(serial, command, FRIDA_TIMEOUT)
    }
}

fn validate_frida_remote_path(value: &str) -> Result<&str, ApiError> {
    let path = validation::remote_path(value)?;
    let relative = path
        .strip_prefix("/data/local/tmp/")
        .filter(|relative| !relative.is_empty() && !relative.contains('/'))
        .ok_or_else(|| {
            ApiError::new(
                "invalid_frida_remote_path",
                "Server path must be a direct child of /data/local/tmp",
            )
        })?;
    let lowercase = relative.to_ascii_lowercase();
    if !lowercase.starts_with("mobius-") || lowercase.contains("frida") {
        return Err(ApiError::new(
            "invalid_frida_remote_path",
            "Remote server name must start with 'mobius-' and must not contain the upstream tool name",
        ));
    }
    Ok(path)
}

fn validate_frida_binary(value: &str) -> Result<std::path::PathBuf, ApiError> {
    let path = validation::local_existing_path(value)?;
    let metadata = path.metadata().map_err(|error| {
        ApiError::new(
            "invalid_frida_binary",
            format!("Unable to inspect server binary: {error}"),
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FRIDA_BYTES {
        return Err(ApiError::new(
            "invalid_frida_binary",
            "Server must be a non-empty regular file no larger than 512 MiB",
        ));
    }
    let file_name = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !file_name.starts_with("frida-server") {
        return Err(ApiError::new(
            "invalid_frida_binary",
            "Selected local file name must begin with 'frida-server'",
        ));
    }
    path.canonicalize().map_err(|error| {
        ApiError::new(
            "invalid_frida_binary",
            format!("Unable to resolve server binary path: {error}"),
        )
    })
}

fn join_output(left: String, right: String) -> String {
    [left, right]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_remote_alias_is_enforced() {
        assert!(validate_frida_remote_path("/data/local/tmp/mobius-agentd").is_ok());
        assert!(validate_frida_remote_path("/data/local/tmp/frida-server").is_err());
        assert!(validate_frida_remote_path("/sdcard/mobius-agentd").is_err());
    }

    #[test]
    fn compatibility_port_maps_both_sides() {
        let request = StartFridaRequest {
            serial: "device".into(),
            platform: None,
            remote_path: None,
            listen_address: None,
            port: Some(27_043),
            device_port: None,
            host_port: None,
        };
        assert_eq!(resolve_ports(&request).unwrap(), (27_043, 27_043));
    }
}
