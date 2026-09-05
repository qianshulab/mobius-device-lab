use super::blocking_api;
use crate::{
    models::{
        ApiError, ApiResult, AppResult, DeleteIosSshRequest, DownloadIosSshFileRequest,
        IosSshAuthMode, IosSshConnectionResult, IosSshPathRequest, IosSshSession, IosSshTransport,
        IosSshTunnelStatus, OperationResult, RemoteFileEntry, StartIosSshSessionRequest,
        UploadIosSshFileRequest,
    },
    runner::{background_command, resolve_tool, run_checked, run_checked_with_env, ProcessOutput},
    state::{AppState, IosSshAuthentication, IosSshConnection, ManagedIosSshSession},
    validation,
};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, State};
use zeroize::Zeroize;

const DEFAULT_SSH_PORT: u16 = 22;
const SSH_TIMEOUT: Duration = Duration::from_secs(15);
const FILE_TIMEOUT: Duration = Duration::from_secs(180);
const TUNNEL_START_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ALLOWED_ROOTS: usize = 32;
const MAX_SESSIONS: usize = 16;
const PROBE_MARKER: &str = "MOBIUS_SSH_READY";
const PATH_MARKER: &str = "MOBIUS_PATH_READY";
const TYPE_MARKER: &str = "MOBIUS_TYPE";
const LIST_MARKER: &str = "MOBIUS_LIST_BEGIN";
const ASKPASS_MARKER_ENV: &str = "MOBIUS_SSH_ASKPASS";
const ASKPASS_PASSWORD_ENV: &str = "MOBIUS_SSH_PASSWORD";

#[derive(Debug)]
struct ProbeResult {
    server_system: Option<String>,
    remote_uid: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteEntryKind {
    Missing,
    File,
    Directory,
    Link,
    Other,
}

struct PendingTunnel {
    child: Option<Child>,
}

impl PendingTunnel {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> AppResult<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| ApiError::new("tunnel_state_error", "USB tunnel process is missing"))
    }

    fn take(&mut self) -> Option<Child> {
        self.child.take()
    }
}

impl Drop for PendingTunnel {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            stop_child(&mut child);
        }
    }
}

#[tauri::command]
pub async fn start_ios_ssh_session(
    request: StartIosSshSessionRequest,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosSshSession>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || start_ios_ssh_session_inner(request, &app, &state)).await)
}

#[tauri::command]
pub async fn test_ios_ssh_connection(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosSshConnectionResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let (connection, tunnel_active) = session_snapshot(&state, &session_id)?;
        let probe = probe_connection(&connection)?;
        Ok(IosSshConnectionResult {
            success: true,
            message: "SSH authentication succeeded".into(),
            connected: true,
            jailbreak_confirmed: true,
            auth_mode: connection.authentication.mode(),
            server_system: probe.server_system,
            remote_uid: probe.remote_uid,
            tunnel_active,
        })
    })
    .await)
}

#[tauri::command]
pub async fn list_ios_ssh_files(
    request: IosSshPathRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<Vec<RemoteFileEntry>>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let (connection, _) = session_snapshot(&state, &request.session_id)?;
        let requested = normalize_remote_path(&request.path)?;
        require_allowed_path(&connection, &requested, false)?;
        let directory = canonicalize_remote_directory(&connection, &requested)?;
        require_allowed_path(&connection, &directory, false)?;
        let command = format!(
            "printf '{}\\n'; LC_ALL=C ls -la -n {}",
            LIST_MARKER,
            validation::quote_remote(&directory)
        );
        let output = run_ssh_command(&connection, &command, SSH_TIMEOUT)?;
        let listing = output
            .stdout
            .lines()
            .skip_while(|line| line.trim() != LIST_MARKER)
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n");
        // Keep the stable configured alias in returned paths. Rootless
        // jailbreaks often expand `/var/mobile` into a volatile `.jbroot-*`
        // path when `pwd -P` is used for the security boundary check.
        Ok(parse_ls_output(&requested, &listing))
    })
    .await)
}

#[tauri::command]
pub async fn upload_ios_ssh_file(
    request: UploadIosSshFileRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let (connection, _) = session_snapshot(&state, &request.session_id)?;
        let source = validated_local_file(&request.local_path)?;
        let source_name = local_file_name(&source)?;
        let requested = normalize_remote_path(&request.remote_path)?;
        require_allowed_path(&connection, &requested, false)?;
        verify_remote_parent_or_allowed_root(&connection, &requested)?;

        let requested_kind = remote_entry_kind(&connection, &requested)?;
        let target = if requested_kind == RemoteEntryKind::Directory {
            verify_remote_directory_boundary(&connection, &requested)?;
            join_remote_path(&requested, &source_name)?
        } else {
            let (parent, name) = remote_parent_and_name(&requested)?;
            verify_remote_directory_boundary(&connection, &parent)?;
            join_remote_path(&parent, &name)?
        };
        require_allowed_path(&connection, &target, true)?;

        match remote_entry_kind(&connection, &target)? {
            RemoteEntryKind::Missing => {}
            RemoteEntryKind::File if request.overwrite => {}
            RemoteEntryKind::File => {
                return Err(ApiError::new(
                    "remote_file_exists",
                    "The remote file already exists; enable overwrite to replace it",
                ));
            }
            RemoteEntryKind::Link => {
                return Err(ApiError::new(
                    "unsafe_remote_link",
                    "Refusing to upload through a remote symbolic link",
                ));
            }
            _ => {
                return Err(ApiError::new(
                    "invalid_remote_target",
                    "The remote destination is not a regular file target",
                ));
            }
        }

        let mut args = scp_base_args(&connection);
        args.push(source.to_string_lossy().into_owned());
        args.push(scp_remote_spec(&connection, &target)?);
        let output = run_connection_tool(&connection, "scp", &args, FILE_TIMEOUT)?;
        Ok(output.into_operation(format!("Uploaded file to {target}")))
    })
    .await)
}

