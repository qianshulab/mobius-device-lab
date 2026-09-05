use super::blocking_api;
use crate::{
    models::{
        AndroidProxyRequest, ApiError, ApiResult, DeleteRemoteRequest, OperationResult,
        PullFileRequest, PushFileRequest, RemoteFileEntry,
    },
    runner::{run_checked, ProcessOutput},
    state::{AndroidProxySettings, AppState, ManagedProxy},
    validation,
};
use std::{
    path::Path,
    sync::{atomic::Ordering, Arc},
    thread,
    time::Duration,
};
use tauri::State;

const FILE_TIMEOUT: Duration = Duration::from_secs(120);
const SHELL_TIMEOUT: Duration = Duration::from_secs(15);
const PROXY_SETTLE_ATTEMPTS: usize = 10;
const PROXY_SETTLE_DELAY: Duration = Duration::from_millis(120);

#[tauri::command]
pub async fn list_remote_files(serial: String, path: String) -> ApiResult<Vec<RemoteFileEntry>> {
    blocking_api(move || list_remote_directory(&serial, &path)).await
}

fn list_remote_directory(serial: &str, path: &str) -> Result<Vec<RemoteFileEntry>, ApiError> {
    validation::serial(serial)?;
    validation::remote_path(path)?;
    let directory = if path == "/" {
        "/".to_string()
    } else {
        path.trim_end_matches('/').to_string()
    };
    // Android exposes /sdcard as a symbolic link on many physical devices.
    // A trailing slash makes toybox `ls` enumerate the target directory
    // instead of returning a single row describing the link itself.
    let listing_operand = directory_listing_operand(&directory);
    let command = format!(
        "LC_ALL=C ls -la -n -- {}",
        validation::quote_remote(&listing_operand)
    );
    let output = run_adb_shell(serial, &command, SHELL_TIMEOUT)?;
    Ok(parse_ls_output(&directory, &output.stdout))
}

fn directory_listing_operand(path: &str) -> String {
    if path == "/" {
        "/".into()
    } else {
        format!("{}/", path.trim_end_matches('/'))
    }
}

#[tauri::command]
pub async fn pull_file(request: PullFileRequest) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::serial(&request.serial)?;
        validation::remote_path(&request.remote_path)?;
        let destination = validation::local_absolute_path(&request.local_path)?;
        ensure_destination_parent(destination)?;
        let final_destination = if destination.is_dir() {
            let name = request
                .remote_path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    ApiError::new("invalid_remote_path", "Remote file name is missing")
                })?;
            destination.join(name)
        } else {
            destination.to_path_buf()
        };
        validate_local_pull_destination(&final_destination, request.overwrite)?;
        let args = vec![
            "-s".into(),
            request.serial,
            "pull".into(),
            request.remote_path,
            request.local_path,
        ];
        let output = run_checked("adb", &args, FILE_TIMEOUT)?;
        Ok(output.into_operation("File pulled from device"))
    })
    .await
}

#[tauri::command]
pub async fn push_file(request: PushFileRequest) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::serial(&request.serial)?;
        let source = validation::local_existing_path(&request.local_path)?
            .canonicalize()
            .map_err(|error| {
                ApiError::new(
                    "invalid_local_path",
                    format!("Unable to resolve local path: {error}"),
                )
            })?;
        validation::remote_path(&request.remote_path)?;
        let source_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ApiError::new("invalid_local_path", "Local file name is missing"))?;
        let probe = format!(
            "if [ -d {target} ]; then printf directory; elif [ -e {target} ] || [ -L {target} ]; then printf exists; else printf missing; fi",
            target = validation::quote_remote(&request.remote_path),
        );
        let requested_kind = run_adb_shell(&request.serial, &probe, SHELL_TIMEOUT)?.stdout;
        let final_target = if requested_kind == "directory" {
            format!("{}/{}", request.remote_path.trim_end_matches('/'), source_name)
        } else {
            request.remote_path.clone()
        };
        validation::remote_path(&final_target)?;
        if !request.overwrite {
            let exists = format!(
                "if [ -e {target} ] || [ -L {target} ]; then printf 1; else printf 0; fi",
                target = validation::quote_remote(&final_target),
            );
            if run_adb_shell(&request.serial, &exists, SHELL_TIMEOUT)?.stdout == "1" {
                return Err(ApiError::new(
                    "remote_file_exists",
                    "The remote file already exists; enable overwrite to replace it",
                ));
            }
        }
        let args = vec![
            "-s".into(),
            request.serial,
            "push".into(),
            source.to_string_lossy().into_owned(),
            final_target,
        ];
        let output = run_checked("adb", &args, FILE_TIMEOUT)?;
        Ok(output.into_operation("File pushed to device"))
    })
    .await
}

