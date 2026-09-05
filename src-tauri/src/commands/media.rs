use super::{blocking_api, files::run_adb_shell};
use crate::{
    models::{
        AndroidScreenFrameResult, AndroidScreenRecordingResult, AndroidScreenRecordingSession,
        AndroidScreenshotRequest, AndroidScreenshotResult, ApiError, ApiResult,
        IosScreenCapability, IosScreenTargetRequest, IosScreenshotRequest,
        StartAndroidScreenRecordingRequest, StopAndroidScreenRecordingRequest,
    },
    runner::{background_command, resolve_tool, run_checked, run_process},
    state::{AppState, ManagedAndroidScreenRecording},
    validation,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_PULL_TIMEOUT: Duration = Duration::from_secs(300);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);
const RECORDING_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const RECORDING_STOP_ATTEMPTS: usize = 60;
const RECORDING_FORCE_STOP_ATTEMPTS: usize = 20;
const RECORDING_PULL_GRACE_SECONDS: u64 = 120;
const RECORDING_PULL_BYTES_PER_SECOND: u64 = 256 * 1024;
const MIN_RECORDING_BIT_RATE: u32 = 100_000;
const MAX_RECORDING_BIT_RATE: u32 = 100_000_000;
const MAX_SCREENSHOT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SCREENSHOT_DIMENSION: u32 = 16_384;
const MAX_SCREENSHOT_PIXELS: u64 = 40_000_000;
const INLINE_FRAME_TIMEOUT: Duration = Duration::from_secs(8);
const IOS_SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_INLINE_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_INLINE_STDERR_BYTES: usize = 64 * 1024;
const MAX_MP4_TOP_LEVEL_BOXES: usize = 100_000;

static CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const RECORDING_PID_MARKER: &str = "__MOBIUS_RECORDING_PID__=";
const RECORDING_EXE_MARKER: &str = "__MOBIUS_RECORDING_EXE__=";
const RECORDING_STAT_MARKER: &str = "__MOBIUS_RECORDING_STAT__=";
const RECORDING_SIGNALLED_MARKER: &str = "__MOBIUS_RECORDING_SIGNALLED__";
const RECORDING_SIGNAL_SKIPPED_MARKER: &str = "__MOBIUS_RECORDING_SIGNAL_SKIPPED__";

#[tauri::command]
pub async fn capture_android_screenshot(
    request: AndroidScreenshotRequest,
    app: AppHandle,
) -> ApiResult<AndroidScreenshotResult> {
    blocking_api(move || capture_screenshot(request, &app)).await
}

#[tauri::command]
pub async fn capture_android_screen_frame(serial: String) -> ApiResult<AndroidScreenFrameResult> {
    blocking_api(move || capture_inline_frame(&serial)).await
}

#[tauri::command]
pub async fn probe_ios_screen_capability(
    request: IosScreenTargetRequest,
) -> ApiResult<IosScreenCapability> {
    blocking_api(move || probe_ios_screen(&request.udid)).await
}

#[tauri::command]
pub async fn capture_ios_screen_frame(
    request: IosScreenTargetRequest,
) -> ApiResult<AndroidScreenFrameResult> {
    blocking_api(move || capture_ios_inline_frame(&request.udid)).await
}

#[tauri::command]
pub async fn capture_ios_screenshot(
    request: IosScreenshotRequest,
    app: AppHandle,
) -> ApiResult<AndroidScreenshotResult> {
    blocking_api(move || capture_ios_screenshot_inner(request, &app)).await
}

#[tauri::command]
pub async fn start_android_screen_recording(
    request: StartAndroidScreenRecordingRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<AndroidScreenRecordingSession>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || start_screen_recording(&state, request)).await)
}

#[tauri::command]
pub async fn stop_android_screen_recording(
    request: StopAndroidScreenRecordingRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<AndroidScreenRecordingResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || stop_screen_recording(&state, request)).await)
}

fn capture_screenshot(
    request: AndroidScreenshotRequest,
    app: &AppHandle,
) -> Result<AndroidScreenshotResult, ApiError> {
    validation::serial(&request.serial)?;
    if request.destination_directory.is_none() && !request.copy_to_clipboard {
        return Err(ApiError::new(
            "missing_capture_destination",
            "Choose a PC save directory, enable clipboard copy, or both",
        ));
    }

    let mut target = CaptureTarget::prepare(
        request.destination_directory.as_deref(),
        "mobius-screenshot",
        "png",
    )?;
    let remote_path = remote_capture_path("shot", "png");
    let operation = (|| {
        run_checked(
            "adb",
            &[
                "-s".into(),
                request.serial.clone(),
                "shell".into(),
                "screencap".into(),
                "-p".into(),
                remote_path.clone(),
            ],
            SCREENSHOT_TIMEOUT,
        )?;
        pull_remote_capture(&request.serial, &remote_path, target.path())?;

        let (width, height, size_bytes) = inspect_png(target.path())?;
        let image = decode_png(target.path())?;
        if image.width() != width || image.height() != height {
            return Err(ApiError::new(
                "invalid_screenshot_image",
                "Decoded screenshot dimensions do not match its PNG header",
            ));
        }
        let mut warnings = Vec::new();
        let mut copied_to_clipboard = false;
        if request.copy_to_clipboard {
            match copy_image_to_clipboard(app, &image) {
                Ok(()) => copied_to_clipboard = true,
                Err(error) if target.is_persistent() => warnings.push(error.message),
                Err(error) => return Err(error),
            }
        }

        let saved_path = target.persistent_path();
        target.mark_complete();
        let message = match (saved_path.is_some(), copied_to_clipboard) {
            (true, true) => "Screenshot saved to the PC and copied to the clipboard",
            (true, false) => "Screenshot saved to the PC",
            (false, true) => "Screenshot copied to the clipboard",
            (false, false) => "Screenshot captured",
        };
        Ok(AndroidScreenshotResult {
            success: true,
            message: message.into(),
            saved_path,
            copied_to_clipboard,
            size_bytes,
            width,
            height,
            warnings,
        })
    })();

    let cleanup_warning = cleanup_remote_capture(&request.serial, &remote_path, false);
    match operation {
        Ok(mut result) => {
            if let Some(warning) = cleanup_warning {
                result.warnings.push(warning);
            }
            Ok(result)
        }
        Err(error) => {
            if let Some(warning) = cleanup_warning {
                eprintln!("Mobius media cleanup: {warning}");
            }
            Err(error)
        }
    }
}