#[tauri::command]
pub async fn download_ios_ssh_file(
    request: DownloadIosSshFileRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let (connection, _) = session_snapshot(&state, &request.session_id)?;
        let requested = normalize_remote_path(&request.remote_path)?;
        require_allowed_path(&connection, &requested, true)?;
        let (parent, remote_name) = remote_parent_and_name(&requested)?;
        verify_remote_directory_boundary(&connection, &parent)?;
        let remote_source = join_remote_path(&parent, &remote_name)?;
        require_allowed_path(&connection, &remote_source, true)?;
        match remote_entry_kind(&connection, &remote_source)? {
            RemoteEntryKind::File => {}
            RemoteEntryKind::Link => {
                return Err(ApiError::new(
                    "unsafe_remote_link",
                    "Refusing to download through a remote symbolic link",
                ));
            }
            _ => {
                return Err(ApiError::new(
                    "remote_file_not_found",
                    "The remote path is not a regular file",
                ));
            }
        }

        let requested_local = validation::local_absolute_path(&request.local_path)?;
        let destination = if requested_local.is_dir() {
            requested_local.join(&remote_name)
        } else {
            requested_local.to_path_buf()
        };
        validate_local_destination(&destination, request.overwrite)?;

        let mut args = scp_base_args(&connection);
        args.push(scp_remote_spec(&connection, &remote_source)?);
        args.push(destination.to_string_lossy().into_owned());
        let output = run_connection_tool(&connection, "scp", &args, FILE_TIMEOUT)?;
        Ok(output.into_operation(format!("Downloaded file to {}", destination.display())))
    })
    .await)
}

#[tauri::command]
pub async fn mkdir_ios_ssh(
    request: IosSshPathRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let (connection, _) = session_snapshot(&state, &request.session_id)?;
        let target = normalize_remote_path(&request.path)?;
        require_allowed_path(&connection, &target, true)?;
        let (parent, name) = remote_parent_and_name(&target)?;
        let physical_parent = canonicalize_remote_directory(&connection, &parent)?;
        require_allowed_path(&connection, &physical_parent, false)?;
        if remote_entry_kind(&connection, &target)? != RemoteEntryKind::Missing {
            return Err(ApiError::new(
                "remote_path_exists",
                "The remote path already exists",
            ));
        }
        let command = guarded_parent_command(
            &connection,
            &parent,
            &format!("mkdir {}", validation::quote_remote(&format!("./{name}"))),
        );
        let output = run_ssh_command(&connection, &command, SSH_TIMEOUT)?;
        Ok(output.into_operation(format!("Created remote directory {target}")))
    })
    .await)
}

#[tauri::command]
pub async fn delete_ios_ssh(
    request: DeleteIosSshRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let (connection, _) = session_snapshot(&state, &request.session_id)?;
        let target = normalize_remote_path(&request.path)?;
        require_allowed_path(&connection, &target, true)?;
        let (parent, name) = remote_parent_and_name(&target)?;
        let physical_parent = canonicalize_remote_directory(&connection, &parent)?;
        require_allowed_path(&connection, &physical_parent, false)?;
        let kind = remote_entry_kind(&connection, &target)?;
        if kind == RemoteEntryKind::Missing {
            return Err(ApiError::new(
                "remote_path_not_found",
                "The remote path does not exist",
            ));
        }
        if kind == RemoteEntryKind::Directory && !request.recursive {
            return Err(ApiError::new(
                "recursive_delete_required",
                "Deleting a directory requires recursive=true",
            ));
        }
        let flag = if request.recursive { "-rf" } else { "-f" };
        let command = guarded_parent_command(
            &connection,
            &parent,
            &format!(
                "rm {flag} {}",
                validation::quote_remote(&format!("./{name}"))
            ),
        );
        let output = run_ssh_command(&connection, &command, SSH_TIMEOUT)?;
        Ok(output.into_operation(format!("Deleted remote item {target}")))
    })
    .await)
}

#[tauri::command]
pub async fn stop_ios_ssh_session(
    session_id: String,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        validate_session_id(&session_id)?;
        let _frida_lifecycle = state
            .ios_frida_lock
            .lock()
            .map_err(|_| ApiError::new("state_error", "iOS server lifecycle lock was poisoned"))?;
        let mut session = state
            .ios_ssh_sessions
            .lock()
            .map_err(|_| ApiError::new("state_error", "iOS SSH session lock was poisoned"))?
            .remove(&session_id)
            .ok_or_else(|| {
                ApiError::new(
                    "ios_ssh_session_not_found",
                    "The iOS SSH session is not active",
                )
            })?;
        if let Ok(mut confirmations) = state.ios_action_confirmations.lock() {
            confirmations.retain(|_, pending| pending.session_id != session_id);
        }
        let mut cleanup_warnings = super::ios_frida::cleanup_ios_frida_for_session(&mut session);
        cleanup_warnings.extend(super::ios_ports::cleanup_ios_port_tunnels_for_session(
            &state,
            &session_id,
        ));
        let pid = session.tunnel.as_ref().map(Child::id);
        let tunnel_stopped = session.tunnel.as_mut().is_some_and(stop_child);
        Ok(OperationResult {
            success: true,
            message: if !cleanup_warnings.is_empty() {
                format!(
                    "SSH session closed with cleanup warnings: {}",
                    cleanup_warnings.join("; ")
                )
            } else if tunnel_stopped {
                "SSH session, managed iOS resources, port tunnels, and USB tunnel closed".into()
            } else {
                "SSH session and managed iOS resources closed".into()
            },
            stdout: None,
            stderr: None,
            pid,
            exit_code: Some(0),
            timed_out: false,
        })
    })
    .await)
}

