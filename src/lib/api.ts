import SparkMD5 from "spark-md5";
import type { SelectedPackageFile } from "./dialog";
import type { AndroidAppAction, AndroidAppOperationResult, AndroidScreenRecordingSession, AndroidScreenStream, CreateIosPortTunnelRequest, Device, FridaServerResult, InstalledApp, IosAppCapabilities, IosAppExportResult, IosDeviceAction, IosDeviceActionConfirmation, IosDeviceActionResult, IosDiagnosticKind, IosDiagnosticResult, IosFridaServerResult, IosHostDiagnosticKind, IosHostDiagnosticResult, IosInstalledApp, IosPackageInstallResult, IosPackageInstallerId, IosPortTunnel, IosScreenCapability, IosSshConnectionTest, IosSshSession, IosSshSessionRequest, MediaCaptureResult, OperationResult, PackageAnalysis, PackagePermission, PackageTransferResult, PortMapping, RemoteFile, ScanResult, ScreenFrame, ToolchainConfiguration, ToolHealth } from "../types";

const isTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const mockDevices: Device[] = [
  {
    id: "emulator-5554",
    name: "Pixel 8 Lab",
    platform: "android",
    osVersion: "14",
    state: "online",
    transport: "emulator",
    model: "Pixel 8",
    architecture: "arm64-v8a",
    battery: 86,
    rooted: true,
    product: "shiba",
  },
  {
    id: "00008110-001234567890801E",
    name: "iPhone 14 Lab",
    platform: "ios",
    osVersion: "16.7",
    state: "online",
    transport: "usbmux",
    model: "iPhone14,7",
    architecture: "arm64",
  },
];

const mockTool = (tool: ToolHealth): ToolHealth => ({ purpose: "设备开发能力", required: false, installHint: "可在设置中选择已安装工具。", ...tool });
const mockTools: ToolHealth[] = [
  mockTool({ id: "adb", name: "Android Debug Bridge", version: "36.0.2", state: "ready", path: "/opt/homebrew/bin/adb", source: "path", required: true }),
  mockTool({ id: "scrcpy", name: "scrcpy", version: "4.0", state: "ready", path: "/opt/homebrew/bin/scrcpy", source: "path" }),
  mockTool({ id: "ffmpeg", name: "FFmpeg", version: "8.1.1", state: "ready", path: "/opt/homebrew/bin/ffmpeg", source: "path" }),
  mockTool({ id: "frida", name: "Frida CLI", state: "warning", hint: "客户端版本未加入当前工具目录" }),
  mockTool({ id: "aapt2", name: "Android Asset Packaging Tool", version: "35.0.0", state: "ready", path: "/Android/sdk/build-tools/35.0.0/aapt2", source: "sdk" }),
  mockTool({ id: "apkanalyzer", name: "Android APK Analyzer", version: "35.0.0", state: "ready", path: "/Android/sdk/cmdline-tools/latest/bin/apkanalyzer", source: "sdk" }),
  mockTool({ id: "idevice_id", name: "iOS Device Discovery", version: "1.3.0", state: "ready", path: "/opt/homebrew/bin/idevice_id", source: "path", required: true }),
  mockTool({ id: "ideviceinfo", name: "iOS Device Info", version: "1.3.0", state: "ready", path: "/opt/homebrew/bin/ideviceinfo", source: "path", required: true }),
  mockTool({ id: "idevicepair", name: "iOS Pairing", version: "1.3.0", state: "ready", path: "/opt/homebrew/bin/idevicepair", source: "path" }),
  mockTool({ id: "idevicesyslog", name: "iOS Syslog Relay", version: "1.3.0", state: "ready", path: "/opt/homebrew/bin/idevicesyslog", source: "path" }),
  mockTool({ id: "ideviceinstaller", name: "iOS Package Installer", version: "1.1.1", state: "warning", hint: "未检测到；只影响 IPA 安装" }),
  mockTool({ id: "idevicescreenshot", name: "iOS Screenshot Service Client", version: "1.4.0", state: "ready", path: "/opt/homebrew/bin/idevicescreenshot", source: "path" }),
  mockTool({ id: "iproxy", name: "USB Port Tunnel", version: "1.1.1", state: "ready", path: "/opt/homebrew/bin/iproxy", source: "path" }),
  mockTool({ id: "ssh", name: "OpenSSH Client", version: "OpenSSH_9.9", state: "ready", path: "/usr/bin/ssh", source: "path", required: true }),
  mockTool({ id: "scp", name: "OpenSSH Secure Copy", state: "ready", path: "/usr/bin/scp", source: "path", required: true }),
];

