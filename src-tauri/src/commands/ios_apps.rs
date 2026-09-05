use super::{blocking_api, ios_ssh, packages};
use crate::{
    models::{
        ApiError, ApiResult, AppResult, ExportIosAppBundleRequest, InstallIosPackageSshRequest,
        IosAppCapabilities, IosAppExportResult, IosInstalledApp, IosInstalledAppScope,
        IosPackageInstallResult, IosPackageInstaller, IosPackageInstallerId,
        ListIosInstalledAppsRequest, ProbeIosAppCapabilitiesRequest,
    },
    state::{AppState, IosSshConnection},
    validation,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::State;

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const COPY_TIMEOUT: Duration = Duration::from_secs(600);
const EXPORT_TIMEOUT: Duration = Duration::from_secs(600);
const RUNTIME_DIRECTORY_NAME: &str = ".mobius-runtime";
const MAX_IOS_APPS: usize = 500;
const DEFAULT_IOS_APPS: usize = 300;
const METADATA_BATCH_SIZE: usize = 60;
const MAX_APP_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const APP_LINE_MARKER: &str = "MOBIUS_IOS_APP";
const TOOL_LINE_MARKER: &str = "MOBIUS_IOS_TOOL";

#[derive(Debug, Clone, Copy)]
struct InstallerSpec {
    id: IosPackageInstallerId,
    name: &'static str,
    path: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlutilMode {
    Extract,
    Key,
}

impl PlutilMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Extract => "extract",
            Self::Key => "key",
        }
    }
}

const INSTALLER_SPECS: &[InstallerSpec] = &[
    InstallerSpec {
        id: IosPackageInstallerId::Appinst,
        name: "appinst",
        path: "/usr/bin/appinst",
    },
    InstallerSpec {
        id: IosPackageInstallerId::Appinst,
        name: "appinst (rootless)",
        path: "/var/jb/usr/bin/appinst",
    },
    InstallerSpec {
        id: IosPackageInstallerId::Appinst,
        name: "appinst (local)",
        path: "/usr/local/bin/appinst",
    },
    InstallerSpec {
        id: IosPackageInstallerId::Ipainstaller,
        name: "IPA Installer Console",
        path: "/usr/bin/ipainstaller",
    },
    InstallerSpec {
        id: IosPackageInstallerId::Ipainstaller,
        name: "IPA Installer Console (rootless)",
        path: "/var/jb/usr/bin/ipainstaller",
    },
    InstallerSpec {
        id: IosPackageInstallerId::Ipainstaller,
        name: "IPA Installer Console (local)",
        path: "/usr/local/bin/ipainstaller",
    },
];

const PLUTIL_PATHS: &[&str] = &["/usr/bin/plutil", "/var/jb/usr/bin/plutil"];
const BASE64_PATHS: &[&str] = &["/usr/bin/base64", "/bin/base64", "/var/jb/usr/bin/base64"];
const TAR_PATHS: &[&str] = &["/usr/bin/tar", "/bin/tar", "/var/jb/usr/bin/tar"];

const SYSTEM_APP_ROOTS: &[&str] = &["/Applications", "/var/jb/Applications"];
const USER_APP_ROOTS: &[&str] = &[
    "/var/containers/Bundle/Application",
    "/private/var/containers/Bundle/Application",
    "/var/mobile/Containers/Bundle/Application",
    "/private/var/mobile/Containers/Bundle/Application",
];

#[tauri::command]
pub async fn probe_ios_app_capabilities(
    request: ProbeIosAppCapabilitiesRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosAppCapabilities>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let (connection, _) = ios_ssh::session_snapshot(&state, &request.session_id)?;
        require_root_session(&connection)?;
        probe_capabilities(&request.session_id, &connection)
    })
    .await)
}

#[tauri::command]
pub async fn install_ios_package_ssh(
    request: InstallIosPackageSshRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosPackageInstallResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || install_package_ssh(&state, request)).await)
}

#[tauri::command]
pub async fn list_ios_installed_apps(
    request: ListIosInstalledAppsRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<Vec<IosInstalledApp>>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || list_installed_apps(&state, request)).await)
}

#[tauri::command]
pub async fn export_ios_app_bundle(
    request: ExportIosAppBundleRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<IosAppExportResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || export_app_bundle(&state, request)).await)
}

fn require_root_session(connection: &IosSshConnection) -> AppResult<()> {
    if connection.remote_uid == Some(0) {
        Ok(())
    } else {
        Err(ApiError::new(
            "ios_root_ssh_required",
            "iOS application management requires a verified root (uid 0) SSH session",
        ))
    }
}

