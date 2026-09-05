use crate::models::{ApiError, AppResult, ConfigureToolchainRequest, ToolchainConfiguration};
use std::{
    cmp::Reverse,
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};

const CONFIG_FILE_NAME: &str = "toolchain.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolSource {
    Configured,
    Bundled,
    Sdk,
    Path,
}

impl ToolSource {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Bundled => "bundled",
            Self::Sdk => "sdk",
            Self::Path => "path",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedTool {
    pub path: PathBuf,
    pub source: ToolSource,
}

#[derive(Debug, Clone, Default)]
struct ResolverState {
    configuration: ToolchainConfiguration,
    bundled_roots: Vec<PathBuf>,
    config_file: Option<PathBuf>,
}

static RESOLVER_STATE: OnceLock<RwLock<ResolverState>> = OnceLock::new();

fn resolver_state() -> &'static RwLock<ResolverState> {
    RESOLVER_STATE.get_or_init(|| RwLock::new(ResolverState::default()))
}

pub(crate) fn initialize(resource_dir: Option<PathBuf>, app_config_dir: Option<PathBuf>) {
    let mut bundled_roots = Vec::new();
    if let Some(resource_dir) = resource_dir.filter(|path| path.is_absolute()) {
        bundled_roots.push(resource_dir.join("tools"));
        bundled_roots.push(resource_dir.join("resources").join("tools"));
    }
    #[cfg(debug_assertions)]
    bundled_roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/tools"));
    deduplicate_paths(&mut bundled_roots);

    let config_file = app_config_dir
        .filter(|path| path.is_absolute())
        .map(|path| path.join(CONFIG_FILE_NAME));
    let configuration = config_file
        .as_deref()
        .and_then(load_configuration)
        .unwrap_or_default();

    match resolver_state().write() {
        Ok(mut state) => {
            state.configuration = configuration;
            state.bundled_roots = bundled_roots;
            state.config_file = config_file;
        }
        Err(_) => eprintln!("Mobius toolchain: resolver lock was poisoned during initialization"),
    }
}

pub(crate) fn configure(request: ConfigureToolchainRequest) -> AppResult<ToolchainConfiguration> {
    let configuration = if request.clear.unwrap_or(false) {
        ToolchainConfiguration::default()
    } else {
        normalize_configuration(ToolchainConfiguration {
            adb_path: request.adb_path,
            scrcpy_path: request.scrcpy_path,
            frida_path: request.frida_path,
            ios_tools_path: request.ios_tools_path,
            managed_tools_path: request.managed_tools_path,
        })?
    };

    let mut state = resolver_state().write().map_err(|_| {
        ApiError::new(
            "toolchain_state_error",
            "Toolchain configuration is temporarily unavailable",
        )
    })?;
    let config_file = state.config_file.as_deref().ok_or_else(|| {
        ApiError::new(
            "toolchain_config_unavailable",
            "The application configuration directory is unavailable",
        )
    })?;
    persist_configuration(config_file, &configuration)?;
    state.configuration = configuration.clone();
    Ok(configuration)
}

pub(crate) fn resolve_tool(program: &str) -> AppResult<ResolvedTool> {
    validate_tool_name(program)?;
    let state = resolver_state().read().map_err(|_| {
        ApiError::new(
            "toolchain_state_error",
            "Toolchain resolver is temporarily unavailable",
        )
    })?;
    let sdk_roots = android_sdk_roots();
    let path_directories = system_path_directories();
    resolve_tool_from_sources(
        program,
        &state.configuration,
        &state.bundled_roots,
        &sdk_roots,
        &path_directories,
    )
}

fn resolve_tool_from_sources(
    program: &str,
    configuration: &ToolchainConfiguration,
    bundled_roots: &[PathBuf],
    sdk_roots: &[PathBuf],
    path_directories: &[PathBuf],
) -> AppResult<ResolvedTool> {
    validate_tool_name(program)?;

    if let Some(path) = configured_file(configuration, program) {
        return executable_at(Path::new(path), program)
            .map(|path| ResolvedTool {
                path,
                source: ToolSource::Configured,
            })
            .map_err(|error| {
                ApiError::new(
                    "configured_tool_unavailable",
                    format!(
                        "The configured {program} executable is unavailable: {}",
                        error.message
                    ),
                )
            });
    }

    let mut configured_directories = Vec::new();
    if let Some(path) = configuration.managed_tools_path.as_deref() {
        configured_directories.push(PathBuf::from(path));
    }
    if is_ios_tool(program) {
        if let Some(path) = configuration.ios_tools_path.as_deref() {
            configured_directories.push(PathBuf::from(path));
        }
    }
    if let Some(path) = find_in_directories(&configured_directories, program) {
        return Ok(ResolvedTool {
            path,
            source: ToolSource::Configured,
        });
    }

    let bundled_directories = bundled_search_directories(bundled_roots);
    if let Some(path) = find_in_directories(&bundled_directories, program) {
        return Ok(ResolvedTool {
            path,
            source: ToolSource::Bundled,
        });
    }

    let sdk_directories = android_sdk_directories(program, sdk_roots);
    if let Some(path) = find_in_directories(&sdk_directories, program) {
        return Ok(ResolvedTool {
            path,
            source: ToolSource::Sdk,
        });
    }

    if let Some(path) = find_in_directories(path_directories, program) {
        return Ok(ResolvedTool {
            path,
            source: ToolSource::Path,
        });
    }

    Err(ApiError::new(
        "tool_not_found",
        format!(
            "Unable to find {program} in configured, bundled, Android SDK, or system PATH locations"
        ),
    ))
}