fn start_ios_ssh_session_inner(
    request: StartIosSshSessionRequest,
    app: &AppHandle,
    state: &AppState,
) -> AppResult<IosSshSession> {
    if state.shutting_down.load(Ordering::Acquire) {
        return Err(ApiError::new(
            "app_shutting_down",
            "Mobius is exiting and cannot start an SSH session",
        ));
    }
    resolve_tool("ssh")?;
    resolve_tool("scp")?;
    let username = request.username.unwrap_or_else(|| "root".into());
    validate_username(&username)?;
    let authentication = match request.auth_mode {
        IosSshAuthMode::Password => {
            let password = request.password.ok_or_else(|| {
                ApiError::new(
                    "missing_ssh_password",
                    "Enter the SSH password for password authentication",
                )
            })?;
            validate_password(password.expose_secret())?;
            IosSshAuthentication::Password(password)
        }
        IosSshAuthMode::PrivateKey => {
            let path = request.private_key_path.as_deref().ok_or_else(|| {
                ApiError::new(
                    "missing_private_key",
                    "Select an SSH private key for private-key authentication",
                )
            })?;
            IosSshAuthentication::PrivateKey(validate_private_key(path)?)
        }
    };
    let configured_roots = validate_allowed_roots(&request.allowed_roots)?;
    let known_hosts_path = prepare_known_hosts(app)?;

    let mut pending_tunnel = None;
    let (mode, ssh_host, ssh_port, device_port, host_key_alias, tunnel_udid) =
        match request.transport {
            IosSshTransport::Usb {
                udid,
                device_port,
                host_port,
            } => {
                validation::serial(&udid)?;
                let device_port = valid_port(device_port.unwrap_or(DEFAULT_SSH_PORT), "device")?;
                let host_port = reserve_loopback_port(host_port)?;
                let mut tunnel = PendingTunnel::new(spawn_iproxy(&udid, host_port, device_port)?);
                wait_for_tunnel(tunnel.child_mut()?, host_port)?;
                let alias = format!("mobius-usb-{udid}");
                pending_tunnel = Some(tunnel);
                (
                    "usb".to_string(),
                    Ipv4Addr::LOCALHOST.to_string(),
                    host_port,
                    Some(device_port),
                    Some(alias),
                    Some(udid),
                )
            }
            IosSshTransport::Lan { host, port } => {
                let ip = validate_lan_host(&host)?;
                let port = valid_port(port.unwrap_or(DEFAULT_SSH_PORT), "SSH")?;
                (
                    "lan".to_string(),
                    ip.to_string(),
                    port,
                    Some(port),
                    None,
                    None,
                )
            }
        };

    let session_id = new_session_id();
    let mut connection = IosSshConnection {
        ssh_host,
        ssh_port,
        device_port,
        username,
        authentication,
        known_hosts_path,
        host_key_alias,
        configured_roots: configured_roots.clone(),
        allowed_roots: Vec::new(),
        server_system: None,
        remote_uid: None,
    };
    let probe = probe_connection(&connection)?;

    let mut canonical_roots = Vec::with_capacity(configured_roots.len());
    let mut seen = HashSet::new();
    for root in &configured_roots {
        let canonical = canonicalize_remote_directory(&connection, root)?;
        if canonical == "/" {
            return Err(ApiError::new(
                "unsafe_allowed_root",
                "The filesystem root cannot be used as an allowed file-management root",
            ));
        }
        if seen.insert(canonical.clone()) {
            canonical_roots.push(canonical);
        }
    }
    connection.allowed_roots = canonical_roots;
    connection.server_system = probe.server_system;
    connection.remote_uid = probe.remote_uid;

    let tunnel_pid = pending_tunnel
        .as_ref()
        .and_then(|guard| guard.child.as_ref())
        .map(Child::id);
    let tunnel_status = match (tunnel_pid, tunnel_udid.as_ref(), connection.device_port) {
        (Some(pid), Some(udid), Some(device_port)) => Some(IosSshTunnelStatus {
            active: true,
            pid,
            udid: udid.clone(),
            bind_address: Ipv4Addr::LOCALHOST.to_string(),
            host_port: connection.ssh_port,
            device_port,
        }),
        _ => None,
    };
    let response = IosSshSession {
        session_id: session_id.clone(),
        mode,
        connected: true,
        jailbreak_confirmed: true,
        ssh_host: connection.ssh_host.clone(),
        ssh_port: connection.ssh_port,
        device_port: connection.device_port,
        username: connection.username.clone(),
        auth_mode: connection.authentication.mode(),
        allowed_roots: connection.configured_roots.clone(),
        server_system: connection.server_system.clone(),
        remote_uid: connection.remote_uid,
        tunnel: tunnel_status,
    };

    let mut registry = state
        .ios_ssh_sessions
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS SSH session lock was poisoned"))?;
    if state.shutting_down.load(Ordering::Acquire) {
        return Err(ApiError::new(
            "app_shutting_down",
            "Mobius is exiting and cannot retain the SSH session",
        ));
    }
    if registry.len() >= MAX_SESSIONS {
        return Err(ApiError::new(
            "ios_ssh_session_limit",
            format!("At most {MAX_SESSIONS} iOS SSH sessions may be active"),
        ));
    }
    let tunnel = pending_tunnel.as_mut().and_then(PendingTunnel::take);
    registry.insert(
        session_id,
        ManagedIosSshSession {
            connection,
            tunnel,
            ios_frida_upload: None,
            ios_frida_process: None,
        },
    );
    Ok(response)
}

fn validate_private_key(value: &str) -> AppResult<PathBuf> {
    let key = validation::local_existing_path(value)?;
    let canonical = key.canonicalize().map_err(|error| {
        ApiError::new(
            "invalid_private_key",
            format!("Unable to resolve the selected private key: {error}"),
        )
    })?;
    let metadata = canonical.metadata().map_err(|error| {
        ApiError::new(
            "invalid_private_key",
            format!("Unable to inspect the selected private key: {error}"),
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
        return Err(ApiError::new(
            "invalid_private_key",
            "The selected private key must be a non-empty regular file no larger than 1 MiB",
        ));
    }
    #[cfg(windows)]
    if canonical.to_string_lossy().starts_with(r"\\") {
        return Err(ApiError::new(
            "unsafe_private_key_path",
            "Private keys on network shares are not supported",
        ));
    }
    Ok(canonical)
}

fn validate_password(value: &str) -> AppResult<()> {
    if value.is_empty()
        || value.len() > 1024
        || value
            .chars()
            .any(|character| matches!(character, '\0' | '\r' | '\n'))
    {
        return Err(ApiError::new(
            "invalid_ssh_password",
            "The SSH password must be 1 to 1024 characters and cannot contain line breaks",
        ));
    }
    Ok(())
}

fn validate_username(value: &str) -> AppResult<&str> {
    if value.is_empty()
        || value.len() > 64
        || value.starts_with('-')
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(ApiError::new(
            "invalid_ssh_username",
            "SSH username contains unsupported characters",
        ));
    }
    Ok(value)
}

fn validate_lan_host(value: &str) -> AppResult<IpAddr> {
    let ip = value.parse::<IpAddr>().map_err(|_| {
        ApiError::new(
            "invalid_ssh_host",
            "LAN SSH host must be a private or link-local IP address",
        )
    })?;
    let allowed = match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            ip.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
    };
    if !allowed {
        return Err(ApiError::new(
            "non_local_ssh_host",
            "LAN SSH is limited to private, loopback, or link-local addresses",
        ));
    }
    Ok(ip)
}

