use super::blocking_api;
use crate::{
    models::{ApiResult, ConfigureToolchainRequest, ToolHealth, ToolchainConfiguration},
    runner::run_process_at,
    toolchain,
};
use std::time::Duration;

const VERSION_TIMEOUT: Duration = Duration::from_secs(4);

struct ToolSpec {
    id: &'static str,
    name: &'static str,
    version_args: Option<&'static [&'static str]>,
    purpose: &'static str,
    required: bool,
    install_hint: &'static str,
}

const TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        id: "adb",
        name: "Android Debug Bridge",
        version_args: Some(&["version"]),
        purpose: "Android device discovery, shell, files, networking, install and export",
        required: true,
        install_hint: "Install Android SDK Platform-Tools or select the adb executable in Toolchain settings.",
    },
    ToolSpec {
        id: "scrcpy",
        name: "scrcpy",
        version_args: Some(&["--version"]),
        purpose: "Android screen mirroring and interactive control",
        required: false,
        install_hint: "Install scrcpy from its official release or package manager, or select its executable.",
    },
    ToolSpec {
        id: "ffmpeg",
        name: "FFmpeg",
        version_args: Some(&["-version"]),
        purpose: "Decode the private scrcpy H.264 stream for the embedded cross-platform screen view",
        required: false,
        install_hint: "Install ffmpeg or place it in the configured managed tools directory to enable embedded live Android video.",
    },
    ToolSpec {
        id: "frida",
        name: "Frida CLI",
        version_args: Some(&["--version"]),
        purpose: "Host-side Frida client used with a user-supplied device server",
        required: false,
        install_hint: "Install frida-tools in an isolated Python environment or select the frida executable.",
    },
    ToolSpec {
        id: "aapt2",
        name: "Android Asset Packaging Tool",
        version_args: Some(&["version"]),
        purpose: "Detailed APK manifest, version, label and permission analysis",
        required: false,
        install_hint: "Install Android SDK Build-Tools or point the managed tools directory at aapt2.",
    },
    ToolSpec {
        id: "apkanalyzer",
        name: "Android APK Analyzer",
        // apkanalyzer has no portable version flag; recent releases return exit 0
        // with an error-like usage line for `--version`, so executable resolution is
        // the honest readiness probe here.
        version_args: None,
        purpose: "Secondary APK metadata analyzer when aapt2 is unavailable",
        required: false,
        install_hint: "Install Android SDK Command-line Tools or provide apkanalyzer in a configured tools directory.",
    },
    ToolSpec {
        id: "idevice_id",
        name: "iOS Device Discovery",
        version_args: Some(&["--version"]),
        purpose: "USB/network discovery of paired iOS devices",
        required: true,
        install_hint: "Install libimobiledevice or provide its iOS command directory.",
    },
    ToolSpec {
        id: "ideviceinfo",
        name: "iOS Device Info",
        version_args: Some(&["--version"]),
        purpose: "Read paired iOS device identity and operating-system information",
        required: true,
        install_hint: "Install libimobiledevice or provide its iOS command directory.",
    },
    ToolSpec {
        id: "idevicepair",
        name: "iOS Pairing Status",
        version_args: Some(&["--version"]),
        purpose: "Validate the selected iOS device pairing and trust relationship",
        required: false,
        install_hint: "Install libimobiledevice or provide idevicepair in the configured iOS tools directory.",
    },
    ToolSpec {
        id: "ideviceinstaller",
        name: "iOS Package Installer",
        version_args: Some(&["--version"]),
        purpose: "Install signed IPA packages on trusted or jailbreak-enabled devices",
        required: false,
        install_hint: "Install ideviceinstaller or provide it in the configured iOS tools directory.",
    },
    ToolSpec {
        id: "idevicescreenshot",
        name: "iOS Screenshot Service Client",
        version_args: Some(&["--version"]),
        purpose: "Capture a paired iOS device screen for inline preview and screenshots",
        required: false,
        install_hint: "Install libimobiledevice with idevicescreenshot, then pair/trust the device and mount its matching Developer Disk Image.",
    },
    ToolSpec {
        id: "idevicesyslog",
        name: "iOS System Log Relay",
        version_args: Some(&["--version"]),
        purpose: "Capture a bounded system-log snapshot from a selected iOS device",
        required: false,
        install_hint: "Install libimobiledevice or provide idevicesyslog in the configured iOS tools directory.",
    },
    ToolSpec {
        id: "iproxy",
        name: "USB Port Tunnel",
        version_args: Some(&["--version"]),
        purpose: "Expose a jailbreak device SSH port over its USB connection",
        required: false,
        install_hint: "Install usbmuxd/libusbmuxd tools or provide iproxy in the iOS tools directory.",
    },
    ToolSpec {
        id: "ssh",
        name: "OpenSSH Client",
        version_args: Some(&["-V"]),
        purpose: "Authenticated command execution on an explicitly selected jailbreak device",
        required: true,
        install_hint: "Enable or install the operating system OpenSSH Client, or provide ssh in the iOS tools directory.",
    },
    ToolSpec {
        id: "scp",
        name: "OpenSSH Secure Copy",
        version_args: None,
        purpose: "File transfer to and from an explicitly selected jailbreak device",
        required: true,
        install_hint: "Enable or install the operating system OpenSSH Client, or provide scp in the iOS tools directory.",
    },
];