fn probe_capabilities(
    session_id: &str,
    connection: &IosSshConnection,
) -> AppResult<IosAppCapabilities> {
    let mut checks = Vec::new();
    for spec in INSTALLER_SPECS {
        checks.push(format!(
            "if [ -x {path} ]; then printf '{TOOL_LINE_MARKER}:installer:{id}:{path}\\n'; fi",
            path = validation::quote_remote(spec.path),
            id = spec.id.as_str(),
        ));
    }
    for path in PLUTIL_PATHS {
        checks.push(format!(
            "if [ -x {path} ] && {path} -extract ProductVersion raw -o - /System/Library/CoreServices/SystemVersion.plist >/dev/null 2>&1; then printf '{TOOL_LINE_MARKER}:plutil:extract:{raw_path}\\n'; elif [ -x {path} ] && {path} -key ProductVersion /System/Library/CoreServices/SystemVersion.plist >/dev/null 2>&1; then printf '{TOOL_LINE_MARKER}:plutil:key:{raw_path}\\n'; fi",
            path = validation::quote_remote(path),
            raw_path = path,
        ));
    }
    for path in BASE64_PATHS {
        checks.push(format!(
            "if [ -x {path} ]; then printf '{TOOL_LINE_MARKER}:base64:{path}\\n'; fi",
            path = validation::quote_remote(path),
        ));
    }
    for path in TAR_PATHS {
        checks.push(format!(
            "if [ -x {path} ]; then printf '{TOOL_LINE_MARKER}:tar:{path}\\n'; fi",
            path = validation::quote_remote(path),
        ));
    }
    let output = ios_ssh::run_ssh_command(connection, &checks.join("; "), PROBE_TIMEOUT)?;
    let mut installers = Vec::new();
    let mut plutil_path = None;
    let mut plutil_mode = None;
    let mut base64_path = None;
    let mut tar_path = None;
    for line in output.stdout.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix(&format!("{TOOL_LINE_MARKER}:installer:")) {
            let Some((id, path)) = value.split_once(':') else {
                continue;
            };
            if let Some(spec) = installer_spec_from_output(id, path) {
                if !installers
                    .iter()
                    .any(|installed: &IosPackageInstaller| installed.path == spec.path)
                {
                    installers.push(IosPackageInstaller {
                        id: spec.id,
                        name: spec.name.to_string(),
                        path: spec.path.to_string(),
                    });
                }
            }
        } else if let Some(path) = line.strip_prefix(&format!("{TOOL_LINE_MARKER}:plutil:")) {
            let Some((mode, path)) = path.split_once(':') else {
                continue;
            };
            if plutil_path.is_none() && PLUTIL_PATHS.contains(&path) {
                let Some(mode) = parse_plutil_mode(mode) else {
                    continue;
                };
                plutil_path = Some(path.to_string());
                plutil_mode = Some(mode.as_str().to_string());
            }
        } else if let Some(path) = line.strip_prefix(&format!("{TOOL_LINE_MARKER}:base64:")) {
            if base64_path.is_none() && BASE64_PATHS.contains(&path) {
                base64_path = Some(path.to_string());
            }
        } else if let Some(path) = line.strip_prefix(&format!("{TOOL_LINE_MARKER}:tar:")) {
            if tar_path.is_none() && TAR_PATHS.contains(&path) {
                tar_path = Some(path.to_string());
            }
        }
    }
    installers.sort_by_key(|installer| match installer.id {
        IosPackageInstallerId::Appinst => 0,
        IosPackageInstallerId::Ipainstaller => 1,
    });
    let preferred_installer = installers.first().cloned();
    let listing_available = plutil_path.is_some() && base64_path.is_some();
    let mut warnings = Vec::new();
    if installers.is_empty() {
        warnings.push(
            "No supported on-device IPA installer was found; Mobius will not install or download one"
                .into(),
        );
    }
    if !listing_available {
        warnings.push(
            "Installed-app metadata requires the device's existing plutil and base64 commands"
                .into(),
        );
    }
    if tar_path.is_none() {
        warnings.push("App-bundle export requires an existing tar command on the device".into());
    }
    Ok(IosAppCapabilities {
        session_id: session_id.to_string(),
        root_session: true,
        installers,
        preferred_installer,
        listing_available,
        export_available: tar_path.is_some(),
        plutil_path,
        plutil_mode,
        base64_path,
        tar_path,
        warnings,
    })
}

fn installer_spec_from_output(id: &str, path: &str) -> Option<&'static InstallerSpec> {
    INSTALLER_SPECS
        .iter()
        .find(|spec| spec.id.as_str() == id && spec.path == path)
}

fn parse_plutil_mode(value: &str) -> Option<PlutilMode> {
    match value {
        "extract" => Some(PlutilMode::Extract),
        "key" => Some(PlutilMode::Key),
        _ => None,
    }
}