fn valid_port(port: u16, label: &str) -> AppResult<u16> {
    if port == 0 {
        return Err(ApiError::new(
            "invalid_ssh_port",
            format!("{label} port must be between 1 and 65535"),
        ));
    }
    Ok(port)
}

fn validate_allowed_roots(values: &[String]) -> AppResult<Vec<String>> {
    if values.is_empty() || values.len() > MAX_ALLOWED_ROOTS {
        return Err(ApiError::new(
            "invalid_allowed_roots",
            format!("Configure between 1 and {MAX_ALLOWED_ROOTS} allowed remote roots"),
        ));
    }
    values
        .iter()
        .map(|value| {
            let path = normalize_remote_path(value)?;
            if path == "/" {
                return Err(ApiError::new(
                    "unsafe_allowed_root",
                    "The filesystem root cannot be used as an allowed file-management root",
                ));
            }
            Ok(path)
        })
        .collect()
}

fn prepare_known_hosts(app: &AppHandle) -> AppResult<PathBuf> {
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|error| {
            ApiError::new(
                "app_data_path_error",
                format!("Unable to locate Mobius application data: {error}"),
            )
        })?
        .join("ssh");
    fs::create_dir_all(&directory).map_err(|error| {
        ApiError::new(
            "known_hosts_error",
            format!("Unable to create the SSH state directory: {error}"),
        )
    })?;
    let path = directory.join("known_hosts");
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| {
            ApiError::new(
                "known_hosts_error",
                format!("Unable to prepare the SSH known-hosts file: {error}"),
            )
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

fn reserve_loopback_port(requested: Option<u16>) -> AppResult<u16> {
    let port = requested.unwrap_or(0);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|error| {
        ApiError::new(
            "ssh_host_port_unavailable",
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
                "ssh_host_port_error",
                format!("Unable to inspect the allocated loopback port: {error}"),
            )
        })
}

fn spawn_iproxy(udid: &str, host_port: u16, device_port: u16) -> AppResult<Child> {
    let executable = resolve_tool("iproxy")?;
    background_command(executable)
        .args([
            "-u",
            udid,
            "-l",
            "-s",
            "127.0.0.1",
            &format!("{host_port}:{device_port}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            ApiError::new(
                "iproxy_spawn_failed",
                format!("Unable to start the USB SSH tunnel: {error}"),
            )
        })
}

fn wait_for_tunnel(child: &mut Child, host_port: u16) -> AppResult<()> {
    let started = Instant::now();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, host_port));
    while started.elapsed() < TUNNEL_START_TIMEOUT {
        if let Some(status) = child.try_wait().map_err(|error| {
            ApiError::new(
                "iproxy_state_error",
                format!("Unable to inspect the USB tunnel: {error}"),
            )
        })? {
            return Err(ApiError::new(
                "iproxy_start_failed",
                format!("iproxy exited before the tunnel was ready: {status}"),
            ));
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(ApiError::new(
        "iproxy_start_timeout",
        "The loopback USB tunnel did not become ready within 5 seconds",
    ))
}

fn new_session_id() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("ios-ssh-{:x}-{nonce:x}", std::process::id())
}

fn validate_session_id(value: &str) -> AppResult<&str> {
    if value.len() < 12
        || value.len() > 96
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err(ApiError::new(
            "invalid_ios_ssh_session_id",
            "Invalid iOS SSH session identifier",
        ));
    }
    Ok(value)
}

pub(crate) fn session_snapshot(
    state: &AppState,
    session_id: &str,
) -> AppResult<(IosSshConnection, Option<bool>)> {
    validate_session_id(session_id)?;
    let mut registry = state
        .ios_ssh_sessions
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS SSH session lock was poisoned"))?;
    let session = registry.get_mut(session_id).ok_or_else(|| {
        ApiError::new(
            "ios_ssh_session_not_found",
            "The iOS SSH session is not active",
        )
    })?;
    let tunnel_active = if let Some(child) = session.tunnel.as_mut() {
        match child.try_wait().map_err(|error| {
            ApiError::new(
                "iproxy_state_error",
                format!("Unable to inspect the USB tunnel: {error}"),
            )
        })? {
            Some(status) => {
                return Err(ApiError::new(
                    "ios_ssh_tunnel_stopped",
                    format!("The USB tunnel is no longer active: {status}"),
                ));
            }
            None => Some(true),
        }
    } else {
        None
    };
    Ok((session.connection.clone(), tunnel_active))
}