const mockPackage: PackageAnalysis = {
  path: "/Users/demo/Downloads/sample.apk",
  fileName: "sample.apk",
  platform: "android",
  fileSize: 24_912_832,
  md5: "5f4dcc3b5aa765d61d8327deb882cf99",
  packageName: "com.example.notes",
  appName: "Sample Notes",
  versionName: "4.8.1",
  versionCode: "4080102",
  minOsVersion: "26",
  targetOsVersion: "35",
  architectures: ["arm64-v8a", "armeabi-v7a"],
  permissions: [
    { name: "android.permission.INTERNET", label: "网络访问", risk: "normal" },
    { name: "android.permission.CAMERA", label: "相机", risk: "sensitive" },
    { name: "android.permission.ACCESS_FINE_LOCATION", label: "精确位置", risk: "dangerous" },
  ],
  iconDataUrl: "/brand/mobius-mark.png",
  debuggable: false,
  warnings: [],
  source: "aapt2",
  fallbackUsed: false,
};

const mockInstalledApps: InstalledApp[] = [
  { packageName: "com.example.calendar", appName: "Sample Calendar", versionName: "4.8.1", versionCode: "4080102", system: false, debuggable: false, paths: ["/data/app/~~demo/base.apk"] },
  { packageName: "com.example.notes", appName: "Notes Lab", versionName: "2.3.0", versionCode: "230", system: false, debuggable: true, paths: ["/data/app/~~notes/base.apk"] },
];

const mockRecordingSessions = new Map<string, { startedAtMs: number; destinationDirectory: string }>();
const mockIosPortTunnels = new Map<string, IosPortTunnel>();

interface WirePackageAnalysis {
  platform: "android" | "ios";
  path: string;
  fileName: string;
  fileSize: number;
  md5: string;
  source: string;
  fallbackUsed: boolean;
  packageName?: string;
  displayName?: string;
  versionName?: string;
  versionCode?: string;
  minimumOsVersion?: string;
  targetSdkVersion?: string;
  permissions: string[];
  usageDescriptions: Record<string, string>;
  architectures?: string[];
  icon?: { archivePath: string; mimeType: string; dataBase64: string; sizeBytes: number };
  warnings: string[];
}

interface WireInstalledApp {
  packageName: string;
  apkPath: string;
  uid?: number;
  versionCode?: string;
  system: boolean;
}

interface WireAndroidExport {
  success: boolean;
  message: string;
  packageName: string;
  destination: string;
  files: Array<{ kind: string; remotePath: string; localPath: string; sizeBytes?: number }>;
  warnings: string[];
}

function permissionRisk(name: string): PackagePermission["risk"] {
  if (/(CAMERA|LOCATION|CONTACTS|CALENDAR|MICROPHONE|RECORD_AUDIO|SMS|CALL_LOG|PHONE|BLUETOOTH_CONNECT|BODY_SENSORS|USAGE_STATS|MANAGE_EXTERNAL_STORAGE)/i.test(name)) return "dangerous";
  if (/(STORAGE|MEDIA_|NOTIFICATION|BIOMETRIC|FACE_ID|TRACKING|PHOTO|LOCAL_NETWORK)/i.test(name)) return "sensitive";
  return "normal";
}

function normalizePackage(raw: WirePackageAnalysis): PackageAnalysis {
  const manifestPermissions = raw.permissions.map((name) => ({ name, label: name.split(".").pop()?.replaceAll("_", " "), risk: permissionRisk(name) }));
  const privacyPermissions = Object.entries(raw.usageDescriptions).map(([name, usageDescription]) => ({ name, label: name.replace(/^NS/, "").replace(/UsageDescription$/, ""), usageDescription, risk: "sensitive" as const }));
  return {
    path: raw.path,
    fileName: raw.fileName,
    platform: raw.platform,
    fileSize: raw.fileSize,
    md5: raw.md5,
    packageName: raw.packageName ?? "未识别",
    appName: raw.displayName ?? raw.fileName,
    versionName: raw.versionName,
    versionCode: raw.versionCode,
    minOsVersion: raw.minimumOsVersion,
    targetOsVersion: raw.targetSdkVersion,
    architectures: raw.architectures ?? [],
    permissions: [...manifestPermissions, ...privacyPermissions],
    iconDataUrl: raw.icon ? `data:${raw.icon.mimeType};base64,${raw.icon.dataBase64}` : undefined,
    warnings: raw.warnings,
    source: raw.source,
    fallbackUsed: raw.fallbackUsed,
  };
}

async function md5BrowserFile(file: File) {
  const digest = new SparkMD5.ArrayBuffer();
  const chunkSize = 4 * 1024 * 1024;
  for (let offset = 0; offset < file.size; offset += chunkSize) {
    digest.append(await file.slice(offset, Math.min(offset + chunkSize, file.size)).arrayBuffer());
  }
  return digest.end();
}

