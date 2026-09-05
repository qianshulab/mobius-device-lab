use super::{blocking_api, ios_ssh};
use crate::{
    models::{
        ApiError, ApiResult, AppResult, IosFridaServerResult, StartIosFridaServerRequest,
        StopIosFridaServerRequest, UploadIosFridaServerRequest,
    },
    runner::{background_command, resolve_tool},
    state::{
        AppState, IosSshConnection, ManagedIosFridaProcess, ManagedIosFridaUpload,
        ManagedIosSshSession,
    },
    validation,
};
use std::{
    fs::{self, File},
    io::Read,
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Stdio},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::State;

const DEFAULT_FRIDA_PORT: u16 = 27_042;
const MAX_SERVER_BYTES: u64 = 512 * 1024 * 1024;
const REMOTE_TIMEOUT: Duration = Duration::from_secs(20);
const COPY_TIMEOUT: Duration = Duration::from_secs(180);
const TUNNEL_START_TIMEOUT: Duration = Duration::from_secs(6);
const PROCESS_STOP_ATTEMPTS: usize = 15;
const RUNTIME_DIRECTORY_NAME: &str = ".mobius-runtime";
const UPLOAD_MARKER: &str = "MOBIUS_IOS_SERVICE_READY";
const PID_MARKER: &str = "MOBIUS_IOS_SERVICE_PID:";
const PROCESS_MARKER: &str = "MOBIUS_IOS_SERVICE_COMMAND:";

#[tauri::command]
pub async fn upload_ios_frida_server(
    request: UploadIosFridaServerRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosFridaServerResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let _lifecycle = lifecycle_lock(&state)?;
        require_running(&state)?;
        let source = validate_macho_server(&request.local_path)?;
        let (connection, _) = ios_ssh::session_snapshot(&state, &request.session_id)?;

        let previous_upload = {
            let registry = ssh_registry(&state)?;
            let session = registry.get(&request.session_id).ok_or_else(|| {
                ApiError::new(
                    "ios_ssh_session_not_found",
                    "The iOS SSH session is not active",
                )
            })?;
            if session.ios_frida_process.is_some() {
                return Err(ApiError::new(
                    "ios_frida_server_active",
                    "Stop the Mobius-managed iOS server before replacing its binary",
                ));
            }
            session.ios_frida_upload.clone()
        };

        let paths = create_remote_paths(&connection)?;
        prepare_runtime_directory(&connection, &paths.runtime_directory)?;

        let mut args = ios_ssh::scp_base_args(&connection);
        args.push(source.to_string_lossy().into_owned());
        args.push(ios_ssh::scp_remote_spec(
            &connection,
            &paths.temporary_path,
        )?);
        if let Err(error) = ios_ssh::run_connection_tool(&connection, "scp", &args, COPY_TIMEOUT) {
            let _ = remove_remote_paths(&connection, &[paths.temporary_path.as_str()]);
            return Err(error);
        }

        let temporary_name = remote_file_name(&paths.temporary_path)?;
        let installed_name = remote_file_name(&paths.installed_path)?;
        let command = format!(
            "cd {directory} && [ \"$(pwd -P)\" = {directory} ] && \
             [ -f {temporary} ] && [ ! -L {temporary} ] && \
             chmod 700 {temporary} && mv -f {temporary} {installed} && \
             printf '{UPLOAD_MARKER}\\n'",
            directory = validation::quote_remote(&paths.runtime_directory),
            temporary = validation::quote_remote(&format!("./{temporary_name}")),
            installed = validation::quote_remote(&format!("./{installed_name}")),
        );
        let install = ios_ssh::run_ssh_command(&connection, &command, REMOTE_TIMEOUT);
        match install {
            Ok(output)
                if output
                    .stdout
                    .lines()
                    .any(|line| line.trim() == UPLOAD_MARKER) => {}
            Ok(_) => {
                let _ = remove_remote_paths(
                    &connection,
                    &[paths.temporary_path.as_str(), paths.installed_path.as_str()],
                );
                return Err(ApiError::new(
                    "ios_frida_upload_validation_failed",
                    "The device did not confirm the managed server installation",
                ));
            }
            Err(error) => {
                let _ = remove_remote_paths(
                    &connection,
                    &[paths.temporary_path.as_str(), paths.installed_path.as_str()],
                );
                return Err(error);
            }
        }

        {
            let mut registry = ssh_registry(&state)?;
            let session = registry.get_mut(&request.session_id).ok_or_else(|| {
                ApiError::new(
                    "ios_ssh_session_not_found",
                    "The iOS SSH session ended during the upload",
                )
            })?;
            session.ios_frida_upload = Some(ManagedIosFridaUpload {
                remote_path: paths.installed_path.clone(),
            });
        }

        if let Some(previous) = previous_upload {
            if previous.remote_path != paths.installed_path {
                let _ = remove_remote_paths(&connection, &[previous.remote_path.as_str()]);
            }
        }

        Ok(IosFridaServerResult {
            success: true,
            message: "iOS instrumentation server uploaded with a neutral managed name".into(),
            session_id: request.session_id,
            active: false,
            remote_path: paths.installed_path,
            pid: None,
            listen_address: None,
            device_port: None,
            host_port: None,
            tunnel_pid: None,
            tunnel_active: Some(false),
        })
    })
    .await)
}

