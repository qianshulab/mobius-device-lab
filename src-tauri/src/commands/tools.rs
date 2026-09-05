use super::blocking_api;
use crate::{
    models::{ApiResult, ConfigureToolchainRequest, ToolHealth, ToolchainConfiguration},
    runner::run_process_at_with_env,
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
        install_hint: "ADB is included with Mobius. Reinstall the application, or select a reviewed replacement executable.",
    },
    ToolSpec {
        id: "scrcpy",
        name: "scrcpy",
        version_args: Some(&["--version"]),
        purpose: "Android screen mirroring and interactive control",
        required: true,
        install_hint: "scrcpy and its matching Server are included with Mobius. Reinstall the application, or select a reviewed replacement.",
    },
    ToolSpec {
        id: "ffmpeg",
        name: "FFmpeg",
        version_args: Some(&["-version"]),
        purpose: "Decode the private scrcpy H.264 stream for the embedded cross-platform screen view",
        required: true,
        install_hint: "A minimal LGPL FFmpeg build is included with Mobius. Reinstall the application if it is unavailable.",
    },
    ToolSpec {
        id: "aapt2",
        name: "Android Asset Packaging Tool",
        version_args: Some(&["version"]),
        purpose: "Detailed APK manifest, version, label and permission analysis",
        required: true,
        install_hint: "The standalone AAPT2 analyzer is included with Mobius. Reinstall the application if it is unavailable.",
    },
    ToolSpec {
        id: "diec",
        name: "Detect It Easy",
        version_args: Some(&["-v"]),
        purpose: "Offline APK and DEX protector, packer and obfuscator signature analysis",
        required: true,
        install_hint: "The reviewed Detect It Easy CLI and signature database are included with Mobius. Reinstall the application if they are unavailable.",
    },
    ToolSpec {
        id: "ios",
        name: "go-ios",
        version_args: Some(&["version"]),
        purpose: "Bundled iOS discovery, device information, apps, installation, screenshots and logs",
        required: true,
        install_hint: "A loopback-hardened go-ios build is included with Mobius. Reinstall the application, or select a reviewed replacement for non-forwarding operations.",
    },
    ToolSpec {
        id: "ssh",
        name: "Mobius SSH Client",
        version_args: Some(&["-V"]),
        purpose: "Authenticated command execution on an explicitly selected jailbreak device",
        required: true,
        install_hint: "The restricted Mobius SSH client is included with the application. Reinstall Mobius, or select a reviewed compatible client.",
    },
    ToolSpec {
        id: "scp",
        name: "Mobius SFTP Transfer",
        version_args: Some(&["-V"]),
        purpose: "File transfer to and from an explicitly selected jailbreak device",
        required: true,
        install_hint: "The restricted Mobius SFTP transfer helper is included with the application. Reinstall Mobius, or select a reviewed compatible client.",
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
                let resolved = match if spec.id == "diec" {
                    toolchain::resolve_bundled_tool(spec.id)
                } else {
                    toolchain::resolve_tool(spec.id)
                } {
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

                if spec.id == "diec"
                    && toolchain::validate_bundled_die_database(&resolved.path).is_err()
                {
                    return common(
                        None,
                        "warning".into(),
                        Some("The bundled Detect It Easy signature database is missing".into()),
                    );
                }

                // The bundled SFTP entry point has a deterministic -V flag.
                // A developer-selected OpenSSH scp fallback has no portable
                // version flag, so keep it healthy after path validation.
                let version_args =
                    if spec.id == "scp" && resolved.source != toolchain::ToolSource::Bundled {
                        None
                    } else {
                        spec.version_args
                    };
                let Some(raw_args) = version_args else {
                    return common(None, "ready".into(), None);
                };
                let args = raw_args
                    .iter()
                    .map(|arg| (*arg).to_string())
                    .collect::<Vec<_>>();
                let environment = toolchain::bundled_tool_environment(spec.id, &resolved.path);
                match run_process_at_with_env(
                    spec.id,
                    &resolved.path,
                    &args,
                    VERSION_TIMEOUT,
                    &[],
                    &environment,
                ) {
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