fn configured_file<'a>(
    configuration: &'a ToolchainConfiguration,
    program: &str,
) -> Option<&'a str> {
    match program {
        "adb" => configuration.adb_path.as_deref(),
        "scrcpy" => configuration.scrcpy_path.as_deref(),
        "frida" => configuration.frida_path.as_deref(),
        _ => None,
    }
}

fn bundled_search_directories(roots: &[PathBuf]) -> Vec<PathBuf> {
    let target = bundled_target_directory();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    roots
        .iter()
        .flat_map(|root| {
            [
                root.join(&target),
                root.join(os).join(arch),
                root.join("common"),
                root.clone(),
            ]
        })
        .collect()
}

fn bundled_target_directory() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn android_sdk_roots() -> Vec<PathBuf> {
    let mut roots = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();

    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Library/Android/sdk"));
    }
    #[cfg(target_os = "linux")]
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Android/Sdk"));
    }
    #[cfg(windows)]
    if let Some(local_data) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local_data).join("Android/Sdk"));
    }
    roots.retain(|path| path.is_absolute());
    deduplicate_paths(&mut roots);
    roots
}

fn android_sdk_directories(program: &str, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for root in roots {
        match program {
            "adb" => directories.push(root.join("platform-tools")),
            "aapt2" => {
                directories.extend(versioned_subdirectories(&root.join("build-tools")));
            }
            "apkanalyzer" => {
                let command_line_tools = root.join("cmdline-tools");
                directories.push(command_line_tools.join("latest/bin"));
                directories.extend(
                    versioned_subdirectories(&command_line_tools)
                        .into_iter()
                        .map(|path| path.join("bin")),
                );
                directories.push(root.join("tools/bin"));
            }
            _ => {}
        }
    }
    directories
}

fn versioned_subdirectories(root: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.file_name().is_some_and(|name| name != "latest"))
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| Reverse(version_path_key(path)));
    paths
}

fn version_path_key(path: &Path) -> Vec<u64> {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn system_path_directories() -> Vec<PathBuf> {
    let mut directories = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .filter(|directory| directory.is_absolute())
        .collect::<Vec<_>>();
    #[cfg(target_os = "macos")]
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ]);
    #[cfg(target_os = "linux")]
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/snap/bin"),
    ]);
    #[cfg(windows)]
    if let Some(windows_dir) = std::env::var_os("WINDIR") {
        directories.push(PathBuf::from(windows_dir).join("System32/OpenSSH"));
    }
    deduplicate_paths(&mut directories);
    directories
}

fn find_in_directories(directories: &[PathBuf], program: &str) -> Option<PathBuf> {
    directories
        .iter()
        .flat_map(|directory| tool_candidates(directory, program))
        .find_map(|candidate| executable_at(&candidate, program).ok())
}

fn executable_at(path: &Path, program: &str) -> AppResult<PathBuf> {
    if !path.is_absolute() {
        return Err(ApiError::new(
            "unsafe_tool_path",
            format!("The {program} path must be absolute"),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        ApiError::new(
            "tool_path_error",
            format!("Unable to resolve {program}: {error}"),
        )
    })?;
    let metadata = canonical.metadata().map_err(|error| {
        ApiError::new(
            "tool_path_error",
            format!("Unable to inspect {program}: {error}"),
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(ApiError::new(
            "tool_path_error",
            format!("The resolved {program} path is not a regular file"),
        ));
    }
    reject_unsafe_windows_path(&canonical, program)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(ApiError::new(
                "tool_not_executable",
                format!("Resolved {program} file is not executable"),
            ));
        }
    }
    Ok(canonical)
}

fn normalize_configuration(
    configuration: ToolchainConfiguration,
) -> AppResult<ToolchainConfiguration> {
    Ok(ToolchainConfiguration {
        adb_path: normalize_file(configuration.adb_path, "adbPath")?,
        scrcpy_path: normalize_file(configuration.scrcpy_path, "scrcpyPath")?,
        frida_path: normalize_file(configuration.frida_path, "fridaPath")?,
        ios_tools_path: normalize_directory(configuration.ios_tools_path, "iosToolsPath")?,
        managed_tools_path: normalize_directory(
            configuration.managed_tools_path,
            "managedToolsPath",
        )?,
    })
}

fn normalize_file(value: Option<String>, field: &str) -> AppResult<Option<String>> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };
    let path = PathBuf::from(&value);
    let canonical = executable_at(&path, field)?;
    canonical
        .into_os_string()
        .into_string()
        .map(Some)
        .map_err(|_| {
            ApiError::new(
                "invalid_tool_path",
                format!("{field} must be representable as UTF-8"),
            )
        })
}