fn capture_inline_frame(serial: &str) -> Result<AndroidScreenFrameResult, ApiError> {
    validation::serial(serial)?;
    let executable = resolve_tool("adb")?;
    let mut child = background_command(executable)
        .args(["-s", serial, "exec-out", "screencap", "-p"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            ApiError::new(
                "screen_frame_spawn_failed",
                format!("Unable to start adb: {error}"),
            )
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::new("screen_frame_io_error", "Unable to capture adb output"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ApiError::new("screen_frame_io_error", "Unable to capture adb errors"))?;
    let stdout_reader = thread::spawn(move || read_bounded_bytes(stdout, MAX_INLINE_FRAME_BYTES));
    let stderr_reader = thread::spawn(move || read_bounded_bytes(stderr, MAX_INLINE_STDERR_BYTES));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < INLINE_FRAME_TIMEOUT => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApiError::new(
                    "screen_frame_timeout",
                    "Android screen preview did not respond within 8 seconds",
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ApiError::new(
                    "screen_frame_wait_failed",
                    format!("Unable to wait for adb: {error}"),
                ));
            }
        }
    };
    let (bytes, stdout_truncated) = stdout_reader.join().map_err(|_| {
        ApiError::new(
            "screen_frame_io_error",
            "Screen frame reader stopped unexpectedly",
        )
    })??;
    let (stderr, _) = stderr_reader.join().map_err(|_| {
        ApiError::new(
            "screen_frame_io_error",
            "Screen frame error reader stopped unexpectedly",
        )
    })??;
    if stdout_truncated {
        return Err(ApiError::new(
            "screen_frame_too_large",
            "Android screen preview exceeded the 16 MiB inline frame limit",
        ));
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr)
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
            .collect::<String>();
        return Err(ApiError::new(
            "screen_frame_failed",
            if detail.trim().is_empty() {
                format!("adb exited with status {:?}", status.code())
            } else {
                format!("adb: {}", detail.trim())
            },
        ));
    }
    let (width, height, size_bytes) = inspect_png_bytes(&bytes, MAX_INLINE_FRAME_BYTES as u64)?;
    Ok(AndroidScreenFrameResult {
        image_data_url: format!("data:image/png;base64,{}", BASE64_STANDARD.encode(bytes)),
        size_bytes,
        width,
        height,
        captured_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    })
}

fn probe_ios_screen(udid: &str) -> Result<IosScreenCapability, ApiError> {
    match detect_ios_screen_transport(udid) {
        Ok(transport) => Ok(IosScreenCapability {
            available: true,
            transport: transport.to_string(),
            message: if transport == "usb" {
                "The paired iOS device is available through USB/usbmux".into()
            } else {
                "The paired iOS device is available through the network lockdown service".into()
            },
        }),
        Err(error) if error.code == "ios_screen_target_unavailable" => Ok(IosScreenCapability {
            available: false,
            transport: "unavailable".into(),
            message: error.message,
        }),
        Err(error) => Err(error),
    }
}

fn capture_ios_inline_frame(udid: &str) -> Result<AndroidScreenFrameResult, ApiError> {
    let (target, _) = capture_ios_png(udid)?;
    let bytes = fs::read(target.path()).map_err(|error| {
        ApiError::new(
            "capture_read_error",
            format!("Unable to read the captured iOS screen: {error}"),
        )
    })?;
    let (width, height, size_bytes) = inspect_png_bytes(&bytes, MAX_INLINE_FRAME_BYTES as u64)?;
    Ok(AndroidScreenFrameResult {
        image_data_url: format!("data:image/png;base64,{}", BASE64_STANDARD.encode(bytes)),
        size_bytes,
        width,
        height,
        captured_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    })
}

fn capture_ios_screenshot_inner(
    request: IosScreenshotRequest,
    app: &AppHandle,
) -> Result<AndroidScreenshotResult, ApiError> {
    if request.destination_directory.is_none() && !request.copy_to_clipboard {
        return Err(ApiError::new(
            "missing_capture_destination",
            "Choose a PC save directory, enable clipboard copy, or both",
        ));
    }
    let (temporary, _) = capture_ios_png(&request.udid)?;
    let (width, height, size_bytes) = inspect_png(temporary.path())?;
    let image = decode_png(temporary.path())?;
    if image.width() != width || image.height() != height {
        return Err(ApiError::new(
            "invalid_screenshot_image",
            "Decoded iOS screenshot dimensions do not match its PNG header",
        ));
    }

    let mut persistent = match request.destination_directory.as_deref() {
        Some(directory) => {
            let target = CaptureTarget::prepare(Some(directory), "mobius-ios-screenshot", "png")?;
            fs::copy(temporary.path(), target.path()).map_err(|error| {
                ApiError::new(
                    "capture_save_error",
                    format!("Unable to save the iOS screenshot to the PC: {error}"),
                )
            })?;
            Some(target)
        }
        None => None,
    };
    let mut warnings = Vec::new();
    let mut copied_to_clipboard = false;
    if request.copy_to_clipboard {
        match copy_image_to_clipboard(app, &image) {
            Ok(()) => copied_to_clipboard = true,
            Err(error) if persistent.is_some() => warnings.push(error.message),
            Err(error) => return Err(error),
        }
    }
    let saved_path = persistent.as_ref().and_then(CaptureTarget::persistent_path);
    if let Some(target) = persistent.as_mut() {
        target.mark_complete();
    }
    let message = match (saved_path.is_some(), copied_to_clipboard) {
        (true, true) => "iOS screenshot saved to the PC and copied to the clipboard",
        (true, false) => "iOS screenshot saved to the PC",
        (false, true) => "iOS screenshot copied to the clipboard",
        (false, false) => "iOS screenshot captured",
    };
    Ok(AndroidScreenshotResult {
        success: true,
        message: message.into(),
        saved_path,
        copied_to_clipboard,
        size_bytes,
        width,
        height,
        warnings,
    })
}

fn capture_ios_png(udid: &str) -> Result<(CaptureTarget, &'static str), ApiError> {
    let transport = detect_ios_screen_transport(udid)?;
    let target = CaptureTarget::prepare(None, "mobius-ios-frame", "png")?;
    let mut args = vec!["-u".into(), udid.into()];
    if transport == "network" {
        args.push("-n".into());
    }
    args.push(target.path().to_string_lossy().into_owned());
    run_checked("idevicescreenshot", &args, IOS_SCREENSHOT_TIMEOUT).map_err(|error| {
        let detail = error.message.to_ascii_lowercase();
        let message = if detail.contains("screenshotr")
            || detail.contains("developer disk")
            || detail.contains("service")
        {
            "The iOS screenshot service is unavailable. Confirm device trust and mount the matching Developer Disk Image."
        } else {
            "The paired iOS device did not return a screenshot. Check the USB/network connection, trust state, and Developer Disk Image."
        };
        ApiError::new("ios_screen_capture_failed", message)
            .with_details(serde_json::json!({ "cause": error.code }))
    })?;
    inspect_png(target.path()).map_err(|_| {
        ApiError::new(
            "unsupported_ios_screen_format",
            "The iOS device returned an unsupported or invalid screen image; PNG is required",
        )
    })?;
    Ok((target, transport))
}

fn detect_ios_screen_transport(udid: &str) -> Result<&'static str, ApiError> {
    validation::serial(udid)?;
    if udid.starts_with("ios-ssh:") {
        return Err(ApiError::new(
            "ios_screen_target_unavailable",
            "An SSH-only iOS endpoint has no paired screenshot service. Connect the device through USB/usbmux or an already paired lockdown network connection.",
        ));
    }
    let usb = run_checked("idevice_id", &["-l".into()], Duration::from_secs(8))?;
    if output_has_exact_identifier(&usb.stdout, udid) {
        return Ok("usb");
    }
    if let Ok(network) = run_checked("idevice_id", &["-n".into()], Duration::from_secs(8)) {
        if output_has_exact_identifier(&network.stdout, udid) {
            return Ok("network");
        }
    }
    Err(ApiError::new(
        "ios_screen_target_unavailable",
        "This UDID is not present on an explicitly paired USB or network iOS connection.",
    ))
}

fn output_has_exact_identifier(output: &str, expected: &str) -> bool {
    output.lines().map(str::trim).any(|value| value == expected)
}

