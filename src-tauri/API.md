# Mobius Tauri command API

All invoke/request/response fields use `camelCase`. Every command resolves to an envelope:

```json
{"ok":true,"data":{},"elapsedMs":12}
```

Failures resolve as `{"ok":false,"error":{"code":"...","message":"...","details":...},"elapsedMs":12}`. A failure to extract Tauri state is the only outer invoke rejection.

## Device and utility commands

| Command | Invoke arguments | `data` |
| --- | --- | --- |
| `configure_toolchain` | `{ request: { adbPath?, scrcpyPath?, fridaPath?, iosToolsPath?, managedToolsPath?, clear? } }` | `ToolchainConfiguration` |
| `get_tool_health` | none | `ToolHealth[]` |
| `list_devices` | none | UI-ready Android + iOS `Device[]` |
| `list_android_devices` | none | `AndroidDevice[]` |
| `list_ios_devices` | none | `IosDevice[]` |
| `get_ios_device_info` | `{ udid }` | `{ udid, properties }` |
| `run_ios_host_diagnostic` | `{ request: { udid, kind, network?, timeoutMs? } }` | `IosHostDiagnosticResult` |
| `adb_connect` | `{ address }` | `OperationResult` |
| `adb_pair` | `{ address, code }` | `OperationResult` |
| `scan_adb_subnet` | `{ cidr?: string, ports?: number[] }` | `{ address, port, latencyMs, state }[]` |
| `list_port_mappings` | `{ serial }` | `PortMapping[]` |
| `create_port_mapping` | `{ request: { serial, direction, local, remote, noRebind? } }` | `OperationResult` |
| `remove_port_mapping` | `{ request: { serial, direction, local } }` | `OperationResult` |
| `list_ios_port_tunnels` | none | `IosPortTunnel[]` |
| `create_ios_port_tunnel` | `{ request: { transport, direction, udid, sessionId?, hostPort?, devicePort } }` | `IosPortTunnel` |
| `remove_ios_port_tunnel` | `{ request: { tunnelId } }` | `IosPortTunnel` with `active:false` |
| `launch_scrcpy` | `{ request: { serial, maxSize?, bitRate?, turnScreenOff?, stayAwake?, noAudio? } }` | `OperationResult` |
| `start_android_screen_stream` | `{ request: { serial, maxSize?, bitRate?, maxFps? } }` | `AndroidScreenStreamResult` with a private loopback MJPEG URL |
| `stop_android_screen_stream` | `{ request: { serial, sessionId } }` | `OperationResult` |
| `capture_android_screen_frame` | `{ serial }` | `ScreenFrameResult` |
| `capture_android_screenshot` | `{ request: { serial, destinationDirectory?: string, copyToClipboard?: boolean } }` | `AndroidScreenshotResult` |
| `start_android_screen_recording` | `{ request: { serial, destinationDirectory, bitRate?, allowRootFallback? } }` | `AndroidScreenRecordingSession` |
| `stop_android_screen_recording` | `{ request: { serial, sessionId } }` | `AndroidScreenRecordingResult` |
| `probe_ios_screen_capability` | `{ request: { udid } }` | `IosScreenCapability` |
| `capture_ios_screen_frame` | `{ request: { udid } }` | `ScreenFrameResult` |
| `capture_ios_screenshot` | `{ request: { udid, destinationDirectory?: string, copyToClipboard?: boolean } }` | `AndroidScreenshotResult` compatible media result |
| `run_device_shell` | `{ serial, command }` | `OperationResult` |
| `list_remote_files` | `{ serial, path }` | `RemoteFile[]` |
| `pull_file` | `{ request: { serial, remotePath, localPath, overwrite? } }` | `OperationResult` |
| `push_file` | `{ request: { serial, localPath, remotePath, overwrite? } }` | `OperationResult` |
| `mkdir_remote` | `{ serial, path }` | `OperationResult` |
| `delete_remote` | `{ request: { serial, path, recursive? } }` | `OperationResult` |
| `set_android_proxy` | `{ request: { serial, host, port } }` | `OperationResult` |
| `clear_android_proxy` | `{ serial }` | `OperationResult` |

For reverse mappings, pass the returned `removeEndpoint` as `remove_port_mapping.request.local`.

### Toolchain resolution and health