pub(crate) fn ssh_base_args(connection: &IosSshConnection) -> Vec<String> {
    let mut args = vec![
        "-T".into(),
        "-F".into(),
        "none".into(),
        "-p".into(),
        connection.ssh_port.to_string(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        format!(
            "UserKnownHostsFile={}",
            connection.known_hosts_path.to_string_lossy()
        ),
        "-o".into(),
        "ConnectTimeout=8".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=2".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
    ];
    append_authentication_args(&mut args, connection, false);
    if let Some(alias) = &connection.host_key_alias {
        args.extend(["-o".into(), format!("HostKeyAlias={alias}")]);
    }
    args
}

pub(crate) fn scp_base_args(connection: &IosSshConnection) -> Vec<String> {
    let mut args = vec![
        "-F".into(),
        "none".into(),
        "-P".into(),
        connection.ssh_port.to_string(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-o".into(),
        format!(
            "UserKnownHostsFile={}",
            connection.known_hosts_path.to_string_lossy()
        ),
        "-o".into(),
        "ConnectTimeout=8".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
    ];
    append_authentication_args(&mut args, connection, true);
    if let Some(alias) = &connection.host_key_alias {
        args.extend(["-o".into(), format!("HostKeyAlias={alias}")]);
    }
    args
}

fn append_authentication_args(args: &mut Vec<String>, connection: &IosSshConnection, scp: bool) {
    match &connection.authentication {
        IosSshAuthentication::PrivateKey(path) => {
            if scp {
                args.push("-B".into());
            }
            args.extend([
                "-i".into(),
                path.to_string_lossy().into_owned(),
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "IdentitiesOnly=yes".into(),
                "-o".into(),
                "PreferredAuthentications=publickey".into(),
                "-o".into(),
                "PasswordAuthentication=no".into(),
                "-o".into(),
                "KbdInteractiveAuthentication=no".into(),
            ]);
        }
        IosSshAuthentication::Password(_) => args.extend([
            "-o".into(),
            "BatchMode=no".into(),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            "PubkeyAuthentication=no".into(),
            "-o".into(),
            "PreferredAuthentications=password".into(),
            "-o".into(),
            "PasswordAuthentication=yes".into(),
            "-o".into(),
            "KbdInteractiveAuthentication=no".into(),
            "-o".into(),
            "NumberOfPasswordPrompts=1".into(),
        ]),
    }
}

fn askpass_environment(connection: &IosSshConnection) -> AppResult<Vec<(String, String)>> {
    let IosSshAuthentication::Password(password) = &connection.authentication else {
        return Ok(Vec::new());
    };
    let helper = std::env::current_exe().map_err(|error| {
        ApiError::new(
            "ssh_askpass_unavailable",
            format!("Unable to locate the Mobius SSH password helper: {error}"),
        )
    })?;
    Ok(vec![
        ("SSH_ASKPASS".into(), helper.to_string_lossy().into_owned()),
        ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
        ("DISPLAY".into(), "mobius:0".into()),
        (ASKPASS_MARKER_ENV.into(), "1".into()),
        (
            ASKPASS_PASSWORD_ENV.into(),
            password.expose_secret().to_owned(),
        ),
    ])
}

fn zeroize_askpass_environment(environment: &mut [(String, String)]) {
    for (key, value) in environment {
        if key == ASKPASS_PASSWORD_ENV {
            value.zeroize();
        }
    }
}

pub(crate) fn apply_ssh_auth_environment(
    command: &mut Command,
    connection: &IosSshConnection,
) -> AppResult<()> {
    let mut environment = askpass_environment(connection)?;
    command.envs(environment.iter().map(|(key, value)| (key, value)));
    zeroize_askpass_environment(&mut environment);
    Ok(())
}

pub(crate) fn run_connection_tool(
    connection: &IosSshConnection,
    program: &str,
    args: &[String],
    timeout: Duration,
) -> AppResult<ProcessOutput> {
    match &connection.authentication {
        IosSshAuthentication::PrivateKey(_) => run_checked(program, args, timeout),
        IosSshAuthentication::Password(password) => {
            let mut environment = askpass_environment(connection)?;
            let result = run_checked_with_env(
                program,
                args,
                timeout,
                &[password.expose_secret()],
                &environment,
            );
            zeroize_askpass_environment(&mut environment);
            result
        }
    }
}

pub(crate) fn ssh_target(connection: &IosSshConnection) -> String {
    format!("{}@{}", connection.username, connection.ssh_host)
}

pub(crate) fn scp_remote_spec(
    connection: &IosSshConnection,
    remote_path: &str,
) -> AppResult<String> {
    validation::remote_path(remote_path)?;
    // OpenSSH 9 uses SFTP for `scp` by default. The remote path is already one
    // argv item, so shell quoting here becomes part of the SFTP filename. At
    // the same time, SFTP's remote-source parser still expands glob syntax;
    // reject it rather than allowing one selected file to match several.
    if remote_path
        .chars()
        .any(|character| matches!(character, '*' | '?' | '[' | ']' | '\\'))
    {
        return Err(ApiError::new(
            "unsupported_scp_path",
            "SCP paths cannot contain wildcard or escape characters (*, ?, [, ], or \\)",
        ));
    }
    let host = if connection.ssh_host.contains(':') {
        format!("[{}]", connection.ssh_host)
    } else {
        connection.ssh_host.clone()
    };
    Ok(format!("{}@{}:{remote_path}", connection.username, host))
}

pub(crate) fn run_ssh_command(
    connection: &IosSshConnection,
    command: &str,
    timeout: Duration,
) -> AppResult<ProcessOutput> {
    let mut args = ssh_base_args(connection);
    args.push(ssh_target(connection));
    args.push(command.to_string());
    run_connection_tool(connection, "ssh", &args, timeout)
}

fn probe_connection(connection: &IosSshConnection) -> AppResult<ProbeResult> {
    let output = run_ssh_command(
        connection,
        "printf 'MOBIUS_SSH_READY\\n'; uname -s; id -u",
        SSH_TIMEOUT,
    )?;
    let lines = output.stdout.lines().map(str::trim).collect::<Vec<_>>();
    let marker = lines
        .iter()
        .position(|line| *line == PROBE_MARKER)
        .ok_or_else(|| {
            ApiError::new(
                "invalid_ssh_probe",
                "SSH connected, but the device did not return the expected validation marker",
            )
        })?;
    let server_system = lines
        .get(marker + 1)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(80).collect());
    let remote_uid = lines.get(marker + 2).and_then(|value| value.parse().ok());
    Ok(ProbeResult {
        server_system,
        remote_uid,
    })
}

fn canonicalize_remote_directory(
    connection: &IosSshConnection,
    requested: &str,
) -> AppResult<String> {
    let command = format!(
        "cd {} && printf '{}\\n' && pwd -P",
        validation::quote_remote(requested),
        PATH_MARKER
    );
    let output = run_ssh_command(connection, &command, SSH_TIMEOUT)?;
    let lines = output.stdout.lines().map(str::trim).collect::<Vec<_>>();
    let marker = lines
        .iter()
        .position(|line| *line == PATH_MARKER)
        .ok_or_else(|| {
            ApiError::new(
                "invalid_remote_path_response",
                "The device did not return a canonical remote directory",
            )
        })?;
    let path = lines.get(marker + 1).ok_or_else(|| {
        ApiError::new(
            "invalid_remote_path_response",
            "The device returned an empty canonical remote directory",
        )
    })?;
    normalize_remote_path(path)
}

fn remote_entry_kind(
    connection: &IosSshConnection,
    remote_path: &str,
) -> AppResult<RemoteEntryKind> {
    let quoted = validation::quote_remote(remote_path);
    let command = format!(
        "if [ -h {quoted} ] || [ -L {quoted} ]; then mobius_kind=link; \
         elif [ -d {quoted} ]; then mobius_kind=directory; \
         elif [ -f {quoted} ]; then mobius_kind=file; \
         elif [ -e {quoted} ]; then mobius_kind=other; \
         else mobius_kind=missing; fi; printf '{}:%s\\n' \"$mobius_kind\"",
        TYPE_MARKER
    );
    let output = run_ssh_command(connection, &command, SSH_TIMEOUT)?;
    let value = output
        .stdout
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&format!("{TYPE_MARKER}:")))
        .ok_or_else(|| {
            ApiError::new(
                "invalid_remote_type_response",
                "The device did not return the remote path type",
            )
        })?;
    match value {
        "missing" => Ok(RemoteEntryKind::Missing),
        "file" => Ok(RemoteEntryKind::File),
        "directory" => Ok(RemoteEntryKind::Directory),
        "link" => Ok(RemoteEntryKind::Link),
        "other" => Ok(RemoteEntryKind::Other),
        _ => Err(ApiError::new(
            "invalid_remote_type_response",
            "The device returned an unknown remote path type",
        )),
    }
}