#[tauri::command]
pub async fn mkdir_remote(serial: String, path: String) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::serial(&serial)?;
        validation::remote_path(&path)?;
        let command = format!("mkdir -p -- {}", validation::quote_remote(&path));
        let output = run_adb_shell(&serial, &command, SHELL_TIMEOUT)?;
        Ok(output.into_operation("Remote directory created"))
    })
    .await
}

#[tauri::command]
pub async fn delete_remote(request: DeleteRemoteRequest) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::serial(&request.serial)?;
        validation::deletable_remote_path(&request.path)?;
        let flag = if request.recursive { "-rf" } else { "-f" };
        let command = format!("rm {flag} -- {}", validation::quote_remote(&request.path));
        let output = run_adb_shell(&request.serial, &command, SHELL_TIMEOUT)?;
        Ok(output.into_operation("Remote item deleted"))
    })
    .await
}

#[tauri::command]
pub async fn set_android_proxy(
    request: AndroidProxyRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    let registry = Arc::clone(&state.proxies);
    Ok(blocking_api(move || {
        validation::serial(&request.serial)?;
        validation::host(&request.host)?;
        if request.port == 0 {
            return Err(ApiError::new(
                "invalid_proxy_port",
                "Proxy port must be between 1 and 65535",
            ));
        }
        // Android's Settings.Global.HTTP_PROXY compatibility interface splits
        // the value on ':', so an IPv6 literal cannot be represented safely.
        if request.host.contains(':') {
            return Err(ApiError::new(
                "unsupported_proxy_host",
                "Android's ADB global-proxy interface requires an IPv4 address or hostname",
            ));
        }
        let proxy = format!("{}:{}", request.host, request.port);
        let mut proxies = registry
            .lock()
            .map_err(|_| ApiError::new("state_error", "Proxy registry lock was poisoned"))?;
        if state.shutting_down.load(Ordering::Acquire) {
            return Err(ApiError::new(
                "app_shutting_down",
                "Mobius is exiting and cannot retain a new proxy setting",
            ));
        }
        let current = read_android_proxy_settings(&request.serial)?;
        let previous = match proxies.get(&request.serial) {
            Some(managed) if current == managed.configured => managed.previous.clone(),
            Some(_) => {
                // Another tool changed at least one proxy field. The explicit
                // Set action may replace it only when Mobius can later restore
                // the complete prior state safely.
                ensure_proxy_restore_supported(&current)?;
                current.clone()
            }
            None => {
                ensure_proxy_restore_supported(&current)?;
                current.clone()
            }
        };

        let configured = match set_effective_static_proxy(
            &request.serial,
            &request.host,
            request.port,
        )
        .and_then(|()| {
            wait_for_proxy_endpoint(&request.serial, Some((&request.host, request.port)))
        }) {
            Ok(configured) => configured,
            Err(error) => {
                let rollback = restore_android_proxy_settings(&request.serial, &previous);
                return Err(match rollback {
                    Ok(()) => error,
                    Err(rollback_error) => ApiError::new(
                        "proxy_set_and_rollback_failed",
                        format!(
                            "Unable to apply the Android proxy and unable to restore its previous state: {}; restore error: {}",
                            error.message, rollback_error.message
                        ),
                    ),
                });
            }
        };
        proxies.insert(
            request.serial.clone(),
            ManagedProxy {
                previous,
                configured,
            },
        );
        Ok(OperationResult {
            success: true,
            message: format!("Android proxy set to {proxy}"),
            stdout: None,
            stderr: None,
            pid: None,
            exit_code: Some(0),
            timed_out: false,
        })
    })
    .await)
}