`configure_toolchain` replaces the saved toolchain selection. `adbPath`, `scrcpyPath`, and
`fridaPath` must each be an absolute path to a regular executable file. `iosToolsPath` and
`managedToolsPath` must each be an absolute directory. Empty or omitted fields are cleared; pass
`clear:true` to clear the complete selection. Configuration validates file metadata and saves the
canonical paths, but never launches a selected program.

Every host tool is resolved in this fixed order:

1. exact configured executable, then the configured managed/iOS directories;
2. the application-controlled `tools` resource directory for the current OS and architecture;
3. Android SDK locations for `adb`, `aapt2`, and `apkanalyzer`, including common per-user SDK paths;
4. the process `PATH` and standard system executable directories.

An exact configured executable that disappears or becomes non-executable fails closed instead of
silently selecting a different program. The bundled resource directory is empty except for its
distribution policy; Mobius does not download or bundle third-party executables by default. That
boundary includes the newly required `scrcpy-server` and `ffmpeg` components.

`get_tool_health` covers `adb`, `scrcpy`, `ffmpeg`, `frida`, `aapt2`, `apkanalyzer`, `idevice_id`,
`ideviceinfo`, `idevicepair`, `ideviceinstaller`, `idevicescreenshot`, `idevicesyslog`, `iproxy`, `ssh`, and `scp`. Each item includes `state`, optional
`version` and `path`, optional `source` (`configured`, `bundled`, `sdk`, or `path`), `purpose`,
`required`, `installHint`, and an optional diagnostic `hint`.

`scrcpy` and `ffmpeg` are separate health entries. Both have `required:false` because the rest of
the workbench can operate without them, but both must be `ready` for the default embedded Android
video. `ffmpeg` is resolved from the configured managed-tools directory, the reviewed bundled-tools
location, or the system search path; there is no separate `ffmpegPath` configuration field. A ready
`scrcpy` executable does not by itself prove that its matching Server file is installed. Stream start
performs that second check against `SCRCPY_SERVER_PATH`, executable-adjacent files, and the standard
adjacent `share/scrcpy` or `lib/scrcpy` locations.

### Fixed libimobiledevice diagnostics

`run_ios_host_diagnostic` accepts only these `kind` enum values and constructs the complete program
and argument list in Rust:

| `kind` | Fixed invocation for USB | Purpose |
| --- | --- | --- |
| `deviceInfo` | `ideviceinfo -u <udid>` | Read device properties |
| `pairing` | `idevicepair -u <udid> validate` | Validate the existing pair/trust relationship |
| `apps` | `ideviceinstaller -u <udid> list --all` | List applications exposed by the host service |
| `syslog` | `idevicesyslog -u <udid> --no-colors` | Collect a bounded live-log sample |

When `network:true`, `-n` is inserted after the selected UDID for the paired-network transport. The
request never accepts a program name, subcommand, free-form argument, or SSH credential. The UDID is
validated as a device identifier and passed as one argument. Availability of one libimobiledevice
program does not imply availability of the others; `idevicepair` and `idevicesyslog` therefore have
their own tool-health entries.

The default timeout is 10,000 ms for `deviceInfo`, `pairing`, and `apps`, and 5,000 ms for `syslog`.
An optional `timeoutMs` must be within 250..30,000. A syslog child stopped at the requested collection
deadline is treated as a successful bounded sample; a timeout is an error for the other three kinds.
Sanitized stdout is limited to 256 KiB and stderr to 16 KiB. `IosHostDiagnosticResult` contains
`success`, `kind`, `title`, `source`, `udid`, `network`, `tool`, `output`, optional `stderr`, optional
`exitCode`, `timedOut`, `truncated`, `durationMs`, and `warnings`.

### Managed iOS port tunnels

`transport` is `"iproxy" | "ssh"`; `direction` is `"hostToDevice" | "deviceToHost"`.
Every generated host listener uses IPv4 loopback. For SSH forwarding, whichever side listens and its
destination endpoint are both fixed to loopback; `iproxy` selects the device service through
UDID/usbmuxd rather than a device IP:

