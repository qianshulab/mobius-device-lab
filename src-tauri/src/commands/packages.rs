use super::{blocking_api, files::run_adb_shell};
use crate::{
    models::{
        AnalyzeMobilePackageRequest, AndroidPackageExport, ApiError, ApiResult,
        ExportAndroidPackageRequest, ExportedAndroidFile, InstallMobilePackageRequest,
        InstalledApp, MobilePackageAnalysis, MobilePlatform, OperationResult, PackageIcon,
    },
    runner::{resolve_tool, run_checked, run_process},
    validation,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use md5::{Digest, Md5};
use plist::Value as PlistValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::Duration,
};
use zip::ZipArchive;

const ANALYZER_TIMEOUT: Duration = Duration::from_secs(25);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const DEVICE_TIMEOUT: Duration = Duration::from_secs(30);
const PULL_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_PACKAGE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 50_000;
const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PLIST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ICON_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
struct PackageMetadata {
    package_name: Option<String>,
    display_name: Option<String>,
    version_name: Option<String>,
    version_code: Option<String>,
    minimum_os_version: Option<String>,
    target_sdk_version: Option<String>,
    permissions: BTreeSet<String>,
    icon_hints: Vec<String>,
}

#[tauri::command]
pub async fn analyze_mobile_package(
    request: AnalyzeMobilePackageRequest,
) -> ApiResult<MobilePackageAnalysis> {
    blocking_api(move || analyze_package(&request.path)).await
}

#[tauri::command]
pub async fn install_mobile_package(
    request: InstallMobilePackageRequest,
) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::serial(&request.serial)?;
        let source = validate_package_file(&request.path, request.platform)?;
        let path = source.to_string_lossy().into_owned();
        let output = match request.platform {
            MobilePlatform::Android => {
                let mut args = vec!["-s".into(), request.serial, "install".into()];
                if request.replace {
                    args.push("-r".into());
                }
                if request.grant_permissions {
                    args.push("-g".into());
                }
                if request.downgrade {
                    args.push("-d".into());
                }
                if request.allow_test_packages {
                    args.push("-t".into());
                }
                args.push(path);
                run_checked("adb", &args, INSTALL_TIMEOUT)?
            }
            MobilePlatform::Ios => {
                require_connected_ios_udid(&request.serial)?;
                run_checked(
                    "ideviceinstaller",
                    &["-u".into(), request.serial, "install".into(), path],
                    INSTALL_TIMEOUT,
                )?
            }
        };
        let message = match request.platform {
            MobilePlatform::Android => "Android package installed",
            MobilePlatform::Ios => {
                "iOS package submitted to ideviceinstaller (normal signing and trust checks remain active)"
            }
        };
        Ok(output.into_operation(message))
    })
    .await
}

fn require_connected_ios_udid(serial: &str) -> Result<(), ApiError> {
    if serial.starts_with("ios-ssh:") || serial.contains(':') {
        return Err(ApiError::new(
            "ios_usb_device_required",
            "IPA installation through ideviceinstaller requires a currently connected USB/usbmux iOS device, not a registered LAN SSH endpoint",
        ));
    }
    let output = run_checked("idevice_id", &["-l".into()], DEVICE_TIMEOUT)?;
    if output
        .stdout
        .lines()
        .map(str::trim)
        .any(|udid| udid == serial)
    {
        Ok(())
    } else {
        Err(ApiError::new(
            "ios_usb_device_not_found",
            "The selected iOS UDID is not present in the current usbmux device list",
        ))
    }
}