fn normalize_directory(value: Option<String>, field: &str) -> AppResult<Option<String>> {
    let Some(value) = non_empty(value) else {
        return Ok(None);
    };
    let path = PathBuf::from(&value);
    if !path.is_absolute() {
        return Err(ApiError::new(
            "invalid_tool_directory",
            format!("{field} must be an absolute directory"),
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        ApiError::new(
            "invalid_tool_directory",
            format!("Unable to resolve {field}: {error}"),
        )
    })?;
    if !canonical
        .metadata()
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
    {
        return Err(ApiError::new(
            "invalid_tool_directory",
            format!("{field} is not a directory"),
        ));
    }
    reject_unsafe_windows_path(&canonical, field)?;
    canonical
        .into_os_string()
        .into_string()
        .map(Some)
        .map_err(|_| {
            ApiError::new(
                "invalid_tool_directory",
                format!("{field} must be representable as UTF-8"),
            )
        })
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn persist_configuration(path: &Path, configuration: &ToolchainConfiguration) -> AppResult<()> {
    let parent = path.parent().ok_or_else(|| {
        ApiError::new(
            "toolchain_config_error",
            "Toolchain configuration path has no parent directory",
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        ApiError::new(
            "toolchain_config_error",
            format!("Unable to create the configuration directory: {error}"),
        )
    })?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(configuration).map_err(|error| {
        ApiError::new(
            "toolchain_config_error",
            format!("Unable to serialize toolchain configuration: {error}"),
        )
    })?;
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        #[cfg(windows)]
        if path.exists() {
            fs::remove_file(path)?;
        }
        fs::rename(&temporary, path)
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(ApiError::new(
            "toolchain_config_error",
            format!("Unable to save toolchain configuration: {error}"),
        ));
    }
    Ok(())
}

fn load_configuration(path: &Path) -> Option<ToolchainConfiguration> {
    if !path.is_file() {
        return None;
    }
    let configuration = match fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ToolchainConfiguration>(&bytes).ok())
    {
        Some(configuration) => configuration,
        None => {
            eprintln!("Mobius toolchain: ignoring an unreadable configuration file");
            return None;
        }
    };
    match normalize_configuration(configuration) {
        Ok(configuration) => Some(configuration),
        Err(error) => {
            eprintln!(
                "Mobius toolchain: ignoring an invalid persisted configuration: {}",
                error.message
            );
            None
        }
    }
}

fn validate_tool_name(program: &str) -> AppResult<()> {
    if program.is_empty()
        || !program
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(ApiError::new(
            "invalid_tool_name",
            "External tool name contains unsupported characters",
        ));
    }
    Ok(())
}

fn is_ios_tool(program: &str) -> bool {
    matches!(
        program,
        "idevice_id"
            | "ideviceinfo"
            | "idevicepair"
            | "ideviceinstaller"
            | "idevicescreenshot"
            | "idevicesyslog"
            | "iproxy"
            | "ssh"
            | "scp"
    )
}

fn tool_candidates(directory: &Path, program: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![
            directory.join(format!("{program}.exe")),
            directory.join(format!("{program}.com")),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![directory.join(program)]
    }
}

fn deduplicate_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