async function previewPackage(selection: SelectedPackageFile): Promise<PackageAnalysis> {
  const platform = selection.name.toLowerCase().endsWith(".ipa") ? "ios" as const : "android" as const;
  const appName = selection.name.replace(/\.(apk|ipa)$/i, "") || selection.name;
  const md5 = selection.browserFile ? await md5BrowserFile(selection.browserFile) : "仅桌面版计算";
  return {
    path: selection.path,
    fileName: selection.name,
    platform,
    fileSize: selection.size ?? 0,
    md5,
    packageName: "等待桌面版解析",
    appName,
    architectures: [],
    permissions: [],
    warnings: ["当前是浏览器界面预览：已读取真实文件名、大小和 MD5，但 APK/IPA 清单解析与安装只在 Mobius 桌面应用中执行。"],
    source: "browserPreview",
    fallbackUsed: true,
    previewOnly: true,
  };
}

async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  const response = await invoke<T | { ok: boolean; data?: T; error?: string | { code?: string; message?: string; details?: string } | null; elapsedMs?: number }>(command, args);
  if (response && typeof response === "object" && "ok" in response && "data" in response) {
    if (!response.ok) {
      const detail = typeof response.error === "string" ? response.error : response.error?.message;
      throw new Error(detail || `${command} 执行失败`);
    }
    return response.data as T;
  }
  if (response && typeof response === "object" && "ok" in response && !response.ok) {
    const detail = typeof response.error === "string" ? response.error : response.error?.message;
    throw new Error(detail || `${command} 执行失败`);
  }
  return response as T;
}

async function call<T>(command: string, args: Record<string, unknown> | undefined, fallback: () => T | Promise<T>): Promise<T> {
  if (!isTauri()) {
    await new Promise((resolve) => setTimeout(resolve, 180));
    return fallback();
  }
  return invokeCommand<T>(command, args);
}

const ok = (message: string): OperationResult => ({ success: true, message });

const mockAndroidAppAction = (serial: string, packageName: string, action: AndroidAppAction, message: string): AndroidAppOperationResult => ({
  success: true,
  message,
  serial,
  packageName,
  action,
});