#[tauri::command]
pub async fn clear_android_proxy(
    serial: String,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let registry = Arc::clone(&state.proxies);
    Ok(blocking_api(move || {
        validation::serial(&serial)?;
        let mut proxies = registry
            .lock()
            .map_err(|_| ApiError::new("state_error", "Proxy registry lock was poisoned"))?;
        let managed = proxies.get(&serial).cloned().ok_or_else(|| {
            ApiError::new(
                "proxy_not_managed",
                "Mobius did not set a proxy for this device during the current session",
            )
        })?;
        let current = read_android_proxy_settings(&serial)?;
        if current != managed.configured {
            proxies.remove(&serial);
            return Err(ApiError::new(
                "proxy_changed_externally",
                "The device proxy changed after Mobius set it; the newer external value was left untouched",
            ));
        }
        restore_android_proxy_settings(&serial, &managed.previous)?;
        proxies.remove(&serial);
        Ok(OperationResult {
            success: true,
            message: if managed.previous.has_effective_proxy() {
                "Previous Android proxy restored".into()
            } else {
                "Android proxy cleared".into()
            },
            stdout: None,
            stderr: None,
            pid: None,
            exit_code: Some(0),
            timed_out: false,
        })
    })
    .await)
}

pub(crate) fn cleanup_managed_proxies(state: &AppState) {
    let proxies = match state.proxies.lock() {
        Ok(registry) => registry.clone(),
        Err(_) => {
            eprintln!("Mobius cleanup: proxy registry lock was poisoned");
            return;
        }
    };
    for (serial, managed) in proxies {
        let current = match read_android_proxy_settings(&serial) {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!(
                    "Mobius cleanup: could not inspect proxy on {serial}: {}",
                    error.message
                );
                continue;
            }
        };
        if current != managed.configured {
            eprintln!(
                "Mobius cleanup: proxy on {serial} changed externally and was left untouched"
            );
            if let Ok(mut registry) = state.proxies.lock() {
                registry.remove(&serial);
            }
            continue;
        }
        if let Err(error) = restore_android_proxy_settings(&serial, &managed.previous) {
            eprintln!(
                "Mobius cleanup: could not restore proxy on {serial}: {}",
                error.message
            );
        } else if let Ok(mut registry) = state.proxies.lock() {
            registry.remove(&serial);
        }
    }
}

impl AndroidProxySettings {
    fn has_effective_proxy(&self) -> bool {
        nonempty_setting(&self.http_proxy).is_some_and(|value| value != ":0")
            || nonempty_setting(&self.host).is_some()
            || nonempty_setting(&self.pac_url).is_some()
    }

    fn static_endpoint(&self) -> Option<(String, u16)> {
        if let (Some(host), Some(port)) = (
            nonempty_setting(&self.host),
            self.port
                .as_deref()
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|port| *port != 0),
        ) {
            return Some((host.to_string(), port));
        }
        let raw = nonempty_setting(&self.http_proxy)?;
        if raw == ":0" {
            return None;
        }
        let (host, port) = raw.rsplit_once(':')?;
        let host = host.trim_matches(['[', ']']);
        let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
        (!host.is_empty() && !host.contains(':')).then(|| (host.to_string(), port))
    }
}

fn nonempty_setting(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .filter(|value| !value.is_empty() && *value != "null")
}

fn ensure_proxy_restore_supported(settings: &AndroidProxySettings) -> Result<(), ApiError> {
    if nonempty_setting(&settings.pac_url).is_some()
        || nonempty_setting(&settings.exclusion_list).is_some()
    {
        return Err(ApiError::new(
            "proxy_restore_unsupported",
            "A pre-existing PAC or exclusion-list proxy is active; Mobius will not replace a proxy it cannot restore live through ADB",
        ));
    }
    if settings.has_effective_proxy() && settings.static_endpoint().is_none() {
        return Err(ApiError::new(
            "proxy_restore_unsupported",
            "The existing Android proxy format cannot be restored safely through ADB",
        ));
    }
    Ok(())
}

