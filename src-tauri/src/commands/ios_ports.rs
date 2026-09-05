use super::{blocking_api, ios_ssh};
use crate::{
    models::{
        ApiError, ApiResult, AppResult, CreateIosPortTunnelRequest, IosPortTunnel,
        IosPortTunnelDirection, IosPortTunnelTransport, RemoveIosPortTunnelRequest,
    },
    runner::{
        background_command, clear_ambient_go_ios_environment, resolve_tool, run_process_at,
        ProcessOutput,
    },
    state::{AppState, IosSshConnection, ManagedIosPortTunnel},
    toolchain::{self, ToolSource},
    validation,
};
use std::{
    fmt::Write as _,
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};
use tauri::State;

const LOOPBACK: &str = "127.0.0.1";
const TUNNEL_START_TIMEOUT: Duration = Duration::from_secs(6);
const REVERSE_STABILITY_WINDOW: Duration = Duration::from_millis(750);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_TUNNELS: usize = 64;
const GO_IOS_VERSION_TIMEOUT: Duration = Duration::from_secs(3);
const PATCHED_GO_IOS_VERSION: &str = "1.3.2-mobius.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsbTunnelProvider {
    BundledGoIos,
    Iproxy,
}

struct PendingChild(Option<Child>);

impl PendingChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> AppResult<&mut Child> {
        self.0
            .as_mut()
            .ok_or_else(|| ApiError::new("ios_tunnel_state_error", "Tunnel child is missing"))
    }

    fn take(&mut self) -> AppResult<Child> {
        self.0
            .take()
            .ok_or_else(|| ApiError::new("ios_tunnel_state_error", "Tunnel child is missing"))
    }
}

impl Drop for PendingChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            ios_ssh::stop_child(&mut child);
        }
    }
}

#[tauri::command]
pub async fn list_ios_port_tunnels(
    state: State<'_, AppState>,
) -> Result<ApiResult<Vec<IosPortTunnel>>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || list_ios_port_tunnels_inner(&state)).await)
}

#[tauri::command]
pub async fn create_ios_port_tunnel(
    request: CreateIosPortTunnelRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosPortTunnel>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || create_ios_port_tunnel_inner(&state, request)).await)
}

#[tauri::command]
pub async fn remove_ios_port_tunnel(
    request: RemoveIosPortTunnelRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosPortTunnel>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || remove_ios_port_tunnel_inner(&state, request)).await)
}

fn list_ios_port_tunnels_inner(state: &AppState) -> AppResult<Vec<IosPortTunnel>> {
    let _lifecycle = lifecycle_lock(state)?;
    let mut registry = tunnel_registry(state)?;
    let mut inactive = Vec::new();
    let mut results = Vec::with_capacity(registry.len());
    for (id, managed) in registry.iter_mut() {
        let active = match managed.child.try_wait() {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(_) => false,
        };
        results.push(tunnel_result(managed, active));
        if !active {
            inactive.push(id.clone());
        }
    }
    for id in inactive {
        if let Some(mut managed) = registry.remove(&id) {
            ios_ssh::stop_child(&mut managed.child);
        }
    }
    results.sort_by(|left, right| left.tunnel_id.cmp(&right.tunnel_id));
    Ok(results)
}

