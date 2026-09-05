use super::blocking_api;
use crate::{
    models::{
        AndroidScreenStreamRequest, AndroidScreenStreamResult, ApiError, ApiResult, AppResult,
        OperationResult, ScrcpyRequest, StopAndroidScreenStreamRequest,
    },
    runner::{
        background_command, clear_ambient_adb_server_environment, resolve_tool, run_process_at,
        run_process_at_with_env,
    },
    state::{AppState, ManagedAndroidScreenStream},
    validation,
};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::State;

const STREAM_SETUP_TIMEOUT: Duration = Duration::from_secs(12);
const STREAM_PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CLIENT_TIMEOUT: Duration = Duration::from_secs(15);
const STREAM_IO_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_REMOTE_PREFIX: &str = "/data/local/tmp/mobius-display-";
const MAX_STREAM_SERVER_BYTES: u64 = 32 * 1024 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 8 * 1024;
const DEFAULT_STREAM_MAX_SIZE: u16 = 1024;
const DEFAULT_STREAM_BIT_RATE: u32 = 4_000_000;
const DEFAULT_STREAM_MAX_FPS: u8 = 15;

#[tauri::command]
pub async fn launch_scrcpy(request: ScrcpyRequest) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::serial(&request.serial)?;
        let mut args = vec!["--serial".to_string(), request.serial];
        if let Some(max_size) = request.max_size {
            // A zero value in the UI means "use the device's original resolution".
            if max_size != 0 {
                if (320..=8192).contains(&max_size) {
                    args.push("--max-size".into());
                    args.push(max_size.to_string());
                } else {
                    return Err(ApiError::new(
                        "invalid_max_size",
                        "scrcpy maxSize must be zero or between 320 and 8192",
                    ));
                }
            }
        }
        if let Some(bit_rate) = request.bit_rate {
            validate_bit_rate(&bit_rate)?;
            args.push("--video-bit-rate".into());
            args.push(bit_rate);
        }
        if request.turn_screen_off {
            args.push("--turn-screen-off".into());
        }
        if request.stay_awake {
            args.push("--stay-awake".into());
        }
        if request.no_audio {
            args.push("--no-audio".into());
        }

        let executable = resolve_tool("scrcpy")?;
        let adb = resolve_tool("adb")?;
        let (_, server) = resolve_matching_scrcpy_server(&executable, &adb)?;
        let mut child = scrcpy_client_command(&executable, &adb, &server)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                let code = if error.kind() == std::io::ErrorKind::NotFound {
                    "tool_not_found"
                } else {
                    "process_spawn_error"
                };
                ApiError::new(code, format!("Unable to start scrcpy: {error}"))
            })?;
        let pid = child.id();
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ApiError::new("process_io_error", "Unable to capture scrcpy stderr"))?;
        let (stderr_sender, stderr_receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = stderr;
            let mut captured = Vec::new();
            let mut buffer = [0_u8; 4096];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let remaining = (64 * 1024_usize).saturating_sub(captured.len());
                captured.extend_from_slice(&buffer[..remaining.min(count)]);
            }
            let _ = stderr_sender.send(String::from_utf8_lossy(&captured).trim().to_string());
        });
        thread::sleep(Duration::from_millis(350));
        if let Some(status) = child.try_wait().map_err(|error| {
            ApiError::new(
                "process_wait_error",
                format!("Unable to inspect scrcpy startup: {error}"),
            )
        })? {
            let stderr = stderr_receiver
                .recv_timeout(Duration::from_millis(500))
                .unwrap_or_default();
            return Err(ApiError::new(
                "scrcpy_start_failed",
                if stderr.is_empty() {
                    format!("scrcpy exited immediately with status {status}")
                } else {
                    stderr
                },
            ));
        }
        // Reap the GUI process when it exits, without blocking the IPC handler.
        thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(OperationResult {
            success: true,
            message: "scrcpy launched".into(),
            stdout: None,
            stderr: None,
            pid: Some(pid),
            exit_code: None,
            timed_out: false,
        })
    })
    .await
}

#[tauri::command]
pub async fn start_android_screen_stream(
    request: AndroidScreenStreamRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<AndroidScreenStreamResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        let result = start_screen_stream(request, &state);
        if let Err(error) = &result {
            eprintln!(
                "Mobius embedded stream failed to start: {} ({})",
                error.message, error.code
            );
        }
        result
    })
    .await)
}