fn read_android_proxy_settings(serial: &str) -> Result<AndroidProxySettings, ApiError> {
    Ok(AndroidProxySettings {
        http_proxy: read_android_global_setting(serial, "http_proxy")?,
        host: read_android_global_setting(serial, "global_http_proxy_host")?,
        port: read_android_global_setting(serial, "global_http_proxy_port")?,
        exclusion_list: read_android_global_setting(serial, "global_http_proxy_exclusion_list")?,
        pac_url: read_android_global_setting(serial, "global_proxy_pac_url")?,
    })
}

fn read_android_global_setting(serial: &str, key: &str) -> Result<Option<String>, ApiError> {
    let output = run_adb_shell(serial, &format!("settings get global {key}"), SHELL_TIMEOUT)?;
    Ok((output.stdout != "null").then_some(output.stdout))
}

fn write_android_global_setting(
    serial: &str,
    key: &str,
    value: Option<&str>,
) -> Result<(), ApiError> {
    let command = match value {
        Some(value) => format!(
            "settings put global {key} {}",
            validation::quote_remote(value)
        ),
        None => format!("settings delete global {key}"),
    };
    run_adb_shell(serial, &command, SHELL_TIMEOUT)?;
    Ok(())
}

fn set_effective_static_proxy(serial: &str, host: &str, port: u16) -> Result<(), ApiError> {
    let endpoint = format!("{host}:{port}");
    write_android_global_setting(serial, "http_proxy", Some(&endpoint))
}

fn clear_effective_proxy(serial: &str) -> Result<(), ApiError> {
    // Deleting HTTP_PROXY alone does not clear ProxyTracker's canonical
    // host/port fields. The empty ':0' value is intentionally observed first,
    // which makes Android clear its in-memory proxy and send PROXY_CHANGE.
    write_android_global_setting(serial, "http_proxy", Some(":0"))?;
    wait_for_proxy_endpoint(serial, None)?;
    Ok(())
}

fn wait_for_proxy_endpoint(
    serial: &str,
    expected: Option<(&str, u16)>,
) -> Result<AndroidProxySettings, ApiError> {
    let mut last = read_android_proxy_settings(serial)?;
    let mut previous_match: Option<AndroidProxySettings> = None;
    for attempt in 0..PROXY_SETTLE_ATTEMPTS {
        let matches = match expected {
            Some((host, port)) => {
                nonempty_setting(&last.host) == Some(host)
                    && last
                        .port
                        .as_deref()
                        .and_then(|value| value.parse::<u16>().ok())
                        == Some(port)
            }
            None => !last.has_effective_proxy(),
        };
        if matches {
            if previous_match.as_ref() == Some(&last) {
                return Ok(last);
            }
            previous_match = Some(last.clone());
        } else {
            previous_match = None;
        }
        if attempt + 1 < PROXY_SETTLE_ATTEMPTS {
            thread::sleep(PROXY_SETTLE_DELAY);
            last = read_android_proxy_settings(serial)?;
        }
    }
    Err(ApiError::new(
        "proxy_state_not_applied",
        "Android did not apply the requested global-proxy state in time",
    ))
}

fn restore_android_proxy_settings(
    serial: &str,
    previous: &AndroidProxySettings,
) -> Result<(), ApiError> {
    ensure_proxy_restore_supported(previous)?;
    match previous.static_endpoint() {
        Some((host, port)) => {
            set_effective_static_proxy(serial, &host, port)?;
            wait_for_proxy_endpoint(serial, Some((&host, port)))?;
        }
        None => clear_effective_proxy(serial)?,
    }

    // Restore the exact raw representation only after Android has broadcast
    // the effective state. This preserves absent-vs-empty fields without
    // recreating the stale half-proxy that deleting HTTP_PROXY alone caused.
    write_android_global_setting(serial, "http_proxy", previous.http_proxy.as_deref())?;
    write_android_global_setting(serial, "global_http_proxy_host", previous.host.as_deref())?;
    write_android_global_setting(serial, "global_http_proxy_port", previous.port.as_deref())?;
    write_android_global_setting(
        serial,
        "global_http_proxy_exclusion_list",
        previous.exclusion_list.as_deref(),
    )?;
    write_android_global_setting(serial, "global_proxy_pac_url", previous.pac_url.as_deref())?;
    Ok(())
}