fn read_bounded_bytes<R: Read>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool), ApiError> {
    let mut kept = Vec::with_capacity(limit.min(1024 * 1024));
    let mut buffer = [0_u8; 64 * 1024];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            ApiError::new(
                "screen_frame_io_error",
                format!("Unable to read adb screen output: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        let keep = remaining.min(read);
        kept.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((kept, truncated))
}

fn inspect_png_bytes(bytes: &[u8], maximum_bytes: u64) -> Result<(u32, u32, u64), ApiError> {
    let size_bytes = bytes.len() as u64;
    if size_bytes < 24 || size_bytes > maximum_bytes {
        return Err(ApiError::new(
            "invalid_screenshot_image",
            "Device screen frame is empty, incomplete, or exceeds the supported size limit",
        ));
    }
    if bytes[..8] != [137, 80, 78, 71, 13, 10, 26, 10] || &bytes[12..16] != b"IHDR" {
        return Err(ApiError::new(
            "invalid_screenshot_image",
            "Device screen frame is not a valid PNG image",
        ));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap_or_default());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap_or_default());
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_SCREENSHOT_DIMENSION
        || height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
    {
        return Err(ApiError::new(
            "invalid_screenshot_dimensions",
            "Device screen frame dimensions exceed the supported safety limits",
        ));
    }
    Ok((width, height, size_bytes))
}

fn start_screen_recording(
    state: &AppState,
    request: StartAndroidScreenRecordingRequest,
) -> Result<AndroidScreenRecordingSession, ApiError> {
    validation::serial(&request.serial)?;
    if request
        .bit_rate
        .is_some_and(|value| !(MIN_RECORDING_BIT_RATE..=MAX_RECORDING_BIT_RATE).contains(&value))
    {
        return Err(ApiError::new(
            "invalid_recording_bit_rate",
            "Recording bit rate must be between 100 Kbit/s and 100 Mbit/s",
        ));
    }

    let destination = validated_destination_directory(&request.destination_directory)?;
    let local_path = reserve_unique_file(&destination, "mobius-recording", "mp4")?;
    let remote_path = remote_capture_path("recording", "mp4");
    let remote_log_path = remote_capture_path("recording-log", "txt");
    let session_id = format!("recording-{}", unique_token());
    let _lifecycle_guard = state
        .screen_recording_lifecycle_lock
        .lock()
        .map_err(|_| ApiError::new("state_error", "Recording lifecycle lock was poisoned"))?;
    let mut recordings = state
        .screen_recordings
        .lock()
        .map_err(|_| ApiError::new("state_error", "Recording registry lock was poisoned"))?;
    if state.shutting_down.load(Ordering::Acquire) {
        let _ = fs::remove_file(&local_path);
        return Err(ApiError::new(
            "app_shutting_down",
            "Mobius is exiting and cannot start a screen recording",
        ));
    }
    if recordings.contains_key(&request.serial) {
        let _ = fs::remove_file(&local_path);
        return Err(ApiError::new(
            "recording_already_active",
            "Mobius is already recording this Android device",
        ));
    }

    let screen_was_woken = wake_sleeping_display(&request.serial).unwrap_or(false);
    let launch = launch_recording_process(
        &request.serial,
        &remote_path,
        &remote_log_path,
        request.bit_rate,
        false,
    );
    let (pid, identity, use_su, warnings) = match launch {
        Ok((pid, identity)) => (pid, identity, false, Vec::new()),
        Err(error) if request.allow_root_fallback && recording_start_allows_root_retry(&error) => {
            let _ = cleanup_remote_capture(&request.serial, &remote_path, false);
            let _ = cleanup_remote_capture(&request.serial, &remote_log_path, false);
            if let Err(root_error) = require_noninteractive_root(&request.serial) {
                rollback_recording_start(
                    &request.serial,
                    &remote_path,
                    &remote_log_path,
                    &local_path,
                    false,
                    screen_was_woken,
                );
                return Err(root_error);
            }
            match launch_recording_process(
                &request.serial,
                &remote_path,
                &remote_log_path,
                request.bit_rate,
                true,
            ) {
                Ok((pid, identity)) => (
                    pid,
                    identity,
                    true,
                    vec!["Device denied standard shell recording; Mobius used the explicitly allowed Root compatibility fallback".into()],
                ),
                Err(root_error) => {
                    rollback_recording_start(
                        &request.serial,
                        &remote_path,
                        &remote_log_path,
                        &local_path,
                        true,
                        screen_was_woken,
                    );
                    return Err(root_error);
                }
            }
        }
        Err(error) => {
            rollback_recording_start(
                &request.serial,
                &remote_path,
                &remote_log_path,
                &local_path,
                false,
                screen_was_woken,
            );
            return Err(error);
        }
    };
    let started_at_ms = epoch_millis();
    let managed = ManagedAndroidScreenRecording {
        session_id: session_id.clone(),
        serial: request.serial.clone(),
        pid,
        executable: identity.executable,
        process_start_time: identity.start_time,
        remote_path,
        remote_log_path,
        local_path: local_path.clone(),
        use_su,
        screen_was_woken,
        started_at: Instant::now(),
        warnings: warnings.clone(),
    };
    recordings.insert(request.serial.clone(), managed);
    Ok(AndroidScreenRecordingSession {
        success: true,
        message: "Screen recording started and will continue until stopped".into(),
        session_id,
        serial: request.serial,
        started_at_ms,
        planned_saved_path: local_path.to_string_lossy().into_owned(),
        warnings,
    })
}

fn stop_screen_recording(
    state: &AppState,
    request: StopAndroidScreenRecordingRequest,
) -> Result<AndroidScreenRecordingResult, ApiError> {
    validation::serial(&request.serial)?;
    if request.session_id.is_empty() || request.session_id.len() > 192 {
        return Err(ApiError::new(
            "invalid_recording_session",
            "Recording session ID is missing or malformed",
        ));
    }
    let _lifecycle_guard = state
        .screen_recording_lifecycle_lock
        .lock()
        .map_err(|_| ApiError::new("state_error", "Recording lifecycle lock was poisoned"))?;
    let managed = {
        let recordings = state
            .screen_recordings
            .lock()
            .map_err(|_| ApiError::new("state_error", "Recording registry lock was poisoned"))?;
        let managed = recordings.get(&request.serial).ok_or_else(|| {
            ApiError::new(
                "recording_not_found",
                "No active Mobius recording exists for this Android device",
            )
        })?;
        if managed.session_id != request.session_id || managed.serial != request.serial {
            return Err(ApiError::new(
                "recording_session_mismatch",
                "The recording session does not belong to the selected Android device",
            ));
        }
        managed.clone()
    };

    let result = finalize_recording(&managed)?;
    let mut recordings = state
        .screen_recordings
        .lock()
        .map_err(|_| ApiError::new("state_error", "Recording registry lock was poisoned"))?;
    if recordings
        .get(&request.serial)
        .is_some_and(|current| current.session_id == request.session_id)
    {
        recordings.remove(&request.serial);
    }
    Ok(result)
}

#[derive(Debug)]
struct RecordingProcessIdentity {
    executable: String,
    start_time: String,
}

fn launch_recording_process(
    serial: &str,
    remote_path: &str,
    remote_log_path: &str,
    bit_rate: Option<u32>,
    use_su: bool,
) -> Result<(u32, RecordingProcessIdentity), ApiError> {
    let command = recording_launch_command(remote_path, remote_log_path, bit_rate);
    let output = run_recording_shell(serial, &command, use_su)?;
    let pid = parse_recording_pid(&output.stdout)?;
    let identity = match parse_recording_identity(&output.stdout) {
        Ok(identity) => identity,
        Err(error) => {
            abort_unaccepted_recording(serial, pid, use_su);
            return Err(error);
        }
    };
    if identity.executable.rsplit('/').next() != Some("screenrecord") {
        abort_unaccepted_recording(serial, pid, use_su);
        return Err(ApiError::new(
            "recording_identity_mismatch",
            "The started Android process was not the system screen recorder",
        ));
    }
    Ok((pid, identity))
}

fn recording_launch_command(
    remote_path: &str,
    remote_log_path: &str,
    bit_rate: Option<u32>,
) -> String {
    let output = validation::quote_remote(remote_path);
    let log = validation::quote_remote(remote_log_path);
    let mut screenrecord = "screenrecord --time-limit 0".to_string();
    if let Some(bit_rate) = bit_rate {
        screenrecord.push_str(&format!(" --bit-rate {bit_rate}"));
    }
    screenrecord.push(' ');
    screenrecord.push_str(&output);
    format!(
        "mobius_abort() {{ \
           kill -2 \"$mobius_pid\" 2>/dev/null || true; sleep 1; \
           if kill -0 \"$mobius_pid\" 2>/dev/null; then kill -15 \"$mobius_pid\" 2>/dev/null || true; fi; \
           sleep 1; \
           if kill -0 \"$mobius_pid\" 2>/dev/null; then kill -9 \"$mobius_pid\" 2>/dev/null || true; fi; \
         }}; \
         rm -f {output} {log}; nohup {screenrecord} </dev/null >{log} 2>&1 & \
         mobius_pid=$!; sleep 1; \
         if kill -0 \"$mobius_pid\" 2>/dev/null; then \
           mobius_exe=$(readlink -f \"/proc/$mobius_pid/exe\" 2>/dev/null || true); \
           mobius_stat=$(cat \"/proc/$mobius_pid/stat\" 2>/dev/null || true); \
           case \"$mobius_exe\" in \
             */screenrecord|screenrecord) \
               if [ -n \"$mobius_stat\" ]; then \
                 printf '{RECORDING_PID_MARKER}%s\\n' \"$mobius_pid\"; \
                 printf '{RECORDING_EXE_MARKER}%s\\n' \"$mobius_exe\"; \
                 printf '{RECORDING_STAT_MARKER}%s\\n' \"$mobius_stat\"; \
               else mobius_abort; cat {log} 2>/dev/null; exit 76; fi ;; \
             *) mobius_abort; cat {log} 2>/dev/null; exit 76 ;; \
           esac; \
         else cat {log} 2>/dev/null; exit 75; fi"
    )
}

fn marker_value<'a>(stdout: &'a str, marker: &str) -> Option<&'a str> {
    stdout
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(marker))
        .filter(|value| !value.is_empty())
}