#[tauri::command]
pub async fn stop_android_screen_stream(
    request: StopAndroidScreenStreamRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || stop_screen_stream(&request, &state)).await)
}

fn start_screen_stream(
    request: AndroidScreenStreamRequest,
    state: &AppState,
) -> AppResult<AndroidScreenStreamResult> {
    validation::serial(&request.serial)?;
    let _lifecycle_guard = state.screen_stream_lifecycle_lock.lock().map_err(|_| {
        ApiError::new(
            "stream_state_error",
            "Screen stream lifecycle state is unavailable",
        )
    })?;
    if state.shutting_down.load(Ordering::Acquire) {
        return Err(ApiError::new(
            "application_shutting_down",
            "The application is closing and cannot start a screen stream",
        ));
    }
    let max_size = request.max_size.unwrap_or(DEFAULT_STREAM_MAX_SIZE);
    if !(320..=1920).contains(&max_size) {
        return Err(ApiError::new(
            "invalid_stream_max_size",
            "Embedded stream maxSize must be between 320 and 1920",
        ));
    }
    let bit_rate = request.bit_rate.unwrap_or(DEFAULT_STREAM_BIT_RATE);
    if !(250_000..=20_000_000).contains(&bit_rate) {
        return Err(ApiError::new(
            "invalid_stream_bit_rate",
            "Embedded stream bitRate must be between 250 Kbit/s and 20 Mbit/s",
        ));
    }
    let max_fps = request.max_fps.unwrap_or(DEFAULT_STREAM_MAX_FPS);
    if !(5..=30).contains(&max_fps) {
        return Err(ApiError::new(
            "invalid_stream_frame_rate",
            "Embedded stream maxFps must be between 5 and 30",
        ));
    }

    if let Some(previous) = remove_stream_for_serial(state, &request.serial)? {
        stop_managed_stream(&previous);
    }

    let adb = resolve_tool("adb")?;
    let scrcpy = resolve_tool("scrcpy")?;
    let ffmpeg = resolve_tool("ffmpeg").map_err(|_| {
        ApiError::new(
            "embedded_stream_transcoder_missing",
            "Embedded live video needs ffmpeg. Put ffmpeg in the managed tools directory or install it on this computer.",
        )
    })?;
    let (server_version, server_path) = resolve_matching_scrcpy_server(&scrcpy, &adb)?;

    let state_output = run_process_at(
        "adb",
        &adb,
        &["-s".into(), request.serial.clone(), "get-state".into()],
        STREAM_PROCESS_TIMEOUT,
        &[],
    )?;
    if state_output.timed_out
        || state_output.exit_code != Some(0)
        || state_output.stdout.trim() != "device"
    {
        return Err(ApiError::new(
            "device_not_online",
            "The selected Android device is not online",
        ));
    }

    let random = random_bytes()?;
    let scid_number = u32::from_be_bytes(random[0..4].try_into().unwrap()) & 0x7fff_ffff;
    let scid = format!("{scid_number:08x}");
    let session_id = hex_bytes(&random);
    let token = hex_bytes(&random_bytes()?);
    let remote_path = format!("{STREAM_REMOTE_PREFIX}{scid}.jar");
    let socket_name = format!("localabstract:scrcpy_{scid}");

    let input_listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| {
        ApiError::new(
            "stream_listener_error",
            format!("Unable to reserve a loopback video listener: {error}"),
        )
    })?;
    input_listener.set_nonblocking(true).map_err(|error| {
        ApiError::new(
            "stream_listener_error",
            format!("Unable to configure the loopback video listener: {error}"),
        )
    })?;
    let input_port = input_listener
        .local_addr()
        .map_err(|error| ApiError::new("stream_listener_error", error.to_string()))?
        .port();
    let http_listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| {
        ApiError::new(
            "stream_listener_error",
            format!("Unable to reserve the embedded-view listener: {error}"),
        )
    })?;
    http_listener.set_nonblocking(true).map_err(|error| {
        ApiError::new(
            "stream_listener_error",
            format!("Unable to configure the embedded-view listener: {error}"),
        )
    })?;
    let http_port = http_listener
        .local_addr()
        .map_err(|error| ApiError::new("stream_listener_error", error.to_string()))?
        .port();

    let push_result = run_process_at(
        "adb",
        &adb,
        &[
            "-s".into(),
            request.serial.clone(),
            "push".into(),
            server_path.to_string_lossy().into_owned(),
            remote_path.clone(),
        ],
        STREAM_SETUP_TIMEOUT,
        &[],
    )
    .and_then(require_success);
    if let Err(error) = push_result {
        cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
        return Err(ApiError::new(
            "stream_server_upload_failed",
            format!(
                "Unable to prepare the matching scrcpy server: {}",
                error.message
            ),
        ));
    }

    let mapping_result = run_process_at(
        "adb",
        &adb,
        &[
            "-s".into(),
            request.serial.clone(),
            "reverse".into(),
            socket_name.clone(),
            format!("tcp:{input_port}"),
        ],
        STREAM_PROCESS_TIMEOUT,
        &[],
    )
    .and_then(require_success);
    if let Err(error) = mapping_result {
        cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
        return Err(ApiError::new(
            "stream_reverse_unavailable",
            format!(
                "Unable to create the private scrcpy reverse tunnel: {}",
                error.message
            ),
        ));
    }

    let mut server_command = background_command(&adb);
    clear_ambient_adb_server_environment(&mut server_command);
    let mut server = match server_command
        .args([
            "-s",
            request.serial.as_str(),
            "shell",
            &format!("CLASSPATH={remote_path}"),
            "app_process",
            "/",
            "com.genymobile.scrcpy.Server",
            server_version.as_str(),
            &format!("scid={scid}"),
            "log_level=warn",
            "audio=false",
            "control=false",
            "cleanup=true",
            "raw_stream=true",
            &format!("max_size={max_size}"),
            &format!("max_fps={max_fps}"),
            &format!("video_bit_rate={bit_rate}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
            return Err(ApiError::new(
                "stream_server_start_failed",
                format!("Unable to start the matching scrcpy server: {error}"),
            ));
        }
    };
    let server_diagnostics = server.stderr.take().map(capture_stderr);

    let accepted =
        wait_for_device_stream(&input_listener, &mut server, server_diagnostics.as_ref());
    let (device_stream, _) = match accepted {
        Ok(connection) => connection,
        Err(error) => {
            let _ = server.kill();
            let _ = server.wait();
            cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
            return Err(error);
        }
    };
    if let Err(error) = remove_stream_reverse(&adb, &request.serial, &scid) {
        let _ = server.kill();
        let _ = server.wait();
        cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
        return Err(ApiError::new(
            "stream_reverse_cleanup_failed",
            format!(
                "The private scrcpy reverse tunnel could not be released after connection: {}",
                error.message
            ),
        ));
    }
    if let Err(error) = device_stream.set_nonblocking(false) {
        let _ = server.kill();
        let _ = server.wait();
        cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
        return Err(ApiError::new(
            "stream_socket_error",
            format!("Unable to configure the device video socket: {error}"),
        ));
    }
    if let Err(error) = device_stream.set_read_timeout(Some(Duration::from_millis(500))) {
        let _ = server.kill();
        let _ = server.wait();
        cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
        return Err(ApiError::new("stream_socket_error", error.to_string()));
    }
    let state_socket = match device_stream.try_clone() {
        Ok(socket) => socket,
        Err(error) => {
            let _ = server.kill();
            let _ = server.wait();
            cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
            return Err(ApiError::new(
                "stream_socket_error",
                format!("Unable to manage the device video socket: {error}"),
            ));
        }
    };

    let mut transcoder = match background_command(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "warning",
            "-probesize",
            "32768",
            "-analyzeduration",
            "0",
            "-fpsprobesize",
            "0",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            "format=yuvj420p",
            "-fps_mode",
            "passthrough",
            "-c:v",
            "mjpeg",
            "-q:v",
            "6",
            "-flush_packets",
            "1",
            "-f",
            "mpjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = server.kill();
            let _ = server.wait();
            cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
            return Err(ApiError::new(
                "stream_transcoder_start_failed",
                format!("Unable to start the embedded video transcoder: {error}"),
            ));
        }
    };
    let (transcoder_input, transcoder_output) =
        match (transcoder.stdin.take(), transcoder.stdout.take()) {
            (Some(input), Some(output)) => (input, output),
            _ => {
                let _ = transcoder.kill();
                let _ = transcoder.wait();
                let _ = server.kill();
                let _ = server.wait();
                cleanup_stream_artifacts(&adb, &request.serial, &scid, input_port, &remote_path);
                return Err(ApiError::new(
                    "stream_transcoder_io_error",
                    "Unable to open the embedded video transcoder pipes",
                ));
            }
        };
    let transcoder_diagnostics = transcoder.stderr.take().map(capture_stderr);

    let stop = Arc::new(AtomicBool::new(false));
    let input_socket = Arc::new(Mutex::new(Some(state_socket)));
    let server_process = Arc::new(Mutex::new(Some(server)));
    let transcoder_process = Arc::new(Mutex::new(Some(transcoder)));
    let managed = ManagedAndroidScreenStream {
        session_id: session_id.clone(),
        serial: request.serial.clone(),
        scid: scid.clone(),
        remote_path: remote_path.clone(),
        adb_path: adb.clone(),
        stop: stop.clone(),
        input_socket: input_socket.clone(),
        server_process: server_process.clone(),
        transcoder_process: transcoder_process.clone(),
    };
    match state.screen_streams.lock() {
        Ok(mut streams) => {
            streams.insert(request.serial.clone(), managed.clone());
        }
        Err(_) => {
            stop_managed_stream(&managed);
            return Err(ApiError::new(
                "stream_state_error",
                "Screen stream state is unavailable",
            ));
        }
    }

    spawn_video_relay(
        device_stream,
        transcoder_input,
        stop.clone(),
        transcoder_process.clone(),
    );
    spawn_http_stream(
        http_listener,
        transcoder_output,
        session_id.clone(),
        token.clone(),
        managed,
        state.clone(),
        transcoder_diagnostics,
    );

    let (width, height) = probe_stream_dimensions(&adb, &request.serial, max_size);
    Ok(AndroidScreenStreamResult {
        success: true,
        message: "Embedded scrcpy video stream started".into(),
        session_id: session_id.clone(),
        stream_url: format!("http://127.0.0.1:{http_port}/screen/{session_id}?token={token}"),
        serial: request.serial,
        codec: "H.264 -> MJPEG".into(),
        transport: "adb-reverse-loopback".into(),
        max_size,
        max_fps,
        width,
        height,
    })
}