pub(crate) fn run_adb_shell(
    serial: &str,
    command: &str,
    timeout: Duration,
) -> Result<ProcessOutput, ApiError> {
    validation::serial(serial)?;
    // Internal callers construct a fixed command and quote every variable with quote_remote.
    run_checked(
        "adb",
        &[
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            command.to_string(),
        ],
        timeout,
    )
}

fn ensure_destination_parent(path: &Path) -> Result<(), ApiError> {
    let parent = if path.is_dir() {
        path
    } else {
        path.parent().ok_or_else(|| {
            ApiError::new("invalid_local_path", "Destination has no parent directory")
        })?
    };
    if !parent.exists() || !parent.is_dir() {
        return Err(ApiError::new(
            "local_directory_not_found",
            format!("Destination directory does not exist: {}", parent.display()),
        ));
    }
    Ok(())
}

fn validate_local_pull_destination(path: &Path, overwrite: bool) -> Result<(), ApiError> {
    ensure_destination_parent(path)?;
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(ApiError::new(
            "unsafe_local_link",
            "Refusing to overwrite a local symbolic link",
        ));
    }
    if path.exists() && path.is_dir() {
        return Err(ApiError::new(
            "invalid_local_destination",
            "The resolved local destination is a directory",
        ));
    }
    if path.exists() && !overwrite {
        return Err(ApiError::new(
            "local_file_exists",
            "The local destination already exists; enable overwrite to replace it",
        ));
    }
    Ok(())
}

fn parse_ls_output(parent: &str, output: &str) -> Vec<RemoteFileEntry> {
    output
        .lines()
        .filter_map(|line| parse_ls_line(parent, line))
        .collect()
}

