use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResult<T> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    pub elapsed_ms: u64,
}

impl<T> ApiResult<T> {
    pub fn from_result(result: Result<T, ApiError>, elapsed_ms: u64) -> Self {
        match result {
            Ok(data) => Self {
                ok: true,
                data: Some(data),
                error: None,
                elapsed_ms,
            },
            Err(error) => Self {
                ok: false,
                data: None,
                error: Some(error),
                elapsed_ms,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

pub type AppResult<T> = Result<T, ApiError>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolHealth {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub purpose: String,
    pub required: bool,
    pub install_hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigureToolchainRequest {
    #[serde(default)]
    pub adb_path: Option<String>,
    #[serde(default)]
    pub scrcpy_path: Option<String>,
    #[serde(default)]
    pub frida_path: Option<String>,
    #[serde(default)]
    pub ios_tools_path: Option<String>,
    #[serde(default)]
    pub managed_tools_path: Option<String>,
    #[serde(default)]
    pub clear: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolchainConfiguration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adb_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrcpy_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frida_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ios_tools_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_tools_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidDevice {
    pub serial: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport_id: Option<String>,
    pub connection: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDevice {
    pub udid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_version: Option<String>,
    pub connection: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub os_version: String,
    pub state: String,
    pub transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rooted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jailbroken: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDeviceInfo {
    pub udid: String,
    pub properties: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IosHostDiagnosticKind {
    DeviceInfo,
    Pairing,
    Apps,
    Syslog,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosHostDiagnosticRequest {
    pub udid: String,
    pub kind: IosHostDiagnosticKind,
    #[serde(default)]
    pub network: bool,
    /// Execution window in milliseconds. Syslog intentionally runs until this
    /// bounded window expires; all other actions must finish within it.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosHostDiagnosticResult {
    pub success: bool,
    pub kind: IosHostDiagnosticKind,
    pub title: String,
    pub source: String,
    pub udid: String,
    pub network: bool,
    pub tool: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
    pub duration_ms: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IosPortTunnelTransport {
    Iproxy,
    Ssh,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IosPortTunnelDirection {
    HostToDevice,
    DeviceToHost,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateIosPortTunnelRequest {
    pub transport: IosPortTunnelTransport,
    pub direction: IosPortTunnelDirection,
    pub udid: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Host-side loopback port. Omit it for an automatically allocated port
    /// when creating a host-to-device tunnel.
    #[serde(default)]
    pub host_port: Option<u16>,
    pub device_port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveIosPortTunnelRequest {
    pub tunnel_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IosPortTunnel {
    pub tunnel_id: String,
    pub transport: IosPortTunnelTransport,
    pub direction: IosPortTunnelDirection,
    pub udid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub bind_address: String,
    pub host_port: u16,
    pub device_port: u16,
    pub pid: u32,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanEndpoint {
    /// IP or hostname only; the UI renders the port separately.
    pub address: String,
    pub port: u16,
    pub latency_ms: u64,
    pub state: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PortDirection {
    Forward,
    Reverse,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortMappingRequest {
    pub serial: String,
    pub direction: PortDirection,
    pub local: String,
    pub remote: String,
    #[serde(default = "default_true")]
    pub no_rebind: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemovePortMappingRequest {
    pub serial: String,
    pub direction: PortDirection,
    /// Forward: host-side endpoint. Reverse: device-side endpoint.
    pub local: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortMapping {
    pub serial: String,
    pub direction: PortDirection,
    pub local: String,
    pub remote: String,
    /// The endpoint expected by remove_port_mapping.request.local.
    pub remove_endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScrcpyRequest {
    pub serial: String,
    pub max_size: Option<u32>,
    pub bit_rate: Option<String>,
    #[serde(default)]
    pub turn_screen_off: bool,
    #[serde(default)]
    pub stay_awake: bool,
    #[serde(default)]
    pub no_audio: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidScreenStreamRequest {
    pub serial: String,
    pub max_size: Option<u16>,
    pub bit_rate: Option<u32>,
    pub max_fps: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidScreenStreamResult {
    pub success: bool,
    pub message: String,
    pub session_id: String,
    pub stream_url: String,
    pub serial: String,
    pub codec: String,
    pub transport: String,
    pub max_size: u16,
    pub max_fps: u8,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopAndroidScreenStreamRequest {
    pub serial: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidScreenshotRequest {
    pub serial: String,
    /// When omitted, the screenshot is kept only long enough to populate the clipboard.
    pub destination_directory: Option<String>,
    #[serde(default)]
    pub copy_to_clipboard: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidScreenshotResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    pub copied_to_clipboard: bool,
    pub size_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidScreenFrameResult {
    pub image_data_url: String,
    pub size_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub captured_at_ms: u128,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosScreenTargetRequest {
    pub udid: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosScreenCapability {
    pub available: bool,
    pub transport: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosScreenshotRequest {
    pub udid: String,
    /// When omitted, the screenshot is kept only long enough to populate the clipboard.
    pub destination_directory: Option<String>,
    #[serde(default)]
    pub copy_to_clipboard: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartAndroidScreenRecordingRequest {
    pub serial: String,
    pub destination_directory: String,
    pub bit_rate: Option<u32>,
    #[serde(default)]
    pub allow_root_fallback: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopAndroidScreenRecordingRequest {
    pub serial: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidScreenRecordingSession {
    pub success: bool,
    pub message: String,
    pub session_id: String,
    pub serial: String,
    pub started_at_ms: u64,
    pub planned_saved_path: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidScreenRecordingResult {
    pub success: bool,
    pub message: String,
    pub saved_path: String,
    pub size_bytes: u64,
    pub duration_seconds: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileEntry {
    pub name: String,
    pub path: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PullFileRequest {
    pub serial: String,
    pub remote_path: String,
    pub local_path: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushFileRequest {
    pub serial: String,
    pub local_path: String,
    pub remote_path: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteRemoteRequest {
    pub serial: String,
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidProxyRequest {
    pub serial: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadFridaRequest {
    pub serial: String,
    pub platform: Option<MobilePlatform>,
    pub local_path: String,
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartFridaRequest {
    pub serial: String,
    pub platform: Option<MobilePlatform>,
    pub remote_path: Option<String>,
    pub listen_address: Option<String>,
    /// Deprecated compatibility alias. When supplied alone, it selects both ports.
    pub port: Option<u16>,
    pub device_port: Option<u16>,
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MobilePlatform {
    Android,
    Ios,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageIcon {
    pub archive_path: String,
    pub mime_type: String,
    pub data_base64: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyzeMobilePackageRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobilePackageAnalysis {
    pub platform: MobilePlatform,
    pub path: String,
    pub file_name: String,
    pub file_size: u64,
    pub md5: String,
    pub architectures: Vec<String>,
    pub source: String,
    pub fallback_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_sdk_version: Option<String>,
    pub permissions: Vec<String>,
    pub usage_descriptions: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<PackageIcon>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallMobilePackageRequest {
    pub serial: String,
    pub platform: MobilePlatform,
    pub path: String,
    #[serde(default = "default_true")]
    pub replace: bool,
    #[serde(default)]
    pub grant_permissions: bool,
    #[serde(default)]
    pub downgrade: bool,
    #[serde(default)]
    pub allow_test_packages: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledApp {
    pub package_name: String,
    pub apk_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_code: Option<String>,
    pub system: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidAppTargetRequest {
    pub serial: String,
    pub package_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AndroidAppAction {
    Launch,
    ForceStop,
    ClearData,
    Uninstall,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidAppOperationResult {
    pub success: bool,
    pub message: String,
    pub serial: String,
    pub package_name: String,
    pub action: AndroidAppAction,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportAndroidPackageRequest {
    pub serial: String,
    pub package_name: String,
    pub destination: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedAndroidFile {
    pub kind: String,
    pub remote_path: String,
    pub local_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidPackageExport {
    pub success: bool,
    pub message: String,
    pub package_name: String,
    pub destination: String,
    pub files: Vec<ExportedAndroidFile>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FridaPortMapping {
    pub direction: String,
    pub local: String,
    pub remote: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FridaServerResult {
    pub success: bool,
    pub message: String,
    pub platform: MobilePlatform,
    pub active: bool,
    pub remote_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping: Option<FridaPortMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "lowercase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum IosSshTransport {
    Usb {
        udid: String,
        device_port: Option<u16>,
        host_port: Option<u16>,
    },
    Lan {
        host: String,
        port: Option<u16>,
    },
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IosSshAuthMode {
    #[default]
    Password,
    PrivateKey,
}

#[derive(Clone, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub(crate) fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartIosSshSessionRequest {
    pub transport: IosSshTransport,
    #[serde(default)]
    pub auth_mode: IosSshAuthMode,
    pub username: Option<String>,
    pub password: Option<SecretString>,
    pub private_key_path: Option<String>,
    pub allowed_roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosSshTunnelStatus {
    pub active: bool,
    pub pid: u32,
    pub udid: String,
    pub bind_address: String,
    pub host_port: u16,
    pub device_port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosSshSession {
    pub session_id: String,
    pub mode: String,
    pub connected: bool,
    /// This means only that the user-supplied SSH credentials were accepted.
    pub jailbreak_confirmed: bool,
    pub ssh_host: String,
    pub ssh_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_port: Option<u16>,
    pub username: String,
    pub auth_mode: IosSshAuthMode,
    pub allowed_roots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel: Option<IosSshTunnelStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosSshConnectionResult {
    pub success: bool,
    pub message: String,
    pub connected: bool,
    /// This means only that the user-supplied SSH credentials were accepted.
    pub jailbreak_confirmed: bool,
    pub auth_mode: IosSshAuthMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_active: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosSshPathRequest {
    pub session_id: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadIosSshFileRequest {
    pub session_id: String,
    pub local_path: String,
    pub remote_path: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadIosSshFileRequest {
    pub session_id: String,
    pub remote_path: String,
    pub local_path: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteIosSshRequest {
    pub session_id: String,
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadIosFridaServerRequest {
    pub session_id: String,
    pub local_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartIosFridaServerRequest {
    pub session_id: String,
    pub device_port: Option<u16>,
    pub host_port: Option<u16>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StopIosFridaServerRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosFridaServerResult {
    pub success: bool,
    pub message: String,
    pub session_id: String,
    pub active: bool,
    pub remote_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunnel_active: Option<bool>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IosPackageInstallerId {
    Appinst,
    Ipainstaller,
}

impl IosPackageInstallerId {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Appinst => "appinst",
            Self::Ipainstaller => "ipainstaller",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosPackageInstaller {
    pub id: IosPackageInstallerId,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeIosAppCapabilitiesRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosAppCapabilities {
    pub session_id: String,
    pub root_session: bool,
    pub installers: Vec<IosPackageInstaller>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_installer: Option<IosPackageInstaller>,
    pub listing_available: bool,
    pub export_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plutil_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plutil_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base64_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tar_path: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallIosPackageSshRequest {
    pub session_id: String,
    pub path: String,
    pub installer_id: Option<IosPackageInstallerId>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosPackageInstallResult {
    pub success: bool,
    pub message: String,
    pub session_id: String,
    pub installer: IosPackageInstaller,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    pub remote_temporary_path: String,
    pub temporary_file_cleaned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IosInstalledAppScope {
    #[default]
    All,
    User,
    System,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListIosInstalledAppsRequest {
    pub session_id: String,
    #[serde(default)]
    pub scope: IosInstalledAppScope,
    pub limit: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosInstalledApp {
    pub bundle_id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_version: Option<String>,
    pub app_path: String,
    pub system: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportIosAppBundleRequest {
    pub session_id: String,
    pub bundle_id: String,
    pub app_path: String,
    pub destination: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosAppExportResult {
    pub success: bool,
    pub message: String,
    pub session_id: String,
    pub bundle_id: String,
    pub app_path: String,
    pub local_path: String,
    pub format: String,
    pub size_bytes: u64,
    pub installable: bool,
    pub encryption_status: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IosDiagnosticKind {
    Overview,
    Processes,
    Tools,
    Syslog,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosDiagnosticRequest {
    pub session_id: String,
    pub kind: IosDiagnosticKind,
    #[serde(default)]
    pub syslog_lines: Option<u16>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IosDiagnosticToolStatus {
    pub id: String,
    pub name: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<bool>,
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDiagnosticResult {
    pub success: bool,
    pub kind: IosDiagnosticKind,
    pub title: String,
    pub output: String,
    pub truncated: bool,
    pub source: String,
    pub tools: Vec<IosDiagnosticToolStatus>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IosDeviceAction {
    Respring,
    Reboot,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareIosDeviceActionRequest {
    pub session_id: String,
    pub action: IosDeviceAction,
    pub expected_ssh_host: String,
    pub expected_ssh_port: u16,
    pub expected_username: String,
    #[serde(default)]
    pub expected_server_system: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IosDeviceActionTarget {
    pub ssh_host: String,
    pub ssh_port: u16,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_system: Option<String>,
    pub host_key_identity: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDeviceActionConfirmation {
    pub success: bool,
    pub confirmation_id: String,
    pub session_id: String,
    pub action: IosDeviceAction,
    pub target: IosDeviceActionTarget,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosDeviceActionRequest {
    pub confirmation_id: String,
    pub session_id: String,
    pub action: IosDeviceAction,
    pub expected_ssh_host: String,
    pub expected_ssh_port: u16,
    pub expected_username: String,
    #[serde(default)]
    pub expected_server_system: Option<String>,
    pub expected_host_key_identity: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IosDeviceActionResult {
    pub success: bool,
    pub action: IosDeviceAction,
    pub message: String,
    pub accepted: bool,
}