fn install_package_ssh(
    state: &AppState,
    request: InstallIosPackageSshRequest,
) -> AppResult<IosPackageInstallResult> {
    let (connection, _) = ios_ssh::session_snapshot(state, &request.session_id)?;
    require_root_session(&connection)?;
    let source =
        packages::validate_package_file(&request.path, crate::models::MobilePlatform::Ios)?;
    let analysis = packages::analyze_package(&source.to_string_lossy())?;
    if analysis.platform != crate::models::MobilePlatform::Ios {
        return Err(ApiError::new(
            "package_platform_mismatch",
            "The selected package is not an IPA",
        ));
    }
    let capabilities = probe_capabilities(&request.session_id, &connection)?;
    let installer = select_installer(&capabilities.installers, request.installer_id)?;
    let runtime = prepare_runtime_directory(&connection)?;
    let remote_path = format!("{runtime}/.pkg-{}.ipa", operation_nonce());
    validate_runtime_path(&connection, &remote_path)?;

    let mut scp_args = ios_ssh::scp_base_args(&connection);
    scp_args.push(source.to_string_lossy().into_owned());
    scp_args.push(ios_ssh::scp_remote_spec(&connection, &remote_path)?);
    if let Err(error) = ios_ssh::run_connection_tool(&connection, "scp", &scp_args, COPY_TIMEOUT) {
        let _ = cleanup_remote_file(&connection, &runtime, &remote_path);
        return Err(error);
    }

    let install_command = build_installer_command(&installer, &remote_path);
    let install_result = ios_ssh::run_ssh_command(&connection, &install_command, INSTALL_TIMEOUT);
    let cleanup = cleanup_remote_file(&connection, &runtime, &remote_path);
    match install_result {
        Ok(output) => {
            let mut warnings = vec![
                "The device installer remains responsible for signature, trust, provisioning, and AppSync compatibility; Mobius leaves those checks unchanged"
                    .into(),
            ];
            let cleaned = match cleanup {
                Ok(()) => true,
                Err(error) => {
                    warnings.push(format!(
                        "The temporary uploaded IPA could not be removed: {}",
                        error.message
                    ));
                    false
                }
            };
            Ok(IosPackageInstallResult {
                success: true,
                message: format!("IPA installation completed through {}", installer.name),
                session_id: request.session_id,
                installer,
                package_name: analysis.package_name,
                remote_temporary_path: remote_path,
                temporary_file_cleaned: cleaned,
                stdout: non_empty_limited(output.stdout, 64 * 1024),
                stderr: non_empty_limited(output.stderr, 64 * 1024),
                warnings,
            })
        }
        Err(mut error) => {
            if let Err(cleanup_error) = cleanup {
                error.message = format!(
                    "{}; temporary IPA cleanup also failed: {}",
                    error.message, cleanup_error.message
                );
            }
            Err(error)
        }
    }
}

fn select_installer(
    installers: &[IosPackageInstaller],
    requested: Option<IosPackageInstallerId>,
) -> AppResult<IosPackageInstaller> {
    let selected = match requested {
        Some(id) => installers.iter().find(|installer| installer.id == id),
        None => installers.first(),
    };
    selected.cloned().ok_or_else(|| {
        ApiError::new(
            "ios_package_installer_unavailable",
            match requested {
                Some(id) => format!(
                    "The selected {} installer is not available in a fixed supported device path",
                    id.as_str()
                ),
                None => "No supported on-device IPA installer is available; install one on the test device yourself or use USB ideviceinstaller"
                    .into(),
            },
        )
    })
}

fn build_installer_command(installer: &IosPackageInstaller, remote_path: &str) -> String {
    // Both allowlisted tools accept one IPA path. No user-supplied options are forwarded.
    format!(
        "{} {}",
        validation::quote_remote(&installer.path),
        validation::quote_remote(remote_path)
    )
}

fn list_installed_apps(
    state: &AppState,
    request: ListIosInstalledAppsRequest,
) -> AppResult<Vec<IosInstalledApp>> {
    let (connection, _) = ios_ssh::session_snapshot(state, &request.session_id)?;
    require_root_session(&connection)?;
    let capabilities = probe_capabilities(&request.session_id, &connection)?;
    let plutil = capabilities.plutil_path.ok_or_else(|| {
        ApiError::new(
            "ios_plutil_unavailable",
            "The device does not expose a supported plutil command for application metadata",
        )
    })?;
    let plutil_mode = capabilities
        .plutil_mode
        .as_deref()
        .and_then(parse_plutil_mode)
        .ok_or_else(|| {
            ApiError::new(
                "ios_plutil_mode_unavailable",
                "The device plutil dialect could not be selected safely",
            )
        })?;
    let base64 = capabilities.base64_path.ok_or_else(|| {
        ApiError::new(
            "ios_base64_unavailable",
            "The device does not expose a supported base64 command for safe metadata transport",
        )
    })?;
    let limit = request
        .limit
        .map(usize::from)
        .unwrap_or(DEFAULT_IOS_APPS)
        .clamp(1, MAX_IOS_APPS);
    let roots = roots_for_scope(request.scope);
    let path_output = ios_ssh::run_ssh_command(
        &connection,
        &build_app_path_discovery_command(roots, limit + 1),
        LIST_TIMEOUT,
    )?;
    let mut paths = path_output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|path| validate_ios_app_plist_path(path).is_ok())
        .take(limit)
        .map(str::to_string)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();

    let mut by_bundle_id = BTreeMap::<String, IosInstalledApp>::new();
    for batch in paths.chunks(METADATA_BATCH_SIZE) {
        let output = ios_ssh::run_ssh_command(
            &connection,
            &build_metadata_command(batch, &plutil, plutil_mode, &base64),
            LIST_TIMEOUT,
        )?;
        for app in parse_app_metadata(&output.stdout) {
            if !scope_accepts_path(request.scope, &app.app_path) {
                continue;
            }
            by_bundle_id
                .entry(app.bundle_id.clone())
                .and_modify(|existing| {
                    if prefer_app_path(&app.app_path, &existing.app_path) {
                        *existing = app.clone();
                    }
                })
                .or_insert(app);
        }
    }
    let mut apps = by_bundle_id.into_values().collect::<Vec<_>>();
    apps.sort_by(|left, right| {
        left.system
            .cmp(&right.system)
            .then_with(|| {
                left.display_name
                    .to_lowercase()
                    .cmp(&right.display_name.to_lowercase())
            })
            .then_with(|| left.bundle_id.cmp(&right.bundle_id))
    });
    apps.truncate(limit);
    Ok(apps)
}