fn create_ios_port_tunnel_inner(
    state: &AppState,
    request: CreateIosPortTunnelRequest,
) -> AppResult<IosPortTunnel> {
    let _lifecycle = lifecycle_lock(state)?;
    require_running(state)?;
    validate_request(&request)?;

    let connection = if request.transport == IosPortTunnelTransport::Ssh {
        let session_id = request.session_id.as_deref().ok_or_else(|| {
            ApiError::new(
                "ios_tunnel_session_required",
                "SSH port tunnels require an active iOS SSH session",
            )
        })?;
        Some(ios_ssh::session_snapshot(state, session_id)?.0)
    } else {
        if let Some(session_id) = request.session_id.as_deref() {
            // An optional session binds an iproxy tunnel to the same lifecycle.
            ios_ssh::session_snapshot(state, session_id)?;
        }
        None
    };

    let host_port = match request.direction {
        IosPortTunnelDirection::HostToDevice => reserve_loopback_port(request.host_port)?,
        IosPortTunnelDirection::DeviceToHost => {
            request.host_port.expect("validated reverse host port")
        }
    };
    let child = match request.transport {
        IosPortTunnelTransport::Iproxy => {
            spawn_usb_tunnel(&request.udid, host_port, request.device_port)?
        }
        IosPortTunnelTransport::Ssh => spawn_ssh_tunnel(
            connection.as_ref().expect("validated SSH connection"),
            request.direction,
            host_port,
            request.device_port,
        )?,
    };
    let mut pending = PendingChild::new(child);
    match (request.transport, request.direction) {
        // spawn_usb_tunnel() has already proved that the loopback listener is
        // ready. Connecting again here would send a second empty connection to
        // an arbitrary device service.
        (IosPortTunnelTransport::Iproxy, IosPortTunnelDirection::HostToDevice) => {}
        (IosPortTunnelTransport::Ssh, IosPortTunnelDirection::HostToDevice) => {
            wait_for_local_listener(pending.child_mut()?, host_port)?
        }
        (IosPortTunnelTransport::Ssh, IosPortTunnelDirection::DeviceToHost) => {
            wait_for_reverse_forward(pending.child_mut()?)?
        }
        (IosPortTunnelTransport::Iproxy, IosPortTunnelDirection::DeviceToHost) => {
            unreachable!("validated USB tunnel direction")
        }
    }
    require_running(state)?;
    if let Some(session_id) = request.session_id.as_deref() {
        // The session may have been closed while the external process was
        // authenticating. Never retain a child against a stale session.
        ios_ssh::session_snapshot(state, session_id)?;
    }

    let tunnel_id = new_tunnel_id()?;
    let mut registry = tunnel_registry(state)?;
    if registry.len() >= MAX_TUNNELS {
        return Err(ApiError::new(
            "ios_tunnel_limit",
            format!("At most {MAX_TUNNELS} managed iOS port tunnels may be active"),
        ));
    }
    if registry.contains_key(&tunnel_id) {
        return Err(ApiError::new(
            "ios_tunnel_id_collision",
            "Unable to allocate a unique iOS tunnel identifier",
        ));
    }
    let child = pending.take()?;
    let managed = ManagedIosPortTunnel {
        tunnel_id: tunnel_id.clone(),
        transport: request.transport,
        direction: request.direction,
        udid: request.udid,
        session_id: request.session_id,
        host_port,
        device_port: request.device_port,
        child,
    };
    let response = tunnel_result(&managed, true);
    registry.insert(tunnel_id, managed);
    Ok(response)
}

fn remove_ios_port_tunnel_inner(
    state: &AppState,
    request: RemoveIosPortTunnelRequest,
) -> AppResult<IosPortTunnel> {
    validate_tunnel_id(&request.tunnel_id)?;
    let _lifecycle = lifecycle_lock(state)?;
    let mut managed = tunnel_registry(state)?
        .remove(&request.tunnel_id)
        .ok_or_else(|| {
            ApiError::new(
                "ios_tunnel_not_found",
                "The managed iOS port tunnel is not active",
            )
        })?;
    ios_ssh::stop_child(&mut managed.child);
    Ok(tunnel_result(&managed, false))
}

fn validate_request(request: &CreateIosPortTunnelRequest) -> AppResult<()> {
    valid_port(request.device_port, "device")?;
    if request.host_port == Some(0) {
        return Err(ApiError::new(
            "invalid_ios_tunnel_port",
            "The host port must be between 1 and 65535",
        ));
    }
    if request.direction == IosPortTunnelDirection::DeviceToHost && request.host_port.is_none() {
        return Err(ApiError::new(
            "ios_tunnel_host_port_required",
            "A device-to-host tunnel requires the existing host service port",
        ));
    }
    validation::serial(&request.udid)?;
    if let Some(session_id) = request.session_id.as_deref() {
        validate_session_id(session_id)?;
    }
    match request.transport {
        IosPortTunnelTransport::Iproxy => {
            if request.direction != IosPortTunnelDirection::HostToDevice {
                return Err(ApiError::new(
                    "unsupported_iproxy_direction",
                    "iproxy supports only host-to-device USB forwarding",
                ));
            }
        }
        IosPortTunnelTransport::Ssh if request.session_id.is_none() => {
            return Err(ApiError::new(
                "ios_tunnel_session_required",
                "SSH port tunnels require an active iOS SSH session",
            ));
        }
        IosPortTunnelTransport::Ssh => {}
    }
    Ok(())
}