#[tauri::command]
pub async fn start_ios_frida_server(
    request: StartIosFridaServerRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosFridaServerResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let _lifecycle = lifecycle_lock(&state)?;
        require_running(&state)?;
        let (connection, _) = ios_ssh::session_snapshot(&state, &request.session_id)?;
        if connection.remote_uid != Some(0) {
            return Err(ApiError::new(
                "ios_frida_root_required",
                "Starting the iOS instrumentation server requires a verified uid=0 SSH session",
            ));
        }

        let remote_path = {
            let registry = ssh_registry(&state)?;
            let session = registry.get(&request.session_id).ok_or_else(|| {
                ApiError::new(
                    "ios_ssh_session_not_found",
                    "The iOS SSH session is not active",
                )
            })?;
            if session.ios_frida_process.is_some() {
                return Err(ApiError::new(
                    "ios_frida_server_active",
                    "A Mobius-managed server is already active for this SSH session",
                ));
            }
            session
                .ios_frida_upload
                .as_ref()
                .map(|upload| upload.remote_path.clone())
                .ok_or_else(|| {
                    ApiError::new(
                        "ios_frida_server_not_uploaded",
                        "Upload a server binary through this SSH session before starting it",
                    )
                })?
        };
        validate_managed_remote_path(&connection, &remote_path)?;

        let device_port = valid_port(request.device_port.unwrap_or(DEFAULT_FRIDA_PORT), "device")?;
        let host_port = reserve_loopback_port(request.host_port)?;
        let log_path = format!("{remote_path}.log");
        validate_managed_remote_path(&connection, &log_path)?;

        let command = launch_command(&remote_path, &log_path, device_port);
        let output = ios_ssh::run_ssh_command(&connection, &command, REMOTE_TIMEOUT)?;
        let pid = parse_started_pid(&output.stdout)?;
        let mut managed = ManagedIosFridaProcess {
            pid,
            remote_path: remote_path.clone(),
            log_path,
            device_port,
            host_port,
            tunnel: None,
        };
        if !remote_process_matches(&connection, pid, &remote_path)? {
            let mut registry = ssh_registry(&state)?;
            if let Some(session) = registry.get_mut(&request.session_id) {
                session.ios_frida_process = Some(managed);
            }
            return Err(ApiError::new(
                "ios_frida_identity_mismatch",
                "The returned PID did not match the uploaded managed binary; no signal was sent and the resource remains recorded for safe cleanup",
            ));
        }
        let mut tunnel = match spawn_local_forward(&connection, host_port, device_port) {
            Ok(child) => child,
            Err(error) => {
                return Err(rollback_start_or_record(
                    &state,
                    &request.session_id,
                    &connection,
                    managed,
                    error,
                ));
            }
        };
        if let Err(error) = wait_for_local_forward(&mut tunnel, host_port) {
            ios_ssh::stop_child(&mut tunnel);
            return Err(rollback_start_or_record(
                &state,
                &request.session_id,
                &connection,
                managed,
                error,
            ));
        }
        let tunnel_pid = tunnel.id();
        managed.tunnel = Some(tunnel);

        {
            let mut registry = ssh_registry(&state)?;
            let session = registry.get_mut(&request.session_id).ok_or_else(|| {
                ApiError::new(
                    "ios_ssh_session_not_found",
                    "The iOS SSH session ended while the server was starting",
                )
            })?;
            if session.ios_frida_process.is_some() {
                drop(registry);
                let error = ApiError::new(
                    "ios_frida_server_active",
                    "A managed server was registered concurrently for this SSH session",
                );
                return Err(rollback_start_or_record(
                    &state,
                    &request.session_id,
                    &connection,
                    managed,
                    error,
                ));
            }
            session.ios_frida_process = Some(managed);
        }

        Ok(IosFridaServerResult {
            success: true,
            message: format!("iOS instrumentation server is available at 127.0.0.1:{host_port}"),
            session_id: request.session_id,
            active: true,
            remote_path,
            pid: Some(pid),
            listen_address: Some("127.0.0.1".into()),
            device_port: Some(device_port),
            host_port: Some(host_port),
            tunnel_pid: Some(tunnel_pid),
            tunnel_active: Some(true),
        })
    })
    .await)
}

