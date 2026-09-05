use std::{
    collections::HashMap,
    net::TcpStream,
    path::PathBuf,
    process::Child,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::Instant,
};

use crate::models::{
    IosDeviceAction, IosPortTunnelDirection, IosPortTunnelTransport, IosSshAuthMode, PortDirection,
    SecretString,
};

#[derive(Debug, Clone)]
pub(crate) struct ManagedFridaProcess {
    pub pid: u32,
    pub remote_path: String,
    pub start_time: String,
    pub use_su: bool,
    pub listen_address: String,
    pub device_port: u16,
    pub host_port: u16,
    pub forward_local_endpoint: String,
    pub forward_remote_endpoint: String,
    pub forward_owned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedPortMapping {
    pub serial: String,
    pub direction: PortDirection,
    pub remove_endpoint: String,
    pub expected_remote: String,
    pub owner: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AndroidProxySettings {
    /// Deprecated trigger watched by ConnectivityService. Writing this value also
    /// updates the four canonical global-proxy fields and broadcasts the change.
    pub http_proxy: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub exclusion_list: Option<String>,
    pub pac_url: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedProxy {
    pub previous: AndroidProxySettings,
    pub configured: AndroidProxySettings,
}

#[derive(Debug, Clone)]
pub(crate) enum IosSshAuthentication {
    Password(SecretString),
    PrivateKey(PathBuf),
}

impl IosSshAuthentication {
    pub(crate) fn mode(&self) -> IosSshAuthMode {
        match self {
            Self::Password(_) => IosSshAuthMode::Password,
            Self::PrivateKey(_) => IosSshAuthMode::PrivateKey,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IosSshConnection {
    pub ssh_host: String,
    pub ssh_port: u16,
    pub device_port: Option<u16>,
    pub username: String,
    pub authentication: IosSshAuthentication,
    pub known_hosts_path: PathBuf,
    pub host_key_alias: Option<String>,
    /// Stable user-facing roots such as `/var/mobile`.
    pub configured_roots: Vec<String>,
    /// Physical roots returned by `pwd -P`, used for boundary enforcement.
    pub allowed_roots: Vec<String>,
    pub server_system: Option<String>,
    pub remote_uid: Option<u32>,
}

#[derive(Debug)]
pub(crate) struct ManagedIosSshSession {
    pub connection: IosSshConnection,
    pub tunnel: Option<Child>,
    pub ios_frida_upload: Option<ManagedIosFridaUpload>,
    pub ios_frida_process: Option<ManagedIosFridaProcess>,
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedIosFridaUpload {
    pub remote_path: String,
}

#[derive(Debug)]
pub(crate) struct ManagedIosFridaProcess {
    pub pid: u32,
    pub remote_path: String,
    pub log_path: String,
    pub device_port: u16,
    pub host_port: u16,
    pub tunnel: Option<Child>,
}

#[derive(Debug)]
pub(crate) struct ManagedIosPortTunnel {
    pub tunnel_id: String,
    pub transport: IosPortTunnelTransport,
    pub direction: IosPortTunnelDirection,
    pub udid: String,
    pub session_id: Option<String>,
    pub host_port: u16,
    pub device_port: u16,
    pub child: Child,
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedIosActionConfirmation {
    pub confirmation_id: String,
    pub session_id: String,
    pub action: IosDeviceAction,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub username: String,
    pub server_system: Option<String>,
    pub host_key_identity: String,
    pub expires_at: Instant,
}

#[derive(Clone)]
pub(crate) struct ManagedAndroidScreenStream {
    pub session_id: String,
    pub serial: String,
    pub scid: String,
    pub remote_path: String,
    pub adb_path: PathBuf,
    pub stop: Arc<AtomicBool>,
    pub input_socket: Arc<Mutex<Option<TcpStream>>>,
    pub server_process: Arc<Mutex<Option<Child>>>,
    pub transcoder_process: Arc<Mutex<Option<Child>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedAndroidScreenRecording {
    pub session_id: String,
    pub serial: String,
    pub pid: u32,
    pub executable: String,
    pub process_start_time: String,
    pub remote_path: String,
    pub remote_log_path: String,
    pub local_path: PathBuf,
    pub use_su: bool,
    pub screen_was_woken: bool,
    pub started_at: Instant,
    pub warnings: Vec<String>,
}

#[derive(Clone, Default)]
pub(crate) struct AppState {
    pub screen_streams: Arc<Mutex<HashMap<String, ManagedAndroidScreenStream>>>,
    /// Keeps embedded-screen start, stop, and shutdown cleanup ordered. The UI owns one
    /// active preview, so a single lifecycle lock also prevents an older slow start from
    /// replacing a newer reconnect and leaving either process outside the state table.
    pub screen_stream_lifecycle_lock: Arc<Mutex<()>>,
    pub screen_recordings: Arc<Mutex<HashMap<String, ManagedAndroidScreenRecording>>>,
    /// Orders recording start, explicit stop, device-switch cleanup, and application exit.
    pub screen_recording_lifecycle_lock: Arc<Mutex<()>>,
    pub frida_processes: Arc<Mutex<HashMap<String, ManagedFridaProcess>>>,
    pub proxies: Arc<Mutex<HashMap<String, ManagedProxy>>>,
    pub port_mappings: Arc<Mutex<Vec<ManagedPortMapping>>>,
    pub ios_ssh_sessions: Arc<Mutex<HashMap<String, ManagedIosSshSession>>>,
    pub ios_port_tunnels: Arc<Mutex<HashMap<String, ManagedIosPortTunnel>>>,
    /// Orders iOS tunnel creation/removal with SSH-session and application cleanup.
    pub ios_port_tunnel_lock: Arc<Mutex<()>>,
    pub ios_action_confirmations: Arc<Mutex<HashMap<String, ManagedIosActionConfirmation>>>,
    /// Serializes iOS instrumentation lifecycle changes with SSH-session teardown.
    pub ios_frida_lock: Arc<Mutex<()>>,
    pub cleanup_lock: Arc<Mutex<()>>,
    pub shutting_down: Arc<AtomicBool>,
}