fn roots_for_scope(scope: IosInstalledAppScope) -> &'static [&'static str] {
    match scope {
        IosInstalledAppScope::All => &[
            "/Applications",
            "/var/jb/Applications",
            "/var/containers/Bundle/Application",
            "/private/var/containers/Bundle/Application",
            "/var/mobile/Containers/Bundle/Application",
            "/private/var/mobile/Containers/Bundle/Application",
        ],
        IosInstalledAppScope::User => USER_APP_ROOTS,
        IosInstalledAppScope::System => SYSTEM_APP_ROOTS,
    }
}

fn build_app_path_discovery_command(roots: &[&str], limit: usize) -> String {
    let commands = roots
        .iter()
        .map(|root| {
            let depth = if SYSTEM_APP_ROOTS.contains(root) { 1 } else { 2 };
            let quoted = validation::quote_remote(root);
            format!(
                "if [ -d {quoted} ]; then find {quoted} -xdev -mindepth {depth} -maxdepth {depth} -type d -name '*.app' -print 2>/dev/null | while IFS= read -r mobius_app; do if [ -f \"$mobius_app/Info.plist\" ] && [ ! -L \"$mobius_app/Info.plist\" ]; then printf '%s/Info.plist\\n' \"$mobius_app\"; fi; done; fi"
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("{{ {commands}; }} | head -n {limit}")
}

fn build_metadata_command(
    paths: &[String],
    plutil: &str,
    plutil_mode: PlutilMode,
    base64: &str,
) -> String {
    let plutil = validation::quote_remote(plutil);
    let base64 = validation::quote_remote(base64);
    paths
        .iter()
        .map(|path| {
            let path = validation::quote_remote(path);
            let read_id = plist_read_command(
                &plutil,
                plutil_mode,
                "CFBundleIdentifier",
                "\"$mobius_plist\"",
            );
            let read_display_name = plist_read_command(
                &plutil,
                plutil_mode,
                "CFBundleDisplayName",
                "\"$mobius_plist\"",
            );
            let read_name =
                plist_read_command(&plutil, plutil_mode, "CFBundleName", "\"$mobius_plist\"");
            let read_version = plist_read_command(
                &plutil,
                plutil_mode,
                "CFBundleShortVersionString",
                "\"$mobius_plist\"",
            );
            let read_build =
                plist_read_command(&plutil, plutil_mode, "CFBundleVersion", "\"$mobius_plist\"");
            format!(
                "mobius_plist={path}; \
                 mobius_id=$({read_id} 2>/dev/null); \
                 mobius_name=$({read_display_name} 2>/dev/null); \
                 if [ -z \"$mobius_name\" ]; then mobius_name=$({read_name} 2>/dev/null); fi; \
                 mobius_version=$({read_version} 2>/dev/null); \
                 mobius_build=$({read_build} 2>/dev/null); \
                 if [ -n \"$mobius_id\" ]; then \
                   printf '{APP_LINE_MARKER}\\t'; \
                   printf '%s' \"$mobius_plist\" | {base64} | tr -d '\\r\\n'; printf '\\t'; \
                   printf '%s' \"$mobius_id\" | {base64} | tr -d '\\r\\n'; printf '\\t'; \
                   printf '%s' \"$mobius_name\" | {base64} | tr -d '\\r\\n'; printf '\\t'; \
                   printf '%s' \"$mobius_version\" | {base64} | tr -d '\\r\\n'; printf '\\t'; \
                   printf '%s' \"$mobius_build\" | {base64} | tr -d '\\r\\n'; printf '\\n'; \
                 fi"
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn plist_read_command(plutil: &str, mode: PlutilMode, key: &str, target: &str) -> String {
    match mode {
        PlutilMode::Extract => format!("{plutil} -extract {key} raw -o - {target}"),
        PlutilMode::Key => format!("{plutil} -key {key} {target}"),
    }
}

fn parse_app_metadata(output: &str) -> Vec<IosInstalledApp> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != 6 || fields[0] != APP_LINE_MARKER {
                return None;
            }
            let plist_path = decode_field(fields[1])?;
            validate_ios_app_plist_path(&plist_path).ok()?;
            let app_path = plist_path.strip_suffix("/Info.plist")?.to_string();
            let bundle_id = decode_field(fields[2])?;
            validate_ios_bundle_id(&bundle_id).ok()?;
            let display_name = decode_field(fields[3])
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| app_bundle_name(&app_path));
            let version_name = decode_field(fields[4]).filter(|value| !value.trim().is_empty());
            let build_version = decode_field(fields[5]).filter(|value| !value.trim().is_empty());
            Some(IosInstalledApp {
                bundle_id,
                display_name: display_name.chars().take(256).collect(),
                version_name: version_name.map(|value| value.chars().take(128).collect()),
                build_version: build_version.map(|value| value.chars().take(128).collect()),
                system: is_system_app_path(&app_path),
                app_path,
            })
        })
        .collect()
}

fn decode_field(value: &str) -> Option<String> {
    let bytes = BASE64_STANDARD.decode(value.as_bytes()).ok()?;
    if bytes.len() > 4096 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn validate_ios_bundle_id(value: &str) -> AppResult<&str> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with(['.', '-'])
        || value.ends_with(['.', '-'])
        || !value.contains('.')
        || value
            .chars()
            .any(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_')))
        || value.split('.').any(str::is_empty)
    {
        return Err(ApiError::new(
            "invalid_ios_bundle_id",
            "The iOS bundle identifier is invalid",
        ));
    }
    Ok(value)
}

fn validate_ios_app_plist_path(value: &str) -> AppResult<&str> {
    validation::remote_path(value)?;
    let app_path = value.strip_suffix("/Info.plist").ok_or_else(|| {
        ApiError::new(
            "invalid_ios_app_path",
            "The application metadata path must end in .app/Info.plist",
        )
    })?;
    validate_ios_app_path(app_path)?;
    Ok(value)
}

fn validate_ios_app_path(value: &str) -> AppResult<&str> {
    validation::remote_path(value)?;
    if !value.ends_with(".app") {
        return Err(ApiError::new(
            "invalid_ios_app_path",
            "The selected path must be a top-level .app bundle",
        ));
    }
    let valid = SYSTEM_APP_ROOTS.iter().any(|root| {
        value.strip_prefix(&format!("{root}/")).is_some_and(|rest| {
            rest.ends_with(".app") && !rest.trim_end_matches(".app").contains('/')
        })
    }) || USER_APP_ROOTS.iter().any(|root| {
        value.strip_prefix(&format!("{root}/")).is_some_and(|rest| {
            let mut parts = rest.split('/');
            let container = parts.next().unwrap_or_default();
            let app = parts.next().unwrap_or_default();
            parts.next().is_none()
                && !container.is_empty()
                && app.ends_with(".app")
                && !app.trim_end_matches(".app").is_empty()
        })
    });
    if valid {
        Ok(value)
    } else {
        Err(ApiError::new(
            "ios_app_path_outside_fixed_roots",
            "The selected .app bundle is not in a supported fixed iOS application directory",
        ))
    }
}

fn scope_accepts_path(scope: IosInstalledAppScope, path: &str) -> bool {
    match scope {
        IosInstalledAppScope::All => true,
        IosInstalledAppScope::User => !is_system_app_path(path),
        IosInstalledAppScope::System => is_system_app_path(path),
    }
}

fn is_system_app_path(path: &str) -> bool {
    SYSTEM_APP_ROOTS
        .iter()
        .any(|root| path.strip_prefix(&format!("{root}/")).is_some())
}

fn prefer_app_path(candidate: &str, current: &str) -> bool {
    app_path_preference(candidate) < app_path_preference(current)
}

fn app_path_preference(path: &str) -> u8 {
    if path.starts_with("/Applications/") {
        0
    } else if path.starts_with("/var/containers/") || path.starts_with("/var/mobile/Containers/") {
        1
    } else if path.starts_with("/private/") {
        2
    } else {
        3
    }
}

fn app_bundle_name(path: &str) -> String {
    path.rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix(".app"))
        .filter(|name| !name.is_empty())
        .unwrap_or("iOS App")
        .chars()
        .take(256)
        .collect()
}

fn export_app_bundle(
    state: &AppState,
    request: ExportIosAppBundleRequest,
) -> AppResult<IosAppExportResult> {
    validate_ios_bundle_id(&request.bundle_id)?;
    validate_ios_app_path(&request.app_path)?;
    let (connection, _) = ios_ssh::session_snapshot(state, &request.session_id)?;
    require_root_session(&connection)?;
    let capabilities = probe_capabilities(&request.session_id, &connection)?;
    let plutil = capabilities.plutil_path.ok_or_else(|| {
        ApiError::new(
            "ios_plutil_unavailable",
            "plutil is required to verify the selected application bundle",
        )
    })?;
    let plutil_mode = capabilities
        .plutil_mode
        .as_deref()
        .and_then(parse_plutil_mode)
        .ok_or_else(|| {
            ApiError::new(
                "ios_plutil_mode_unavailable",
                "The device plutil dialect could not be selected safely",
            )
        })?;
    let tar = capabilities.tar_path.ok_or_else(|| {
        ApiError::new(
            "ios_tar_unavailable",
            "No supported tar command is installed on the device",
        )
    })?;
    let canonical_app_path = canonicalize_app_path(&connection, &request.app_path)?;
    let remote_bundle_id =
        extract_bundle_id(&connection, &plutil, plutil_mode, &canonical_app_path)?;
    if remote_bundle_id != request.bundle_id {
        return Err(ApiError::new(
            "ios_app_identity_changed",
            "The selected application path no longer matches its listed bundle identifier; refresh the app list",
        ));
    }
    preflight_app_size(&connection, &canonical_app_path)?;

    let destination = validate_export_directory(&request.destination)?;
    let target = destination.join(format!(
        "{}-mobius-app.tar.gz",
        safe_local_component(&request.bundle_id)
    ));
    validate_export_target(&target, request.overwrite)?;
    let temporary = destination.join(format!(
        ".mobius-ios-export-{}-{}.part",
        std::process::id(),
        operation_nonce()
    ));
    if temporary.exists() {
        return Err(ApiError::new(
            "ios_export_busy",
            "A previous iOS application export appears to still be in progress",
        ));
    }

    let runtime = prepare_runtime_directory(&connection)?;
    let remote_archive = format!("{runtime}/.app-export-{}.tar.gz", operation_nonce());
    validate_runtime_path(&connection, &remote_archive)?;
    let archive_result = (|| {
        let (parent, name) = canonical_app_path.rsplit_once('/').ok_or_else(|| {
            ApiError::new(
                "invalid_ios_app_path",
                "The app path has no parent directory",
            )
        })?;
        let command = format!(
            "cd {parent} && [ \"$(pwd -P)\" = {parent} ] && {tar} -czf {archive} -- {bundle}",
            parent = validation::quote_remote(parent),
            tar = validation::quote_remote(&tar),
            archive = validation::quote_remote(&remote_archive),
            bundle = validation::quote_remote(&format!("./{name}")),
        );
        ios_ssh::run_ssh_command(&connection, &command, EXPORT_TIMEOUT)?;
        let archive_size = remote_file_size(&connection, &remote_archive)?;
        if archive_size == 0 || archive_size > MAX_APP_ARCHIVE_BYTES {
            return Err(ApiError::new(
                "ios_app_archive_size_invalid",
                "The generated app analysis archive is empty or exceeds 4 GiB",
            ));
        }
        let mut args = ios_ssh::scp_base_args(&connection);
        args.push(ios_ssh::scp_remote_spec(&connection, &remote_archive)?);
        args.push(temporary.to_string_lossy().into_owned());
        ios_ssh::run_connection_tool(&connection, "scp", &args, COPY_TIMEOUT)?;
        let copied_size = temporary
            .metadata()
            .map_err(|error| {
                ApiError::new(
                    "ios_export_validation_failed",
                    format!("Unable to inspect the downloaded archive: {error}"),
                )
            })?
            .len();
        if copied_size != archive_size {
            return Err(ApiError::new(
                "ios_export_size_mismatch",
                "The downloaded archive size does not match the device archive",
            ));
        }
        if target.exists() {
            fs::remove_file(&target).map_err(|error| {
                ApiError::new(
                    "ios_export_replace_failed",
                    format!("Unable to replace the existing export: {error}"),
                )
            })?;
        }
        fs::rename(&temporary, &target).map_err(|error| {
            ApiError::new(
                "ios_export_finalize_failed",
                format!("Unable to finalize the downloaded archive: {error}"),
            )
        })?;
        Ok((archive_size, target))
    })();

    let cleanup = cleanup_remote_file(&connection, &runtime, &remote_archive);
    match archive_result {
        Ok((size_bytes, local_path)) => {
            let mut warnings = vec![
                "This is a development-analysis .app tar.gz archive, not an IPA, and it is not guaranteed to be installable"
                    .into(),
                "Executable protection state is not determined; Mobius does not change it or reconstruct signatures, provisioning, or a distributable IPA"
                    .into(),
            ];
            if let Err(error) = cleanup {
                warnings.push(format!(
                    "The temporary device archive could not be removed: {}",
                    error.message
                ));
            }
            Ok(IosAppExportResult {
                success: true,
                message: "Exported .app bundle as a development-analysis tar.gz archive".into(),
                session_id: request.session_id,
                bundle_id: request.bundle_id,
                app_path: canonical_app_path,
                local_path: local_path.to_string_lossy().into_owned(),
                format: "analysisTarGz".into(),
                size_bytes,
                installable: false,
                encryption_status: "unknown".into(),
                warnings,
            })
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            if let Err(cleanup_error) = cleanup {
                return Err(ApiError::new(
                    error.code,
                    format!(
                        "{}; temporary device archive cleanup also failed: {}",
                        error.message, cleanup_error.message
                    ),
                ));
            }
            Err(error)
        }
    }
}

fn canonicalize_app_path(connection: &IosSshConnection, path: &str) -> AppResult<String> {
    validate_ios_app_path(path)?;
    let quoted = validation::quote_remote(path);
    let command = format!(
        "if [ -L {quoted} ] || [ ! -d {quoted} ]; then exit 74; fi; cd {quoted} && printf 'MOBIUS_APP_CANONICAL:%s\\n' \"$(pwd -P)\""
    );
    let output = ios_ssh::run_ssh_command(connection, &command, PROBE_TIMEOUT)?;
    let canonical = output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("MOBIUS_APP_CANONICAL:"))
        .ok_or_else(|| {
            ApiError::new(
                "ios_app_path_validation_failed",
                "The device did not return a canonical application path",
            )
        })?;
    validate_ios_app_path(canonical)?;
    Ok(canonical.to_string())
}

fn extract_bundle_id(
    connection: &IosSshConnection,
    plutil: &str,
    plutil_mode: PlutilMode,
    app_path: &str,
) -> AppResult<String> {
    let plist = format!("{app_path}/Info.plist");
    validate_ios_app_plist_path(&plist)?;
    let read_bundle_id = plist_read_command(
        &validation::quote_remote(plutil),
        plutil_mode,
        "CFBundleIdentifier",
        &validation::quote_remote(&plist),
    );
    let command = format!(
        "if [ -L {plist} ] || [ ! -f {plist} ]; then exit 74; fi; mobius_id=$({read_bundle_id} 2>/dev/null) && printf 'MOBIUS_APP_BUNDLE_ID:%s\\n' \"$mobius_id\"",
        plist = validation::quote_remote(&plist),
    );
    let output = ios_ssh::run_ssh_command(connection, &command, PROBE_TIMEOUT)?;
    let value = output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("MOBIUS_APP_BUNDLE_ID:"))
        .ok_or_else(|| {
            ApiError::new(
                "ios_app_metadata_unavailable",
                "Unable to read CFBundleIdentifier from the selected app",
            )
        })?;
    validate_ios_bundle_id(value)?;
    Ok(value.to_string())
}

fn preflight_app_size(connection: &IosSshConnection, app_path: &str) -> AppResult<()> {
    let command = format!(
        "mobius_kb=$(du -sk {} 2>/dev/null | awk '{{print $1}}'); printf 'MOBIUS_APP_KB:%s\\n' \"$mobius_kb\"",
        validation::quote_remote(app_path)
    );
    let output = ios_ssh::run_ssh_command(connection, &command, PROBE_TIMEOUT)?;
    let kilobytes = output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("MOBIUS_APP_KB:"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            ApiError::new(
                "ios_app_size_unavailable",
                "Unable to determine the selected app bundle size",
            )
        })?;
    if kilobytes == 0 || kilobytes.saturating_mul(1024) > MAX_APP_ARCHIVE_BYTES {
        Err(ApiError::new(
            "ios_app_too_large",
            "The selected app bundle is empty or exceeds the 4 GiB export safety limit",
        ))
    } else {
        Ok(())
    }
}

