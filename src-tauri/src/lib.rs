mod commands;
mod models;
mod runner;
mod state;
mod toolchain;
mod validation;

use commands::*;
use state::AppState;
use std::sync::atomic::Ordering;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = AppState::default();
    let cleanup_state = state.clone();
    let window_cleanup_state = state.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .setup(|app| {
            toolchain::initialize(
                app.path().resource_dir().ok(),
                app.path().app_config_dir().ok(),
            );
            Ok(())
        })
        .on_window_event(move |window, event| {
            if window.label() == "main"
                && matches!(event, tauri::WindowEvent::CloseRequested { .. })
            {
                cleanup_session(&window_cleanup_state, false);
            }
        })
        .invoke_handler(tauri::generate_handler![
            configure_toolchain,
            get_tool_health,
            list_devices,
            list_android_devices,
            list_ios_devices,
            get_ios_device_info,
            run_ios_host_diagnostic,
            adb_connect,
            adb_pair,
            scan_adb_subnet,
            list_port_mappings,
            create_port_mapping,
            remove_port_mapping,
            list_ios_port_tunnels,
            create_ios_port_tunnel,
            remove_ios_port_tunnel,
            launch_scrcpy,
            start_android_screen_stream,
            stop_android_screen_stream,
            capture_android_screen_frame,
            capture_android_screenshot,
            start_android_screen_recording,
            stop_android_screen_recording,
            probe_ios_screen_capability,
            capture_ios_screen_frame,
            capture_ios_screenshot,
            run_device_shell,
            list_remote_files,
            pull_file,
            push_file,
            mkdir_remote,
            delete_remote,
            set_android_proxy,
            clear_android_proxy,
            upload_frida_server,
            start_frida_server,
            stop_frida_server,
            start_ios_ssh_session,
            test_ios_ssh_connection,
            list_ios_ssh_files,
            upload_ios_ssh_file,
            download_ios_ssh_file,
            mkdir_ios_ssh,
            delete_ios_ssh,
            stop_ios_ssh_session,
            upload_ios_frida_server,
            start_ios_frida_server,
            stop_ios_frida_server,
            probe_ios_app_capabilities,
            install_ios_package_ssh,
            list_ios_installed_apps,
            export_ios_app_bundle,
            get_ios_runtime_snapshot,
            prepare_ios_device_action,
            run_ios_device_action,
            analyze_mobile_package,
            install_mobile_package,
            list_installed_apps,
            export_android_package,
            launch_android_app,
            force_stop_android_app,
            clear_android_app_data,
            uninstall_android_app,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Mobius Device Lab");
    app.run(move |_app_handle, event| {
        if matches!(
            event,
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
        ) {
            cleanup_session(&cleanup_state, true);
        }
    });
}

fn cleanup_session(state: &AppState, final_exit: bool) {
    if final_exit {
        state.shutting_down.store(true, Ordering::Release);
    }
    let _cleanup_guard = match state.cleanup_lock.lock() {
        Ok(guard) => guard,
        Err(_) => {
            eprintln!("Mobius cleanup: lifecycle cleanup lock was poisoned");
            return;
        }
    };
    if !final_exit && state.shutting_down.load(Ordering::Acquire) {
        return;
    }
    cleanup_managed_screen_recordings(state);
    cleanup_managed_screen_streams(state);
    cleanup_managed_frida(state);
    cleanup_managed_ios_port_tunnels(state);
    cleanup_managed_ios_ssh(state);
    cleanup_managed_proxies(state);
    cleanup_managed_mappings(state);
}