fn normalize_remote_path(value: &str) -> AppResult<String> {
    validation::remote_path(value)?;
    if value.chars().any(char::is_control) {
        return Err(ApiError::new(
            "invalid_remote_path",
            "Remote paths cannot contain control characters",
        ));
    }
    let trimmed = value.trim_end_matches('/');
    Ok(if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.to_string()
    })
}

fn require_allowed_path(
    connection: &IosSshConnection,
    path: &str,
    strict_descendant: bool,
) -> AppResult<()> {
    let matches_root = |root: &String| {
        if strict_descendant {
            path != root && is_path_within(path, root)
        } else {
            is_path_within(path, root)
        }
    };
    // Accept both the stable configured alias and the physical root. Any
    // operation that follows a directory canonicalizes it and checks again,
    // preventing a link below an alias from escaping the physical boundary.
    let allowed = connection.configured_roots.iter().any(matches_root)
        || connection.allowed_roots.iter().any(matches_root);
    if allowed {
        Ok(())
    } else {
        Err(ApiError::new(
            "remote_path_outside_allowed_roots",
            if strict_descendant {
                "The remote path must be below, and cannot equal, an allowed root"
            } else {
                "The remote path is outside the configured allowed roots"
            },
        ))
    }
}

fn is_path_within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn verify_remote_parent_or_allowed_root(
    connection: &IosSshConnection,
    path: &str,
) -> AppResult<()> {
    if connection.configured_roots.iter().any(|root| path == root)
        || connection.allowed_roots.iter().any(|root| path == root)
    {
        return Ok(());
    }
    let (parent, _) = remote_parent_and_name(path)?;
    verify_remote_directory_boundary(connection, &parent)
}

/// Resolves a directory only for the allowed-root boundary check.
///
/// The canonical path must not escape this function and become an SCP path:
/// rootless jailbreaks can expose a stable path such as `/var/mobile/...` to
/// SSH commands while `pwd -P` expands it to a volatile `/rootfs/.../.jbroot-*`
/// path that a separately launched SCP process cannot resolve.
fn verify_remote_directory_boundary(
    connection: &IosSshConnection,
    lexical_directory: &str,
) -> AppResult<()> {
    let physical_directory = canonicalize_remote_directory(connection, lexical_directory)?;
    require_allowed_path(connection, &physical_directory, false)
}

fn remote_parent_and_name(path: &str) -> AppResult<(String, String)> {
    let normalized = normalize_remote_path(path)?;
    if normalized == "/" {
        return Err(ApiError::new(
            "protected_remote_path",
            "The filesystem root cannot be used as a file operation target",
        ));
    }
    let (parent, name) = normalized.rsplit_once('/').ok_or_else(|| {
        ApiError::new(
            "invalid_remote_path",
            "Remote path does not contain a parent directory",
        )
    })?;
    validate_remote_name(name)?;
    Ok((
        if parent.is_empty() {
            "/".into()
        } else {
            parent.into()
        },
        name.into(),
    ))
}

fn validate_remote_name(value: &str) -> AppResult<&str> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.contains('/')
        || value.chars().any(char::is_control)
        || value.len() > 255
    {
        return Err(ApiError::new(
            "invalid_remote_name",
            "Remote item name is invalid",
        ));
    }
    Ok(value)
}

fn join_remote_path(parent: &str, name: &str) -> AppResult<String> {
    validate_remote_name(name)?;
    Ok(if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    })
}

fn guarded_parent_command(
    connection: &IosSshConnection,
    lexical_parent: &str,
    action: &str,
) -> String {
    let patterns = connection
        .allowed_roots
        .iter()
        .flat_map(|root| {
            let quoted = validation::quote_remote(root);
            [quoted.clone(), format!("{quoted}/*")]
        })
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "cd {} && mobius_parent=$(pwd -P) && case \"$mobius_parent\" in {patterns}) {action} ;; *) exit 73 ;; esac",
        validation::quote_remote(lexical_parent)
    )
}

fn validated_local_file(value: &str) -> AppResult<PathBuf> {
    let path = validation::local_existing_path(value)?;
    let canonical = path.canonicalize().map_err(|error| {
        ApiError::new(
            "invalid_local_path",
            format!("Unable to resolve the local file: {error}"),
        )
    })?;
    if !canonical.is_file() {
        return Err(ApiError::new(
            "invalid_local_file",
            "Upload source must be a regular local file",
        ));
    }
    Ok(canonical)
}