#[tauri::command]
pub async fn stop_ios_frida_server(
    request: StopIosFridaServerRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosFridaServerResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let _lifecycle = lifecycle_lock(&state)?;
        let (connection, _) = ios_ssh::session_snapshot(&state, &request.session_id)?;
        let (process, upload) = {
            let mut registry = ssh_registry(&state)?;
            let session = registry.get_mut(&request.session_id).ok_or_else(|| {
                ApiError::new(
                    "ios_ssh_session_not_found",
                    "The iOS SSH session is not active",
                )
            })?;
            (
                session.ios_frida_process.take(),
                session.ios_frida_upload.clone(),
            )
        };

        let remembered_path = process
            .as_ref()
            .map(|managed| managed.remote_path.clone())
            .or_else(|| upload.as_ref().map(|item| item.remote_path.clone()))
            .ok_or_else(|| {
                ApiError::new(
                    "ios_frida_not_managed",
                    "No iOS server uploaded by this Mobius SSH session is recorded",
                )
            })?;

        let (pid, device_port, host_port, tunnel_pid) = if let Some(mut managed) = process {
            let tunnel_pid = managed.tunnel.as_ref().map(Child::id);
            if let Some(mut tunnel) = managed.tunnel.take() {
                ios_ssh::stop_child(&mut tunnel);
            }
            if let Err(error) = terminate_remote_process(&connection, &managed) {
                let mut registry = ssh_registry(&state)?;
                if let Some(session) = registry.get_mut(&request.session_id) {
                    session.ios_frida_process = Some(managed);
                }
                return Err(error);
            }
            remove_remote_paths(
                &connection,
                &[managed.remote_path.as_str(), managed.log_path.as_str()],
            )?;
            (
                Some(managed.pid),
                Some(managed.device_port),
                Some(managed.host_port),
                tunnel_pid,
            )
        } else {
            remove_remote_paths(&connection, &[remembered_path.as_str()])?;
            (None, None, None, None)
        };

        {
            let mut registry = ssh_registry(&state)?;
            if let Some(session) = registry.get_mut(&request.session_id) {
                if session
                    .ios_frida_upload
                    .as_ref()
                    .is_some_and(|item| item.remote_path == remembered_path)
                {
                    session.ios_frida_upload = None;
                }
            }
        }

        Ok(IosFridaServerResult {
            success: true,
            message: "Mobius-managed iOS server, binary, and local tunnel were stopped and removed"
                .into(),
            session_id: request.session_id,
            active: false,
            remote_path: remembered_path,
            pid,
            listen_address: Some("127.0.0.1".into()),
            device_port,
            host_port,
            tunnel_pid,
            tunnel_active: Some(false),
        })
    })
    .await)
}

struct RemotePaths {
    runtime_directory: String,
    temporary_path: String,
    installed_path: String,
}

fn lifecycle_lock(state: &AppState) -> AppResult<std::sync::MutexGuard<'_, ()>> {
    state
        .ios_frida_lock
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS server lifecycle lock was poisoned"))
}

fn ssh_registry(
    state: &AppState,
) -> AppResult<std::sync::MutexGuard<'_, std::collections::HashMap<String, ManagedIosSshSession>>> {
    state
        .ios_ssh_sessions
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS SSH session lock was poisoned"))
}