fn remote_file_size(connection: &IosSshConnection, path: &str) -> AppResult<u64> {
    let command = format!(
        "if [ -L {path} ] || [ ! -f {path} ]; then exit 74; fi; mobius_bytes=$(wc -c < {path}); printf 'MOBIUS_ARCHIVE_BYTES:%s\\n' \"$mobius_bytes\"",
        path = validation::quote_remote(path)
    );
    let output = ios_ssh::run_ssh_command(connection, &command, PROBE_TIMEOUT)?;
    output
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("MOBIUS_ARCHIVE_BYTES:"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| {
            ApiError::new(
                "ios_app_archive_size_unavailable",
                "Unable to verify the generated device archive size",
            )
        })
}

fn prepare_runtime_directory(connection: &IosSshConnection) -> AppResult<String> {
    let root = connection.allowed_roots.first().ok_or_else(|| {
        ApiError::new(
            "invalid_allowed_roots",
            "The SSH session has no allowed root for temporary application operations",
        )
    })?;
    let directory = format!("{}/{RUNTIME_DIRECTORY_NAME}", root.trim_end_matches('/'));
    validate_runtime_path(connection, &directory)?;
    let quoted = validation::quote_remote(&directory);
    let command = format!(
        "if [ -L {quoted} ]; then exit 74; fi; mkdir -p {quoted} && chmod 700 {quoted} && cd {quoted} && [ \"$(pwd -P)\" = {quoted} ]"
    );
    ios_ssh::run_ssh_command(connection, &command, PROBE_TIMEOUT)?;
    Ok(directory)
}