| Transport/direction | Managed child shape | Requirements |
| --- | --- | --- |
| `iproxy` + `hostToDevice` | `iproxy -u <udid> -l -s 127.0.0.1 <hostPort>:<devicePort>` | USB/usbmux device; `sessionId` is optional |
| `ssh` + `hostToDevice` | bundled Mobius SSH `-L 127.0.0.1:<hostPort>:127.0.0.1:<devicePort>` | active, authenticated `sessionId` |
| `ssh` + `deviceToHost` | bundled Mobius SSH `-R 127.0.0.1:<devicePort>:127.0.0.1:<hostPort>` | active, authenticated `sessionId`; an existing host service port is required |

`iproxy` with `deviceToHost` is rejected. Mobius SSH tunnel children are also fixed to `-N`,
`ExitOnForwardFailure=yes`, and a loopback bind request; authentication and the actual SSH endpoint
are copied from the recorded session instead of being supplied by this request. For `-R`, the device
sshd ultimately enforces the listener address, so a server configured with `GatewayPorts yes` may
override the requested loopback bind and must not be used for a reverse tunnel. For SSH transport,
`udid` is validated and retained as the UI ownership label, while `sessionId` determines the actual
connection. The command does not change an iOS system proxy.

`devicePort` is always required and nonzero. `hostPort` may be omitted only for a
`hostToDevice` tunnel, in which case an available loopback port is allocated; it is required for
`deviceToHost` because it identifies the already-running host service. Before registering a forward,
the backend waits for its local listener. For `deviceToHost`, where the remote listener is not
directly probeable from the host, it verifies only that the SSH child remains running through the
startup stability window. At most 64 managed iOS tunnel children may be active.

`IosPortTunnel` contains `tunnelId`, `transport`, `direction`, `udid`, optional `sessionId`,
`bindAddress` (currently `127.0.0.1`), `hostPort`, `devicePort`, `pid`, and `active`. IDs are random
and validated on removal. Listing checks each recorded child and prunes exited entries; removal kills
and reaps only the exact recorded child, returning its final representation with `active:false`.
Closing an iOS SSH session stops tunnels carrying that session's `sessionId`. A session-independent
USB `iproxy` remains until explicit removal or application shutdown; application shutdown stops all
remaining managed iOS port tunnels. These cleanup paths do not enumerate or terminate `iproxy` or
`ssh` processes created outside Mobius.

`capture_android_screenshot` requires a PC save directory, clipboard copy, or both. When
`destinationDirectory` is omitted, its PC-side temporary PNG is removed immediately after a
successful clipboard copy. `AndroidScreenshotResult` returns `savedPath` when retained,
`copiedToClipboard`, `sizeBytes`, `width`, `height`, and `warnings`.

`start_android_screen_recording` starts a managed Android system recording with no fixed time limit;
the device page shows its elapsed time until the user clicks the same action to stop. The returned
session binds the exact Android serial, an opaque session ID, its start timestamp, and the reserved
PC output path. `stop_android_screen_recording` accepts that serial/session pair, verifies the saved
PID's executable and `/proc` start time, sends SIGINT so `screenrecord` can finalize the MP4, pulls
and validates the file, then removes its device-side temporary media and log. Device switching,
leaving the device page, and application exit all request the same managed stop path. A sleeping
display is temporarily woken and returned to sleep after capture. When `allowRootFallback=true`, a
device that rejects the standard shell output path may use a non-interactive Root fallback, which is
reported in `warnings`. Optional `bitRate` is limited to 100,000–100,000,000 bits/s.
`AndroidScreenRecordingResult` returns `savedPath`, `sizeBytes`, the measured `durationSeconds`, and
`warnings`. Screenshot and recording commands generate collision-resistant filenames, select the
Android device with an explicit serial, validate pulled media, and clean up their private
`/data/local/tmp/mobius-*` artifacts on the normal and application-exit paths.
Clipboard image access uses Tauri's official desktop clipboard manager from this Rust command;
the WebView is not granted the plugin's general clipboard read/write commands.

When `scan_adb_subnet.cidr` is omitted, the backend enumerates and prioritizes physical Wi-Fi or
Ethernet RFC1918 interfaces, excludes common VPN/TUN and virtual interfaces, and tries up to four
active `/24` networks until an ADB endpoint is found. An explicit CIDR must match a detected active
private `/24`. The default port is 5555. The command only returns probed candidates; the default UI
automatically connects entries whose state was positively identified as `adb`.