fn local_file_name(path: &Path) -> AppResult<String> {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            ApiError::new(
                "invalid_local_file_name",
                "The local filename must be valid Unicode",
            )
        })?;
    validate_remote_name(name)?;
    Ok(name.to_string())
}

fn validate_local_destination(path: &Path, overwrite: bool) -> AppResult<()> {
    let parent = path.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_local_path",
            "The local destination has no parent directory",
        )
    })?;
    if !parent.is_dir() {
        return Err(ApiError::new(
            "local_directory_not_found",
            format!("Destination directory does not exist: {}", parent.display()),
        ));
    }
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(ApiError::new(
            "unsafe_local_link",
            "Refusing to overwrite a local symbolic link",
        ));
    }
    if path.exists() && path.is_dir() {
        return Err(ApiError::new(
            "invalid_local_destination",
            "The resolved local destination is a directory",
        ));
    }
    if path.exists() && !overwrite {
        return Err(ApiError::new(
            "local_file_exists",
            "The local file already exists; enable overwrite to replace it",
        ));
    }
    Ok(())
}

fn parse_ls_output(parent: &str, output: &str) -> Vec<RemoteFileEntry> {
    output
        .lines()
        .filter_map(|line| parse_ls_line(parent, line))
        .collect()
}

fn parse_ls_line(parent: &str, line: &str) -> Option<RemoteFileEntry> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 7 || fields.first().map_or(true, |value| value.len() < 10) {
        return None;
    }
    let permissions = fields[0];
    let name_index = if fields.get(5) == Some(&"?") {
        6
    } else if fields
        .get(5)
        .is_some_and(|value| value.len() == 10 && value.as_bytes().get(4) == Some(&b'-'))
    {
        7
    } else {
        8
    };
    if fields.len() <= name_index {
        return None;
    }
    let display_name = fields[name_index..].join(" ");
    let (name, link_target) = if permissions.starts_with('l') {
        match display_name.split_once(" -> ") {
            Some((name, target)) => (name.to_string(), Some(target.to_string())),
            None => (display_name, None),
        }
    } else {
        (display_name, None)
    };
    if matches!(name.as_str(), "." | "..") {
        return None;
    }
    let path = if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    };
    let kind = match permissions.as_bytes().first().copied() {
        Some(b'd') => "directory",
        Some(b'l') => "link",
        Some(b'-') => "file",
        _ => "unknown",
    };
    let modified_end = name_index.min(fields.len());
    let modified = (modified_end > 5).then(|| fields[5..modified_end].join(" "));
    Some(RemoteFileEntry {
        name,
        path,
        kind: kind.into(),
        permissions: Some(permissions.to_string()),
        owner: fields.get(2).map(|value| (*value).to_string()),
        group: fields.get(3).map(|value| (*value).to_string()),
        size: fields.get(4).and_then(|value| value.parse().ok()),
        modified,
        link_target,
    })
}

pub(crate) fn stop_child(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(Some(_)) => {
            let _ = child.wait();
            false
        }
        Ok(None) => {
            let killed = child.kill().is_ok();
            let _ = child.wait();
            killed
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    }
}