fn stop_screen_stream(
    request: &StopAndroidScreenStreamRequest,
    state: &AppState,
) -> AppResult<OperationResult> {
    validation::serial(&request.serial)?;
    if request.session_id.len() != 32
        || !request.session_id.chars().all(|ch| ch.is_ascii_hexdigit())
    {
        return Err(ApiError::new(
            "invalid_stream_session",
            "Screen stream session id is invalid",
        ));
    }
    let _lifecycle_guard = state.screen_stream_lifecycle_lock.lock().map_err(|_| {
        ApiError::new(
            "stream_state_error",
            "Screen stream lifecycle state is unavailable",
        )
    })?;
    let managed = {
        let mut streams = state.screen_streams.lock().map_err(|_| {
            ApiError::new("stream_state_error", "Screen stream state is unavailable")
        })?;
        match streams.get(&request.serial) {
            Some(stream) if stream.session_id == request.session_id => {
                streams.remove(&request.serial)
            }
            Some(_) => {
                return Err(ApiError::new(
                    "stream_session_mismatch",
                    "The requested stream session is no longer active for this device",
                ))
            }
            None => None,
        }
    };
    if let Some(managed) = managed {
        stop_managed_stream(&managed);
    }
    Ok(OperationResult {
        success: true,
        message: "Embedded screen stream stopped".into(),
        stdout: None,
        stderr: None,
        pid: None,
        exit_code: Some(0),
        timed_out: false,
    })
}