The default Android device-page view invokes `start_android_screen_stream`; it does not launch the
standalone scrcpy GUI. The desktop layout gives a portrait phone a tall preview on the left, keeps
only screen actions on its right, and places the compact device selector table below; cross-page
device switching stays in the global top bar. The preview widens only when the returned/displayed
dimensions are landscape. The request defaults and accepted ranges are:

| Field | Default | Accepted range |
| --- | ---: | ---: |
| `maxSize` | `1024` | `320..1920` pixels |
| `bitRate` | `4000000` | `250000..20000000` bits/s |
| `maxFps` | `15` | `5..30` FPS |

Starting a stream validates an exact online ADB serial, reads a safe version token from the selected
scrcpy client, and requires a regular 16 KiB–32 MiB Server file from that scrcpy installation or the
explicit development override. The Server is started with that client version, audio/control
disabled, raw H.264 enabled, and a random SCID. A private exact-device ADB reverse carries H.264 to
a random host loopback port; FFmpeg transcodes it to MJPEG. A second random `127.0.0.1` listener
serves exactly one WebView client. Its URL contains both a random 128-bit session id and an
independent 128-bit token, requires an exact path and loopback `Host`, rejects any supplied foreign
`Origin`, and sends no-cache response headers. Treat `streamUrl` as a session secret and do not log
or persist it.

`AndroidScreenStreamResult` contains `success`, `message`, 32-hex-character `sessionId`, private
`streamUrl`, exact `serial`, `codec` (`"H.264 -> MJPEG"`), `transport`
(`"adb-reverse-loopback"`), effective `maxSize`/`maxFps`, and optional probed `width`/`height`.
Starting another stream for the same serial first replaces the recorded stream. Stop requires that
same serial and exact session id; a stale id returns `stream_session_mismatch`, while stopping an
already absent matching session is idempotent success.

Explicit stop, device switch, pause/view teardown, video EOF, WebView disconnect and app exit perform
best-effort ownership-scoped cleanup: close the socket, kill and reap the recorded scrcpy Server and
FFmpeg children, remove only the generated SCID reverse, and delete only the generated remote jar.
The UI falls back to `capture_android_screen_frame` only when scrcpy/FFmpeg prerequisites are missing
or stream startup/runtime fails. That standalone frame command runs an explicit
`adb -s <serial> exec-out screencap -p` process with an 8-second timeout and a 16 MiB inline-frame
ceiling; it is not the normal continuous-video path. The iOS screen commands still use paired
screenshotr sampling: they reject manual `ios-ssh:*` endpoint identifiers before invoking a host
tool, require the exact UDID to appear in `idevice_id -l` or `idevice_id -n`, and then run
`idevicescreenshot -u <udid>` (plus `-n` only for a paired network device). Captures are first written
to a private temporary directory and validated as bounded PNG images. A paired/trusted device and
matching mounted Developer Disk Image are required. The frame response is a PNG data URL; retained
screenshots use collision-resistant files, while clipboard-only temporary files are removed on every
outcome.

### Android proxy state and restoration

`set_android_proxy` is only called after an explicit UI system-proxy action; creating or removing an
ADB reverse alone does not invoke it. The request accepts a validated hostname/IPv4 value and a
nonzero `u16` port; IPv6 literals are rejected because Android's compatibility setting cannot
represent them safely.

Before the first managed write, the backend snapshots all five raw global settings as one state:
`http_proxy`, `global_http_proxy_host`, `global_http_proxy_port`,
`global_http_proxy_exclusion_list`, and `global_proxy_pac_url`. It writes the effective static value
through `http_proxy`, waits for Android's canonical host/port state to match in two consecutive
reads, and records the complete resulting five-field snapshot. Repeated explicit sets preserve the
original baseline while the current complete snapshot still equals Mobius's recorded configured
state. A failed set attempts to restore the complete baseline before returning an error.

`clear_android_proxy` means restore, not unconditional delete. It requires a proxy managed by this
app session and compares all five current fields with the recorded configured snapshot. If any field
was changed externally, it drops its ownership record, returns `proxy_changed_externally`, and leaves
the newer state untouched; app-exit cleanup follows the same rule. A fresh explicit Set after an
external change may establish that current state as a new baseline only when it is safely
restorable.