fn valid_port(port: u16, label: &str) -> AppResult<u16> {
    if port == 0 {
        Err(ApiError::new(
            "invalid_ios_tunnel_port",
            format!("The {label} port must be between 1 and 65535"),
        ))
    } else {
        Ok(port)
    }
}

fn reserve_loopback_port(requested: Option<u16>) -> AppResult<u16> {
    let port = requested.unwrap_or(0);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|error| {
        ApiError::new(
            "ios_tunnel_host_port_unavailable",
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
                "ios_tunnel_host_port_error",
                format!("Unable to inspect the allocated loopback port: {error}"),
            )
        })
}

fn spawn_iproxy_tunnel(udid: &str, host_port: u16, device_port: u16) -> AppResult<Child> {
    let executable = resolve_tool("iproxy")?;
    background_command(executable)
        .args(iproxy_args(udid, host_port, device_port))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            ApiError::new(
                "iproxy_spawn_failed",
                format!("Unable to start the managed USB port tunnel: {error}"),
            )
        })
}

pub(crate) fn spawn_usb_tunnel(udid: &str, host_port: u16, device_port: u16) -> AppResult<Child> {
    let mut go_ios_error = None;
    if let Some(executable) = verified_bundled_go_ios_forward() {
        match spawn_go_ios_tunnel(&executable, udid, host_port, device_port) {
            Ok(mut child) => match wait_for_local_listener(&mut child, host_port) {
                Ok(()) => return Ok(child),
                Err(error) => {
                    ios_ssh::stop_child(&mut child);
                    go_ios_error = Some(error);
                }
            },
            Err(error) => go_ios_error = Some(error),
        }
    }

    let fallback = match spawn_iproxy_tunnel(udid, host_port, device_port) {
        Ok(mut child) => match wait_for_local_listener(&mut child, host_port) {
            Ok(()) => return Ok(child),
            Err(error) => {
                ios_ssh::stop_child(&mut child);
                error
            }
        },
        Err(error) => error,
    };
    let Some(primary) = go_ios_error else {
        return Err(fallback);
    };
    Err(ApiError::new(
        "ios_usb_tunnel_providers_failed",
        format!(
            "The bundled go-ios tunnel and iproxy fallback both failed: {}; {}",
            primary.message, fallback.message
        ),
    )
    .with_details(serde_json::json!({
        "goIos": { "code": primary.code, "message": primary.message },
        "iproxy": { "code": fallback.code, "message": fallback.message }
    })))
}

fn verified_bundled_go_ios_forward() -> Option<PathBuf> {
    let resolved = toolchain::resolve_bundled_tool("ios").ok()?;
    if resolved.source != ToolSource::Bundled {
        return None;
    }
    let output = run_process_at(
        "ios",
        &resolved.path,
        &["version".into()],
        GO_IOS_VERSION_TIMEOUT,
        &[],
    )
    .ok()?;
    (select_usb_tunnel_provider(resolved.source, &output) == UsbTunnelProvider::BundledGoIos)
        .then_some(resolved.path)
}

fn select_usb_tunnel_provider(source: ToolSource, output: &ProcessOutput) -> UsbTunnelProvider {
    if verified_go_ios_version(source, output) {
        UsbTunnelProvider::BundledGoIos
    } else {
        UsbTunnelProvider::Iproxy
    }
}

fn verified_go_ios_version(source: ToolSource, output: &ProcessOutput) -> bool {
    source == ToolSource::Bundled
        && !output.timed_out
        && !output.truncated
        && output.exit_code == Some(0)
        && serde_json::from_str::<serde_json::Value>(output.stdout.trim())
            .ok()
            .and_then(|value| {
                value
                    .get("version")?
                    .as_str()
                    .map(|version| version == PATCHED_GO_IOS_VERSION)
            })
            .unwrap_or(false)
}