fn validate_runtime_path(connection: &IosSshConnection, value: &str) -> AppResult<()> {
    validation::remote_path(value)?;
    let root = connection.allowed_roots.first().ok_or_else(|| {
        ApiError::new(
            "invalid_allowed_roots",
            "The SSH session has no allowed root for temporary application operations",
        )
    })?;
    let runtime = format!("{}/{RUNTIME_DIRECTORY_NAME}", root.trim_end_matches('/'));
    if value == runtime
        || value
            .strip_prefix(&runtime)
            .is_some_and(|suffix| suffix.starts_with('/'))
    {
        Ok(())
    } else {
        Err(ApiError::new(
            "unsafe_ios_app_runtime_path",
            "The temporary application path is outside Mobius's session runtime directory",
        ))
    }
}

fn cleanup_remote_file(
    connection: &IosSshConnection,
    runtime: &str,
    remote_path: &str,
) -> AppResult<()> {
    validate_runtime_path(connection, runtime)?;
    validate_runtime_path(connection, remote_path)?;
    let command = format!(
        "if rm -f {}; then cd /; rmdir {} 2>/dev/null || true; printf 'MOBIUS_IOS_CLEANUP_OK\\n'; else exit 75; fi",
        validation::quote_remote(remote_path),
        validation::quote_remote(runtime)
    );
    let output = ios_ssh::run_ssh_command(connection, &command, PROBE_TIMEOUT)?;
    if output
        .stdout
        .lines()
        .any(|line| line.trim() == "MOBIUS_IOS_CLEANUP_OK")
    {
        Ok(())
    } else {
        Err(ApiError::new(
            "ios_temporary_cleanup_unconfirmed",
            "The device did not confirm temporary-file cleanup",
        ))
    }
}