fn parse_recording_pid(stdout: &str) -> Result<u32, ApiError> {
    marker_value(stdout, RECORDING_PID_MARKER)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|pid| *pid > 1)
        .ok_or_else(|| {
            ApiError::new(
                "recording_start_failed",
                "Android did not return a valid managed screenrecord PID",
            )
        })
}

fn parse_recording_identity(stdout: &str) -> Result<RecordingProcessIdentity, ApiError> {
    let executable = marker_value(stdout, RECORDING_EXE_MARKER).ok_or_else(|| {
        ApiError::new(
            "recording_identity_error",
            "Android did not return the managed screenrecord executable identity",
        )
    })?;
    let stat = marker_value(stdout, RECORDING_STAT_MARKER).ok_or_else(|| {
        ApiError::new(
            "recording_identity_error",
            "Android did not return the managed screenrecord process metadata",
        )
    })?;
    Ok(RecordingProcessIdentity {
        executable: executable.to_string(),
        start_time: parse_process_start_time(stat)?,
    })
}

fn abort_unaccepted_recording(serial: &str, pid: u32, use_su: bool) {
    let command = format!(
        "mobius_exe=$(readlink -f /proc/{pid}/exe 2>/dev/null || true); \
         case \"$mobius_exe\" in \
           */screenrecord|screenrecord) kill -2 {pid} 2>/dev/null || true ;; \
         esac"
    );
    let _ = run_recording_shell(serial, &command, use_su);
}

fn run_recording_shell(
    serial: &str,
    command: &str,
    use_su: bool,
) -> Result<crate::runner::ProcessOutput, ApiError> {
    if use_su {
        run_adb_shell(
            serial,
            &format!("su -c {}", validation::quote_remote(command)),
            RECORDING_COMMAND_TIMEOUT,
        )
    } else {
        run_adb_shell(serial, command, RECORDING_COMMAND_TIMEOUT)
    }
}

fn query_recording_process_identity(
    serial: &str,
    pid: u32,
    use_su: bool,
) -> Result<Option<RecordingProcessIdentity>, ApiError> {
    let command = format!(
        "if [ -r /proc/{pid}/stat ]; then readlink -f /proc/{pid}/exe; cat /proc/{pid}/stat; fi"
    );
    let output = run_recording_shell(serial, &command, use_su)?;
    let mut lines = output.stdout.lines();
    let executable = match lines.next().map(str::trim).filter(|line| !line.is_empty()) {
        Some(value) => value.to_string(),
        None => return Ok(None),
    };
    let stat = lines.next().ok_or_else(|| {
        ApiError::new(
            "recording_identity_error",
            "Android returned incomplete screenrecord process metadata",
        )
    })?;
    let start_time = parse_process_start_time(stat)?;
    Ok(Some(RecordingProcessIdentity {
        executable,
        start_time,
    }))
}

fn parse_process_start_time(stat: &str) -> Result<String, ApiError> {
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .map(str::to_string)
        .ok_or_else(|| {
            ApiError::new(
                "recording_identity_error",
                "Android returned malformed screenrecord process metadata",
            )
        })
}

fn recording_process_matches(
    identity: Option<&RecordingProcessIdentity>,
    managed: &ManagedAndroidScreenRecording,
) -> bool {
    identity.is_some_and(|identity| {
        identity.executable == managed.executable
            && identity.start_time == managed.process_start_time
    })
}

fn signal_recording_process(
    managed: &ManagedAndroidScreenRecording,
    signal: &str,
) -> Result<bool, ApiError> {
    let signal = match signal {
        "INT" => "2",
        "TERM" => "15",
        "KILL" => "9",
        _ => {
            return Err(ApiError::new(
                "invalid_recording_signal",
                "Unsupported managed recording signal",
            ))
        }
    };
    let command = recording_signal_command(
        managed.pid,
        &managed.executable,
        &managed.process_start_time,
        signal,
    );
    run_recording_shell(&managed.serial, &command, managed.use_su).map(|output| {
        output
            .stdout
            .lines()
            .any(|line| line.trim() == RECORDING_SIGNALLED_MARKER)
    })
}

fn recording_signal_command(
    pid: u32,
    executable: &str,
    process_start_time: &str,
    signal: &str,
) -> String {
    let expected_executable = validation::quote_remote(executable);
    let expected_start_time = validation::quote_remote(process_start_time);
    let command = format!(
        "set -f; \
         mobius_exe=$(readlink -f /proc/{pid}/exe 2>/dev/null || true); \
         mobius_stat=$(cat /proc/{pid}/stat 2>/dev/null || true); \
         mobius_rest=${{mobius_stat##*) }}; set -- $mobius_rest; mobius_start=${{20}}; \
         if [ \"$mobius_exe\" = {expected_executable} ] && [ \"$mobius_start\" = {expected_start_time} ]; then \
           if kill -{signal} {pid} 2>/dev/null; then printf '{RECORDING_SIGNALLED_MARKER}\\n'; else exit 77; fi; \
         else printf '{RECORDING_SIGNAL_SKIPPED_MARKER}\\n'; fi",
    );
    command
}