fn require_running(state: &AppState) -> AppResult<()> {
    if state.shutting_down.load(Ordering::Acquire) {
        Err(ApiError::new(
            "app_shutting_down",
            "Mobius is exiting and cannot change a managed iOS server",
        ))
    } else {
        Ok(())
    }
}

fn create_remote_paths(connection: &IosSshConnection) -> AppResult<RemotePaths> {
    let runtime_directory = managed_runtime_directory(connection).ok_or_else(|| {
        ApiError::new(
            "invalid_allowed_roots",
            "The SSH session has no allowed root for its managed runtime directory",
        )
    })?;
    validate_managed_remote_path(connection, &runtime_directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let suffix = format!("{:x}-{nonce:x}", std::process::id());
    let temporary_path = format!("{runtime_directory}/.upload-{suffix}");
    let installed_path = format!("{runtime_directory}/.service-{suffix}");
    validate_managed_remote_path(connection, &temporary_path)?;
    validate_managed_remote_path(connection, &installed_path)?;
    Ok(RemotePaths {
        runtime_directory,
        temporary_path,
        installed_path,
    })
}

fn managed_runtime_directory(connection: &IosSshConnection) -> Option<String> {
    connection
        .allowed_roots
        .first()
        .map(|root| format!("{}/{RUNTIME_DIRECTORY_NAME}", root.trim_end_matches('/')))
}

fn validate_managed_remote_path(connection: &IosSshConnection, value: &str) -> AppResult<()> {
    validation::remote_path(value)?;
    if value.to_ascii_lowercase().contains("frida") {
        return Err(ApiError::new(
            "unsafe_ios_server_path",
            "The managed remote path must not contain the upstream tool name",
        ));
    }
    let runtime = managed_runtime_directory(connection).ok_or_else(|| {
        ApiError::new(
            "invalid_allowed_roots",
            "The SSH session has no allowed root for its managed runtime directory",
        )
    })?;
    if value != runtime
        && !value
            .strip_prefix(&runtime)
            .is_some_and(|suffix| suffix.starts_with('/'))
    {
        return Err(ApiError::new(
            "unsafe_ios_server_path",
            "The managed remote path is outside Mobius's private runtime directory",
        ));
    }
    Ok(())
}

fn prepare_runtime_directory(connection: &IosSshConnection, directory: &str) -> AppResult<()> {
    validate_managed_remote_path(connection, directory)?;
    let quoted = validation::quote_remote(directory);
    let command = format!(
        "if [ -L {quoted} ]; then exit 74; fi; \
         mkdir -p {quoted} && chmod 700 {quoted} && cd {quoted} && \
         [ \"$(pwd -P)\" = {quoted} ]"
    );
    ios_ssh::run_ssh_command(connection, &command, REMOTE_TIMEOUT).map(|_| ())
}

fn validate_macho_server(value: &str) -> AppResult<PathBuf> {
    let path = validation::local_absolute_path(value)?;
    let link_metadata = fs::symlink_metadata(path).map_err(|error| {
        ApiError::new(
            "invalid_ios_frida_binary",
            format!("Unable to inspect the selected server binary: {error}"),
        )
    })?;
    if link_metadata.file_type().is_symlink() || !link_metadata.is_file() {
        return Err(ApiError::new(
            "invalid_ios_frida_binary",
            "The selected iOS server must be a regular local file, not a symbolic link",
        ));
    }
    if link_metadata.len() < 4 || link_metadata.len() > MAX_SERVER_BYTES {
        return Err(ApiError::new(
            "invalid_ios_frida_binary",
            "The selected iOS server must be between 4 bytes and 512 MiB",
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        ApiError::new(
            "invalid_ios_frida_binary",
            format!("Unable to resolve the selected server binary: {error}"),
        )
    })?;
    let mut file = File::open(&canonical).map_err(|error| {
        ApiError::new(
            "invalid_ios_frida_binary",
            format!("Unable to read the selected server binary: {error}"),
        )
    })?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic).map_err(|error| {
        ApiError::new(
            "invalid_ios_frida_binary",
            format!("Unable to read the Mach-O header: {error}"),
        )
    })?;
    if !is_macho_magic(magic) {
        return Err(ApiError::new(
            "invalid_ios_frida_binary",
            "The selected file is not an uncompressed thin or universal Mach-O binary",
        ));
    }
    Ok(canonical)
}

fn is_macho_magic(magic: [u8; 4]) -> bool {
    matches!(
        magic,
        [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xce]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbf, 0xba, 0xfe, 0xca]
    )
}

