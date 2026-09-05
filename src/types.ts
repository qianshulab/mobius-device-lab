export type PageKey = "devices" | "apps" | "files" | "network" | "debug" | "settings";
export type DevicePlatform = "android" | "ios";
export type DeviceState = "online" | "offline" | "unauthorized" | "connecting" | "registered";

export interface Device {
  id: string;
  name: string;
  platform: DevicePlatform;
  osVersion: string;
  state: DeviceState;
  transport: "usb" | "wifi" | "emulator" | "usbmux";
  address?: string;
  model?: string;
  architecture?: string;
  battery?: number;
  rooted?: boolean;
  jailbroken?: boolean;
  product?: string;
  /** Frontend-only origin marker used for user-registered LAN SSH endpoints. */
  connectionSource?: "discovered" | "manual";
}

export type ToolState = "ready" | "missing" | "warning";

export interface ToolHealth {
  id: string;
  name: string;
  version?: string;
  state: ToolState;
  path?: string;
  hint?: string;
  source?: "configured" | "bundled" | "sdk" | "path";
  purpose?: string;
  required?: boolean;
  installHint?: string;
}

export interface ToolchainConfiguration {
  adbPath?: string;
  scrcpyPath?: string;
  fridaPath?: string;
  iosToolsPath?: string;
  managedToolsPath?: string;
}

export interface ActivityItem {
  id: string;
  title: string;
  detail: string;
  status: "success" | "warning" | "error" | "running" | "info";
  at: string;
}

export interface OperationResult {
  success: boolean;
  message: string;
  stdout?: string;
  stderr?: string;
  pid?: number;
}

export interface PortMapping {
  id?: string;
  serial: string;
  direction: "forward" | "reverse" | "iproxy";
  local: string;
  remote: string;
  removeEndpoint?: string;
  status?: "active" | "inactive";
  createdAt?: string;
}

export type IosPortTunnelTransport = "iproxy" | "ssh";
export type IosPortTunnelDirection = "hostToDevice" | "deviceToHost";

export interface IosPortTunnel {
  tunnelId: string;
  udid: string;
  sessionId?: string;
  transport: IosPortTunnelTransport;
  direction: IosPortTunnelDirection;
  bindAddress: string;
  hostPort: number;
  devicePort: number;
  pid: number;
  active: boolean;
}

export interface CreateIosPortTunnelRequest {
  udid: string;
  sessionId?: string;
  transport: IosPortTunnelTransport;
  direction: IosPortTunnelDirection;
  hostPort: number;
  devicePort: number;
}

export interface ScanResult {
  address: string;
  port: number;
  latencyMs: number;
  state: "open" | "adb" | "unreachable";
}

export interface RemoteFile {
  name: string;
  path: string;
  kind: "file" | "directory" | "link" | "unknown";
  size?: number;
  modified?: string;
  permissions?: string;
  owner?: string;
  group?: string;
  linkTarget?: string;
}

export interface AppSettings {
  adbPath: string;
  scrcpyPath: string;
  fridaPath: string;
  iosToolsPath: string;
  managedToolsPath: string;
  mediaDirectory: string;
  appExportDirectory: string;
  scanCidr: string;
  scanPort: string;
  proxyHost: string;
  proxyPort: string;
  operationConfirmations: boolean;
  redactLogs: boolean;
  compactMode: boolean;
}

export type PackagePlatform = "android" | "ios";

export interface PackagePermission {
  name: string;
  label?: string;
  description?: string;
  risk?: "normal" | "sensitive" | "dangerous" | "unknown";
  usageDescription?: string;
}

export interface PackageComponent {
  kind: "activity" | "service" | "receiver" | "provider" | "extension" | "url-scheme" | "other";
  name: string;
  exported?: boolean;
  permission?: string;
}

export interface PackageSignature {
  scheme?: string;
  subject?: string;
  issuer?: string;
  serialNumber?: string;
  sha256?: string;
}

export interface PackageAnalysis {
  path: string;
  fileName: string;
  platform: PackagePlatform;
  fileSize: number;
  md5: string;
  packageName: string;
  appName: string;
  versionName?: string;
  versionCode?: string;
  minOsVersion?: string;
  targetOsVersion?: string;
  architectures: string[];
  permissions: PackagePermission[];
  components?: PackageComponent[];
  signature?: PackageSignature;
  iconDataUrl?: string;
  encrypted?: boolean;
  debuggable?: boolean;
  warnings: string[];
  source?: string;
  fallbackUsed?: boolean;
  previewOnly?: boolean;
}

export interface InstalledApp {
  packageName: string;
  appName?: string;
  versionName?: string;
  versionCode?: string;
  system?: boolean;
  debuggable?: boolean;
  paths?: string[];
}

export type AndroidAppAction = "launch" | "forceStop" | "clearData" | "uninstall";

export interface AndroidAppOperationResult {
  success: boolean;
  message: string;
  serial: string;
  packageName: string;
  action: AndroidAppAction;
}

export type IosPackageInstallerId = "appinst" | "ipainstaller";

export interface IosPackageInstaller {
  id: IosPackageInstallerId;
  name: string;
  path: string;
}

export interface IosAppCapabilities {
  sessionId: string;
  rootSession: boolean;
  installers: IosPackageInstaller[];
  preferredInstaller?: IosPackageInstaller;
  listingAvailable: boolean;
  exportAvailable: boolean;
  plutilPath?: string;
  plutilMode?: "extract" | "key";
  base64Path?: string;
  tarPath?: string;
  warnings: string[];
}