fn spawn_go_ios_tunnel(
    executable: &Path,
    udid: &str,
    host_port: u16,
    device_port: u16,
) -> AppResult<Child> {
    let mut command = background_command(executable);
    clear_ambient_go_ios_environment(&mut command);
    command
        .args(go_ios_forward_args(udid, host_port, device_port))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            ApiError::new(
                "go_ios_tunnel_spawn_failed",
                format!("Unable to start the managed go-ios USB tunnel: {error}"),
            )
        })
}

fn go_ios_forward_args(udid: &str, host_port: u16, device_port: u16) -> Vec<String> {
    vec![
        "forward".into(),
        host_port.to_string(),
        device_port.to_string(),
        format!("--udid={udid}"),
    ]
}

fn iproxy_args(udid: &str, host_port: u16, device_port: u16) -> Vec<String> {
    vec![
        "-u".into(),
        udid.into(),
        "-l".into(),
        "-s".into(),
        LOOPBACK.into(),
        format!("{host_port}:{device_port}"),
    ]
}

fn spawn_ssh_tunnel(
    connection: &IosSshConnection,
    direction: IosPortTunnelDirection,
    host_port: u16,
    device_port: u16,
) -> AppResult<Child> {
    let executable = resolve_tool("ssh")?;
    let mut args = ios_ssh::ssh_base_args(connection);
    args.extend(ssh_tunnel_args(direction, host_port, device_port));
    args.push(ios_ssh::ssh_target(connection));
    let mut command = background_command(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ios_ssh::apply_ssh_auth_environment(&mut command, connection)?;
    command.spawn().map_err(|error| {
        ApiError::new(
            "ios_ssh_tunnel_spawn_failed",
            format!("Unable to start the managed SSH port tunnel: {error}"),
        )
    })
}

fn ssh_tunnel_args(
    direction: IosPortTunnelDirection,
    host_port: u16,
    device_port: u16,
) -> Vec<String> {
    let (flag, specification) = match direction {
        IosPortTunnelDirection::HostToDevice => (
            "-L",
            format!("{LOOPBACK}:{host_port}:{LOOPBACK}:{device_port}"),
        ),
        IosPortTunnelDirection::DeviceToHost => (
            "-R",
            format!("{LOOPBACK}:{device_port}:{LOOPBACK}:{host_port}"),
        ),
    };
    vec![
        "-N".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "GatewayPorts=no".into(),
        flag.into(),
        specification,
    ]
}

fn wait_for_local_listener(child: &mut Child, host_port: u16) -> AppResult<()> {
    let started = Instant::now();
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, host_port));
    while started.elapsed() < TUNNEL_START_TIMEOUT {
        ensure_child_running(child)?;
        if TcpStream::connect_timeout(&address, Duration::from_millis(150)).is_ok() {
            return Ok(());
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err(ApiError::new(
        "ios_tunnel_start_timeout",
        "The managed loopback tunnel did not become ready within 6 seconds",
    ))
}

fn wait_for_reverse_forward(child: &mut Child) -> AppResult<()> {
    let started = Instant::now();
    while started.elapsed() < REVERSE_STABILITY_WINDOW {
        ensure_child_running(child)?;
        thread::sleep(POLL_INTERVAL);
    }
    ensure_child_running(child)
}

fn ensure_child_running(child: &mut Child) -> AppResult<()> {
    match child.try_wait().map_err(|error| {
        ApiError::new(
            "ios_tunnel_state_error",
            format!("Unable to inspect the managed tunnel: {error}"),
        )
    })? {
        Some(status) => Err(ApiError::new(
            "ios_tunnel_start_failed",
            format!("The managed tunnel exited before it was ready: {status}"),
        )),
        None => Ok(()),
    }
}

fn lifecycle_lock(state: &AppState) -> AppResult<std::sync::MutexGuard<'_, ()>> {
    state
        .ios_port_tunnel_lock
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS tunnel lifecycle lock was poisoned"))
}

fn tunnel_registry(
    state: &AppState,
) -> AppResult<std::sync::MutexGuard<'_, std::collections::HashMap<String, ManagedIosPortTunnel>>> {
    state
        .ios_port_tunnels
        .lock()
        .map_err(|_| ApiError::new("state_error", "iOS tunnel registry lock was poisoned"))
}

fn require_running(state: &AppState) -> AppResult<()> {
    if state.shutting_down.load(Ordering::Acquire) {
        Err(ApiError::new(
            "app_shutting_down",
            "Mobius is exiting and cannot change iOS port tunnels",
        ))
    } else {
        Ok(())
    }
}