To restore an original no-effective-proxy state, the backend first writes `http_proxy=:0`. Android
uses that compatibility trigger to clear its in-memory proxy and broadcast `PROXY_CHANGE`; after the
state settles, Mobius writes back the exact five raw values so absent and empty fields remain
distinct. A pre-existing nonempty PAC URL, nonempty exclusion list, or unparseable effective static
proxy cannot be reconstructed safely through this live ADB interface, so an explicit Set is rejected
with `proxy_restore_unsupported` before changing the device.

## Mobile package commands

`platform` is the string enum `"android" | "ios"`.

| Command | Invoke arguments | `data` |
| --- | --- | --- |
| `analyze_mobile_package` | `{ request: { path } }` | `MobilePackageAnalysis` |
| `install_mobile_package` | `{ request: { serial, platform, path, replace?, grantPermissions?, downgrade?, allowTestPackages? } }` | `OperationResult` |
| `list_installed_apps` | `{ serial }` | `InstalledApp[]` |
| `export_android_package` | `{ request: { serial, packageName, destination, overwrite? } }` | `AndroidPackageExport` |
| `launch_android_app` | `{ request: { serial, packageName } }` | `AndroidAppOperationResult` |
| `force_stop_android_app` | `{ request: { serial, packageName } }` | `AndroidAppOperationResult` |
| `clear_android_app_data` | `{ request: { serial, packageName } }` | `AndroidAppOperationResult` |
| `uninstall_android_app` | `{ request: { serial, packageName } }` | `AndroidAppOperationResult` |

`MobilePackageAnalysis`:

```json
{
  "platform": "android",
  "path": "/absolute/path/app.apk",
  "fileName": "app.apk",
  "fileSize": 123,
  "md5": "32-lowercase-hex-characters",
  "architectures": ["arm64-v8a"],
  "source": "aapt2",
  "fallbackUsed": false,
  "packageName": "dev.example.app",
  "displayName": "Example",
  "versionName": "1.0",
  "versionCode": "1",
  "minimumOsVersion": "23",
  "targetSdkVersion": "35",
  "permissions": ["android.permission.CAMERA"],
  "usageDescriptions": {},
  "icon": {
    "archivePath": "res/mipmap-xxxhdpi/ic_launcher.png",
    "mimeType": "image/png",
    "dataBase64": "...",
    "sizeBytes": 123
  },
  "warnings": []
}
```

Nullable/unknown metadata fields and `icon` are omitted. APK metadata prefers `aapt2`, then `apkanalyzer`; ZIP-only fallback is explicit. APK `architectures` are collected from `lib/<abi>/`; IPA architectures are read from thin/fat Mach-O headers, with a warning and an empty array when no supported header is available. IPA metadata comes from binary/XML `Info.plist`; privacy `*UsageDescription` strings are returned in `usageDescriptions`. Icon extraction is bounded and only returns PNG, WebP, or JPEG data. Hashing is streamed and does not load the package into memory.

`InstalledApp` contains `packageName`, `apkPath`, optional `uid`, optional `versionCode`, and `system`. `AndroidPackageExport` contains `success`, `message`, `packageName`, canonical `destination`, `files[]` (`kind`, `remotePath`, `localPath`, optional `sizeBytes`), and `warnings`. Base and split APKs are exported together. Existing files require `overwrite:true`.

Android application management accepts only a validated package identifier and an exact ADB serial. Every operation verifies that the selected device is online and that the package is currently installed. Launch uses one fixed launcher-category `monkey` event; stop uses fixed `am force-stop` arguments. Clear-data and uninstall independently re-check Package Manager's system-app classification and reject system applications. No endpoint accepts a shell command or command fragment. Clear-data and uninstall return success only when Android prints its exact `Success` marker.

Android installation invokes `adb install`. A currently enumerated USB/usbmux iOS device can use host `ideviceinstaller`; an authenticated Root SSH session can use the separate fixed on-device-installer endpoint below. Code signing, provisioning, device trust, jailbreak state, and AppSync compatibility remain unchanged and are handled by the existing device environment. Missing tools return an explicit error instead of simulated success.

### Jailbroken-iOS application workflow

These endpoints require an active SSH session already verified as UID 0. They do not accept a host, credential, arbitrary command, installer path, or arbitrary device application root.