fn wait_for_device_stream(
    listener: &TcpListener,
    server: &mut Child,
    diagnostics: Option<&Arc<Mutex<String>>>,
) -> AppResult<(TcpStream, std::net::SocketAddr)> {
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok(connection) => return Ok(connection),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(ApiError::new(
                    "stream_accept_error",
                    format!("Unable to accept the private device video stream: {error}"),
                ))
            }
        }
        if let Some(status) = server.try_wait().map_err(|error| {
            ApiError::new(
                "stream_server_wait_error",
                format!("Unable to inspect the scrcpy server: {error}"),
            )
        })? {
            let detail = diagnostics
                .and_then(|value| value.lock().ok().map(|text| text.trim().to_string()))
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| format!("scrcpy server exited with {status}"));
            return Err(ApiError::new("stream_server_start_failed", detail));
        }
        if started.elapsed() >= STREAM_SETUP_TIMEOUT {
            return Err(ApiError::new(
                "stream_server_timeout",
                "The matching scrcpy server did not connect to its private loopback tunnel in time",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_video_relay(
    mut device_stream: TcpStream,
    mut transcoder_input: impl Write + Send + 'static,
    stop: Arc<AtomicBool>,
    transcoder_process: Arc<Mutex<Option<Child>>>,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 64 * 1024];
        while !stop.load(Ordering::Acquire) {
            match device_stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if transcoder_input.write_all(&buffer[..count]).is_err() {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
        stop.store(true, Ordering::Release);
        drop(transcoder_input);
        kill_child(&transcoder_process);
    });
}

fn spawn_http_stream(
    listener: TcpListener,
    transcoder_output: impl Read + Send + 'static,
    session_id: String,
    token: String,
    managed: ManagedAndroidScreenStream,
    state: AppState,
    transcoder_diagnostics: Option<Arc<Mutex<String>>>,
) {
    thread::spawn(move || {
        let result = serve_one_mjpeg_client(
            &listener,
            transcoder_output,
            &session_id,
            &token,
            &managed.stop,
        );
        managed.stop.store(true, Ordering::Release);
        stop_managed_stream(&managed);
        if let Ok(mut streams) = state.screen_streams.lock() {
            let should_remove = streams
                .get(&managed.serial)
                .is_some_and(|stream| stream.session_id == managed.session_id);
            if should_remove {
                streams.remove(&managed.serial);
            }
        }
        if let Err(error) = result {
            let transcoder_detail = transcoder_diagnostics
                .and_then(|value| value.lock().ok().map(|text| text.trim().to_string()))
                .filter(|text| !text.is_empty());
            eprintln!(
                "Mobius embedded stream ended: {}{}",
                error,
                transcoder_detail
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            );
        }
    });
}

fn serve_one_mjpeg_client(
    listener: &TcpListener,
    mut transcoder_output: impl Read,
    session_id: &str,
    token: &str,
    stop: &AtomicBool,
) -> io::Result<()> {
    let started = Instant::now();
    let (mut client, address) = loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if started.elapsed() >= STREAM_CLIENT_TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "embedded view did not connect in time",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "non-loopback stream client rejected",
        ));
    }
    client.set_nonblocking(false)?;
    client.set_read_timeout(Some(STREAM_IO_TIMEOUT))?;
    client.set_write_timeout(Some(STREAM_IO_TIMEOUT))?;
    let header = read_http_header(&mut client)?;
    let expected_target = format!("/screen/{session_id}?token={token}");
    if !valid_stream_request(&header, &expected_target) {
        let _ = client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "embedded stream request rejected",
        ));
    }
    client.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=ffmpeg\r\nCache-Control: no-store, no-cache, must-revalidate\r\nPragma: no-cache\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
    )?;
    let mut buffer = [0_u8; 64 * 1024];
    while !stop.load(Ordering::Acquire) {
        let count = transcoder_output.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        client.write_all(&buffer[..count])?;
        client.flush()?;
    }
    Ok(())
}