fn validate_export_directory(value: &str) -> AppResult<PathBuf> {
    let destination = validation::local_existing_path(value)?
        .canonicalize()
        .map_err(|error| ApiError::new("invalid_destination", error.to_string()))?;
    if !destination.is_dir() {
        return Err(ApiError::new(
            "invalid_destination",
            "Export destination must be an existing local directory",
        ));
    }
    Ok(destination)
}

fn validate_export_target(path: &Path, overwrite: bool) -> AppResult<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(ApiError::new(
            "unsafe_export_target",
            "Refusing to overwrite a local symbolic link",
        ));
    }
    if path.exists() && path.is_dir() {
        return Err(ApiError::new(
            "invalid_export_target",
            "The export target is an existing directory",
        ));
    }
    if path.exists() && !overwrite {
        return Err(ApiError::new(
            "export_file_exists",
            format!("Export target already exists: {}", path.display()),
        ));
    }
    Ok(())
}

fn safe_local_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn operation_nonce() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}-{nonce:x}", std::process::id())
}

fn non_empty_limited(value: String, limit: usize) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.chars().take(limit).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_top_level_fixed_ios_app_paths() {
        assert!(validate_ios_app_path("/Applications/MobileSafari.app").is_ok());
        assert!(
            validate_ios_app_path("/var/containers/Bundle/Application/ABC-123/Demo.app").is_ok()
        );
        assert!(validate_ios_app_path(
            "/private/var/mobile/Containers/Bundle/Application/ABC/Demo.app"
        )
        .is_ok());
        assert!(validate_ios_app_path("/var/mobile/Documents/Demo.app").is_err());
        assert!(validate_ios_app_path(
            "/var/containers/Bundle/Application/ABC/Demo.app/PlugIns/Nested.app"
        )
        .is_err());
    }

    #[test]
    fn installer_selection_is_allowlist_only_and_prefers_appinst() {
        let installers = vec![
            IosPackageInstaller {
                id: IosPackageInstallerId::Appinst,
                name: "appinst".into(),
                path: "/usr/bin/appinst".into(),
            },
            IosPackageInstaller {
                id: IosPackageInstallerId::Ipainstaller,
                name: "IPA Installer Console".into(),
                path: "/usr/bin/ipainstaller".into(),
            },
        ];
        assert_eq!(
            select_installer(&installers, None).unwrap().id,
            IosPackageInstallerId::Appinst
        );
        assert_eq!(
            select_installer(&installers, Some(IosPackageInstallerId::Ipainstaller))
                .unwrap()
                .path,
            "/usr/bin/ipainstaller"
        );
    }

    #[test]
    fn parses_base64_metadata_without_delimiter_injection() {
        let encoded = |value: &str| BASE64_STANDARD.encode(value);
        let output = format!(
            "{APP_LINE_MARKER}\t{}\t{}\t{}\t{}\t{}",
            encoded("/Applications/Demo.app/Info.plist"),
            encoded("dev.mobius.demo"),
            encoded("Demo App"),
            encoded("1.2.3"),
            encoded("42")
        );
        let apps = parse_app_metadata(&output);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].bundle_id, "dev.mobius.demo");
        assert_eq!(apps[0].display_name, "Demo App");
        assert!(apps[0].system);
    }

    #[test]
    fn generated_commands_do_not_forward_installer_options() {
        let installer = IosPackageInstaller {
            id: IosPackageInstallerId::Appinst,
            name: "appinst".into(),
            path: "/usr/bin/appinst".into(),
        };
        assert_eq!(
            build_installer_command(&installer, "/var/mobile/.mobius-runtime/.pkg-a.ipa"),
            "'/usr/bin/appinst' '/var/mobile/.mobius-runtime/.pkg-a.ipa'"
        );
    }
}