export const api = {
  configureToolchain: (configuration: ToolchainConfiguration & { clear?: boolean }) => call<ToolchainConfiguration>("configure_toolchain", { request: configuration }, () => configuration),
  toolHealth: () => call<ToolHealth[]>("get_tool_health", undefined, () => mockTools),
  devices: () => call<Device[]>("list_devices", undefined, () => mockDevices),
  connect: (address: string) => call<OperationResult>("adb_connect", { address }, () => ok(`已连接 ${address}`)),
  pair: (address: string, code: string) => call<OperationResult>("adb_pair", { address, code }, () => ok(`已配对 ${address}`)),
  scan: (cidr: string | undefined, ports: number[]) =>
    call<ScanResult[]>("scan_adb_subnet", { ...(cidr ? { cidr } : {}), ports }, () => [
      { address: (cidr ?? "192.168.1.0/24").replace(/\.0\/24$/, ".42"), port: ports[0] ?? 5555, latencyMs: 12, state: "adb" },
    ]),
  mappings: (serial: string) => call<PortMapping[]>("list_port_mappings", { serial }, () => []),
  createMapping: (mapping: PortMapping) =>
    call<OperationResult>("create_port_mapping", { request: mapping }, () => ok(`已创建 ${mapping.direction} 映射`)),
  removeMapping: (mapping: PortMapping) =>
    call<OperationResult>("remove_port_mapping", { request: { serial: mapping.serial, direction: mapping.direction, local: mapping.removeEndpoint ?? mapping.local } }, () => ok("映射已移除")),
  launchScrcpy: (serial: string, options = {}) =>
    call<OperationResult>("launch_scrcpy", { request: { serial, ...options } }, () => ({ ...ok("scrcpy 已启动"), pid: 18420 })),
  shell: (serial: string, command: string) =>
    call<OperationResult>("run_device_shell", { serial, command }, () => ({ ...ok("命令执行完成"), stdout: `$ ${command}\nMock output from ${serial}\n` })),
  files: (serial: string, path: string) =>
    call<RemoteFile[]>("list_remote_files", { serial, path }, () => [
      { name: "Download", path: `${path.replace(/\/$/, "")}/Download`, kind: "directory", permissions: "drwxrwx---", owner: "shell" },
      { name: "Pictures", path: `${path.replace(/\/$/, "")}/Pictures`, kind: "directory", permissions: "drwxrwx---", owner: "shell" },
      { name: "screenshot.png", path: `${path.replace(/\/$/, "")}/screenshot.png`, kind: "file", size: 5_638_144, modified: "2026-09-04 21:42", permissions: "-rw-rw----", owner: "shell" },
    ]),
  mkdir: (serial: string, path: string) => call<OperationResult>("mkdir_remote", { serial, path }, () => ok(`已创建 ${path}`)),
  deleteFile: (serial: string, path: string, recursive = false) =>
    call<OperationResult>("delete_remote", { request: { serial, path, recursive } }, () => ok(`已删除 ${path}`)),
  pullFile: (serial: string, remotePath: string, localPath: string, overwrite = false) =>
    call<OperationResult>("pull_file", { request: { serial, remotePath, localPath, overwrite } }, () => ok("下载完成")),
  pushFile: (serial: string, localPath: string, remotePath: string, overwrite = false) =>
    call<OperationResult>("push_file", { request: { serial, localPath, remotePath, overwrite } }, () => ok("上传完成")),
  setProxy: (serial: string, host: string, port: number) =>
    call<OperationResult>("set_android_proxy", { request: { serial, host, port } }, () => ok(`代理已设为 ${host}:${port}`)),
  clearProxy: (serial: string) => call<OperationResult>("clear_android_proxy", { serial }, () => ok("系统代理已恢复")),
  analyzePackage: async (selection: SelectedPackageFile) => {
    if (!isTauri()) return previewPackage(selection);
    return normalizePackage(await invokeCommand<WirePackageAnalysis>("analyze_mobile_package", { request: { path: selection.path } }));
  },
  installedApps: async (serial: string) => {
    if (!isTauri()) return call<InstalledApp[]>("list_installed_apps", undefined, () => mockInstalledApps);
    const apps = await invokeCommand<WireInstalledApp[]>("list_installed_apps", { serial });
    return apps.map((app) => ({ packageName: app.packageName, versionCode: app.versionCode, system: app.system, paths: [app.apkPath] }));
  },
  launchAndroidApp: (serial: string, packageName: string) =>
    call<AndroidAppOperationResult>("launch_android_app", { request: { serial, packageName } }, () => mockAndroidAppAction(serial, packageName, "launch", "Android 应用已启动")),
  forceStopAndroidApp: (serial: string, packageName: string) =>
    call<AndroidAppOperationResult>("force_stop_android_app", { request: { serial, packageName } }, () => mockAndroidAppAction(serial, packageName, "forceStop", "Android 应用已停止")),
  clearAndroidAppData: (serial: string, packageName: string) =>
    call<AndroidAppOperationResult>("clear_android_app_data", { request: { serial, packageName } }, () => mockAndroidAppAction(serial, packageName, "clearData", "Android 应用数据已清除")),
  uninstallAndroidApp: (serial: string, packageName: string) =>
    call<AndroidAppOperationResult>("uninstall_android_app", { request: { serial, packageName } }, () => mockAndroidAppAction(serial, packageName, "uninstall", "Android 应用已卸载")),
  installPackage: (serial: string, platform: "android" | "ios", path: string) =>
    call<PackageTransferResult>("install_mobile_package", { request: { serial, platform, path } }, () => ({ ...ok(`已安装 ${path.split(/[\\/]/).pop()}`), files: [path] })),
  exportAndroidPackage: async (serial: string, packageName: string, destination: string) => {
    if (!isTauri()) return call<PackageTransferResult>("export_android_package", undefined, () => ({ ...ok(`已导出 ${packageName}`), files: [`${destination}/${packageName}/base.apk`] }));
    const result = await invokeCommand<WireAndroidExport>("export_android_package", { request: { serial, packageName, destination } });
    return { success: result.success, message: [result.message, ...result.warnings].join(" · "), files: result.files.map((file) => file.localPath) } satisfies PackageTransferResult;
  },
  iosAppCapabilities: (sessionId: string) => call<IosAppCapabilities>("probe_ios_app_capabilities", { request: { sessionId } }, () => ({
    sessionId,
    rootSession: true,
    installers: [{ id: "appinst", name: "appinst", path: "/usr/bin/appinst" }],
    preferredInstaller: { id: "appinst", name: "appinst", path: "/usr/bin/appinst" },
    listingAvailable: true,
    exportAvailable: true,
    plutilPath: "/usr/bin/plutil",
    plutilMode: "key",
    base64Path: "/usr/bin/base64",
    tarPath: "/usr/bin/tar",
    warnings: [],
  })),
  iosInstalledApps: (sessionId: string, scope: "all" | "user" | "system" = "all", limit = 300) => call<IosInstalledApp[]>("list_ios_installed_apps", { request: { sessionId, scope, limit } }, () => [
    { bundleId: "com.example.ioslab", displayName: "iOS Lab", versionName: "3.2.1", buildVersion: "321", appPath: "/var/containers/Bundle/Application/PREVIEW/iOSLab.app", system: false },
    { bundleId: "com.apple.Preferences", displayName: "Settings", versionName: "1.0", buildVersion: "1", appPath: "/Applications/Preferences.app", system: true },
  ]),
  installIosPackageSsh: (sessionId: string, path: string, installerId?: IosPackageInstallerId) => call<IosPackageInstallResult>("install_ios_package_ssh", { request: { sessionId, path, ...(installerId ? { installerId } : {}) } }, () => ({
    success: true,
    message: "IPA 已通过设备端安装器安装",
    sessionId,
    installer: { id: installerId ?? "appinst", name: installerId ?? "appinst", path: `/usr/bin/${installerId ?? "appinst"}` },
    remoteTemporaryPath: "/var/mobile/.mobius-runtime/.pkg-preview.ipa",
    temporaryFileCleaned: true,
    warnings: ["仅调用设备上已安装的安装器；签名、信任与 AppSync 条件仍由设备正常校验。"],
  })),
  exportIosAppBundle: (sessionId: string, bundleId: string, appPath: string, destination: string) => call<IosAppExportResult>("export_ios_app_bundle", { request: { sessionId, bundleId, appPath, destination } }, () => ({
    success: true,
    message: "已导出 .app 开发分析归档",
    sessionId,
    bundleId,
    appPath,
    localPath: `${destination}/${bundleId}-mobius-app.tar.gz`,
    format: "analysisTarGz",
    sizeBytes: 42_194_304,
    installable: false,
    encryptionStatus: "unknown",
    warnings: ["该文件是 .app 分析归档，不是可直接安装的 IPA；不判定二进制是否加密。"],
  })),
  startFrida: async (serial: string, platform: "android" | "ios", localPath: string, devicePort: number, hostPort: number) => {
    if (!isTauri()) return call<FridaServerResult>("start_frida_server", undefined, () => ({ success: true, message: "Instrumentation Server 已启动并通过健康检查", platform, active: true, remotePath: "/data/local/tmp/mobius-agentd", pid: 2418, listenAddress: "127.0.0.1", devicePort, hostPort, mapping: { direction: "forward", local: `tcp:${hostPort}`, remote: `tcp:${devicePort}` } }));
    const uploaded = await call<FridaServerResult>("upload_frida_server", { request: { serial, platform, localPath } }, () => ({ success: true, message: "上传完成", platform, active: false, remotePath: "/data/local/tmp/mobius-agentd" }));
    if (!uploaded.success) return uploaded;
    return call<FridaServerResult>("start_frida_server", { request: { serial, platform, remotePath: uploaded.remotePath, listenAddress: "127.0.0.1", devicePort, hostPort } }, () => ({ success: true, message: "Instrumentation Server 已启动", platform, active: true, remotePath: uploaded.remotePath, pid: 2418, devicePort, hostPort }));
  },
  stopFrida: (serial: string) => call<FridaServerResult>("stop_frida_server", { serial }, () => ({ success: true, message: "Instrumentation Server 已停止", platform: "android", active: false, remotePath: "/data/local/tmp/mobius-agentd" })),
  uploadIosFridaServer: (sessionId: string, localPath: string) => call<IosFridaServerResult>("upload_ios_frida_server", { request: { sessionId, localPath } }, () => ({
    success: true,
    message: "iOS Server 已以中性名称上传",
    sessionId,
    active: false,
    remotePath: "/var/mobile/.mobius-runtime/.service-preview",
    tunnelActive: false,
  })),
  startIosFridaServer: (sessionId: string, devicePort?: number, hostPort?: number) => call<IosFridaServerResult>("start_ios_frida_server", { request: { sessionId, ...(devicePort ? { devicePort } : {}), ...(hostPort ? { hostPort } : {}) } }, () => ({
    success: true,
    message: "iOS Server 已启动并创建本机回环隧道",
    sessionId,
    active: true,
    remotePath: "/var/mobile/.mobius-runtime/.service-preview",
    pid: 317,
    listenAddress: "127.0.0.1",
    devicePort: devicePort ?? 27042,
    hostPort: hostPort ?? 39042,
    tunnelPid: 9321,
    tunnelActive: true,
  })),
  stopIosFridaServer: (sessionId: string) => call<IosFridaServerResult>("stop_ios_frida_server", { request: { sessionId } }, () => ({
    success: true,
    message: "iOS Server、上传文件与本机隧道已停止并清理",
    sessionId,
    active: false,
    remotePath: "/var/mobile/.mobius-runtime/.service-preview",
    tunnelActive: false,
  })),
  iosHostDiagnostic: (udid: string, kind: IosHostDiagnosticKind, network = false) => call<IosHostDiagnosticResult>("run_ios_host_diagnostic", { request: { udid, kind, network } }, () => {
    const output = kind === "deviceInfo"
      ? "DeviceName: iPhone 14 Lab\nProductType: iPhone14,7\nProductVersion: 16.7\nBuildVersion: 20H19\nSerialNumber: PREVIEW"
      : kind === "pairing"
        ? "SUCCESS: Validated pairing with device PREVIEW"
        : kind === "apps"
          ? "CFBundleIdentifier, CFBundleVersion, CFBundleDisplayName\ncom.example.ioslab, 321, iOS Lab\ncom.apple.Preferences, 1, Settings"
          : "Sep 05 13:58:02 iPhone SpringBoard[278] <Notice>: application state changed\nSep 05 13:58:03 iPhone backboardd[92] <Notice>: display state updated";
    return {
      success: true,
      kind,
      title: kind === "deviceInfo" ? "libimobiledevice 设备信息" : kind === "pairing" ? "配对状态" : kind === "apps" ? "已安装应用" : "设备实时日志采样",
      output,
      source: kind === "deviceInfo" ? "ideviceinfo" : kind === "pairing" ? "idevicepair validate" : kind === "apps" ? "ideviceinstaller list --all" : "idevicesyslog · 3 秒采样",
      truncated: false,
      warnings: [],
    };
  }),
  listIosPortTunnels: () => call<IosPortTunnel[]>("list_ios_port_tunnels", undefined, () => [...mockIosPortTunnels.values()]),
  createIosPortTunnel: (request: CreateIosPortTunnelRequest) => call<IosPortTunnel>("create_ios_port_tunnel", { request }, () => {
    if (request.transport === "iproxy" && request.direction === "deviceToHost") throw new Error("iproxy 仅支持本机访问 iPhone；反向访问需要 SSH 会话");
    const tunnel: IosPortTunnel = {
      tunnelId: `preview-ios-tunnel-${Date.now()}`,
      udid: request.udid,
      sessionId: request.sessionId,
      transport: request.transport,
      direction: request.direction,
      bindAddress: "127.0.0.1",
      hostPort: request.hostPort,
      devicePort: request.devicePort,
      pid: Math.floor(3000 + Math.random() * 5000),
      active: true,
    };
    mockIosPortTunnels.set(tunnel.tunnelId, tunnel);
    return tunnel;
  }),
  removeIosPortTunnel: async (tunnelId: string): Promise<OperationResult> => {
    const removed = await call<IosPortTunnel>("remove_ios_port_tunnel", { request: { tunnelId } }, () => {
      const tunnel = mockIosPortTunnels.get(tunnelId);
      if (!tunnel) throw new Error("该 iOS 端口隧道已不存在");
      mockIosPortTunnels.delete(tunnelId);
      return { ...tunnel, active: false };
    });
    return ok(`iOS 端口隧道已停止 · PID ${removed.pid}`);
  },
  iosRuntimeSnapshot: (sessionId: string, kind: IosDiagnosticKind, syslogLines = 120) => call<IosDiagnosticResult>("get_ios_runtime_snapshot", { request: { sessionId, kind, ...(kind === "syslog" ? { syslogLines } : {}) } }, () => {
    if (kind === "tools") return {
      success: true,
      kind,
      title: "调试工具状态",
      output: "已检测 9 项，7 项可用。",
      truncated: false,
      source: "固定绝对路径清单（不启动工具）",
      tools: [
        { id: "frida-server", name: "Frida Server", available: true, path: "/usr/sbin/frida-server", purpose: "动态插桩服务" },
        { id: "sshd", name: "OpenSSH Server", available: true, path: "/usr/sbin/sshd", purpose: "SSH 远程服务" },
        { id: "lldb", name: "LLDB", available: true, path: "/usr/bin/lldb", purpose: "原生调试器" },
        { id: "debugserver", name: "debugserver", available: true, path: "/usr/bin/debugserver", purpose: "远程原生调试" },
        { id: "dpkg", name: "dpkg", available: true, path: "/usr/bin/dpkg", purpose: "越狱软件包查询" },
        { id: "ldid", name: "ldid", available: true, path: "/usr/bin/ldid", purpose: "Mach-O 签名信息" },
        { id: "otool", name: "otool", available: true, path: "/usr/bin/otool", purpose: "Mach-O 元数据" },
        { id: "log", name: "Apple log", available: false, purpose: "统一日志查询" },
        { id: "plutil", name: "plutil", available: false, purpose: "属性列表读取" },
      ],
      warnings: [],
    };
    const mockOutput = kind === "overview"
      ? "[Kernel]\nDarwin iPhone 22.6.0 Darwin Kernel Version 22.6.0 arm64\n\n[Identity]\nuid=0(root) gid=0(wheel)\n\n[Disk]\nFilesystem Size Used Avail Capacity Mounted on\n/dev/disk1s1 128G 43G 85G 34% /"
      : kind === "processes"
        ? "[Processes]\nroot       1   0.0  launchd\nmobile   278   1.2  SpringBoard\nroot     441   0.1  .service-runtime"
        : "2026-09-05 01:22:04 SpringBoard[278]: Application state changed\n2026-09-05 01:22:05 backboardd[92]: display state updated";
    return { success: true, kind, title: kind === "overview" ? "设备概览" : kind === "processes" ? "进程快照" : "最近系统日志", output: mockOutput, truncated: false, source: kind === "syslog" ? "Unified log · 最近 5 分钟" : "SSH 固定只读诊断", tools: [], warnings: [] };
  }),
  prepareIosDeviceAction: (session: IosSshSession, action: IosDeviceAction) => call<IosDeviceActionConfirmation>("prepare_ios_device_action", { request: { sessionId: session.sessionId, action, expectedSshHost: session.sshHost, expectedSshPort: session.sshPort, expectedUsername: session.username, expectedServerSystem: session.serverSystem } }, () => ({ success: true, confirmationId: "0123456789abcdef0123456789abcdef", sessionId: session.sessionId, action, target: { sshHost: session.sshHost, sshPort: session.sshPort, username: session.username, serverSystem: session.serverSystem, hostKeyIdentity: `[${session.sshHost}]:${session.sshPort}` }, expiresInSeconds: 30 })),
  runIosDeviceAction: (confirmation: IosDeviceActionConfirmation) => call<IosDeviceActionResult>("run_ios_device_action", { request: { confirmationId: confirmation.confirmationId, sessionId: confirmation.sessionId, action: confirmation.action, expectedSshHost: confirmation.target.sshHost, expectedSshPort: confirmation.target.sshPort, expectedUsername: confirmation.target.username, expectedServerSystem: confirmation.target.serverSystem, expectedHostKeyIdentity: confirmation.target.hostKeyIdentity } }, () => ({ success: true, action: confirmation.action, accepted: true, message: confirmation.action === "respring" ? "Respring 已调度，SpringBoard 将重新载入。" : "设备重启已调度，SSH 连接将暂时中断。" })),
  startIosSshSession: (request: IosSshSessionRequest) => call<IosSshSession>("start_ios_ssh_session", { request }, () => ({
    sessionId: "preview-ios-ssh",
    mode: request.transport.mode,
    connected: true,
    jailbreakConfirmed: true,
    sshHost: request.transport.mode === "usb" ? "127.0.0.1" : request.transport.host,
    sshPort: request.transport.mode === "usb" ? request.transport.hostPort ?? 2222 : request.transport.port ?? 22,
    devicePort: request.transport.mode === "usb" ? request.transport.devicePort ?? 22 : request.transport.port ?? 22,
    username: request.username,
    authMode: request.authMode,
    allowedRoots: request.allowedRoots,
    serverSystem: "Darwin iPhone 23.0.0",
    remoteUid: request.username === "root" ? 0 : 501,
    tunnel: request.transport.mode === "usb" ? { active: true, pid: 2451, udid: request.transport.udid, bindAddress: "127.0.0.1", hostPort: request.transport.hostPort ?? 2222, devicePort: request.transport.devicePort ?? 22 } : undefined,
  })),
  testIosSshConnection: (sessionId: string) => call<IosSshConnectionTest>("test_ios_ssh_connection", { sessionId }, () => ({ success: true, message: "SSH 连接正常", connected: true, authMode: "password", serverSystem: "Darwin iPhone", remoteUid: 501 })),
  iosSshFiles: (sessionId: string, path: string) => call<RemoteFile[]>("list_ios_ssh_files", { request: { sessionId, path } }, () => [
    { name: "Documents", path: `${path.replace(/\/$/, "")}/Documents`, kind: "directory", permissions: "drwx------", owner: "mobile" },
    { name: "Library", path: `${path.replace(/\/$/, "")}/Library`, kind: "directory", permissions: "drwx------", owner: "mobile" },
    { name: "mobius.log", path: `${path.replace(/\/$/, "")}/mobius.log`, kind: "file", size: 13248, modified: "2026-09-05 00:42", permissions: "-rw-r--r--", owner: "mobile" },
  ]),
  uploadIosSshFile: (sessionId: string, localPath: string, remotePath: string, overwrite = false) => call<OperationResult>("upload_ios_ssh_file", { request: { sessionId, localPath, remotePath, overwrite } }, () => ok("SSH 上传完成")),
  downloadIosSshFile: (sessionId: string, remotePath: string, localPath: string, overwrite = false) => call<OperationResult>("download_ios_ssh_file", { request: { sessionId, remotePath, localPath, overwrite } }, () => ok("SSH 下载完成")),
  mkdirIosSsh: (sessionId: string, path: string) => call<OperationResult>("mkdir_ios_ssh", { request: { sessionId, path } }, () => ok(`已创建 ${path}`)),
  deleteIosSsh: (sessionId: string, path: string, recursive = false) => call<OperationResult>("delete_ios_ssh", { request: { sessionId, path, recursive } }, () => ok(`已删除 ${path}`)),
  stopIosSshSession: (sessionId: string) => call<OperationResult>("stop_ios_ssh_session", { sessionId }, () => ok("SSH 会话已关闭")),
  captureAndroidScreenFrame: (serial: string) => call<ScreenFrame>("capture_android_screen_frame", { serial }, () => ({
    imageDataUrl: "/brand/mobius-mark.png",
    sizeBytes: 0,
    width: 1080,
    height: 2400,
    capturedAtMs: Date.now(),
  })),
  startAndroidScreenStream: (serial: string, maxSize = 1024, bitRate = 4_000_000, maxFps = 15) => call<AndroidScreenStream>("start_android_screen_stream", { request: { serial, maxSize, bitRate, maxFps } }, () => ({
    success: true,
    message: "已启动内嵌实时视频（浏览器预览）",
    sessionId: `preview-${Date.now()}`,
    streamUrl: "/brand/mobius-mark.png",
    serial,
    codec: "H.264 -> MJPEG",
    transport: "browser-preview",
    maxSize,
    maxFps,
    width: 432,
    height: 960,
  })),
  stopAndroidScreenStream: (serial: string, sessionId: string) => call<OperationResult>("stop_android_screen_stream", { request: { serial, sessionId } }, () => ok("内嵌屏幕流已停止")),
  probeIosScreenCapability: (udid: string) => call<IosScreenCapability>("probe_ios_screen_capability", { request: { udid } }, () => ({
    available: !udid.startsWith("ios-ssh:"),
    transport: udid.startsWith("ios-ssh:") ? "unavailable" : "usb",
    message: udid.startsWith("ios-ssh:") ? "SSH-only endpoint has no paired screenshot service" : "Paired USB screenshot service is available",
  })),
  captureIosScreenFrame: (udid: string) => call<ScreenFrame>("capture_ios_screen_frame", { request: { udid } }, () => ({
    imageDataUrl: "/brand/mobius-mark.png",
    sizeBytes: 0,
    width: 1170,
    height: 2532,
    capturedAtMs: Date.now(),
  })),
  captureIosScreenshot: (udid: string, destinationDirectory?: string, copyToClipboard = false) => call<MediaCaptureResult>("capture_ios_screenshot", { request: { udid, ...(destinationDirectory ? { destinationDirectory } : {}), copyToClipboard } }, () => ({ ...ok(destinationDirectory && copyToClipboard ? "iOS 截图已保存并复制" : copyToClipboard ? "iOS 截图已复制到剪贴板" : "iOS 截图已保存"), copiedToClipboard: copyToClipboard, savedPath: destinationDirectory ? `${destinationDirectory}/mobius-ios-screenshot.png` : undefined, sizeBytes: 318420, width: 1170, height: 2532, warnings: [] })),
  captureAndroidScreenshot: (serial: string, destinationDirectory?: string, copyToClipboard = false) => call<MediaCaptureResult>("capture_android_screenshot", { request: { serial, ...(destinationDirectory ? { destinationDirectory } : {}), copyToClipboard } }, () => ({ ...ok(destinationDirectory && copyToClipboard ? "截图已保存并复制" : copyToClipboard ? "截图已复制到剪贴板" : "截图已保存"), copiedToClipboard: copyToClipboard, savedPath: destinationDirectory ? `${destinationDirectory}/mobius-screenshot.png` : undefined, sizeBytes: 248132, width: 1080, height: 2400, warnings: [] })),
  startAndroidScreenRecording: (serial: string, destinationDirectory: string, bitRate?: number, allowRootFallback = false) => call<AndroidScreenRecordingSession>("start_android_screen_recording", { request: { serial, destinationDirectory, ...(bitRate ? { bitRate } : {}), allowRootFallback } }, () => {
    const sessionId = `preview-recording-${Date.now()}`;
    const startedAtMs = Date.now();
    mockRecordingSessions.set(sessionId, { startedAtMs, destinationDirectory });
    return { success: true, message: "录屏已开始，点击停止后保存", sessionId, serial, startedAtMs, plannedSavedPath: `${destinationDirectory}/mobius-recording.mp4`, warnings: [] };
  }),
  stopAndroidScreenRecording: (serial: string, sessionId: string) => call<MediaCaptureResult>("stop_android_screen_recording", { request: { serial, sessionId } }, () => {
    const session = mockRecordingSessions.get(sessionId);
    mockRecordingSessions.delete(sessionId);
    const durationSeconds = session ? Math.max(1, Math.floor((Date.now() - session.startedAtMs) / 1000)) : 1;
    return { ...ok("录屏已停止并保存"), savedPath: `${session?.destinationDirectory ?? "."}/mobius-recording.mp4`, durationSeconds, sizeBytes: 4821942, warnings: [] };
  }),
};

export const runningInDesktop = isTauri;