export interface IosInstalledApp {
  bundleId: string;
  displayName: string;
  versionName?: string;
  buildVersion?: string;
  appPath: string;
  system: boolean;
}

export interface IosPackageInstallResult extends OperationResult {
  sessionId: string;
  installer: IosPackageInstaller;
  packageName?: string;
  remoteTemporaryPath: string;
  temporaryFileCleaned: boolean;
  warnings: string[];
}

export interface IosAppExportResult extends OperationResult {
  sessionId: string;
  bundleId: string;
  appPath: string;
  localPath: string;
  format: "analysisTarGz";
  sizeBytes: number;
  installable: false;
  encryptionStatus: "unknown";
  warnings: string[];
}

export interface PackageTransferResult extends OperationResult {
  files?: string[];
}

export interface FridaPortMapping {
  direction: "forward";
  local: string;
  remote: string;
}

export interface FridaServerResult {
  success: boolean;
  message: string;
  platform: PackagePlatform;
  active: boolean;
  remotePath: string;
  pid?: number;
  listenAddress?: string;
  devicePort?: number;
  hostPort?: number;
  mapping?: FridaPortMapping;
  stdout?: string;
  stderr?: string;
}

export interface IosFridaServerResult {
  success: boolean;
  message: string;
  sessionId: string;
  active: boolean;
  remotePath: string;
  pid?: number;
  listenAddress?: string;
  devicePort?: number;
  hostPort?: number;
  tunnelPid?: number;
  tunnelActive?: boolean;
}

export type IosSshTransport =
  | { mode: "usb"; udid: string; devicePort?: number; hostPort?: number }
  | { mode: "lan"; host: string; port?: number };

export type IosSshAuthMode = "password" | "privateKey";

export interface IosSshSessionRequest {
  transport: IosSshTransport;
  authMode: IosSshAuthMode;
  username: string;
  password?: string;
  privateKeyPath?: string;
  allowedRoots: string[];
}

export interface IosSshTunnel {
  active: boolean;
  pid: number;
  udid: string;
  bindAddress: string;
  hostPort: number;
  devicePort: number;
}

export interface IosSshSession {
  sessionId: string;
  mode: "usb" | "lan";
  connected: boolean;
  jailbreakConfirmed: boolean;
  sshHost: string;
  sshPort: number;
  devicePort?: number;
  username: string;
  authMode: IosSshAuthMode;
  allowedRoots: string[];
  serverSystem?: string;
  remoteUid?: number;
  tunnel?: IosSshTunnel;
}

export interface IosSshConnectionTest {
  success: boolean;
  message: string;
  connected?: boolean;
  jailbreakConfirmed?: boolean;
  authMode?: IosSshAuthMode;
  serverSystem?: string;
  remoteUid?: number;
  tunnelActive?: boolean;
}

export type IosDiagnosticKind = "overview" | "processes" | "tools" | "syslog";

export type IosHostDiagnosticKind = "deviceInfo" | "pairing" | "apps" | "syslog";

export interface IosHostDiagnosticResult {
  success: boolean;
  kind: IosHostDiagnosticKind;
  title: string;
  output: string;
  source: string;
  truncated: boolean;
  warnings: string[];
}

export interface IosDiagnosticToolStatus {
  id: string;
  name: string;
  available: boolean;
  path?: string;
  version?: string;
  running?: boolean;
  purpose: string;
}

export interface IosDiagnosticResult {
  success: boolean;
  kind: IosDiagnosticKind;
  title: string;
  output: string;
  truncated: boolean;
  source: string;
  tools: IosDiagnosticToolStatus[];
  warnings: string[];
}

export type IosDeviceAction = "respring" | "reboot";

export interface IosDeviceActionTarget {
  sshHost: string;
  sshPort: number;
  username: string;
  serverSystem?: string;
  hostKeyIdentity: string;
}

export interface IosDeviceActionConfirmation {
  success: boolean;
  confirmationId: string;
  sessionId: string;
  action: IosDeviceAction;
  target: IosDeviceActionTarget;
  expiresInSeconds: number;
}

export interface IosDeviceActionResult {
  success: boolean;
  action: IosDeviceAction;
  message: string;
  accepted: boolean;
}

export interface MediaCaptureResult extends OperationResult {
  savedPath?: string;
  copiedToClipboard?: boolean;
  durationSeconds?: number;
  sizeBytes?: number;
  width?: number;
  height?: number;
  warnings?: string[];
}

export interface AndroidScreenRecordingSession {
  success: boolean;
  message: string;
  sessionId: string;
  serial: string;
  startedAtMs: number;
  plannedSavedPath: string;
  warnings: string[];
}

export interface ScreenFrame {
  imageDataUrl: string;
  sizeBytes: number;
  width: number;
  height: number;
  capturedAtMs: number;
}

export interface AndroidScreenStream {
  success: boolean;
  message: string;
  sessionId: string;
  streamUrl: string;
  serial: string;
  codec: string;
  transport: string;
  maxSize: number;
  maxFps: number;
  width?: number;
  height?: number;
}

export interface IosScreenCapability {
  available: boolean;
  transport: "usb" | "network" | "unavailable";
  message: string;
}


export interface ToastMessage {
  id: number;
  type: "success" | "error" | "info" | "warning";
  title: string;
  detail?: string;
}