#[tauri::command]
pub async fn list_installed_apps(serial: String) -> ApiResult<Vec<InstalledApp>> {
    blocking_api(move || {
        validation::serial(&serial)?;
        let output = run_adb_shell(
            &serial,
            "pm list packages -f -U --show-versioncode",
            DEVICE_TIMEOUT,
        )
        .or_else(|_| run_adb_shell(&serial, "pm list packages -f -U", DEVICE_TIMEOUT))?;
        let mut apps = output
            .stdout
            .lines()
            .filter_map(parse_installed_app)
            .collect::<Vec<_>>();
        // Updated system applications can have their current APK below /data/app.
        // Ask Package Manager for its authoritative system flag so the UI can
        // disable destructive management actions before a confirmation is shown.
        let system_packages = run_checked(
            "adb",
            &[
                "-s".into(),
                serial.clone(),
                "shell".into(),
                "pm".into(),
                "list".into(),
                "packages".into(),
                "-s".into(),
            ],
            DEVICE_TIMEOUT,
        )
        .map(|system_output| {
            system_output
                .stdout
                .lines()
                .filter_map(parse_package_only_row)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
        for app in &mut apps {
            app.system |= system_packages.contains(&app.package_name);
        }
        apps.sort_by(|left, right| left.package_name.cmp(&right.package_name));
        Ok(apps)
    })
    .await
}

#[tauri::command]
pub async fn export_android_package(
    request: ExportAndroidPackageRequest,
) -> ApiResult<AndroidPackageExport> {
    blocking_api(move || export_android(request)).await
}

pub(crate) fn analyze_package(value: &str) -> Result<MobilePackageAnalysis, ApiError> {
    let path = validation::local_existing_path(value)?
        .canonicalize()
        .map_err(|error| ApiError::new("invalid_package_path", error.to_string()))?;
    let metadata = path
        .metadata()
        .map_err(|error| ApiError::new("invalid_package_path", error.to_string()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_PACKAGE_BYTES {
        return Err(ApiError::new(
            "invalid_mobile_package",
            "Package must be a non-empty regular APK or IPA no larger than 4 GiB",
        ));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let platform = match extension.as_str() {
        "apk" => MobilePlatform::Android,
        "ipa" => MobilePlatform::Ios,
        _ => {
            return Err(ApiError::new(
                "unsupported_package_type",
                "Only .apk and .ipa files are supported",
            ))
        }
    };
    let md5 = stream_md5(&path)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("package")
        .to_string();
    match platform {
        MobilePlatform::Android => analyze_apk(path, file_name, metadata.len(), md5),
        MobilePlatform::Ios => analyze_ipa(path, file_name, metadata.len(), md5),
    }
}

fn analyze_apk(
    path: PathBuf,
    file_name: String,
    file_size: u64,
    md5: String,
) -> Result<MobilePackageAnalysis, ApiError> {
    ensure_zip(&path)?;
    let mut warnings = Vec::new();
    let mut metadata = PackageMetadata::default();
    let path_arg = path.to_string_lossy().into_owned();
    let mut source = "zipFallback".to_string();
    let mut parsed_external = false;

    match resolve_tool("aapt2") {
        Ok(_) => match run_process(
            "aapt2",
            &["dump".into(), "badging".into(), path_arg.clone()],
            ANALYZER_TIMEOUT,
            &[],
        ) {
            Ok(output) if output.exit_code == Some(0) && !output.timed_out => {
                parse_aapt_badging(&output.stdout, &mut metadata);
                if metadata.package_name.is_some()
                    || metadata.version_name.is_some()
                    || !metadata.permissions.is_empty()
                {
                    source = "aapt2".into();
                    parsed_external = true;
                } else {
                    warnings.push("aapt2 returned no usable package metadata".into());
                }
                if output.truncated {
                    warnings.push("aapt2 output was truncated at the Mobius capture limit".into());
                }
            }
            Ok(output) => warnings.push(format!(
                "aapt2 could not parse this APK (status {:?}, timedOut={})",
                output.exit_code, output.timed_out
            )),
            Err(error) => warnings.push(format!("aapt2 failed: {}", error.message)),
        },
        Err(_) => warnings.push("aapt2 was not found; trying apkanalyzer".into()),
    }

    if metadata.package_name.is_none() || metadata.display_name.is_none() {
        match collect_apkanalyzer(&path_arg, &mut metadata) {
            Ok(any) if any => {
                if !parsed_external {
                    source = "apkanalyzer".into();
                } else {
                    source = "aapt2+apkanalyzer".into();
                }
                parsed_external = true;
            }
            Ok(_) => warnings.push("apkanalyzer returned no usable manifest metadata".into()),
            Err(error) => warnings.push(error),
        }
    }

    if !parsed_external {
        warnings.push(
            "Manifest metadata is unavailable because AndroidManifest.xml is binary and no compatible Android analyzer succeeded"
                .into(),
        );
    }
    if metadata.package_name.is_none() {
        warnings.push("Android package identifier could not be resolved".into());
    }
    if metadata.display_name.is_none() {
        warnings.push(
            "Application label could not be resolved (it may only exist as a compiled resource)"
                .into(),
        );
    }
    let architectures = apk_architectures(&path)?;
    let icon = extract_best_icon(
        &path,
        &metadata.icon_hints,
        MobilePlatform::Android,
        None,
        &mut warnings,
    )?;
    let fallback_used = source != "aapt2";
    Ok(MobilePackageAnalysis {
        platform: MobilePlatform::Android,
        path: path.to_string_lossy().into_owned(),
        file_name,
        file_size,
        md5,
        architectures,
        source,
        fallback_used,
        package_name: metadata.package_name,
        display_name: metadata.display_name,
        version_name: metadata.version_name,
        version_code: metadata.version_code,
        minimum_os_version: metadata.minimum_os_version,
        target_sdk_version: metadata.target_sdk_version,
        permissions: metadata.permissions.into_iter().collect(),
        usage_descriptions: BTreeMap::new(),
        icon,
        warnings,
    })
}

fn analyze_ipa(
    path: PathBuf,
    file_name: String,
    file_size: u64,
    md5: String,
) -> Result<MobilePackageAnalysis, ApiError> {
    let mut archive = open_safe_zip(&path, "invalid_ipa")?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ApiError::new(
            "archive_too_large",
            "IPA contains too many ZIP entries",
        ));
    }
    let mut plist_indices = Vec::new();
    for index in 0..archive.len() {
        if archive
            .by_index(index)
            .ok()
            .is_some_and(|entry| is_root_ipa_info_plist(&entry))
        {
            plist_indices.push(index);
        }
    }
    let plist_index = match plist_indices.as_slice() {
        [index] => *index,
        [] => {
            return Err(ApiError::new(
                "ipa_info_plist_missing",
                "IPA has no canonical Payload/<App>.app/Info.plist",
            ))
        }
        _ => {
            return Err(ApiError::new(
                "ipa_info_plist_ambiguous",
                "IPA contains multiple root application Info.plist entries",
            ))
        }
    };
    let (plist_path, plist_bytes) = {
        let mut entry = archive
            .by_index(plist_index)
            .map_err(|error| ApiError::new("invalid_ipa", error.to_string()))?;
        if entry.size() > MAX_PLIST_BYTES {
            return Err(ApiError::new(
                "ipa_info_plist_too_large",
                "Info.plist exceeds the 4 MiB safety limit",
            ));
        }
        let name = entry.name().to_string();
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .by_ref()
            .take(MAX_PLIST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(package_io_error)?;
        if bytes.len() as u64 > MAX_PLIST_BYTES {
            return Err(ApiError::new(
                "ipa_info_plist_too_large",
                "Info.plist exceeds the 4 MiB decompression safety limit",
            ));
        }
        (name, bytes)
    };
    let plist = PlistValue::from_reader(Cursor::new(plist_bytes)).map_err(|error| {
        ApiError::new(
            "invalid_ipa_plist",
            format!("Unable to decode Info.plist: {error}"),
        )
    })?;
    let dictionary = plist
        .as_dictionary()
        .ok_or_else(|| ApiError::new("invalid_ipa_plist", "Info.plist root is not a dictionary"))?;
    let string_value = |key: &str| {
        dictionary
            .get(key)
            .and_then(PlistValue::as_string)
            .map(str::to_string)
    };
    let package_name = string_value("CFBundleIdentifier");
    let display_name = string_value("CFBundleDisplayName").or_else(|| string_value("CFBundleName"));
    let version_name = string_value("CFBundleShortVersionString");
    let version_code = string_value("CFBundleVersion");
    let minimum_os_version = string_value("MinimumOSVersion");
    let executable = string_value("CFBundleExecutable");
    let mut usage_descriptions = BTreeMap::new();
    for (key, value) in dictionary {
        if key.ends_with("UsageDescription") {
            if let Some(text) = value.as_string() {
                usage_descriptions.insert(key.clone(), text.to_string());
            }
        }
    }
    let mut permissions = usage_descriptions.keys().cloned().collect::<BTreeSet<_>>();
    if let Some(capabilities) = dictionary.get("UIRequiredDeviceCapabilities") {
        collect_capabilities(capabilities, &mut permissions);
    }
    let mut icon_hints = Vec::new();
    collect_ipa_icon_names(&plist, None, &mut icon_hints);
    let app_prefix = plist_path.trim_end_matches("Info.plist");
    for hint in &mut icon_hints {
        if !hint.starts_with(app_prefix) {
            *hint = format!("{app_prefix}{hint}");
        }
    }
    let mut warnings = Vec::new();
    let architectures = ipa_architectures(
        &mut archive,
        app_prefix,
        executable.as_deref(),
        &mut warnings,
    );
    let icon = extract_best_icon(
        &path,
        &icon_hints,
        MobilePlatform::Ios,
        Some(app_prefix),
        &mut warnings,
    )?;
    if icon.is_none() && dictionary.contains_key("CFBundleIconName") {
        warnings.push(
            "The IPA references an asset-catalog icon; Assets.car cannot be decoded by the safe ZIP fallback"
                .into(),
        );
    }
    if package_name.is_none() {
        warnings.push("CFBundleIdentifier is missing from Info.plist".into());
    }
    Ok(MobilePackageAnalysis {
        platform: MobilePlatform::Ios,
        path: path.to_string_lossy().into_owned(),
        file_name,
        file_size,
        md5,
        architectures,
        source: "ipaPlist".into(),
        fallback_used: false,
        package_name,
        display_name,
        version_name,
        version_code,
        minimum_os_version,
        target_sdk_version: None,
        permissions: permissions.into_iter().collect(),
        usage_descriptions,
        icon,
        warnings,
    })
}

fn stream_md5(path: &Path) -> Result<String, ApiError> {
    let file = File::open(path).map_err(package_io_error)?;
    let mut reader = BufReader::new(file);
    let mut digest = Md5::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(package_io_error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn ensure_zip(path: &Path) -> Result<(), ApiError> {
    let archive = open_safe_zip(path, "invalid_apk")?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ApiError::new(
            "archive_too_large",
            "Package contains too many ZIP entries",
        ));
    }
    Ok(())
}

fn open_safe_zip(path: &Path, error_code: &str) -> Result<ZipArchive<File>, ApiError> {
    let mut file = File::open(path).map_err(package_io_error)?;
    preflight_zip_structure(&mut file)?;
    file.seek(SeekFrom::Start(0)).map_err(package_io_error)?;
    ZipArchive::new(file)
        .map_err(|error| ApiError::new(error_code, format!("Invalid package ZIP archive: {error}")))
}

fn preflight_zip_structure(file: &mut File) -> Result<(), ApiError> {
    let file_len = file.metadata().map_err(package_io_error)?.len();
    if file_len < 22 {
        return Err(ApiError::new(
            "invalid_package_archive",
            "ZIP end-of-central-directory record is missing",
        ));
    }
    let tail_len = file_len.min(65_557) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(package_io_error)?;
    let mut tail = vec![0_u8; tail_len];
    file.read_exact(&mut tail).map_err(package_io_error)?;
    let eocd_offset = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|offset| {
            tail.get(*offset..*offset + 4) == Some(b"PK\x05\x06")
                && read_le_u16(&tail[*offset + 20..*offset + 22])
                    .is_some_and(|comment_len| *offset + 22 + comment_len as usize == tail.len())
        })
        .ok_or_else(|| {
            ApiError::new(
                "invalid_package_archive",
                "ZIP end-of-central-directory record is missing or malformed",
            )
        })?;
    let eocd = &tail[eocd_offset..eocd_offset + 22];
    if read_le_u16(&eocd[4..6]) != Some(0) || read_le_u16(&eocd[6..8]) != Some(0) {
        return Err(ApiError::new(
            "unsupported_package_archive",
            "Multi-disk ZIP packages are not supported",
        ));
    }
    let mut entry_count = read_le_u16(&eocd[10..12]).unwrap_or(u16::MAX) as u64;
    let mut central_size = read_le_u32(&eocd[12..16]).unwrap_or(u32::MAX) as u64;
    let mut central_offset = read_le_u32(&eocd[16..20]).unwrap_or(u32::MAX) as u64;
    if entry_count == u16::MAX as u64
        || central_size == u32::MAX as u64
        || central_offset == u32::MAX as u64
    {
        let eocd_absolute = file_len - tail_len as u64 + eocd_offset as u64;
        if eocd_absolute < 20 {
            return Err(ApiError::new(
                "invalid_package_archive",
                "ZIP64 locator is missing",
            ));
        }
        let mut locator = [0_u8; 20];
        file.seek(SeekFrom::Start(eocd_absolute - 20))
            .and_then(|_| file.read_exact(&mut locator))
            .map_err(package_io_error)?;
        if locator[..4] != *b"PK\x06\x07"
            || read_le_u32(&locator[4..8]) != Some(0)
            || read_le_u32(&locator[16..20]) != Some(1)
        {
            return Err(ApiError::new(
                "invalid_package_archive",
                "ZIP64 locator is malformed or describes multiple disks",
            ));
        }
        let zip64_offset = read_le_u64(&locator[8..16]).ok_or_else(|| {
            ApiError::new(
                "invalid_package_archive",
                "ZIP64 record offset is malformed",
            )
        })?;
        let mut zip64 = [0_u8; 56];
        file.seek(SeekFrom::Start(zip64_offset))
            .and_then(|_| file.read_exact(&mut zip64))
            .map_err(package_io_error)?;
        if zip64[..4] != *b"PK\x06\x06"
            || read_le_u32(&zip64[16..20]) != Some(0)
            || read_le_u32(&zip64[20..24]) != Some(0)
        {
            return Err(ApiError::new(
                "invalid_package_archive",
                "ZIP64 central-directory record is malformed or uses multiple disks",
            ));
        }
        entry_count = read_le_u64(&zip64[32..40]).unwrap_or(u64::MAX);
        central_size = read_le_u64(&zip64[40..48]).unwrap_or(u64::MAX);
        central_offset = read_le_u64(&zip64[48..56]).unwrap_or(u64::MAX);
    }
    if entry_count > MAX_ARCHIVE_ENTRIES as u64 {
        return Err(ApiError::new(
            "archive_too_large",
            "Package contains too many ZIP entries",
        ));
    }
    let central_end_is_invalid = match central_offset.checked_add(central_size) {
        Some(end) => end > file_len,
        None => true,
    };
    if central_size > MAX_CENTRAL_DIRECTORY_BYTES || central_end_is_invalid {
        return Err(ApiError::new(
            "archive_too_large",
            "ZIP central directory exceeds the Mobius safety limit",
        ));
    }
    Ok(())
}

fn read_le_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.try_into().ok()?))
}

