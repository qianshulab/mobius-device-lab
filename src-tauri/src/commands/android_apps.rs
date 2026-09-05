use super::blocking_api;
use crate::{
    models::{
        AndroidAppAction, AndroidAppOperationResult, AndroidAppTargetRequest, ApiError, ApiResult,
        AppResult,
    },
    runner::{run_checked, ProcessOutput},
    validation,
};
use std::time::Duration;

const APP_ACTION_TIMEOUT: Duration = Duration::from_secs(30);

#[tauri::command]
pub async fn launch_android_app(
    request: AndroidAppTargetRequest,
) -> ApiResult<AndroidAppOperationResult> {
    blocking_api(move || run_app_action(request, AndroidAppAction::Launch)).await
}

#[tauri::command]
pub async fn force_stop_android_app(
    request: AndroidAppTargetRequest,
) -> ApiResult<AndroidAppOperationResult> {
    blocking_api(move || run_app_action(request, AndroidAppAction::ForceStop)).await
}

#[tauri::command]
pub async fn clear_android_app_data(
    request: AndroidAppTargetRequest,
) -> ApiResult<AndroidAppOperationResult> {
    blocking_api(move || run_app_action(request, AndroidAppAction::ClearData)).await
}

#[tauri::command]
pub async fn uninstall_android_app(
    request: AndroidAppTargetRequest,
) -> ApiResult<AndroidAppOperationResult> {
    blocking_api(move || run_app_action(request, AndroidAppAction::Uninstall)).await
}

fn run_app_action(
    request: AndroidAppTargetRequest,
    action: AndroidAppAction,
) -> AppResult<AndroidAppOperationResult> {
    validation::serial(&request.serial)?;
    validation::package_name(&request.package_name)?;
    require_online_device(&request.serial)?;
    let installed_in_system_partition =
        require_installed_package(&request.serial, &request.package_name)?;

    if matches!(
        action,
        AndroidAppAction::ClearData | AndroidAppAction::Uninstall
    ) && (installed_in_system_partition
        || is_system_package(&request.serial, &request.package_name)?)
    {
        return Err(ApiError::new(
            "android_system_app_protected",
            "System applications cannot be cleared or uninstalled from Mobius",
        ));
    }

    let args = action_arguments(action, &request.serial, &request.package_name);
    let output = run_checked("adb", &args, APP_ACTION_TIMEOUT)?;
    verify_action_output(action, &output)?;

    Ok(AndroidAppOperationResult {
        success: true,
        message: action_success_message(action).to_string(),
        serial: request.serial,
        package_name: request.package_name,
        action,
    })
}

fn require_online_device(serial: &str) -> AppResult<()> {
    let output = run_checked(
        "adb",
        &["-s".into(), serial.into(), "get-state".into()],
        APP_ACTION_TIMEOUT,
    )?;
    if output.stdout.trim() != "device" {
        return Err(ApiError::new(
            "android_device_not_online",
            "The selected Android device is not online",
        ));
    }
    Ok(())
}

fn require_installed_package(serial: &str, package_name: &str) -> AppResult<bool> {
    let output = run_checked(
        "adb",
        &[
            "-s".into(),
            serial.into(),
            "shell".into(),
            "pm".into(),
            "path".into(),
            package_name.into(),
        ],
        APP_ACTION_TIMEOUT,
    )?;
    let paths = output
        .stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix("package:"))
        .filter(|path| path.starts_with('/'))
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Err(ApiError::new(
            "android_package_not_found",
            "The selected package is not installed on the selected device",
        ));
    }
    Ok(paths.iter().any(|path| is_system_partition_path(path)))
}

fn is_system_partition_path(path: &str) -> bool {
    [
        "/system/",
        "/system_ext/",
        "/product/",
        "/vendor/",
        "/odm/",
        "/apex/",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}

fn is_system_package(serial: &str, package_name: &str) -> AppResult<bool> {
    let output = run_checked(
        "adb",
        &[
            "-s".into(),
            serial.into(),
            "shell".into(),
            "pm".into(),
            "list".into(),
            "packages".into(),
            "-s".into(),
            package_name.into(),
        ],
        APP_ACTION_TIMEOUT,
    )?;
    Ok(output
        .stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix("package:"))
        .any(|candidate| candidate == package_name))
}

fn action_arguments(action: AndroidAppAction, serial: &str, package_name: &str) -> Vec<String> {
    let mut args = vec!["-s".into(), serial.into()];
    match action {
        AndroidAppAction::Launch => args.extend([
            "shell".into(),
            "monkey".into(),
            "-p".into(),
            package_name.into(),
            "-c".into(),
            "android.intent.category.LAUNCHER".into(),
            "1".into(),
        ]),
        AndroidAppAction::ForceStop => args.extend([
            "shell".into(),
            "am".into(),
            "force-stop".into(),
            package_name.into(),
        ]),
        AndroidAppAction::ClearData => args.extend([
            "shell".into(),
            "pm".into(),
            "clear".into(),
            package_name.into(),
        ]),
        AndroidAppAction::Uninstall => {
            args.extend(["uninstall".into(), package_name.into()]);
        }
    }
    args
}