| Command | Invoke arguments | `data` |
| --- | --- | --- |
| `probe_ios_app_capabilities` | `{ request: { sessionId } }` | `IosAppCapabilities` |
| `install_ios_package_ssh` | `{ request: { sessionId, path, installerId?: "appinst" | "ipainstaller" } }` | `IosPackageInstallResult` |
| `list_ios_installed_apps` | `{ request: { sessionId, scope?: "all" | "user" | "system", limit?: 1..500 } }` | `IosInstalledApp[]` |
| `export_ios_app_bundle` | `{ request: { sessionId, bundleId, appPath, destination, overwrite? } }` | `IosAppExportResult` |

Capability probing checks only compiled-in executable paths for `appinst`, `ipainstaller`, `plutil`, `base64`, and `tar`; it never downloads or installs a device tool. SSH installation first performs the same bounded local IPA ZIP/`Info.plist` validation as package analysis, uploads to a generated neutral file below the SSH session's first canonical allowed root, invokes exactly one detected installer with only the generated IPA path, and attempts cleanup on every outcome. Code signing, trust, provisioning, jailbreak, and AppSync behavior remains the responsibility of the existing device environment.

Application listing reads only top-level `.app/Info.plist` files below a compiled-in set of standard system, rootless, and user-container application directories. It returns at most 500 records with `bundleId`, `displayName`, `versionName`, `buildVersion`, `appPath`, and `system`; metadata fields are base64-framed to prevent delimiter injection and command output remains bounded.

Export revalidates that the canonical `.app` path is inside a fixed application root and that its current `CFBundleIdentifier` matches the requested record. It creates a temporary device archive, verifies its size (maximum 4 GiB), downloads through the authenticated session, and then removes the temporary device file. `IosAppExportResult` always identifies the format as `analysisTarGz`, reports `installable:false` and `encryptionStatus:"unknown"`, and warns that this is not an IPA. Mobius does not change executable protection state, reconstruct signing/provisioning, or claim that the archive can be installed.

## Instrumentation server commands

| Command | Invoke arguments | `data` |
| --- | --- | --- |
| `upload_frida_server` | `{ request: { serial, platform?: "android", localPath, remotePath? } }` | `FridaServerResult` |
| `start_frida_server` | `{ request: { serial, platform?: "android", remotePath?, listenAddress?, devicePort?, hostPort?, port? } }` | `FridaServerResult` |
| `stop_frida_server` | `{ serial }` | `FridaServerResult` |
| `upload_ios_frida_server` | `{ request: { sessionId, localPath } }` | `IosFridaServerResult` |
| `start_ios_frida_server` | `{ request: { sessionId, devicePort?, hostPort? } }` | `IosFridaServerResult` |
| `stop_ios_frida_server` | `{ request: { sessionId } }` | `IosFridaServerResult` |

`port` is a deprecated compatibility alias that sets both `devicePort` and `hostPort`. Explicit `devicePort`/`hostPort` win. Defaults are device `27042` and the same host port. `listenAddress` is limited to Android loopback (`127.0.0.1` or `::1`).

The default remote path is `/data/local/tmp/mobius-agentd`. A custom path must be a direct child of `/data/local/tmp`, start with `mobius-`, and use a neutral name. The local upload must be a user-selected `frida-server*` binary; Mobius never downloads or bundles one.

`FridaServerResult` is a mutation-result superset with `success`, `message`, `platform`, `active`, actual `remotePath`, optional `pid`, `listenAddress`, `devicePort`, `hostPort`, `mapping` (`direction`, `local`, `remote`), `mappingActive`, `stdout`, and `stderr`. These three original commands remain Android-only; passing iOS to them returns `frida_ios_not_implemented`. Jailbroken-iOS lifecycle uses the three session-bound commands above.

The iOS upload accepts only an absolute, non-symlink, regular file up to 512 MiB whose first four bytes are a supported thin or universal Mach-O magic. It never downloads a server. The binary is installed with mode `0700` below the SSH session's first canonical allowed root, in `.mobius-runtime`, using a generated `.service-*` filename. The complete remote path is rejected if it contains `frida` (case-insensitive), including when the selected allowed root contains that string.