fn read_le_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn read_le_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

fn is_root_ipa_info_plist(entry: &zip::read::ZipFile<'_>) -> bool {
    if entry.is_dir() {
        return false;
    }
    let Some(path) = entry.enclosed_name() else {
        return false;
    };
    let components = path.components().collect::<Vec<_>>();
    matches!(components.as_slice(),
        [std::path::Component::Normal(payload), std::path::Component::Normal(app), std::path::Component::Normal(info)]
            if *payload == std::ffi::OsStr::new("Payload")
                && app.to_string_lossy().ends_with(".app")
                && *info == std::ffi::OsStr::new("Info.plist"))
}

fn package_io_error(error: std::io::Error) -> ApiError {
    ApiError::new("package_io_error", error.to_string())
}

fn quoted_attr(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}='");
    let rest = line.split_once(&marker)?.1;
    Some(rest.split_once('\'')?.0.to_string())
}

fn parse_aapt_badging(output: &str, metadata: &mut PackageMetadata) {
    for line in output.lines() {
        if line.starts_with("package:") {
            metadata.package_name = quoted_attr(line, "name");
            metadata.version_code = quoted_attr(line, "versionCode");
            metadata.version_name = quoted_attr(line, "versionName");
        } else if line.starts_with("application-label:") {
            metadata.display_name = line
                .split_once(':')
                .map(|(_, value)| value.trim_matches('\'').to_string());
        } else if metadata.display_name.is_none() && line.starts_with("application-label-") {
            metadata.display_name = line
                .split_once(':')
                .map(|(_, value)| value.trim_matches('\'').to_string());
        } else if line.starts_with("uses-permission") {
            if let Some(value) = quoted_attr(line, "name").or_else(|| {
                line.split_once(':')
                    .map(|(_, value)| value.trim_matches('\'').to_string())
            }) {
                metadata.permissions.insert(value);
            }
        } else if line.starts_with("application:") {
            metadata.display_name = quoted_attr(line, "label");
            if let Some(icon) = quoted_attr(line, "icon") {
                metadata.icon_hints.push(icon);
            }
        } else if line.starts_with("sdkVersion:") {
            metadata.minimum_os_version = line
                .split_once(':')
                .map(|(_, value)| value.trim_matches('\'').to_string());
        } else if line.starts_with("targetSdkVersion:") {
            metadata.target_sdk_version = line
                .split_once(':')
                .map(|(_, value)| value.trim_matches('\'').to_string());
        } else if line.starts_with("application-icon-") {
            if let Some((_, value)) = line.split_once(':') {
                metadata
                    .icon_hints
                    .push(value.trim_matches('\'').to_string());
            }
        }
    }
}