#[tauri::command]
pub async fn configure_toolchain(
    request: ConfigureToolchainRequest,
) -> ApiResult<ToolchainConfiguration> {
    blocking_api(move || toolchain::configure(request)).await
}

#[tauri::command]
pub async fn get_tool_health() -> ApiResult<Vec<ToolHealth>> {
    blocking_api(|| {
        let tools = TOOL_SPECS
            .iter()
            .map(|spec| {
                let resolved = match toolchain::resolve_tool(spec.id) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        return ToolHealth {
                            id: spec.id.to_string(),
                            name: spec.name.to_string(),
                            version: None,
                            state: "missing".into(),
                            path: None,
                            source: None,
                            purpose: spec.purpose.to_string(),
                            required: spec.required,
                            install_hint: spec.install_hint.to_string(),
                            hint: Some(error.message),
                        };
                    }
                };
                let common = |version, state, hint| ToolHealth {
                    id: spec.id.to_string(),
                    name: spec.name.to_string(),
                    version,
                    state,
                    path: Some(resolved.path.to_string_lossy().into_owned()),
                    source: Some(resolved.source.as_str().to_string()),
                    purpose: spec.purpose.to_string(),
                    required: spec.required,
                    install_hint: spec.install_hint.to_string(),
                    hint,
                };

                let Some(raw_args) = spec.version_args else {
                    return common(None, "ready".into(), None);
                };
                let args = raw_args
                    .iter()
                    .map(|arg| (*arg).to_string())
                    .collect::<Vec<_>>();
                match run_process_at(spec.id, &resolved.path, &args, VERSION_TIMEOUT, &[]) {
                    Ok(output) if output.exit_code == Some(0) && !output.timed_out => {
                        let text = if output.stdout.is_empty() {
                            output.stderr
                        } else {
                            output.stdout
                        };
                        common(first_meaningful_line(&text), "ready".into(), None)
                    }
                    Ok(output) => {
                        let warning = if output.timed_out {
                            "Version check timed out".to_string()
                        } else if output.stderr.is_empty() {
                            format!("Version check exited with status {:?}", output.exit_code)
                        } else {
                            output.stderr
                        };
                        common(None, "warning".into(), Some(warning))
                    }
                    Err(error) => common(None, "warning".into(), Some(error.message)),
                }
            })
            .collect();
        Ok(tools)
    })
    .await
}

fn first_meaningful_line(value: &str) -> Option<String> {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(240).collect())
}