fn read_http_header(stream: &mut TcpStream) -> io::Result<String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 512];
    while bytes.len() < MAX_HTTP_HEADER_BYTES {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP request header")
            });
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "embedded stream HTTP header is incomplete or too large",
    ))
}

fn valid_stream_request(header: &str, expected_target: &str) -> bool {
    let mut lines = header.split("\r\n");
    let mut request = match lines.next().map(|line| line.split_whitespace()) {
        Some(parts) => parts,
        None => return false,
    };
    if request.next() != Some("GET")
        || request.next() != Some(expected_target)
        || !matches!(request.next(), Some("HTTP/1.1") | Some("HTTP/1.0"))
        || request.next().is_some()
    {
        return false;
    }
    let mut host_is_loopback = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            host_is_loopback = value == "localhost"
                || value.starts_with("localhost:")
                || value == "127.0.0.1"
                || value.starts_with("127.0.0.1:");
        }
        if name.eq_ignore_ascii_case("origin") && !allowed_stream_origin(value) {
            return false;
        }
    }
    host_is_loopback
}

fn allowed_stream_origin(origin: &str) -> bool {
    matches!(
        origin,
        "tauri://localhost"
            | "http://tauri.localhost"
            | "https://tauri.localhost"
            | "http://localhost:1420"
            | "http://127.0.0.1:1420"
    )
}