Starting requires the same still-active SSH session to have verified `remoteUid: 0`; Mobius uses only that session's existing permissions. The server always binds on-device to `127.0.0.1:devicePort` (default `27042`). Mobius creates a separate bundled SSH local forward bound to `127.0.0.1:hostPort`; omitting `hostPort` allocates an available loopback port. It verifies the exact generated process path for the returned PID and verifies that the local forward is listening before returning success.

`IosFridaServerResult` includes `success`, `message`, `sessionId`, `active`, the generated `remotePath`, optional remote `pid`, `listenAddress`, `devicePort`, `hostPort`, local `tunnelPid`, and `tunnelActive`. Stop signals a PID only while its current command still begins with the exact generated path recorded for that SSH session. It then removes only that generated binary/log and stops only that session's recorded local-forward child. It never searches for or signals a pre-existing service such as `/usr/sbin/frida-server`. Closing the SSH session or the app runs the same best-effort cleanup.

## Jailbroken iOS SSH file management

This adapter manages only a user-owned development device that already exposes SSH. It starts from normal SSH authentication and an existing device permission model; it does not change either one. `jailbreakConfirmed` becomes `true` only after the supplied password or selected private key successfully authenticates and a fixed SSH validation command completes.

| Command | Invoke arguments | `data` |
| --- | --- | --- |
| `start_ios_ssh_session` | `{ request: { transport, authMode, username?, password?, privateKeyPath?, allowedRoots } }` | `IosSshSession` |
| `test_ios_ssh_connection` | `{ sessionId }` | `IosSshConnectionResult` |
| `list_ios_ssh_files` | `{ request: { sessionId, path } }` | `RemoteFile[]` |
| `upload_ios_ssh_file` | `{ request: { sessionId, localPath, remotePath, overwrite? } }` | `OperationResult` |
| `download_ios_ssh_file` | `{ request: { sessionId, remotePath, localPath, overwrite? } }` | `OperationResult` |
| `mkdir_ios_ssh` | `{ request: { sessionId, path } }` | `OperationResult` |
| `delete_ios_ssh` | `{ request: { sessionId, path, recursive? } }` | `OperationResult` |
| `stop_ios_ssh_session` | `{ sessionId }` | `OperationResult` |

USB transport is `{ mode: "usb", udid, devicePort?: 22, hostPort?: number }`. It starts the exact bundled, loopback-hardened go-ios adapter on `127.0.0.1` and uses an explicitly configured `iproxy` only as a compatibility fallback; omitting `hostPort` allocates an available loopback port. LAN transport is `{ mode: "lan", host, port?: 22 }` and accepts only literal private, loopback, or link-local IP addresses. `authMode` is `"password"` or `"privateKey"`; the matching credential field is required. The response includes `sessionId`, `mode`, `authMode`, `connected`, `jailbreakConfirmed`, `sshHost`, `sshPort`, `devicePort`, `username`, stable configured `allowedRoots`, optional `serverSystem`/`remoteUid`, and USB `tunnel` status (`active`, `pid`, `udid`, `bindAddress`, `hostPort`, `devicePort`). Physical roots are resolved separately for every session and enforced after directory links are followed, so rootless-jailbreak `.jbroot-*` aliases stay out of the UI without weakening the path boundary.

Password mode gives the bundled Mobius SSH client a one-time loopback capability; it fetches the password directly from the in-process broker and immediately removes that capability from its environment. The password stays in current-process memory, is redacted from captured output/errors, and is never placed in arguments, logs, preferences, or a credential file. An explicitly configured OpenSSH-compatible fallback uses the same broker through `SSH_ASKPASS`. Authentication is limited to the selected password or private key without method fallback. The UI defaults to `root` / `alpine` for the user's lab fleet but saves only `authMode`, connection fields, allowed roots, and an optional private-key path; changing devices resets the in-memory password to the default. It automatically attempts that default only once per explicitly selected device/address during one app run; failure opens the settings and requires a manual retry. Both modes use accept-new host-key behavior; host keys are persisted in the app data directory and a changed key is rejected. File operations use the bundled single-file SFTP implementation and are limited to the canonical allowed roots; symbolic-link traversal outside those roots is rejected, and an allowed root itself cannot be deleted. Uploading to a directory appends the local filename. Downloading to an existing local directory appends the remote filename. Existing destination files require `overwrite: true`, and final symbolic-link destinations are never overwritten.