fn parse_ls_line(parent: &str, line: &str) -> Option<RemoteFileEntry> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 7 || fields.first().map_or(true, |value| value.len() < 10) {
        return None;
    }
    let permissions = fields[0];
    let name_index = if fields.get(5) == Some(&"?") {
        6
    } else if fields
        .get(5)
        .is_some_and(|value| value.len() == 10 && value.as_bytes().get(4) == Some(&b'-'))
    {
        7
    } else {
        8
    };
    if fields.len() <= name_index {
        return None;
    }
    let display_name = fields[name_index..].join(" ");
    let (name, link_target) = if permissions.starts_with('l') {
        match display_name.split_once(" -> ") {
            Some((name, target)) => (name.to_string(), Some(target.to_string())),
            None => (display_name, None),
        }
    } else {
        (display_name, None)
    };
    if matches!(name.as_str(), "." | "..") {
        return None;
    }
    let path = if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    };
    let kind = match permissions.as_bytes().first().copied() {
        Some(b'd') => "directory",
        Some(b'l') => "link",
        Some(b'-') => "file",
        _ => "unknown",
    };
    let modified_end = name_index.min(fields.len());
    let modified = (modified_end > 5).then(|| fields[5..modified_end].join(" "));
    Some(RemoteFileEntry {
        name,
        path,
        kind: kind.into(),
        permissions: Some(permissions.to_string()),
        owner: fields.get(2).map(|value| (*value).to_string()),
        group: fields.get(3).map(|value| (*value).to_string()),
        size: fields.get(4).and_then(|value| value.parse().ok()),
        modified,
        link_target,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        directory_listing_operand, ensure_proxy_restore_supported, list_remote_directory,
        parse_ls_line, validate_local_pull_destination,
    };
    use crate::state::AndroidProxySettings;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn parses_toybox_listing_with_spaces() {
        let entry = parse_ls_line(
            "/sdcard",
            "-rw-r--r-- 1 1023 1023 42 2026-09-04 12:30 hello world.txt",
        )
        .expect("entry");
        assert_eq!(entry.name, "hello world.txt");
        assert_eq!(entry.path, "/sdcard/hello world.txt");
        assert_eq!(entry.size, Some(42));
    }

    #[test]
    fn follows_android_directory_links_for_listing() {
        assert_eq!(directory_listing_operand("/sdcard"), "/sdcard/");
        assert_eq!(directory_listing_operand("/sdcard/"), "/sdcard/");
        assert_eq!(directory_listing_operand("/"), "/");
    }

    #[test]
    fn parses_inaccessible_and_symbolic_link_rows() {
        let inaccessible = parse_ls_line("/", "d????????? ? ? ? ? ? data_mirror")
            .expect("inaccessible directory row");
        assert_eq!(inaccessible.name, "data_mirror");
        assert_eq!(inaccessible.path, "/data_mirror");
        assert_eq!(inaccessible.kind, "directory");

        let link = parse_ls_line(
            "/",
            "lrw-r--r-- 1 0 0 21 2026-05-18 11:15 sdcard -> /storage/self/primary",
        )
        .expect("symbolic link row");
        assert_eq!(link.name, "sdcard");
        assert_eq!(link.kind, "link");
        assert_eq!(link.link_target.as_deref(), Some("/storage/self/primary"));
    }

    #[test]
    #[ignore = "requires an explicitly authorized live Android device"]
    fn live_android_file_browser_follows_sdcard_and_opens_directories() {
        let serial = std::env::var("MOBIUS_LIVE_ANDROID_SERIAL")
            .expect("set MOBIUS_LIVE_ANDROID_SERIAL to the authorized device serial");
        let root = list_remote_directory(&serial, "/").expect("list Android root");
        let sdcard_link = root
            .iter()
            .find(|entry| entry.name == "sdcard")
            .expect("root sdcard entry");
        assert_eq!(sdcard_link.kind, "link");

        let sdcard = list_remote_directory(&serial, "/sdcard").expect("list /sdcard target");
        let download = sdcard
            .iter()
            .find(|entry| entry.name == "Download" && entry.kind == "directory")
            .expect("/sdcard/Download directory");
        assert_eq!(download.path, "/sdcard/Download");
        let _download_entries =
            list_remote_directory(&serial, &download.path).expect("open /sdcard/Download");
    }

    #[test]
    fn local_pull_requires_explicit_overwrite_for_existing_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("mobius-pull-test-{unique}"));
        fs::create_dir(&directory).expect("create test directory");
        let target = directory.join("capture.png");
        fs::write(&target, b"existing").expect("create existing target");

        let rejected = validate_local_pull_destination(&target, false)
            .expect_err("existing file should require overwrite");
        assert_eq!(rejected.code, "local_file_exists");
        assert!(validate_local_pull_destination(&target, true).is_ok());

        fs::remove_file(&target).expect("remove test target");
        fs::remove_dir(&directory).expect("remove test directory");
    }

    #[test]
    fn recognizes_complete_android_proxy_states() {
        let empty = AndroidProxySettings::default();
        assert!(!empty.has_effective_proxy());
        assert!(empty.static_endpoint().is_none());
        assert!(ensure_proxy_restore_supported(&empty).is_ok());

        let static_proxy = AndroidProxySettings {
            http_proxy: None,
            host: Some("192.168.1.20".into()),
            port: Some("8080".into()),
            exclusion_list: Some(String::new()),
            pac_url: Some(String::new()),
        };
        assert!(static_proxy.has_effective_proxy());
        assert_eq!(
            static_proxy.static_endpoint(),
            Some(("192.168.1.20".into(), 8080))
        );
        assert!(ensure_proxy_restore_supported(&static_proxy).is_ok());
    }

    #[test]
    fn refuses_to_replace_proxy_forms_that_adb_cannot_restore_live() {
        let pac = AndroidProxySettings {
            pac_url: Some("https://proxy.example/proxy.pac".into()),
            ..AndroidProxySettings::default()
        };
        assert_eq!(
            ensure_proxy_restore_supported(&pac)
                .expect_err("PAC must be preserved")
                .code,
            "proxy_restore_unsupported"
        );

        let exclusions = AndroidProxySettings {
            host: Some("192.168.1.20".into()),
            port: Some("8080".into()),
            exclusion_list: Some("localhost,*.example".into()),
            ..AndroidProxySettings::default()
        };
        assert_eq!(
            ensure_proxy_restore_supported(&exclusions)
                .expect_err("exclusion list must be preserved")
                .code,
            "proxy_restore_unsupported"
        );
    }
}