fn resolve_matching_scrcpy_server(scrcpy: &Path, adb: &Path) -> AppResult<(String, PathBuf)> {
    let environment = scrcpy_adb_environment(adb);
    let version_output = run_process_at_with_env(
        "scrcpy",
        scrcpy,
        &["--version".into()],
        STREAM_PROCESS_TIMEOUT,
        &[],
        &environment,
    )?;
    let combined = format!("{}\n{}", version_output.stdout, version_output.stderr);
    let version = parse_scrcpy_version(&combined).ok_or_else(|| {
        ApiError::new(
            "scrcpy_version_unavailable",
            "Unable to read a safe scrcpy client version for the embedded stream",
        )
    })?;

    let executable = fs::canonicalize(scrcpy).unwrap_or_else(|_| scrcpy.to_path_buf());
    let parent = executable.parent().ok_or_else(|| {
        ApiError::new(
            "scrcpy_server_unavailable",
            "The scrcpy executable has no parent directory",
        )
    })?;
    let mut candidates = vec![
        parent.join("scrcpy-server"),
        parent.join("scrcpy-server.jar"),
        parent.join("../share/scrcpy/scrcpy-server"),
        parent.join("../lib/scrcpy/scrcpy-server"),
    ];
    if let Some(configured) = std::env::var_os("SCRCPY_SERVER_PATH") {
        candidates.push(PathBuf::from(configured));
    }
    for candidate in candidates {
        let Ok(candidate) = fs::canonicalize(candidate) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && (16 * 1024..=MAX_STREAM_SERVER_BYTES).contains(&metadata.len()) {
            return Ok((version, candidate));
        }
    }
    Err(ApiError::new(
        "scrcpy_server_unavailable",
        "The matching scrcpy-server file was not found next to this scrcpy installation. Install the complete official scrcpy package or set SCRCPY_SERVER_PATH.",
    ))
}

fn scrcpy_client_command(scrcpy: &Path, adb: &Path, server: &Path) -> std::process::Command {
    let mut command = background_command(scrcpy);
    clear_ambient_adb_server_environment(&mut command);
    command.envs(scrcpy_adb_environment(adb));
    command.env("SCRCPY_SERVER_PATH", server);
    command
}

fn scrcpy_adb_environment(adb: &Path) -> [(OsString, OsString); 1] {
    [(OsString::from("ADB"), adb.as_os_str().to_os_string())]
}

fn parse_scrcpy_version(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("scrcpy "))
        .and_then(|suffix| suffix.split_whitespace().next())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 32
                && value
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        })
        .map(str::to_string)
}

fn probe_stream_dimensions(adb: &Path, serial: &str, max_size: u16) -> (Option<u32>, Option<u32>) {
    let output = run_process_at(
        "adb",
        adb,
        &[
            "-s".into(),
            serial.into(),
            "shell".into(),
            "wm".into(),
            "size".into(),
        ],
        STREAM_PROCESS_TIMEOUT,
        &[],
    );
    let Some((width, height)) = output.ok().and_then(|output| {
        output
            .stdout
            .split_whitespace()
            .filter_map(|token| token.trim().split_once('x'))
            .filter_map(|(width, height)| {
                let width: u32 = width
                    .trim_matches(|ch: char| !ch.is_ascii_digit())
                    .parse()
                    .ok()?;
                let height: u32 = height
                    .trim_matches(|ch: char| !ch.is_ascii_digit())
                    .parse()
                    .ok()?;
                Some((width, height))
            })
            .next_back()
    }) else {
        return (None, None);
    };
    let longest = width.max(height);
    if longest <= u32::from(max_size) {
        return (Some(width), Some(height));
    }
    let scale = f64::from(max_size) / f64::from(longest);
    let scaled_width = ((f64::from(width) * scale).round() as u32).max(2) & !1;
    let scaled_height = ((f64::from(height) * scale).round() as u32).max(2) & !1;
    (Some(scaled_width), Some(scaled_height))
}

fn require_success(
    output: crate::runner::ProcessOutput,
) -> AppResult<crate::runner::ProcessOutput> {
    if output.timed_out {
        return Err(ApiError::new("process_timeout", "External tool timed out"));
    }
    if output.exit_code != Some(0) {
        let detail = if output.stderr.trim().is_empty() {
            output.stdout.trim()
        } else {
            output.stderr.trim()
        };
        return Err(ApiError::new(
            "process_failed",
            if detail.is_empty() {
                String::from("External tool failed")
            } else {
                detail.to_string()
            },
        ));
    }
    Ok(output)
}