fn remote_file_name(path: &str) -> AppResult<&str> {
    path.rsplit_once('/')
        .map(|(_, name)| name)
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-'))
        })
        .ok_or_else(|| {
            ApiError::new(
                "unsafe_ios_server_path",
                "The generated managed filename is invalid",
            )
        })
}

fn valid_port(port: u16, label: &str) -> AppResult<u16> {
    if port == 0 {
        Err(ApiError::new(
            "invalid_ios_frida_port",
            format!("The {label} port must be between 1 and 65535"),
        ))
    } else {
        Ok(port)
    }
}

fn reserve_loopback_port(requested: Option<u16>) -> AppResult<u16> {
    if requested == Some(0) {
        return Err(ApiError::new(
            "invalid_ios_frida_port",
            "The host port must be between 1 and 65535",
        ));
    }
    let port = requested.unwrap_or(0);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|error| {
        ApiError::new(
            "ios_frida_host_port_unavailable",
            if port == 0 {
                format!("Unable to allocate a loopback port: {error}")
            } else {
                format!("Loopback port {port} is unavailable: {error}")
            },
        )
    })?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| {
            ApiError::new(
                "ios_frida_host_port_error",
                format!("Unable to inspect the allocated loopback port: {error}"),
            )
        })
}

fn launch_command(remote_path: &str, log_path: &str, device_port: u16) -> String {
    let directory = remote_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("/");
    let binary = validation::quote_remote(remote_path);
    let log = validation::quote_remote(log_path);
    let directory = validation::quote_remote(directory);
    format!(
        "if [ -L {directory} ] || ! cd {directory} || [ \"$(pwd -P)\" != {directory} ]; then exit 71; fi; \
         if [ -L {binary} ] || [ ! -f {binary} ]; then exit 72; fi; \
         chmod 700 {binary} && rm -f {log}; \
         nohup {binary} -l 127.0.0.1:{device_port} >{log} 2>&1 </dev/null & \
         mobius_pid=$!; sleep 1; \
         if kill -0 \"$mobius_pid\" 2>/dev/null; then \
           printf '{PID_MARKER}%s\\n' \"$mobius_pid\"; \
         else cat {log} 2>/dev/null; exit 75; fi"
    )
}

fn parse_started_pid(stdout: &str) -> AppResult<u32> {
    stdout
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(PID_MARKER))
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid > 1)
        .ok_or_else(|| {
            ApiError::new(
                "ios_frida_invalid_pid",
                "The device did not return a valid managed server PID",
            )
        })
}

fn remote_process_matches(
    connection: &IosSshConnection,
    pid: u32,
    remote_path: &str,
) -> AppResult<bool> {
    let command = format!(
        "mobius_command=$(ps -p {pid} -o command= 2>/dev/null || true); \
         printf '{PROCESS_MARKER}%s\\n' \"$mobius_command\""
    );
    let output = ios_ssh::run_ssh_command(connection, &command, REMOTE_TIMEOUT)?;
    let actual = output
        .stdout
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(PROCESS_MARKER))
        .unwrap_or_default();
    Ok(actual == remote_path
        || actual
            .strip_prefix(remote_path)
            .is_some_and(|suffix| suffix.starts_with(char::is_whitespace)))
}

fn signal_remote_process(connection: &IosSshConnection, pid: u32, signal: &str) -> AppResult<()> {
    let signal = match signal {
        "TERM" => "TERM",
        "KILL" => "KILL",
        _ => {
            return Err(ApiError::new(
                "invalid_signal",
                "Unsupported managed process signal",
            ));
        }
    };
    ios_ssh::run_ssh_command(connection, &format!("kill -{signal} {pid}"), REMOTE_TIMEOUT)
        .map(|_| ())
}