fn wait_for_recording_stop(
    managed: &ManagedAndroidScreenRecording,
    attempts: usize,
) -> Result<bool, ApiError> {
    for _ in 0..attempts {
        thread::sleep(Duration::from_millis(100));
        let identity =
            query_recording_process_identity(&managed.serial, managed.pid, managed.use_su)?;
        if !recording_process_matches(identity.as_ref(), managed) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn stop_recording_process(
    managed: &ManagedAndroidScreenRecording,
) -> Result<Vec<String>, ApiError> {
    let identity = query_recording_process_identity(&managed.serial, managed.pid, managed.use_su)?;
    if identity.is_none() {
        return Ok(vec![
            "The device recording process had already stopped before the stop request".into(),
        ]);
    }
    if !recording_process_matches(identity.as_ref(), managed) {
        return Ok(vec![
            "The recorded PID was reused by another process and was left untouched".into(),
        ]);
    }

    if !signal_recording_process(managed, "INT")? {
        return Ok(vec![
            "The managed recording process ended or changed identity before SIGINT and was left untouched".into(),
        ]);
    }
    if wait_for_recording_stop(managed, RECORDING_STOP_ATTEMPTS)? {
        return Ok(Vec::new());
    }

    if !signal_recording_process(managed, "TERM")? {
        return Ok(vec![
            "The managed recording process ended or changed identity before SIGTERM and was left untouched".into(),
        ]);
    }
    if wait_for_recording_stop(managed, RECORDING_FORCE_STOP_ATTEMPTS)? {
        return Ok(vec![
            "Android screenrecord did not finish after SIGINT and required SIGTERM".into(),
        ]);
    }
    if !signal_recording_process(managed, "KILL")? {
        return Ok(vec![
            "The managed recording process ended or changed identity before SIGKILL and was left untouched".into(),
        ]);
    }
    if !wait_for_recording_stop(managed, 5)? {
        return Err(ApiError::new(
            "recording_stop_failed",
            "Android screenrecord remained alive after the managed stop sequence",
        ));
    }
    Ok(vec![
        "Android screenrecord required SIGKILL; the recording may be incomplete".into(),
    ])
}

fn finalize_recording(
    managed: &ManagedAndroidScreenRecording,
) -> Result<AndroidScreenRecordingResult, ApiError> {
    let mut warnings = managed.warnings.clone();
    warnings.extend(stop_recording_process(managed)?);
    let recording_duration = managed.started_at.elapsed();
    thread::sleep(Duration::from_millis(250));
    if managed.use_su {
        run_recording_shell(
            &managed.serial,
            &format!(
                "chmod 0644 {}",
                validation::quote_remote(&managed.remote_path)
            ),
            true,
        )?;
    }
    let remote_size = query_recording_file_size(managed).ok();
    let pull_timeout = recording_pull_timeout(remote_size, recording_duration);
    if let Err(error) = pull_remote_capture_with_timeout(
        &managed.serial,
        &managed.remote_path,
        &managed.local_path,
        pull_timeout,
    ) {
        let _ = fs::remove_file(&managed.local_path);
        return Err(error);
    }
    let size_bytes = match inspect_mp4(&managed.local_path) {
        Ok(size) => size,
        Err(error) => {
            let _ = fs::remove_file(&managed.local_path);
            return Err(error);
        }
    };
    if remote_size.is_some_and(|expected| expected != size_bytes) {
        let _ = fs::remove_file(&managed.local_path);
        return Err(ApiError::new(
            "incomplete_recording_transfer",
            "The saved recording size does not match the finalized file on the Android device",
        )
        .with_details(serde_json::json!({
            "expectedBytes": remote_size,
            "receivedBytes": size_bytes,
        })));
    }
    if let Some(warning) =
        cleanup_remote_capture(&managed.serial, &managed.remote_path, managed.use_su)
    {
        warnings.push(warning);
    }
    if let Some(warning) =
        cleanup_remote_capture(&managed.serial, &managed.remote_log_path, managed.use_su)
    {
        warnings.push(warning);
    }
    if let Some(warning) = managed
        .screen_was_woken
        .then(|| restore_display_sleep(&managed.serial))
        .flatten()
    {
        warnings.push(warning);
    }
    Ok(AndroidScreenRecordingResult {
        success: true,
        message: "Screen recording stopped and saved to the PC".into(),
        saved_path: managed.local_path.to_string_lossy().into_owned(),
        size_bytes,
        duration_seconds: recording_duration.as_secs(),
        warnings,
    })
}

fn query_recording_file_size(managed: &ManagedAndroidScreenRecording) -> Result<u64, ApiError> {
    let output = run_recording_shell(
        &managed.serial,
        &format!("wc -c < {}", validation::quote_remote(&managed.remote_path)),
        managed.use_su,
    )?;
    output.stdout.trim().parse::<u64>().map_err(|_| {
        ApiError::new(
            "recording_size_error",
            "Android returned an invalid finalized recording size",
        )
    })
}

fn recording_pull_timeout(remote_size: Option<u64>, recording_duration: Duration) -> Duration {
    let estimated_seconds = match remote_size {
        Some(size) => {
            size.saturating_add(RECORDING_PULL_BYTES_PER_SECOND - 1)
                / RECORDING_PULL_BYTES_PER_SECOND
        }
        None => recording_duration.as_secs().saturating_mul(2),
    }
    .saturating_add(RECORDING_PULL_GRACE_SECONDS)
    .max(DEFAULT_PULL_TIMEOUT.as_secs());
    Duration::from_secs(estimated_seconds)
}

fn rollback_recording_start(
    serial: &str,
    remote_path: &str,
    remote_log_path: &str,
    local_path: &Path,
    use_su: bool,
    screen_was_woken: bool,
) {
    if let Some(warning) = cleanup_remote_capture(serial, remote_path, use_su) {
        eprintln!("Mobius recording rollback: {warning}");
    }
    if let Some(warning) = cleanup_remote_capture(serial, remote_log_path, use_su) {
        eprintln!("Mobius recording rollback: {warning}");
    }
    let _ = fs::remove_file(local_path);
    if screen_was_woken {
        if let Some(warning) = restore_display_sleep(serial) {
            eprintln!("Mobius display restore: {warning}");
        }
    }
}

pub(crate) fn cleanup_managed_screen_recordings(state: &AppState) {
    let _lifecycle_guard = match state.screen_recording_lifecycle_lock.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    let recordings = match state.screen_recordings.lock() {
        Ok(mut recordings) => recordings
            .drain()
            .map(|(_, recording)| recording)
            .collect::<Vec<_>>(),
        Err(_) => return,
    };
    for managed in recordings {
        if let Err(error) = finalize_recording(&managed) {
            eprintln!(
                "Mobius cleanup: recording {} on {} could not be finalized: {}",
                managed.session_id, managed.serial, error.message
            );
            let process_stopped =
                query_recording_process_identity(&managed.serial, managed.pid, managed.use_su)
                    .map(|identity| !recording_process_matches(identity.as_ref(), &managed))
                    .unwrap_or(false);
            if process_stopped {
                let _ =
                    cleanup_remote_capture(&managed.serial, &managed.remote_path, managed.use_su);
                let _ = cleanup_remote_capture(
                    &managed.serial,
                    &managed.remote_log_path,
                    managed.use_su,
                );
            }
            let _ = fs::remove_file(&managed.local_path);
            if managed.screen_was_woken {
                let _ = restore_display_sleep(&managed.serial);
            }
        }
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn wake_sleeping_display(serial: &str) -> Result<bool, ApiError> {
    let output = run_checked(
        "adb",
        &[
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            "dumpsys".into(),
            "power".into(),
        ],
        Duration::from_secs(10),
    )?;
    let sleeping = output.stdout.lines().any(|line| {
        let line = line.trim();
        line == "mWakefulness=Asleep" || line == "mWakefulness=Dozing"
    });
    if sleeping {
        run_checked(
            "adb",
            &[
                "-s".into(),
                serial.to_string(),
                "shell".into(),
                "input".into(),
                "keyevent".into(),
                "224".into(),
            ],
            Duration::from_secs(5),
        )?;
        thread::sleep(Duration::from_millis(300));
    }
    Ok(sleeping)
}

fn restore_display_sleep(serial: &str) -> Option<String> {
    match run_process(
        "adb",
        &[
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            "input".into(),
            "keyevent".into(),
            "223".into(),
        ],
        Duration::from_secs(5),
        &[],
    ) {
        Ok(output) if output.exit_code == Some(0) && !output.timed_out => None,
        Ok(output) => Some(format!(
            "Could not restore the device display sleep state (status {:?}, timedOut={})",
            output.exit_code, output.timed_out
        )),
        Err(error) => Some(format!(
            "Could not restore the device display sleep state: {}",
            error.message
        )),
    }
}

fn pull_remote_capture(serial: &str, remote_path: &str, local_path: &Path) -> Result<(), ApiError> {
    pull_remote_capture_with_timeout(serial, remote_path, local_path, DEFAULT_PULL_TIMEOUT)
}

fn pull_remote_capture_with_timeout(
    serial: &str,
    remote_path: &str,
    local_path: &Path,
    timeout: Duration,
) -> Result<(), ApiError> {
    run_checked(
        "adb",
        &[
            "-s".into(),
            serial.to_string(),
            "pull".into(),
            remote_path.to_string(),
            local_path.to_string_lossy().into_owned(),
        ],
        timeout,
    )?;
    Ok(())
}

fn cleanup_remote_capture(serial: &str, remote_path: &str, use_root: bool) -> Option<String> {
    let args = if use_root {
        vec![
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            "su".into(),
            "-c".into(),
            format!("rm -f -- {}", validation::quote_remote(remote_path)),
        ]
    } else {
        vec![
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            "rm".into(),
            "-f".into(),
            "--".into(),
            remote_path.to_string(),
        ]
    };
    match run_process("adb", &args, CLEANUP_TIMEOUT, &[]) {
        Ok(output) if output.exit_code == Some(0) && !output.timed_out => None,
        Ok(output) => Some(format!(
            "Could not remove temporary device media (status {:?}, timedOut={})",
            output.exit_code, output.timed_out
        )),
        Err(error) => Some(format!(
            "Could not remove temporary device media: {}",
            error.message
        )),
    }
}

fn permission_denied(error: &ApiError) -> bool {
    let details = error
        .details
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_default();
    format!("{} {details}", error.message)
        .to_ascii_lowercase()
        .contains("permission denied")
}

fn recording_start_allows_root_retry(error: &ApiError) -> bool {
    permission_denied(error)
        || (error.code == "process_failed"
            && error
                .details
                .as_ref()
                .and_then(|details| details.get("exitCode"))
                .and_then(serde_json::Value::as_i64)
                == Some(75))
}

fn require_noninteractive_root(serial: &str) -> Result<(), ApiError> {
    let output = run_checked(
        "adb",
        &[
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            "su".into(),
            "-c".into(),
            "id".into(),
        ],
        Duration::from_secs(5),
    )?;
    if !output
        .stdout
        .split_whitespace()
        .any(|part| part == "uid=0(root)")
    {
        return Err(ApiError::new(
            "root_recording_unavailable",
            "The device did not grant non-interactive Root access for recording",
        ));
    }
    Ok(())
}

fn decode_png(path: &Path) -> Result<tauri::image::Image<'static>, ApiError> {
    let bytes = fs::read(path).map_err(|error| {
        ApiError::new(
            "capture_read_error",
            format!("Unable to read the captured screenshot: {error}"),
        )
    })?;
    tauri::image::Image::from_bytes(&bytes)
        .map(tauri::image::Image::to_owned)
        .map_err(|error| {
            ApiError::new(
                "invalid_screenshot_image",
                format!("Unable to decode the device screenshot: {error}"),
            )
        })
}

fn copy_image_to_clipboard(
    app: &AppHandle,
    image: &tauri::image::Image<'_>,
) -> Result<(), ApiError> {
    app.clipboard().write_image(image).map_err(|error| {
        ApiError::new(
            "clipboard_write_failed",
            format!("Unable to copy the screenshot to the system clipboard: {error}"),
        )
    })
}

fn inspect_png(path: &Path) -> Result<(u32, u32, u64), ApiError> {
    let metadata = path.metadata().map_err(|error| {
        ApiError::new(
            "capture_read_error",
            format!("Unable to inspect the captured screenshot: {error}"),
        )
    })?;
    if metadata.len() == 0 || metadata.len() > MAX_SCREENSHOT_BYTES {
        return Err(ApiError::new(
            "invalid_screenshot_image",
            "Device screenshot is empty or exceeds the 64 MiB safety limit",
        ));
    }
    let mut file = fs::File::open(path).map_err(|error| {
        ApiError::new(
            "capture_read_error",
            format!("Unable to open the captured screenshot: {error}"),
        )
    })?;
    let mut header = [0_u8; 24];
    file.read_exact(&mut header).map_err(|_| {
        ApiError::new(
            "invalid_screenshot_image",
            "Device screenshot does not contain a complete PNG header",
        )
    })?;
    if header[..8] != [137, 80, 78, 71, 13, 10, 26, 10] || &header[12..16] != b"IHDR" {
        return Err(ApiError::new(
            "invalid_screenshot_image",
            "Device screenshot is not a valid PNG image",
        ));
    }
    let width = u32::from_be_bytes(header[16..20].try_into().unwrap_or_default());
    let height = u32::from_be_bytes(header[20..24].try_into().unwrap_or_default());
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_SCREENSHOT_DIMENSION
        || height > MAX_SCREENSHOT_DIMENSION
        || pixels > MAX_SCREENSHOT_PIXELS
    {
        return Err(ApiError::new(
            "invalid_screenshot_dimensions",
            "Device screenshot dimensions exceed the supported safety limits",
        ));
    }
    Ok((width, height, metadata.len()))
}

fn inspect_mp4(path: &Path) -> Result<u64, ApiError> {
    let metadata = path.metadata().map_err(|error| {
        ApiError::new(
            "capture_read_error",
            format!("Unable to inspect the captured recording: {error}"),
        )
    })?;
    if metadata.len() < 24 {
        return Err(ApiError::new(
            "invalid_screen_recording",
            "Device screen recording is empty or incomplete",
        ));
    }
    let mut file = fs::File::open(path).map_err(|error| {
        ApiError::new(
            "capture_read_error",
            format!("Unable to open the captured recording: {error}"),
        )
    })?;
    let file_size = metadata.len();
    let mut position = 0_u64;
    let mut box_count = 0_usize;
    let mut has_ftyp = false;
    let mut has_moov = false;
    let mut has_media_data = false;
    while position < file_size {
        box_count += 1;
        if box_count > MAX_MP4_TOP_LEVEL_BOXES || file_size - position < 8 {
            return Err(ApiError::new(
                "invalid_screen_recording",
                "Device screen recording contains an incomplete MP4 box table",
            ));
        }
        file.seek(SeekFrom::Start(position)).map_err(|error| {
            ApiError::new(
                "capture_read_error",
                format!("Unable to seek within the captured recording: {error}"),
            )
        })?;
        let mut header = [0_u8; 8];
        file.read_exact(&mut header).map_err(|error| {
            ApiError::new(
                "capture_read_error",
                format!("Unable to read the captured recording: {error}"),
            )
        })?;
        let short_size = u32::from_be_bytes(header[..4].try_into().unwrap_or_default());
        let mut header_size = 8_u64;
        let box_size = match short_size {
            0 => file_size - position,
            1 => {
                if file_size - position < 16 {
                    return Err(ApiError::new(
                        "invalid_screen_recording",
                        "Device screen recording contains an incomplete extended MP4 box",
                    ));
                }
                let mut extended = [0_u8; 8];
                file.read_exact(&mut extended).map_err(|error| {
                    ApiError::new(
                        "capture_read_error",
                        format!("Unable to read the captured recording: {error}"),
                    )
                })?;
                header_size = 16;
                u64::from_be_bytes(extended)
            }
            value => u64::from(value),
        };
        let next = position
            .checked_add(box_size)
            .filter(|next| *next <= file_size);
        if box_size < header_size || next.is_none() {
            return Err(ApiError::new(
                "invalid_screen_recording",
                "Device screen recording contains a truncated or malformed MP4 box",
            ));
        }
        let payload_size = box_size - header_size;
        match &header[4..8] {
            b"ftyp" if payload_size >= 4 => has_ftyp = true,
            b"moov" if payload_size > 0 => has_moov = true,
            b"mdat" if payload_size > 0 => has_media_data = true,
            _ => {}
        }
        position = next.unwrap_or(file_size);
    }
    if !has_ftyp || !has_moov || !has_media_data {
        return Err(ApiError::new(
            "invalid_screen_recording",
            "Device screen recording is missing required MP4 metadata or media data",
        ));
    }
    Ok(file_size)
}

struct CaptureTarget {
    path: PathBuf,
    temporary_directory: Option<PathBuf>,
    persistent: bool,
    complete: bool,
}

impl CaptureTarget {
    fn prepare(
        destination_directory: Option<&str>,
        prefix: &str,
        extension: &str,
    ) -> Result<Self, ApiError> {
        let (directory, temporary_directory, persistent) =
            if let Some(value) = destination_directory {
                let directory = validated_destination_directory(value)?;
                (directory, None, true)
            } else {
                let directory = create_private_temp_directory()?;
                (directory.clone(), Some(directory), false)
            };
        let path = reserve_unique_file(&directory, prefix, extension)?;
        Ok(Self {
            path,
            temporary_directory,
            persistent,
            complete: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn is_persistent(&self) -> bool {
        self.persistent
    }

    fn persistent_path(&self) -> Option<String> {
        self.persistent
            .then(|| self.path.to_string_lossy().into_owned())
    }

    fn mark_complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for CaptureTarget {
    fn drop(&mut self) {
        if !self.persistent || !self.complete {
            let _ = fs::remove_file(&self.path);
        }
        if let Some(directory) = &self.temporary_directory {
            let _ = fs::remove_dir(directory);
        }
    }
}

fn validated_destination_directory(value: &str) -> Result<PathBuf, ApiError> {
    let path = validation::local_absolute_path(value)?;
    if !path.exists() || !path.is_dir() {
        return Err(ApiError::new(
            "local_directory_not_found",
            format!("PC save directory does not exist: {}", path.display()),
        ));
    }
    // Keep the user's absolute spelling. On Windows, canonicalization adds a `\\?\`
    // prefix that some platform-tools releases do not accept as an adb pull target.
    Ok(path.to_path_buf())
}

fn create_private_temp_directory() -> Result<PathBuf, ApiError> {
    let base = std::env::temp_dir();
    if !base.is_absolute() {
        return Err(ApiError::new(
            "invalid_temp_directory",
            "System temporary directory is not absolute",
        ));
    }
    for _ in 0..32 {
        let candidate = base.join(format!("mobius-capture-{}", unique_token()));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700)).map_err(
                        |error| {
                            let _ = fs::remove_dir(&candidate);
                            ApiError::new(
                                "temp_directory_error",
                                format!("Unable to secure temporary capture directory: {error}"),
                            )
                        },
                    )?;
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ApiError::new(
                    "temp_directory_error",
                    format!("Unable to create temporary capture directory: {error}"),
                ))
            }
        }
    }
    Err(ApiError::new(
        "temp_directory_error",
        "Unable to allocate a unique temporary capture directory",
    ))
}

fn reserve_unique_file(
    directory: &Path,
    prefix: &str,
    extension: &str,
) -> Result<PathBuf, ApiError> {
    for _ in 0..32 {
        let path = directory.join(format!("{prefix}-{}.{}", unique_token(), extension));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ApiError::new(
                    "capture_file_error",
                    format!("Unable to reserve PC capture file: {error}"),
                ))
            }
        }
    }
    Err(ApiError::new(
        "capture_file_error",
        "Unable to reserve a unique PC capture file",
    ))
}