pub(crate) fn cleanup_managed_ios_ssh(state: &AppState) {
    let _frida_lifecycle = match state.ios_frida_lock.lock() {
        Ok(guard) => guard,
        Err(_) => {
            eprintln!("Mobius cleanup: iOS server lifecycle lock was poisoned");
            return;
        }
    };
    let sessions: Vec<ManagedIosSshSession> = match state.ios_ssh_sessions.lock() {
        Ok(mut registry) => registry.drain().map(|(_, session)| session).collect(),
        Err(_) => {
            eprintln!("Mobius cleanup: iOS SSH session lock was poisoned");
            return;
        }
    };
    if let Ok(mut confirmations) = state.ios_action_confirmations.lock() {
        confirmations.clear();
    }
    for mut session in sessions {
        for warning in super::ios_frida::cleanup_ios_frida_for_session(&mut session) {
            eprintln!("Mobius cleanup: {warning}");
        }
        if let Some(mut tunnel) = session.tunnel.take() {
            stop_child(&mut tunnel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(authentication: IosSshAuthentication) -> IosSshConnection {
        IosSshConnection {
            ssh_host: "127.0.0.1".into(),
            ssh_port: 22,
            device_port: Some(22),
            username: "root".into(),
            authentication,
            known_hosts_path: PathBuf::from("/tmp/mobius-known-hosts"),
            host_key_alias: None,
            configured_roots: vec!["/var/mobile".into()],
            allowed_roots: vec!["/var/mobile".into()],
            server_system: Some("Darwin".into()),
            remote_uid: Some(0),
        }
    }

    #[test]
    fn password_auth_uses_askpass_without_putting_secret_in_arguments() {
        let secret: crate::models::SecretString =
            serde_json::from_str("\"sample-password\"").expect("secret");
        let connection = connection(IosSshAuthentication::Password(secret));
        let args = ssh_base_args(&connection);
        assert!(!args
            .iter()
            .any(|argument| argument.contains("sample-password")));
        assert!(args
            .iter()
            .any(|argument| argument == "PubkeyAuthentication=no"));
        assert!(args
            .iter()
            .any(|argument| argument == "PreferredAuthentications=password"));
        assert!(args
            .iter()
            .any(|argument| argument == "NumberOfPasswordPrompts=1"));
        assert!(args
            .iter()
            .any(|argument| argument == "KbdInteractiveAuthentication=no"));
        assert!(!args.iter().any(|argument| argument == "-i"));

        let mut environment = askpass_environment(&connection).expect("environment");
        assert_eq!(
            environment
                .iter()
                .find(|(key, _)| key == ASKPASS_PASSWORD_ENV)
                .map(|(_, value)| value.as_str()),
            Some("sample-password")
        );
        zeroize_askpass_environment(&mut environment);
        assert_eq!(
            environment
                .iter()
                .find(|(key, _)| key == ASKPASS_PASSWORD_ENV)
                .map(|(_, value)| value.as_str()),
            Some("")
        );
    }

    #[test]
    fn private_key_auth_remains_batch_only() {
        let connection = connection(IosSshAuthentication::PrivateKey(PathBuf::from(
            "/tmp/test-key",
        )));
        let args = ssh_base_args(&connection);
        assert!(args.iter().any(|argument| argument == "BatchMode=yes"));
        assert!(args
            .iter()
            .any(|argument| argument == "PasswordAuthentication=no"));
        assert!(askpass_environment(&connection)
            .expect("environment")
            .is_empty());
    }

    #[test]
    fn password_validation_never_includes_secret_in_errors() {
        let invalid = "line-one\nline-two";
        let error = validate_password(invalid).expect_err("invalid password");
        assert!(!error.message.contains(invalid));
        assert!(validate_password("alpine").is_ok());
    }

    #[test]
    fn lan_ssh_is_limited_to_local_addresses() {
        assert!(validate_lan_host("192.168.1.9").is_ok());
        assert!(validate_lan_host("10.0.0.4").is_ok());
        assert!(validate_lan_host("fe80::1234").is_ok());
        assert!(validate_lan_host("fd00::5").is_ok());
        assert!(validate_lan_host("8.8.8.8").is_err());
        assert!(validate_lan_host("example.com").is_err());
    }

    #[test]
    fn allowed_roots_use_path_component_boundaries() {
        assert!(is_path_within("/var/mobile", "/var/mobile"));
        assert!(is_path_within("/var/mobile/Documents/a", "/var/mobile"));
        assert!(!is_path_within("/var/mobile2/a", "/var/mobile"));
    }

    #[test]
    fn accepts_rootless_alias_and_physical_root_without_prefix_confusion() {
        let mut connection = connection(IosSshAuthentication::PrivateKey(PathBuf::from(
            "/tmp/test-key",
        )));
        connection.allowed_roots = vec!["/rootfs/private/var/mobile/.jbroot-ABC/var/mobile".into()];

        assert!(require_allowed_path(&connection, "/var/mobile/Documents", false).is_ok());
        assert!(require_allowed_path(
            &connection,
            "/rootfs/private/var/mobile/.jbroot-ABC/var/mobile/Documents",
            false,
        )
        .is_ok());
        assert!(require_allowed_path(&connection, "/var/mobile2/Documents", false).is_err());
        assert!(require_allowed_path(
            &connection,
            "/rootfs/private/var/mobile/.jbroot-ABC/var/mobile2/Documents",
            false,
        )
        .is_err());
        assert!(require_allowed_path(&connection, "/var/mobile", true).is_err());
    }

    #[test]
    fn scp_transfer_keeps_rootless_alias_instead_of_physical_jbroot_path() {
        let connection = connection(IosSshAuthentication::PrivateKey(PathBuf::from(
            "/tmp/test-key",
        )));
        let lexical_parent =
            "/var/mobile/Containers/Shared/AppGroup/.jbroot-ABC/var/mobile/Library/Preferences";
        let lexical_file = join_remote_path(lexical_parent, "example.plist").expect("path");
        let spec = scp_remote_spec(&connection, &lexical_file).expect("SCP spec");

        assert_eq!(
            lexical_file,
            "/var/mobile/Containers/Shared/AppGroup/.jbroot-ABC/var/mobile/Library/Preferences/example.plist"
        );
        assert!(spec.ends_with(
            ":/var/mobile/Containers/Shared/AppGroup/.jbroot-ABC/var/mobile/Library/Preferences/example.plist"
        ));
        assert!(!spec.contains("/rootfs/"));
    }

    #[test]
    fn scp_remote_spec_passes_spaces_and_apostrophes_as_raw_sftp_path() {
        let connection = connection(IosSshAuthentication::PrivateKey(PathBuf::from(
            "/tmp/test-key",
        )));
        let remote_path = "/var/mobile/Library/Preferences/O'Brien Settings.plist";

        assert_eq!(
            scp_remote_spec(&connection, remote_path).expect("SCP spec"),
            "root@127.0.0.1:/var/mobile/Library/Preferences/O'Brien Settings.plist"
        );
    }

    #[test]
    fn scp_remote_spec_rejects_globs_and_escape_characters() {
        let connection = connection(IosSshAuthentication::PrivateKey(PathBuf::from(
            "/tmp/test-key",
        )));

        for remote_path in [
            "/var/mobile/*.plist",
            "/var/mobile/file?.plist",
            "/var/mobile/file[0].plist",
            "/var/mobile/file].plist",
            "/var/mobile/file\\name.plist",
        ] {
            let error = scp_remote_spec(&connection, remote_path).expect_err("unsafe SCP path");
            assert_eq!(error.code, "unsupported_scp_path");
        }
    }

    #[test]
    fn parses_bsd_ios_listing_with_spaces() {
        let entry = parse_ls_line(
            "/var/mobile",
            "-rw-r--r-- 1 501 501 42 Sep  4 12:30 hello world.txt",
        )
        .expect("entry");
        assert_eq!(entry.name, "hello world.txt");
        assert_eq!(entry.path, "/var/mobile/hello world.txt");
        assert_eq!(entry.size, Some(42));
    }

    #[test]
    fn parses_ios_links_and_inaccessible_rows() {
        let link = parse_ls_line(
            "/var/mobile",
            "lrwxr-xr-x 1 0 0 15 Sep  5 09:00 Media -> /var/mobile/Media",
        )
        .expect("symbolic link");
        assert_eq!(link.name, "Media");
        assert_eq!(link.path, "/var/mobile/Media");
        assert_eq!(link.kind, "link");
        assert_eq!(link.link_target.as_deref(), Some("/var/mobile/Media"));

        let inaccessible = parse_ls_line("/var/mobile", "d????????? ? ? ? ? ? Protected")
            .expect("inaccessible directory");
        assert_eq!(inaccessible.name, "Protected");
        assert_eq!(inaccessible.kind, "directory");
    }

    #[test]
    fn rejects_root_and_parent_segments() {
        assert!(validate_allowed_roots(&["/".into()]).is_err());
        assert!(normalize_remote_path("/var/mobile/../root").is_err());
        assert_eq!(
            remote_parent_and_name("/var/mobile/a.txt").expect("path"),
            ("/var/mobile".into(), "a.txt".into())
        );
    }
}