fn new_tunnel_id() -> AppResult<String> {
    let mut random = [0_u8; 16];
    getrandom::getrandom(&mut random).map_err(|_| {
        ApiError::new(
            "ios_tunnel_id_unavailable",
            "Unable to create a secure iOS tunnel identifier",
        )
    })?;
    let mut id = String::with_capacity(11 + random.len() * 2);
    id.push_str("ios-tunnel-");
    for byte in random {
        write!(&mut id, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(id)
}

fn validate_tunnel_id(value: &str) -> AppResult<&str> {
    let suffix = value.strip_prefix("ios-tunnel-").ok_or_else(|| {
        ApiError::new(
            "invalid_ios_tunnel_id",
            "Invalid managed iOS tunnel identifier",
        )
    })?;
    if suffix.len() == 32
        && suffix
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        Ok(value)
    } else {
        Err(ApiError::new(
            "invalid_ios_tunnel_id",
            "Invalid managed iOS tunnel identifier",
        ))
    }
}

fn validate_session_id(value: &str) -> AppResult<&str> {
    if value.len() < 12
        || value.len() > 96
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        Err(ApiError::new(
            "invalid_ios_ssh_session_id",
            "Invalid iOS SSH session identifier",
        ))
    } else {
        Ok(value)
    }
}

fn tunnel_result(managed: &ManagedIosPortTunnel, active: bool) -> IosPortTunnel {
    IosPortTunnel {
        tunnel_id: managed.tunnel_id.clone(),
        transport: managed.transport,
        direction: managed.direction,
        udid: managed.udid.clone(),
        session_id: managed.session_id.clone(),
        bind_address: LOOPBACK.into(),
        host_port: managed.host_port,
        device_port: managed.device_port,
        pid: managed.child.id(),
        active,
    }
}