## Jailbroken iOS SSH diagnostics

| Command | Invoke arguments | `data` |
| --- | --- | --- |
| `get_ios_runtime_snapshot` | `{ request: { sessionId, kind, syslogLines? } }` | `IosDiagnosticResult` |
| `prepare_ios_device_action` | `{ request: { sessionId, action, expectedSshHost, expectedSshPort, expectedUsername, expectedServerSystem? } }` | `IosDeviceActionConfirmation` |
| `run_ios_device_action` | `{ request: { confirmationId, sessionId, action, expectedSshHost, expectedSshPort, expectedUsername, expectedServerSystem?, expectedHostKeyIdentity } }` | `IosDeviceActionResult` |

`kind` is one of `overview`, `processes`, `tools`, or `syslog`. Every request is bound to an active, previously authenticated `IosSshSession`. Overview reads a fixed set of kernel, OS, identity, disk, memory, and uptime facts. Processes uses fixed absolute `ps` paths when available and falls back to a bounded `/bin/launchctl print system` service/process snapshot, returning at most 1,200 lines. Tools checks only compiled-in absolute paths for the development catalog (`frida-server`, OpenSSH Server, LLDB, debugserver, dpkg, ldid, otool, Apple log, and plutil). It reports file availability and the matching path without starting those binaries or running their version subcommands.

Syslog reads at most 20–400 lines (default 120), preferring the fixed `/usr/bin/log show` interface for the last five minutes and falling back to `/var/log/syslog`. Successful output is stripped of control characters and limited to 192 KiB; remote error messages are separately cleaned, limited to 8 KiB, and returned without raw process-detail payloads. Truncation and missing log sources are explicit warnings.

`action` is `respring` or `reboot`. Preparing an action requires a UID 0 session and an exact match with the UI's session endpoint snapshot. The backend returns a random, 30-second, single-use confirmation bound to that session, action, SSH host/port/user, server-system snapshot, and SSH host identity. The UI displays those actual SSH details before confirmation. Execution consumes the ticket first, rechecks the active session and every target field, then schedules one fixed backend action. Switching device or SSH session dismisses the pending dialog, and a used or expired ticket cannot be replayed.

## Safety and lifecycle boundaries

- Host tools are launched directly with `std::process::Command` and argument arrays. No host shell is invoked.
- Pairing secrets are written through stdin, redacted, and cleared from the temporary byte buffer.
- Internal Android shell operations use fixed templates and centralized POSIX quoting. `run_device_shell` is the separately exposed advanced device-side shell action.
- iOS SSH uses the bundled Mobius SSH/SFTP client with argument arrays. Passwords use the in-process one-time broker and never appear in those arrays. USB sessions bind the bundled go-ios adapter (or optional `iproxy` fallback) only to loopback; iOS server forwarding and the independent `-L` / `-R` adapters use separate SSH children with loopback endpoints. Closing an SSH session stops tunnels explicitly bound to that session; application exit stops every remaining managed iOS tunnel, including session-independent USB adapter children.
- Host-side iOS diagnostics expose four fixed libimobiledevice operations (`deviceInfo`, `pairing`, `apps`, `syslog`) with bounded time and output. The separate jailbreak SSH diagnostics expose `overview`, `processes`, `tools`, and `syslog`; Respring and reboot use root-only enums plus a backend-issued, session-bound, single-use confirmation ticket.
- Scanning is limited to prioritized active RFC1918 `/24` networks on physical interfaces, excludes common tunnel/virtual interfaces, bounds ports/concurrency/timeouts, and distinguishes an ADB endpoint from a merely open port.
- Remote deletion is limited to descendants of `/sdcard`, `/storage/emulated/<user>`, or `/data/local/tmp`.
- App close/exit performs best-effort session cleanup: it first stops and finalizes managed recordings, stops recorded embedded-screen children and removes their owned reverse/jar; it stops all recorded iOS tunnel children; it restores a proxy only when the complete current five-field snapshot still matches the state Mobius set; it stops only a recorded instrumentation process whose executable path and Linux start time still match; it removes only recorded Android forward/reverse mappings whose current target still matches. Errors are logged and do not cancel exit.
- The file dialog has only open/select permission. Tauri custom commands use an explicit command manifest and exact per-command capability grants for the main window.