fn remote_capture_path(kind: &str, extension: &str) -> String {
    format!(
        "/data/local/tmp/mobius-{kind}-{}.{}",
        unique_token(),
        extension
    )
}

fn unique_token() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp}-{sequence}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_inspection_accepts_bounded_ihdr() {
        let directory = create_private_temp_directory().expect("temp directory");
        let path = directory.join("image.png");
        let mut bytes = vec![0_u8; 24];
        bytes[..8].copy_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&1080_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&2400_u32.to_be_bytes());
        fs::write(&path, bytes).expect("write png header");
        assert_eq!(inspect_png(&path).expect("valid header").0, 1080);
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(directory);
    }

    #[test]
    fn png_inspection_rejects_excessive_dimensions() {
        let directory = create_private_temp_directory().expect("temp directory");
        let path = directory.join("image.png");
        let mut bytes = vec![0_u8; 24];
        bytes[..8].copy_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&20_000_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&20_000_u32.to_be_bytes());
        fs::write(&path, bytes).expect("write png header");
        assert!(inspect_png(&path).is_err());
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(directory);
    }

    #[test]
    fn generated_remote_paths_stay_within_the_fixed_temp_root() {
        let path = remote_capture_path("shot", "png");
        assert!(path.starts_with("/data/local/tmp/mobius-shot-"));
        assert!(path.ends_with(".png"));
        assert!(validation::remote_path(&path).is_ok());
    }

    #[test]
    fn ios_screen_target_matching_is_exact() {
        let listed = "00008020-001C2D1234567890\n00008030-ABCDEF\n";
        assert!(output_has_exact_identifier(
            listed,
            "00008020-001C2D1234567890"
        ));
        assert!(!output_has_exact_identifier(listed, "00008020"));
    }

    #[test]
    fn ssh_only_ios_endpoint_is_rejected_before_tool_invocation() {
        let error = detect_ios_screen_transport("ios-ssh:192.168.1.42:22")
            .expect_err("manual SSH endpoint must not reach screenshot tools");
        assert_eq!(error.code, "ios_screen_target_unavailable");
    }

    #[test]
    fn recording_launch_command_is_unlimited_and_uses_quoted_fixed_paths() {
        let command = recording_launch_command(
            "/data/local/tmp/mobius-recording-123.mp4",
            "/data/local/tmp/mobius-recording-log-123.txt",
            Some(8_000_000),
        );
        assert!(command.contains("screenrecord --time-limit 0 --bit-rate 8000000"));
        assert!(command.contains("'/data/local/tmp/mobius-recording-123.mp4'"));
        assert!(command.contains("'/data/local/tmp/mobius-recording-log-123.txt'"));
        assert!(command.contains(RECORDING_PID_MARKER));
        assert!(command.contains(RECORDING_EXE_MARKER));
        assert!(command.contains(RECORDING_STAT_MARKER));
        assert!(command.contains("mobius_abort"));
        assert!(!command.contains("--time-limit 20"));
    }

    #[test]
    fn recording_pid_parser_requires_the_private_marker() {
        assert_eq!(
            parse_recording_pid("noise\n__MOBIUS_RECORDING_PID__=4242\n").expect("marked pid"),
            4242
        );
        assert!(parse_recording_pid("4242\n").is_err());
        assert!(parse_recording_pid("__MOBIUS_RECORDING_PID__=1\n").is_err());
    }

    #[test]
    fn recording_identity_is_parsed_from_the_launch_response() {
        let stdout = concat!(
            "__MOBIUS_RECORDING_PID__=4242\n",
            "__MOBIUS_RECORDING_EXE__=/system/bin/screenrecord\n",
            "__MOBIUS_RECORDING_STAT__=42 (screen record worker) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242 20\n",
        );
        let identity = parse_recording_identity(stdout).expect("launch identity");
        assert_eq!(identity.executable, "/system/bin/screenrecord");
        assert_eq!(identity.start_time, "424242");
        assert!(parse_recording_identity("__MOBIUS_RECORDING_PID__=4242\n").is_err());
    }

    #[test]
    fn recording_signal_rechecks_identity_in_the_same_remote_command() {
        let command = recording_signal_command(4242, "/system/bin/screenrecord", "987654", "15");
        assert!(command.contains("readlink -f /proc/4242/exe"));
        assert!(command.contains("cat /proc/4242/stat"));
        assert!(command.contains("'/system/bin/screenrecord'"));
        assert!(command.contains("'987654'"));
        assert!(command.contains("kill -15 4242"));
        assert!(command.contains(RECORDING_SIGNALLED_MARKER));
        assert!(command.contains(RECORDING_SIGNAL_SKIPPED_MARKER));
    }

    #[test]
    fn recording_pull_timeout_scales_beyond_the_screenshot_limit() {
        assert_eq!(
            recording_pull_timeout(Some(1), Duration::from_secs(1)),
            DEFAULT_PULL_TIMEOUT
        );
        assert!(
            recording_pull_timeout(Some(2 * 1024 * 1024 * 1024), Duration::from_secs(60))
                > DEFAULT_PULL_TIMEOUT
        );
        assert!(
            recording_pull_timeout(None, Duration::from_secs(3_600)) >= Duration::from_secs(7_320)
        );
    }

    fn append_mp4_box(target: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
        let size = u32::try_from(8 + payload.len()).expect("small test atom");
        target.extend_from_slice(&size.to_be_bytes());
        target.extend_from_slice(kind);
        target.extend_from_slice(payload);
    }

    #[test]
    fn mp4_inspection_requires_complete_metadata_and_media_boxes() {
        let directory = create_private_temp_directory().expect("temp directory");
        let path = directory.join("recording.mp4");
        let mut complete = Vec::new();
        append_mp4_box(&mut complete, b"ftyp", b"isom");
        append_mp4_box(&mut complete, b"mdat", b"frame");
        append_mp4_box(&mut complete, b"moov", b"index");
        fs::write(&path, &complete).expect("write complete mp4");
        assert_eq!(
            inspect_mp4(&path).expect("complete mp4"),
            complete.len() as u64
        );

        let mut missing_moov = Vec::new();
        append_mp4_box(&mut missing_moov, b"ftyp", b"isom");
        append_mp4_box(&mut missing_moov, b"mdat", b"frame");
        fs::write(&path, missing_moov).expect("write mp4 without moov");
        assert!(inspect_mp4(&path).is_err());

        let mut truncated = Vec::new();
        append_mp4_box(&mut truncated, b"ftyp", b"isom");
        truncated.extend_from_slice(&100_u32.to_be_bytes());
        truncated.extend_from_slice(b"mdat");
        truncated.extend_from_slice(b"short");
        fs::write(&path, truncated).expect("write truncated mp4");
        assert!(inspect_mp4(&path).is_err());
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir(directory);
    }

    #[test]
    fn process_start_time_parser_handles_spaces_in_process_name() {
        let stat =
            "42 (screen record worker) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242 20";
        assert_eq!(
            parse_process_start_time(stat).expect("valid proc stat"),
            "424242"
        );
        assert!(parse_process_start_time("42 malformed").is_err());
    }

    #[test]
    fn permission_error_detection_checks_process_details() {
        let error = ApiError::new("process_failed", "adb failed")
            .with_details(serde_json::json!({ "stderr": "Permission denied" }));
        assert!(permission_denied(&error));
        assert!(recording_start_allows_root_retry(&error));

        let detached_failure = ApiError::new("process_failed", "adb failed")
            .with_details(serde_json::json!({ "exitCode": 75 }));
        assert!(recording_start_allows_root_retry(&detached_failure));
        assert!(!recording_start_allows_root_retry(&ApiError::new(
            "process_failed",
            "unrelated failure"
        )));
    }

    #[test]
    #[ignore = "requires an explicitly authorized live rooted Android device"]
    fn live_android_inline_screen_frame_round_trip() {
        let serial = std::env::var("MOBIUS_LIVE_ANDROID_SERIAL")
            .expect("set MOBIUS_LIVE_ANDROID_SERIAL to the authorized device serial");
        let frame = capture_inline_frame(&serial).expect("capture a bounded inline frame");
        assert!(frame.image_data_url.starts_with("data:image/png;base64,"));
        assert!(frame.size_bytes >= 24);
        assert!(frame.width > 0 && frame.height > 0);
    }

    #[test]
    #[ignore = "requires an explicitly authorized live rooted Android device"]
    fn live_managed_recording_start_stop_round_trip() {
        let serial = std::env::var("MOBIUS_LIVE_ANDROID_SERIAL")
            .expect("set MOBIUS_LIVE_ANDROID_SERIAL to the authorized device serial");
        let directory = create_private_temp_directory().expect("create private QA directory");
        let state = AppState::default();
        let start = start_screen_recording(
            &state,
            StartAndroidScreenRecordingRequest {
                serial: serial.clone(),
                destination_directory: directory.to_string_lossy().into_owned(),
                bit_rate: Some(4_000_000),
                allow_root_fallback: true,
            },
        )
        .expect("start managed recording");
        thread::sleep(Duration::from_secs(2));
        let request = StopAndroidScreenRecordingRequest {
            serial,
            session_id: start.session_id,
        };
        match stop_screen_recording(&state, request) {
            Ok(result) => {
                assert!(result.size_bytes >= 12);
                assert!(result.duration_seconds >= 2);
                let _ = fs::remove_file(result.saved_path);
                let _ = fs::remove_dir(directory);
            }
            Err(error) => {
                let _ = fs::remove_dir(directory);
                panic!("live recording failed: {} ({})", error.message, error.code);
            }
        }
    }
}