#[cfg(windows)]
fn reject_unsafe_windows_path(path: &Path, label: &str) -> AppResult<()> {
    if path.to_string_lossy().starts_with(r"\\") {
        return Err(ApiError::new(
            "unsafe_tool_path",
            format!("Refusing to use {label} from a network share"),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn reject_unsafe_windows_path(_path: &Path, _label: &str) -> AppResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock must be after epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mobius-toolchain-test-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("temporary test directory must be created");
            Self(path)
        }

        fn directory(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("test directory must be created");
            path
        }

        fn executable(&self, relative_dir: &str, program: &str) -> PathBuf {
            let directory = self.directory(relative_dir);
            let path = tool_candidates(&directory, program)
                .into_iter()
                .next()
                .expect("platform must have a tool candidate");
            fs::write(&path, b"test executable").expect("test executable must be created");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = fs::metadata(&path)
                    .expect("test executable metadata must exist")
                    .permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&path, permissions)
                    .expect("test executable must be executable");
            }
            path.canonicalize().expect("test path must canonicalize")
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn exact_configured_file_wins() {
        let tree = TempTree::new();
        let configured = tree.executable("configured", "custom-adb");
        let _path_copy = tree.executable("path", "adb");
        let configuration = ToolchainConfiguration {
            adb_path: Some(configured.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let resolved =
            resolve_tool_from_sources("adb", &configuration, &[], &[], &[tree.0.join("path")])
                .expect("configured adb should resolve");
        assert_eq!(resolved.path, configured);
        assert_eq!(resolved.source, ToolSource::Configured);
    }

    #[test]
    fn configured_directory_precedes_bundled_sdk_and_path() {
        let tree = TempTree::new();
        let configured = tree.executable("managed", "adb");
        let target = bundled_target_directory();
        let _bundled = tree.executable(&format!("bundled/{target}"), "adb");
        let _sdk = tree.executable("sdk/platform-tools", "adb");
        let _path = tree.executable("path", "adb");
        let configuration = ToolchainConfiguration {
            managed_tools_path: Some(tree.0.join("managed").to_string_lossy().into_owned()),
            ..Default::default()
        };
        let resolved = resolve_tool_from_sources(
            "adb",
            &configuration,
            &[tree.0.join("bundled")],
            &[tree.0.join("sdk")],
            &[tree.0.join("path")],
        )
        .expect("managed adb should resolve");
        assert_eq!(resolved.path, configured);
        assert_eq!(resolved.source, ToolSource::Configured);
    }

    #[test]
    fn bundled_precedes_android_sdk_and_path() {
        let tree = TempTree::new();
        let target = bundled_target_directory();
        let bundled = tree.executable(&format!("bundled/{target}"), "adb");
        let _sdk = tree.executable("sdk/platform-tools", "adb");
        let _path = tree.executable("path", "adb");
        let resolved = resolve_tool_from_sources(
            "adb",
            &ToolchainConfiguration::default(),
            &[tree.0.join("bundled")],
            &[tree.0.join("sdk")],
            &[tree.0.join("path")],
        )
        .expect("bundled adb should resolve");
        assert_eq!(resolved.path, bundled);
        assert_eq!(resolved.source, ToolSource::Bundled);
    }

    #[test]
    fn android_sdk_precedes_path() {
        let tree = TempTree::new();
        let sdk = tree.executable("sdk/platform-tools", "adb");
        let _path = tree.executable("path", "adb");
        let resolved = resolve_tool_from_sources(
            "adb",
            &ToolchainConfiguration::default(),
            &[],
            &[tree.0.join("sdk")],
            &[tree.0.join("path")],
        )
        .expect("SDK adb should resolve");
        assert_eq!(resolved.path, sdk);
        assert_eq!(resolved.source, ToolSource::Sdk);
    }

    #[test]
    fn path_is_the_final_fallback() {
        let tree = TempTree::new();
        let path_tool = tree.executable("path", "scrcpy");
        let resolved = resolve_tool_from_sources(
            "scrcpy",
            &ToolchainConfiguration::default(),
            &[],
            &[],
            &[tree.0.join("path")],
        )
        .expect("PATH scrcpy should resolve");
        assert_eq!(resolved.path, path_tool);
        assert_eq!(resolved.source, ToolSource::Path);
    }

    #[test]
    fn stale_exact_configuration_does_not_silently_fall_back() {
        let tree = TempTree::new();
        let _path_copy = tree.executable("path", "adb");
        let configuration = ToolchainConfiguration {
            adb_path: Some(
                tree.0
                    .join("configured/missing-adb")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        };
        let error =
            resolve_tool_from_sources("adb", &configuration, &[], &[], &[tree.0.join("path")])
                .expect_err("a stale exact selection must fail closed");
        assert_eq!(error.code, "configured_tool_unavailable");
    }

    #[test]
    fn relative_configuration_is_rejected() {
        let error = normalize_configuration(ToolchainConfiguration {
            adb_path: Some("relative/adb".into()),
            ..Default::default()
        })
        .expect_err("relative executable path must be rejected");
        assert_eq!(error.code, "unsafe_tool_path");

        let error = normalize_configuration(ToolchainConfiguration {
            managed_tools_path: Some("relative/tools".into()),
            ..Default::default()
        })
        .expect_err("relative tools directory must be rejected");
        assert_eq!(error.code, "invalid_tool_directory");
    }

    #[test]
    fn unsafe_tool_name_is_rejected() {
        let error =
            resolve_tool_from_sources("../adb", &ToolchainConfiguration::default(), &[], &[], &[])
                .expect_err("unsafe tool name must be rejected");
        assert_eq!(error.code, "invalid_tool_name");
    }
}