fn verify_action_output(action: AndroidAppAction, output: &ProcessOutput) -> AppResult<()> {
    let stdout = output.stdout.trim();
    let stderr = output.stderr.trim();
    let combined_lines = stdout.lines().chain(stderr.lines());
    let succeeded = match action {
        AndroidAppAction::Launch => {
            combined_lines
                .clone()
                .any(|line| line.trim() == "Events injected: 1")
                && !stdout.to_ascii_lowercase().contains("monkey aborted")
                && !stderr.to_ascii_lowercase().contains("monkey aborted")
        }
        AndroidAppAction::ForceStop => true,
        AndroidAppAction::ClearData | AndroidAppAction::Uninstall => {
            stdout.lines().any(|line| line.trim() == "Success")
        }
    };
    if succeeded {
        return Ok(());
    }

    let detail = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Android did not confirm the requested application operation");
    Err(ApiError::new(action_failure_code(action), detail))
}

fn action_success_message(action: AndroidAppAction) -> &'static str {
    match action {
        AndroidAppAction::Launch => "Android application launched",
        AndroidAppAction::ForceStop => "Android application stopped",
        AndroidAppAction::ClearData => "Android application data cleared",
        AndroidAppAction::Uninstall => "Android application uninstalled",
    }
}

fn action_failure_code(action: AndroidAppAction) -> &'static str {
    match action {
        AndroidAppAction::Launch => "android_app_not_launchable",
        AndroidAppAction::ForceStop => "android_app_stop_failed",
        AndroidAppAction::ClearData => "android_app_clear_failed",
        AndroidAppAction::Uninstall => "android_app_uninstall_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(stdout: &str, stderr: &str) -> ProcessOutput {
        ProcessOutput {
            program: "adb".into(),
            exit_code: Some(0),
            stdout: stdout.into(),
            stderr: stderr.into(),
            timed_out: false,
            truncated: false,
            duration_ms: 1,
        }
    }

    #[test]
    fn action_arguments_are_fixed_and_keep_target_separate() {
        assert_eq!(
            action_arguments(AndroidAppAction::ForceStop, "device-1", "dev.mobius.demo"),
            vec![
                "-s",
                "device-1",
                "shell",
                "am",
                "force-stop",
                "dev.mobius.demo"
            ]
        );
        assert_eq!(
            action_arguments(AndroidAppAction::Uninstall, "device-1", "dev.mobius.demo"),
            vec!["-s", "device-1", "uninstall", "dev.mobius.demo"]
        );
    }

    #[test]
    fn mutation_results_require_android_success_marker() {
        assert!(
            verify_action_output(AndroidAppAction::ClearData, &output("Success\n", "")).is_ok()
        );
        assert!(verify_action_output(
            AndroidAppAction::Uninstall,
            &output("Failure [DELETE_FAILED_INTERNAL_ERROR]\n", "")
        )
        .is_err());
    }

    #[test]
    fn launch_requires_one_injected_launcher_event() {
        assert!(verify_action_output(
            AndroidAppAction::Launch,
            &output("Events injected: 1\n", "")
        )
        .is_ok());
        assert!(verify_action_output(
            AndroidAppAction::Launch,
            &output("** No activities found to run, monkey aborted.\n", "")
        )
        .is_err());
    }

    #[test]
    fn recognizes_only_fixed_android_system_partitions() {
        assert!(is_system_partition_path(
            "/system/app/Settings/Settings.apk"
        ));
        assert!(is_system_partition_path("/product/priv-app/Demo/Demo.apk"));
        assert!(!is_system_partition_path(
            "/data/app/dev.mobius.demo/base.apk"
        ));
        assert!(!is_system_partition_path("/sdcard/system/app.apk"));
    }

    #[test]
    #[ignore = "requires an explicitly authorized live Android device and a safe launchable package"]
    fn live_launch_and_force_stop_round_trip() {
        let serial = std::env::var("MOBIUS_LIVE_ANDROID_SERIAL")
            .expect("set MOBIUS_LIVE_ANDROID_SERIAL to the authorized device serial");
        let package_name = std::env::var("MOBIUS_LIVE_ANDROID_SAFE_PACKAGE")
            .expect("set MOBIUS_LIVE_ANDROID_SAFE_PACKAGE to a safe launchable package");
        let request = AndroidAppTargetRequest {
            serial: serial.clone(),
            package_name: package_name.clone(),
        };
        let launched = run_app_action(request.clone(), AndroidAppAction::Launch)
            .expect("launch the safe package");
        assert_eq!(launched.serial, serial);
        assert_eq!(launched.package_name, package_name);
        let stopped = run_app_action(request, AndroidAppAction::ForceStop)
            .expect("force-stop the same safe package");
        assert_eq!(stopped.action, AndroidAppAction::ForceStop);
    }
}