pub(crate) fn cleanup_ios_port_tunnels_for_session(
    state: &AppState,
    session_id: &str,
) -> Vec<String> {
    let _lifecycle = match state.ios_port_tunnel_lock.lock() {
        Ok(guard) => guard,
        Err(_) => return vec!["iOS tunnel lifecycle lock was poisoned".into()],
    };
    let mut registry = match state.ios_port_tunnels.lock() {
        Ok(registry) => registry,
        Err(_) => return vec!["iOS tunnel registry lock was poisoned".into()],
    };
    let ids = registry
        .iter()
        .filter(|(_, managed)| managed.session_id.as_deref() == Some(session_id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    for id in ids {
        if let Some(mut managed) = registry.remove(&id) {
            ios_ssh::stop_child(&mut managed.child);
        }
    }
    Vec::new()
}

pub(crate) fn cleanup_managed_ios_port_tunnels(state: &AppState) {
    let _lifecycle = match state.ios_port_tunnel_lock.lock() {
        Ok(guard) => guard,
        Err(_) => {
            eprintln!("Mobius cleanup: iOS tunnel lifecycle lock was poisoned");
            return;
        }
    };
    let tunnels = match state.ios_port_tunnels.lock() {
        Ok(mut registry) => registry
            .drain()
            .map(|(_, tunnel)| tunnel)
            .collect::<Vec<_>>(),
        Err(_) => {
            eprintln!("Mobius cleanup: iOS tunnel registry lock was poisoned");
            return;
        }
    };
    for mut tunnel in tunnels {
        ios_ssh::stop_child(&mut tunnel.child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        transport: IosPortTunnelTransport,
        direction: IosPortTunnelDirection,
    ) -> CreateIosPortTunnelRequest {
        CreateIosPortTunnelRequest {
            transport,
            direction,
            udid: "00008020-ABCDEF".into(),
            session_id: Some("ios-ssh-test-session".into()),
            host_port: Some(27_042),
            device_port: 27_042,
        }
    }

    #[test]
    fn ssh_forward_and_reverse_are_fixed_to_loopback() {
        let forward = ssh_tunnel_args(IosPortTunnelDirection::HostToDevice, 8_080, 9_090);
        assert!(forward
            .iter()
            .any(|value| value == "ExitOnForwardFailure=yes"));
        assert!(forward.iter().any(|value| value == "GatewayPorts=no"));
        assert!(forward.iter().any(|value| value == "-L"));
        assert!(forward
            .iter()
            .any(|value| value == "127.0.0.1:8080:127.0.0.1:9090"));

        let reverse = ssh_tunnel_args(IosPortTunnelDirection::DeviceToHost, 8_080, 9_090);
        assert!(reverse.iter().any(|value| value == "-R"));
        assert!(reverse
            .iter()
            .any(|value| value == "127.0.0.1:9090:127.0.0.1:8080"));
    }

    #[test]
    fn iproxy_arguments_bind_udid_and_host_loopback() {
        assert_eq!(
            iproxy_args("device-1", 8_080, 9_090),
            vec!["-u", "device-1", "-l", "-s", "127.0.0.1", "8080:9090"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn go_ios_forward_arguments_keep_host_and_device_port_order() {
        assert_eq!(
            go_ios_forward_args("device-1", 8_080, 9_090),
            vec!["forward", "8080", "9090", "--udid=device-1"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn go_ios_forward_requires_exact_patched_bundled_version() {
        let output = ProcessOutput {
            program: "ios".into(),
            exit_code: Some(0),
            stdout: format!("{{\"version\":\"{PATCHED_GO_IOS_VERSION}\"}}\n"),
            stderr: String::new(),
            timed_out: false,
            truncated: false,
            duration_ms: 1,
        };
        assert_eq!(
            select_usb_tunnel_provider(ToolSource::Bundled, &output),
            UsbTunnelProvider::BundledGoIos
        );
        assert_eq!(
            select_usb_tunnel_provider(ToolSource::Configured, &output),
            UsbTunnelProvider::Iproxy
        );
        assert_eq!(
            select_usb_tunnel_provider(ToolSource::Path, &output),
            UsbTunnelProvider::Iproxy
        );

        let upstream = ProcessOutput {
            stdout: "{\"version\":\"1.3.2\"}\n".into(),
            ..output.clone()
        };
        assert!(!verified_go_ios_version(ToolSource::Bundled, &upstream));

        let timed_out = ProcessOutput {
            timed_out: true,
            ..output.clone()
        };
        assert!(!verified_go_ios_version(ToolSource::Bundled, &timed_out));

        let wrapped = ProcessOutput {
            stdout: format!("unexpected\n{{\"version\":\"{PATCHED_GO_IOS_VERSION}\"}}\n"),
            ..output
        };
        assert!(!verified_go_ios_version(ToolSource::Bundled, &wrapped));
    }

    #[test]
    fn rejects_iproxy_reverse_and_incomplete_ssh_requests() {
        let reverse = request(
            IosPortTunnelTransport::Iproxy,
            IosPortTunnelDirection::DeviceToHost,
        );
        assert_eq!(
            validate_request(&reverse)
                .expect_err("iproxy reverse must fail")
                .code,
            "unsupported_iproxy_direction"
        );

        let mut ssh = request(
            IosPortTunnelTransport::Ssh,
            IosPortTunnelDirection::HostToDevice,
        );
        ssh.session_id = None;
        assert_eq!(
            validate_request(&ssh)
                .expect_err("session is required")
                .code,
            "ios_tunnel_session_required"
        );
    }

    #[test]
    fn reverse_requires_a_real_host_service_port() {
        let mut reverse = request(
            IosPortTunnelTransport::Ssh,
            IosPortTunnelDirection::DeviceToHost,
        );
        reverse.host_port = None;
        assert_eq!(
            validate_request(&reverse)
                .expect_err("host port is required")
                .code,
            "ios_tunnel_host_port_required"
        );
        reverse.host_port = Some(0);
        assert_eq!(
            validate_request(&reverse).expect_err("zero must fail").code,
            "invalid_ios_tunnel_port"
        );
    }

    #[test]
    fn serde_names_match_the_public_api() {
        assert_eq!(
            serde_json::to_value(IosPortTunnelTransport::Iproxy).expect("transport"),
            "iproxy"
        );
        assert_eq!(
            serde_json::to_value(IosPortTunnelDirection::HostToDevice).expect("direction"),
            "hostToDevice"
        );
        assert_eq!(
            serde_json::to_value(IosPortTunnelDirection::DeviceToHost).expect("direction"),
            "deviceToHost"
        );
    }
}