fn collect_apkanalyzer(path: &str, metadata: &mut PackageMetadata) -> Result<bool, String> {
    if resolve_tool("apkanalyzer").is_err() {
        return Err("apkanalyzer was not found; APK analysis is using ZIP-only fallback".into());
    }
    let commands = [
        ("application-id", "package"),
        ("version-name", "versionName"),
        ("version-code", "versionCode"),
        ("min-sdk", "minSdk"),
        ("target-sdk", "targetSdk"),
        ("permissions", "permissions"),
    ];
    let mut any = false;
    for (command, field) in commands {
        let args = vec!["manifest".into(), command.into(), path.to_string()];
        let Ok(output) = run_process("apkanalyzer", &args, ANALYZER_TIMEOUT, &[]) else {
            continue;
        };
        if output.exit_code != Some(0) || output.timed_out {
            continue;
        }
        let text = output.stdout.trim();
        if text.is_empty() {
            continue;
        }
        any = true;
        match field {
            "package" => metadata.package_name = Some(text.lines().next().unwrap_or(text).into()),
            "versionName" => {
                metadata.version_name = Some(text.lines().next().unwrap_or(text).into())
            }
            "versionCode" => {
                metadata.version_code = Some(text.lines().next().unwrap_or(text).into())
            }
            "minSdk" => {
                metadata.minimum_os_version = Some(text.lines().next().unwrap_or(text).into())
            }
            "targetSdk" => {
                metadata.target_sdk_version = Some(text.lines().next().unwrap_or(text).into())
            }
            "permissions" => metadata.permissions.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string),
            ),
            _ => {}
        }
    }
    if metadata.display_name.is_none() {
        let args = vec!["manifest".into(), "print".into(), path.to_string()];
        if let Ok(output) = run_process("apkanalyzer", &args, ANALYZER_TIMEOUT, &[]) {
            if output.exit_code == Some(0) && !output.timed_out {
                for line in output.stdout.lines() {
                    if let Some(value) = xml_attribute(line, "android:label") {
                        if !value.starts_with('@') {
                            metadata.display_name = Some(value);
                            any = true;
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(any)
}

fn xml_attribute(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=\"");
    let rest = line.split_once(&marker)?.1;
    Some(rest.split_once('"')?.0.to_string())
}

fn apk_architectures(path: &Path) -> Result<Vec<String>, ApiError> {
    let mut archive = open_safe_zip(path, "invalid_apk")?;
    let mut architectures = BTreeSet::new();
    for index in 0..archive.len().min(MAX_ARCHIVE_ENTRIES) {
        let entry = archive
            .by_index(index)
            .map_err(|error| ApiError::new("invalid_apk", error.to_string()))?;
        if !entry.is_dir() {
            if let Some(architecture) = apk_architecture_from_entry(entry.name()) {
                architectures.insert(architecture.to_string());
            }
        }
    }
    Ok(architectures.into_iter().collect())
}

fn apk_architecture_from_entry(name: &str) -> Option<&str> {
    let mut segments = name.split('/');
    if segments.next()? != "lib" {
        return None;
    }
    let architecture = segments.next()?;
    let library = segments.next()?;
    if library.is_empty()
        || !library.to_ascii_lowercase().ends_with(".so")
        || architecture.is_empty()
        || architecture.len() > 64
        || !architecture
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return None;
    }
    Some(architecture)
}

fn ipa_architectures(
    archive: &mut ZipArchive<File>,
    app_prefix: &str,
    executable: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(executable) = executable {
        if !executable.contains('/') && !executable.contains('\\') {
            candidates.push(format!("{app_prefix}{executable}"));
        } else {
            warnings.push("CFBundleExecutable contains an invalid path and was ignored".into());
        }
    }
    for index in 0..archive.len().min(MAX_ARCHIVE_ENTRIES) {
        let Ok(entry) = archive.by_index(index) else {
            continue;
        };
        let name = entry.name();
        if name.starts_with(app_prefix)
            && (name.ends_with(".dylib") || name.contains(".framework/"))
            && !name.ends_with('/')
            && !candidates.iter().any(|candidate| candidate == name)
        {
            candidates.push(name.to_string());
        }
        if candidates.len() >= 33 {
            break;
        }
    }
    let mut architectures = BTreeSet::new();
    for name in candidates {
        let Ok(mut entry) = archive.by_name(&name) else {
            continue;
        };
        let mut header = Vec::with_capacity(4096);
        if entry.by_ref().take(4096).read_to_end(&mut header).is_ok() {
            architectures.extend(parse_macho_architectures(&header));
        }
    }
    if architectures.is_empty() {
        warnings.push(
            "No supported Mach-O architecture header could be read from the IPA; architecture was left unknown"
                .into(),
        );
    }
    architectures.into_iter().collect()
}

#[derive(Clone, Copy)]
enum ByteOrder {
    Big,
    Little,
}

fn parse_macho_architectures(bytes: &[u8]) -> Vec<String> {
    if bytes.len() < 8 {
        return Vec::new();
    }
    let magic = &bytes[..4];
    let single_order = match magic {
        [0xce, 0xfa, 0xed, 0xfe] | [0xcf, 0xfa, 0xed, 0xfe] => Some(ByteOrder::Little),
        [0xfe, 0xed, 0xfa, 0xce] | [0xfe, 0xed, 0xfa, 0xcf] => Some(ByteOrder::Big),
        _ => None,
    };
    if let Some(order) = single_order {
        return read_u32(&bytes[4..8], order)
            .and_then(macho_cpu_name)
            .map(|name| vec![name.to_string()])
            .unwrap_or_default();
    }
    let (order, entry_size) = match magic {
        [0xca, 0xfe, 0xba, 0xbe] => (ByteOrder::Big, 20),
        [0xbe, 0xba, 0xfe, 0xca] => (ByteOrder::Little, 20),
        [0xca, 0xfe, 0xba, 0xbf] => (ByteOrder::Big, 32),
        [0xbf, 0xba, 0xfe, 0xca] => (ByteOrder::Little, 32),
        _ => return Vec::new(),
    };
    let count = read_u32(&bytes[4..8], order).unwrap_or(0).min(32) as usize;
    let mut architectures = BTreeSet::new();
    for index in 0..count {
        let offset = 8 + index * entry_size;
        let Some(cpu_bytes) = bytes.get(offset..offset + 4) else {
            break;
        };
        if let Some(name) = read_u32(cpu_bytes, order).and_then(macho_cpu_name) {
            architectures.insert(name.to_string());
        }
    }
    architectures.into_iter().collect()
}

fn read_u32(bytes: &[u8], order: ByteOrder) -> Option<u32> {
    let bytes: [u8; 4] = bytes.try_into().ok()?;
    Some(match order {
        ByteOrder::Big => u32::from_be_bytes(bytes),
        ByteOrder::Little => u32::from_le_bytes(bytes),
    })
}

fn macho_cpu_name(cpu_type: u32) -> Option<&'static str> {
    match cpu_type {
        7 => Some("x86"),
        12 => Some("arm"),
        18 => Some("powerpc"),
        0x0100_0007 => Some("x86_64"),
        0x0100_000c => Some("arm64"),
        0x0100_0012 => Some("powerpc64"),
        0x0200_000c => Some("arm64_32"),
        _ => None,
    }
}

fn extract_best_icon(
    path: &Path,
    hints: &[String],
    platform: MobilePlatform,
    archive_prefix: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<Option<PackageIcon>, ApiError> {
    let mut archive = open_safe_zip(path, "invalid_package_archive")?;
    let mut candidates = Vec::new();
    for index in 0..archive.len().min(MAX_ARCHIVE_ENTRIES) {
        let entry = archive
            .by_index(index)
            .map_err(|error| ApiError::new("invalid_package_archive", error.to_string()))?;
        let name = entry.name().to_string();
        if archive_prefix.is_some_and(|prefix| !is_within_selected_app(&name, prefix)) {
            continue;
        }
        if entry.is_dir() || entry.size() == 0 || entry.size() > MAX_ICON_BYTES {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        let mime = icon_mime(&lower);
        if mime.is_none() {
            continue;
        }
        let hinted = hints.iter().any(|hint| icon_hint_matches(hint, &name));
        let likely = match platform {
            MobilePlatform::Android => {
                lower.starts_with("res/")
                    && (lower.contains("mipmap") || lower.contains("drawable"))
                    && (lower.contains("launcher") || lower.contains("app_icon") || hinted)
            }
            MobilePlatform::Ios => {
                lower.starts_with("payload/")
                    && lower.contains(".app/")
                    && (lower.contains("appicon") || lower.contains("icon") || hinted)
            }
        };
        if likely {
            candidates.push((icon_score(&lower, hinted, entry.size()), name, entry.size()));
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    let Some((_, name, size)) = candidates.into_iter().next() else {
        warnings.push("No directly extractable raster application icon was found".into());
        return Ok(None);
    };
    let mut entry = archive
        .by_name(&name)
        .map_err(|error| ApiError::new("icon_extract_failed", error.to_string()))?;
    if entry.size() > MAX_ICON_BYTES {
        return Err(ApiError::new(
            "icon_extract_failed",
            "Selected icon exceeds the 8 MiB safety limit",
        ));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    entry
        .by_ref()
        .take(MAX_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(package_io_error)?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Err(ApiError::new(
            "icon_extract_failed",
            "Selected icon exceeds the 8 MiB decompression safety limit",
        ));
    }
    if bytes.windows(4).any(|window| window == b"CgBI") {
        warnings.push(
            "Selected iOS PNG uses Apple's CgBI encoding and may require conversion before preview"
                .into(),
        );
    }
    Ok(Some(PackageIcon {
        archive_path: name.clone(),
        mime_type: icon_mime(&name.to_ascii_lowercase())
            .unwrap_or("application/octet-stream")
            .into(),
        data_base64: BASE64_STANDARD.encode(bytes),
        size_bytes: size,
    }))
}

fn is_within_selected_app(name: &str, app_prefix: &str) -> bool {
    name.strip_prefix(app_prefix).is_some_and(|relative| {
        !relative.is_empty()
            && !relative
                .split('/')
                .any(|segment| segment.to_ascii_lowercase().ends_with(".app"))
    })
}

fn icon_mime(lower: &str) -> Option<&'static str> {
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else {
        None
    }
}

fn icon_hint_matches(hint: &str, candidate: &str) -> bool {
    if hint == candidate {
        return true;
    }
    let hint_name = Path::new(hint)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(hint);
    let candidate_name = Path::new(candidate)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(candidate);
    candidate_name == hint_name || candidate_name.starts_with(&format!("{hint_name}@"))
}

fn icon_score(lower: &str, hinted: bool, size: u64) -> u64 {
    let mut score = size.min(2_000_000);
    if hinted {
        score += 10_000_000;
    }
    for (marker, points) in [
        ("xxxhdpi", 5_000_000),
        ("xxhdpi", 4_000_000),
        ("xhdpi", 3_000_000),
        ("@3x", 5_000_000),
        ("@2x", 4_000_000),
        ("launcher", 2_000_000),
        ("appicon", 2_000_000),
    ] {
        if lower.contains(marker) {
            score += points;
        }
    }
    if lower.contains("foreground") || lower.contains("background") {
        score = score.saturating_sub(1_000_000);
    }
    score
}

fn collect_capabilities(value: &PlistValue, permissions: &mut BTreeSet<String>) {
    if let Some(values) = value.as_array() {
        permissions.extend(
            values
                .iter()
                .filter_map(PlistValue::as_string)
                .map(str::to_string),
        );
    } else if let Some(values) = value.as_dictionary() {
        for (key, enabled) in values {
            if enabled.as_boolean().unwrap_or(false) {
                permissions.insert(key.clone());
            }
        }
    }
}

fn collect_ipa_icon_names(value: &PlistValue, key: Option<&str>, names: &mut Vec<String>) {
    match value {
        PlistValue::Dictionary(values) => {
            for (child_key, child) in values {
                collect_ipa_icon_names(child, Some(child_key), names);
            }
        }
        PlistValue::Array(values) => {
            if key.is_some_and(|value| value.contains("IconFiles")) {
                names.extend(
                    values
                        .iter()
                        .filter_map(PlistValue::as_string)
                        .map(str::to_string),
                );
            } else {
                for child in values {
                    collect_ipa_icon_names(child, key, names);
                }
            }
        }
        PlistValue::String(value)
            if key.is_some_and(|key| key == "CFBundleIconName" || key.contains("IconFiles")) =>
        {
            names.push(value.clone())
        }
        _ => {}
    }
}

pub(crate) fn validate_package_file(
    value: &str,
    platform: MobilePlatform,
) -> Result<PathBuf, ApiError> {
    let path = validation::local_existing_path(value)?
        .canonicalize()
        .map_err(|error| ApiError::new("invalid_package_path", error.to_string()))?;
    if !path.is_file() {
        return Err(ApiError::new(
            "invalid_mobile_package",
            "Selected package is not a regular file",
        ));
    }
    let expected = match platform {
        MobilePlatform::Android => "apk",
        MobilePlatform::Ios => "ipa",
    };
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !extension.eq_ignore_ascii_case(expected) {
        return Err(ApiError::new(
            "package_platform_mismatch",
            format!(
                "Selected {:?} package must use the .{expected} extension",
                platform
            ),
        ));
    }
    Ok(path)
}

fn parse_installed_app(line: &str) -> Option<InstalledApp> {
    let mut fields = line.split_whitespace();
    let package_field = fields.next()?.strip_prefix("package:")?;
    let (apk_path, package_name) = package_field.rsplit_once('=')?;
    if validation::package_name(package_name).is_err() {
        return None;
    }
    let mut uid = None;
    let mut version_code = None;
    for field in fields {
        if let Some(value) = field.strip_prefix("uid:") {
            uid = value.parse().ok();
        } else if let Some(value) = field.strip_prefix("versionCode:") {
            version_code = Some(value.to_string());
        }
    }
    let system = [
        "/system/",
        "/system_ext/",
        "/product/",
        "/vendor/",
        "/apex/",
    ]
    .iter()
    .any(|prefix| apk_path.starts_with(prefix));
    Some(InstalledApp {
        package_name: package_name.to_string(),
        apk_path: apk_path.to_string(),
        uid,
        version_code,
        system,
    })
}

fn parse_package_only_row(line: &str) -> Option<String> {
    let package_name = line.trim().strip_prefix("package:")?;
    validation::package_name(package_name).ok()?;
    Some(package_name.to_string())
}

fn export_android(request: ExportAndroidPackageRequest) -> Result<AndroidPackageExport, ApiError> {
    validation::serial(&request.serial)?;
    validation::package_name(&request.package_name)?;
    let destination = validation::local_existing_path(&request.destination)?
        .canonicalize()
        .map_err(|error| ApiError::new("invalid_destination", error.to_string()))?;
    if !destination.is_dir() {
        return Err(ApiError::new(
            "invalid_destination",
            "Export destination must be an existing directory",
        ));
    }
    let command = format!(
        "pm path {}",
        validation::quote_remote(&request.package_name)
    );
    let paths_output = run_adb_shell(&request.serial, &command, DEVICE_TIMEOUT)?;
    let mut remote_paths = Vec::new();
    let mut warnings = Vec::new();
    for line in paths_output.stdout.lines() {
        let Some(path) = line.trim().strip_prefix("package:") else {
            continue;
        };
        if validation::remote_path(path).is_ok()
            && Path::new(path)
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("apk"))
        {
            remote_paths.push(path.to_string());
        } else {
            warnings.push("The device returned an invalid APK path, which was ignored".into());
        }
    }
    if remote_paths.is_empty() {
        return Err(ApiError::new(
            "android_package_not_found",
            "The selected package is not installed or exposes no APK paths",
        ));
    }
    remote_paths.sort_by_key(|path| !path.ends_with("/base.apk"));
    remote_paths.dedup();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut plans = Vec::new();
    let mut planned_names = BTreeSet::new();
    for (index, remote_path) in remote_paths.iter().enumerate() {
        let basename = Path::new(remote_path)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("package.apk");
        let safe_name = basename
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let local_name = if remote_paths.len() == 1 {
            format!("{}.apk", request.package_name)
        } else {
            format!("{}-{safe_name}", request.package_name)
        };
        if !planned_names.insert(local_name.to_ascii_lowercase()) {
            return Err(ApiError::new(
                "export_name_collision",
                "Two split APK paths resolve to the same safe local file name",
            ));
        }
        let target = destination.join(local_name);
        if target.exists() && !request.overwrite {
            return Err(ApiError::new(
                "export_file_exists",
                format!("Export target already exists: {}", target.display()),
            ));
        }
        let temporary = destination.join(format!(
            ".mobius-export-{}-{nonce}-{index}.part",
            std::process::id(),
        ));
        if temporary.exists() {
            return Err(ApiError::new(
                "export_busy",
                "A previous Mobius package export appears to still be in progress",
            ));
        }
        plans.push((remote_path.clone(), target, temporary));
    }
    let mut files = Vec::new();
    for (index, (remote_path, target, temporary)) in plans.into_iter().enumerate() {
        let pull = run_checked(
            "adb",
            &[
                "-s".into(),
                request.serial.clone(),
                "pull".into(),
                remote_path.clone(),
                temporary.to_string_lossy().into_owned(),
            ],
            PULL_TIMEOUT,
        );
        if let Err(error) = pull {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if target.exists() && request.overwrite {
            fs::remove_file(&target).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                ApiError::new("export_replace_failed", error.to_string())
            })?;
        }
        fs::rename(&temporary, &target).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            ApiError::new("export_finalize_failed", error.to_string())
        })?;
        files.push(ExportedAndroidFile {
            kind: if index == 0 || remote_path.ends_with("/base.apk") {
                "base".into()
            } else {
                "split".into()
            },
            remote_path,
            local_path: target.to_string_lossy().into_owned(),
            size_bytes: target.metadata().ok().map(|value| value.len()),
        });
    }
    Ok(AndroidPackageExport {
        success: true,
        message: format!("Exported {} APK file(s)", files.len()),
        package_name: request.package_name,
        destination: destination.to_string_lossy().into_owned(),
        files,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_aapt_metadata() {
        let mut metadata = PackageMetadata::default();
        parse_aapt_badging(
            "package: name='dev.mobius.demo' versionCode='42' versionName='1.2'\nuses-permission:'android.permission.CAMERA'\napplication: label='Demo' icon='res/mipmap-xxxhdpi/ic_launcher.png'",
            &mut metadata,
        );
        assert_eq!(metadata.package_name.as_deref(), Some("dev.mobius.demo"));
        assert_eq!(metadata.display_name.as_deref(), Some("Demo"));
        assert!(metadata.permissions.contains("android.permission.CAMERA"));
        assert_eq!(metadata.icon_hints.len(), 1);
    }

    #[test]
    fn parses_installed_package_rows() {
        let app = parse_installed_app(
            "package:/data/app/~~token/dev.mobius.demo/base.apk=dev.mobius.demo uid:10234 versionCode:42",
        )
        .expect("package row");
        assert_eq!(app.package_name, "dev.mobius.demo");
        assert_eq!(app.uid, Some(10234));
        assert_eq!(app.version_code.as_deref(), Some("42"));
        assert!(!app.system);
    }

    #[test]
    fn parses_only_exact_valid_system_package_rows() {
        assert_eq!(
            parse_package_only_row("package:dev.mobius.demo\n").as_deref(),
            Some("dev.mobius.demo")
        );
        assert!(parse_package_only_row("package:dev.mobius.demo extra").is_none());
        assert!(parse_package_only_row("package:dev.mobius;demo").is_none());
    }

    #[test]
    fn computes_md5_as_a_stream() {
        let path = std::env::temp_dir().join(format!(
            "mobius-md5-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::write(&path, b"abc").expect("write fixture");
        assert_eq!(
            stream_md5(&path).expect("hash fixture"),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn parses_thin_and_fat_macho_architectures() {
        let thin_arm64 = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
        assert_eq!(parse_macho_architectures(&thin_arm64), vec!["arm64"]);

        let mut fat = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2];
        fat.extend_from_slice(&[0x01, 0x00, 0x00, 0x0c]);
        fat.extend_from_slice(&[0; 16]);
        fat.extend_from_slice(&[0x01, 0x00, 0x00, 0x07]);
        fat.extend_from_slice(&[0; 16]);
        assert_eq!(parse_macho_architectures(&fat), vec!["arm64", "x86_64"]);
    }

    #[test]
    fn extracts_only_native_apk_library_abis() {
        assert_eq!(
            apk_architecture_from_entry("lib/arm64-v8a/libdemo.so"),
            Some("arm64-v8a")
        );
        assert_eq!(apk_architecture_from_entry("lib/arm64-v8a/"), None);
        assert_eq!(
            apk_architecture_from_entry("assets/arm64-v8a/libdemo.so"),
            None
        );
    }

    #[test]
    fn lan_ssh_endpoint_cannot_reach_usbmux_installer() {
        let error = require_connected_ios_udid("ios-ssh:192.168.1.42:22")
            .expect_err("LAN endpoint must be rejected before invoking idevice tools");
        assert_eq!(error.code, "ios_usb_device_required");
    }

    #[test]
    fn analyzes_ipa_plist_icon_and_architecture() {
        let path = std::env::temp_dir().join(format!(
            "mobius-ipa-{}-{}.ipa",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let file = File::create(&path).expect("create IPA fixture");
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        archive
            .start_file("Payload/Demo.app/PlugIns/Decoy.app/Info.plist", options)
            .expect("nested plist entry");
        archive
            .write_all(b"not the root application")
            .expect("write nested plist");
        archive
            .start_file(
                "Payload/Demo.app/PlugIns/Decoy.app/AppIcon60x60@3x.png",
                options,
            )
            .expect("nested icon entry");
        archive.write_all(&[b'x'; 256]).expect("write nested icon");
        archive
            .start_file("Payload/Demo.app/Info.plist", options)
            .expect("plist entry");
        archive
            .write_all(
                br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.mobius.demo</string>
<key>CFBundleDisplayName</key><string>Demo</string>
<key>CFBundleShortVersionString</key><string>1.2</string>
<key>CFBundleVersion</key><string>42</string>
<key>CFBundleExecutable</key><string>Demo</string>
<key>CFBundleIconFiles</key><array><string>AppIcon60x60</string></array>
<key>NSCameraUsageDescription</key><string>Scan a code</string>
</dict></plist>"#,
            )
            .expect("write plist");
        archive
            .start_file("Payload/Demo.app/Demo", options)
            .expect("executable entry");
        archive
            .write_all(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01])
            .expect("write Mach-O header");
        archive
            .start_file("Payload/Demo.app/AppIcon60x60@3x.png", options)
            .expect("icon entry");
        archive
            .write_all(b"\x89PNG\r\n\x1a\nfixture")
            .expect("write icon");
        archive.finish().expect("finish IPA fixture");

        let analysis =
            analyze_package(path.to_str().expect("UTF-8 temp path")).expect("analyze IPA fixture");
        assert_eq!(analysis.package_name.as_deref(), Some("dev.mobius.demo"));
        assert_eq!(analysis.architectures, vec!["arm64"]);
        assert_eq!(
            analysis
                .usage_descriptions
                .get("NSCameraUsageDescription")
                .map(String::as_str),
            Some("Scan a code")
        );
        assert_eq!(
            analysis
                .icon
                .as_ref()
                .map(|icon| icon.archive_path.as_str()),
            Some("Payload/Demo.app/AppIcon60x60@3x.png")
        );
        fs::remove_file(path).expect("remove IPA fixture");
    }

    #[test]
    #[ignore = "requires a user-selected real APK available on the local test host"]
    fn live_real_apk_analysis_extracts_core_metadata() {
        let path = std::env::var("MOBIUS_LIVE_APK_PATH")
            .expect("set MOBIUS_LIVE_APK_PATH to the selected APK");
        let analysis = analyze_package(&path).expect("analyze the real APK");
        assert_eq!(analysis.platform, MobilePlatform::Android);
        assert!(analysis.file_size > 0);
        assert_eq!(analysis.md5.len(), 32);
        assert!(analysis.md5.chars().all(|value| value.is_ascii_hexdigit()));
        assert!(analysis
            .package_name
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
        assert!(analysis
            .display_name
            .as_deref()
            .is_some_and(|value| !value.is_empty()));
    }
}