fn random_bytes() -> AppResult<[u8; 16]> {
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|error| {
        ApiError::new(
            "secure_random_unavailable",
            format!("Unable to create a private screen session: {error}"),
        )
    })?;
    Ok(bytes)
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn capture_stderr(stderr: ChildStderr) -> Arc<Mutex<String>> {
    let captured = Arc::new(Mutex::new(String::new()));
    let writer = captured.clone();
    thread::spawn(move || {
        let mut reader = stderr;
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if let Ok(mut text) = writer.lock() {
                        let remaining = (64 * 1024_usize).saturating_sub(text.len());
                        if remaining > 0 {
                            text.push_str(&String::from_utf8_lossy(
                                &buffer[..count.min(remaining)],
                            ));
                        }
                    }
                }
            }
        }
    });
    captured
}

fn remove_stream_for_serial(
    state: &AppState,
    serial: &str,
) -> AppResult<Option<ManagedAndroidScreenStream>> {
    state
        .screen_streams
        .lock()
        .map_err(|_| ApiError::new("stream_state_error", "Screen stream state is unavailable"))
        .map(|mut streams| streams.remove(serial))
}

fn stop_managed_stream(managed: &ManagedAndroidScreenStream) {
    managed.stop.store(true, Ordering::Release);
    if let Ok(mut socket) = managed.input_socket.lock() {
        if let Some(socket) = socket.take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
    // Remove the persistent adb mapping before terminating child processes. On application exit,
    // this makes the only device-side host resource disappear before the event loop can finish.
    if let Err(error) = remove_stream_reverse(&managed.adb_path, &managed.serial, &managed.scid) {
        eprintln!(
            "Mobius cleanup: embedded screen reverse mapping could not be removed: {}",
            error.message
        );
    }
    kill_child(&managed.transcoder_process);
    kill_child(&managed.server_process);
    remove_stream_remote_artifact(&managed.adb_path, &managed.serial, &managed.remote_path);
}

fn kill_child(child: &Arc<Mutex<Option<Child>>>) {
    if let Ok(mut child) = child.lock() {
        if let Some(mut child) = child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn cleanup_stream_artifacts(
    adb: &Path,
    serial: &str,
    scid: &str,
    _input_port: u16,
    remote_path: &str,
) {
    if scid.len() != 8
        || !scid.chars().all(|ch| ch.is_ascii_hexdigit())
        || !remote_path.starts_with(STREAM_REMOTE_PREFIX)
    {
        return;
    }
    if let Err(error) = remove_stream_reverse(adb, serial, scid) {
        eprintln!(
            "Mobius cleanup: embedded screen reverse mapping could not be removed: {}",
            error.message
        );
    }
    remove_stream_remote_artifact(adb, serial, remote_path);
}

fn remove_stream_reverse(adb: &Path, serial: &str, scid: &str) -> AppResult<()> {
    let output = run_process_at(
        "adb",
        adb,
        &[
            "-s".into(),
            serial.into(),
            "reverse".into(),
            "--remove".into(),
            format!("localabstract:scrcpy_{scid}"),
        ],
        STREAM_PROCESS_TIMEOUT,
        &[],
    )?;
    if !output.timed_out
        && output.exit_code != Some(0)
        && output.stderr.contains("listener")
        && output.stderr.contains("not found")
    {
        return Ok(());
    }
    require_success(output).map(|_| ())
}

fn remove_stream_remote_artifact(adb: &Path, serial: &str, remote_path: &str) {
    let result = run_process_at(
        "adb",
        adb,
        &[
            "-s".into(),
            serial.into(),
            "shell".into(),
            "rm".into(),
            "-f".into(),
            remote_path.into(),
        ],
        STREAM_PROCESS_TIMEOUT,
        &[],
    )
    .and_then(require_success);
    if let Err(error) = result {
        eprintln!(
            "Mobius cleanup: embedded screen server artifact could not be removed: {}",
            error.message
        );
    }
}

pub(crate) fn cleanup_managed_screen_streams(state: &AppState) {
    let _lifecycle_guard = match state.screen_stream_lifecycle_lock.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    let streams = match state.screen_streams.lock() {
        Ok(mut streams) => streams
            .drain()
            .map(|(_, stream)| stream)
            .collect::<Vec<_>>(),
        Err(_) => return,
    };
    for stream in streams {
        stop_managed_stream(&stream);
    }
}

fn validate_bit_rate(value: &str) -> Result<(), ApiError> {
    if value.is_empty() || value.len() > 12 {
        return Err(ApiError::new(
            "invalid_bit_rate",
            "bitRate must look like 8000000, 8000K or 8M",
        ));
    }
    let (digits, multiplier) =
        if let Some(digits) = value.strip_suffix('K').or_else(|| value.strip_suffix('k')) {
            (digits, 1_000_u64)
        } else if let Some(digits) = value.strip_suffix('M').or_else(|| value.strip_suffix('m')) {
            (digits, 1_000_000_u64)
        } else {
            (value, 1_u64)
        };
    let amount = digits
        .parse::<u64>()
        .ok()
        .filter(|amount| *amount > 0)
        .ok_or_else(|| {
            ApiError::new(
                "invalid_bit_rate",
                "bitRate must look like 8000000, 8000K or 8M",
            )
        })?;
    let bits_per_second = amount
        .checked_mul(multiplier)
        .ok_or_else(|| ApiError::new("invalid_bit_rate", "bitRate exceeds the supported range"))?;
    if !(64_000..=100_000_000).contains(&bits_per_second) {
        return Err(ApiError::new(
            "invalid_bit_rate",
            "bitRate must be between 64 Kbit/s and 100 Mbit/s",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_http_request_requires_exact_token_and_loopback_host() {
        let expected = "/screen/001122?token=aabbcc";
        assert!(valid_stream_request(
            "GET /screen/001122?token=aabbcc HTTP/1.1\r\nHost: 127.0.0.1:43000\r\nOrigin: tauri://localhost\r\n\r\n",
            expected,
        ));
        assert!(!valid_stream_request(
            "GET /screen/001122?token=wrong HTTP/1.1\r\nHost: 127.0.0.1:43000\r\n\r\n",
            expected,
        ));
        assert!(!valid_stream_request(
            "GET /screen/001122?token=aabbcc HTTP/1.1\r\nHost: 192.168.1.20:43000\r\n\r\n",
            expected,
        ));
    }

    #[test]
    fn stream_http_request_rejects_foreign_origin() {
        assert!(!valid_stream_request(
            "GET /screen/id?token=secret HTTP/1.1\r\nHost: localhost:43000\r\nOrigin: https://example.invalid\r\n\r\n",
            "/screen/id?token=secret",
        ));
    }

    #[test]
    fn scrcpy_version_parser_only_accepts_safe_version_token() {
        assert_eq!(
            parse_scrcpy_version("scrcpy 4.0 <https://github.com/Genymobile/scrcpy>\n"),
            Some("4.0".into())
        );
        assert_eq!(parse_scrcpy_version("scrcpy 4.0;touch_bad\n"), None);
        assert_eq!(parse_scrcpy_version("not scrcpy\n"), None);
    }

    #[test]
    fn scrcpy_command_receives_the_resolved_adb_path() {
        #[cfg(windows)]
        let (scrcpy, adb, server) = (
            Path::new(r"C:\managed tools\scrcpy.exe"),
            Path::new(r"C:\Android SDK\platform-tools\adb.exe"),
            Path::new(r"C:\managed tools\scrcpy-server"),
        );
        #[cfg(not(windows))]
        let (scrcpy, adb, server) = (
            Path::new("/managed tools/scrcpy"),
            Path::new("/Android SDK/platform-tools/adb"),
            Path::new("/managed tools/scrcpy-server"),
        );

        let command = scrcpy_client_command(scrcpy, adb, server);
        assert_eq!(command.get_program(), scrcpy.as_os_str());
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == std::ffi::OsStr::new("ADB"))
                .and_then(|(_, value)| value),
            Some(adb.as_os_str())
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == std::ffi::OsStr::new("SCRCPY_SERVER_PATH"))
                .and_then(|(_, value)| value),
            Some(server.as_os_str())
        );
    }

    #[test]
    fn bit_rate_validation_is_bounded() {
        assert!(validate_bit_rate("8M").is_ok());
        assert!(validate_bit_rate("63K").is_err());
        assert!(validate_bit_rate("101M").is_err());
    }
}