fn terminate_remote_process(
    connection: &IosSshConnection,
    managed: &ManagedIosFridaProcess,
) -> AppResult<()> {
    if !remote_process_matches(connection, managed.pid, &managed.remote_path)? {
        return Ok(());
    }
    signal_remote_process(connection, managed.pid, "TERM")?;
    for _ in 0..PROCESS_STOP_ATTEMPTS {
        thread::sleep(Duration::from_millis(100));
        if !remote_process_matches(connection, managed.pid, &managed.remote_path)? {
            return Ok(());
        }
    }
    signal_remote_process(connection, managed.pid, "KILL")?;
    thread::sleep(Duration::from_millis(100));
    if remote_process_matches(connection, managed.pid, &managed.remote_path)? {
        return Err(ApiError::new(
            "ios_frida_stop_failed",
            "The managed iOS server remained alive after SIGKILL",
        ));
    }
    Ok(())
}

fn spawn_local_forward(
    connection: &IosSshConnection,
    host_port: u16,
    device_port: u16,
) -> AppResult<Child> {
    let executable = resolve_tool("ssh")?;
    let mut args = ios_ssh::ssh_base_args(connection);
    args.extend([
        "-N".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "GatewayPorts=no".into(),
        "-L".into(),
        format!("127.0.0.1:{host_port}:127.0.0.1:{device_port}"),
        ios_ssh::ssh_target(connection),
    ]);
    let mut command = background_command(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ios_ssh::apply_ssh_auth_environment(&mut command, connection)?;
    command.spawn().map_err(|error| {
        ApiError::new(
            "ios_frida_tunnel_spawn_failed",
            format!("Unable to start the managed SSH local forward: {error}"),
        )
    })
}

fn wait_for_local_forward(child: &mut Child, host_port: u16) -> AppResult<()> {
    let started = Instant::now();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, host_port));
    while started.elapsed() < TUNNEL_START_TIMEOUT {
        if let Some(status) = child.try_wait().map_err(|error| {
            ApiError::new(
                "ios_frida_tunnel_state_error",
                format!("Unable to inspect the managed SSH forward: {error}"),
            )
        })? {
            return Err(ApiError::new(
                "ios_frida_tunnel_start_failed",
                format!("The managed SSH forward exited before it was ready: {status}"),
            ));
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(ApiError::new(
        "ios_frida_tunnel_start_timeout",
        "The managed loopback SSH forward did not become ready within 6 seconds",
    ))
}

fn rollback_start_or_record(
    state: &AppState,
    session_id: &str,
    connection: &IosSshConnection,
    managed: ManagedIosFridaProcess,
    primary: ApiError,
) -> ApiError {
    match terminate_remote_process(connection, &managed) {
        Ok(()) => primary,
        Err(rollback) => {
            if let Ok(mut registry) = ssh_registry(state) {
                if let Some(session) = registry.get_mut(session_id) {
                    session.ios_frida_process = Some(managed);
                }
            }
            ApiError::new(
                "ios_frida_rollback_incomplete",
                format!(
                    "Managed SSH forward failed ({}), and the started server could not be stopped ({}). It remains recorded for explicit cleanup.",
                    primary.message, rollback.message
                ),
            )
        }
    }
}

fn remove_remote_paths(connection: &IosSshConnection, paths: &[&str]) -> AppResult<()> {
    let runtime_directory = managed_runtime_directory(connection).ok_or_else(|| {
        ApiError::new(
            "invalid_allowed_roots",
            "The SSH session has no allowed root for its managed runtime directory",
        )
    })?;
    validate_managed_remote_path(connection, &runtime_directory)?;
    let mut names = Vec::with_capacity(paths.len());
    for path in paths {
        validate_managed_remote_path(connection, path)?;
        let (parent, _) = path.rsplit_once('/').ok_or_else(|| {
            ApiError::new(
                "unsafe_ios_server_path",
                "The managed remote path has no parent directory",
            )
        })?;
        if parent != runtime_directory {
            return Err(ApiError::new(
                "unsafe_ios_server_path",
                "Cleanup is limited to direct children of Mobius's private runtime directory",
            ));
        }
        names.push(remote_file_name(path)?);
    }
    let targets = names
        .iter()
        .map(|name| validation::quote_remote(&format!("./{name}")))
        .collect::<Vec<_>>()
        .join(" ");
    let directory = validation::quote_remote(&runtime_directory);
    ios_ssh::run_ssh_command(
        connection,
        &format!(
            "if [ ! -e {directory} ]; then exit 0; fi; \
             if [ -L {directory} ] || ! cd {directory} || [ \"$(pwd -P)\" != {directory} ]; then exit 74; fi; \
             rm -f {targets}; cd /; rmdir {directory} 2>/dev/null || true"
        ),
        REMOTE_TIMEOUT,
    )
    .map(|_| ())
}

/// Best-effort teardown used before the containing SSH session and its USB tunnel disappear.
pub(crate) fn cleanup_ios_frida_for_session(session: &mut ManagedIosSshSession) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(mut managed) = session.ios_frida_process.take() {
        if let Some(mut tunnel) = managed.tunnel.take() {
            ios_ssh::stop_child(&mut tunnel);
        }
        if let Err(error) = terminate_remote_process(&session.connection, &managed) {
            warnings.push(format!("managed process cleanup failed: {}", error.message));
        } else if let Err(error) = remove_remote_paths(
            &session.connection,
            &[managed.remote_path.as_str(), managed.log_path.as_str()],
        ) {
            warnings.push(format!("managed binary cleanup failed: {}", error.message));
        }
        session.ios_frida_upload = None;
    } else if let Some(upload) = session.ios_frida_upload.take() {
        if let Err(error) = remove_remote_paths(&session.connection, &[upload.remote_path.as_str()])
        {
            warnings.push(format!("uploaded binary cleanup failed: {}", error.message));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(root: &str) -> IosSshConnection {
        IosSshConnection {
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
            device_port: Some(22),
            username: "root".into(),
            authentication: crate::state::IosSshAuthentication::PrivateKey(PathBuf::from(
                "/tmp/key",
            )),
            known_hosts_path: PathBuf::from("/tmp/known_hosts"),
            host_key_alias: None,
            configured_roots: vec![root.into()],
            allowed_roots: vec![root.into()],
            server_system: Some("Darwin".into()),
            remote_uid: Some(0),
        }
    }

    #[test]
    fn accepts_thin_and_universal_macho_magics() {
        assert!(is_macho_magic([0xcf, 0xfa, 0xed, 0xfe]));
        assert!(is_macho_magic([0xca, 0xfe, 0xba, 0xbe]));
        assert!(is_macho_magic([0xbf, 0xba, 0xfe, 0xca]));
        assert!(!is_macho_magic([0x7f, b'E', b'L', b'F']));
        assert!(!is_macho_magic([0xfd, b'7', b'z', b'X']));
    }

    #[test]
    fn generated_paths_are_neutral_and_confined() {
        let connection = connection("/var/mobile");
        let paths = create_remote_paths(&connection).expect("paths");
        assert!(paths
            .installed_path
            .starts_with("/var/mobile/.mobius-runtime/.service-"));
        assert!(!paths.installed_path.to_ascii_lowercase().contains("frida"));
        assert!(validate_managed_remote_path(&connection, &paths.installed_path).is_ok());
        assert!(validate_managed_remote_path(&connection, "/var/mobile/elsewhere/tool").is_err());
    }

    #[test]
    fn rejects_an_allowed_root_that_would_leak_the_upstream_name() {
        let connection = connection("/var/mobile/frida-files");
        assert!(create_remote_paths(&connection).is_err());
    }

    #[test]
    fn parses_only_a_safe_pid_marker() {
        assert_eq!(
            parse_started_pid("MOBIUS_IOS_SERVICE_PID:42\n").unwrap(),
            42
        );
        assert!(parse_started_pid("MOBIUS_IOS_SERVICE_PID:1\n").is_err());
        assert!(parse_started_pid("42\n").is_err());
    }

    #[test]
    fn launch_is_fixed_to_device_loopback() {
        let command = launch_command(
            "/var/mobile/.mobius-runtime/.service-a",
            "/var/mobile/.mobius-runtime/.service-a.log",
            31337,
        );
        assert!(command.contains("-l 127.0.0.1:31337"));
        assert!(!command.contains("0.0.0.0"));
    }

    #[test]
    fn explicit_zero_host_port_is_rejected() {
        assert!(reserve_loopback_port(Some(0)).is_err());
    }
}
