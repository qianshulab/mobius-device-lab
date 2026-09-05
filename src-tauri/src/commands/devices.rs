use super::blocking_api;
use crate::{
    models::{
        AndroidDevice, ApiError, ApiResult, AppResult, Device, IosDevice, IosDeviceInfo,
        OperationResult,
    },
    runner::{run_checked, run_checked_with_stdin},
    validation,
};
use std::{collections::BTreeMap, time::Duration};

const DEVICE_TIMEOUT: Duration = Duration::from_secs(8);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[tauri::command]
pub async fn list_devices() -> ApiResult<Vec<Device>> {
    blocking_api(|| {
        let mut errors = Vec::new();
        let mut devices = Vec::new();
        match list_android_devices_inner() {
            Ok(android) => devices.extend(android.into_iter().map(enrich_android_device)),
            Err(error) => {
                errors.push(format!("Android: {}", error.message));
            }
        }
        match list_ios_devices_inner() {
            Ok(ios) => devices.extend(ios.into_iter().map(|device| Device {
                id: device.udid.clone(),
                name: device.name.unwrap_or_else(|| "iOS Device".into()),
                platform: "ios".into(),
                os_version: device.product_version.unwrap_or_else(|| "Unknown".into()),
                state: "online".into(),
                transport: if device.connection == "network" {
                    "wifi".into()
                } else {
                    "usbmux".into()
                },
                address: None,
                model: device.product_type.clone(),
                architecture: None,
                rooted: None,
                jailbroken: None,
                product: device.product_type,
            })),
            Err(error) => {
                errors.push(format!("iOS: {}", error.message));
            }
        }
        if devices.is_empty() && errors.len() == 2 {
            return Err(ApiError::new(
                "device_tools_unavailable",
                "Neither adb nor libimobiledevice tools are available",
            )
            .with_details(serde_json::json!({ "warnings": errors })));
        }
        Ok(devices)
    })
    .await
}

#[tauri::command]
pub async fn list_android_devices() -> ApiResult<Vec<AndroidDevice>> {
    blocking_api(list_android_devices_inner).await
}

#[tauri::command]
pub async fn list_ios_devices() -> ApiResult<Vec<IosDevice>> {
    blocking_api(list_ios_devices_inner).await
}

#[tauri::command]
pub async fn get_ios_device_info(udid: String) -> ApiResult<IosDeviceInfo> {
    blocking_api(move || ios_device_info_inner(&udid)).await
}

#[tauri::command]
pub async fn adb_connect(address: String) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::host_port(&address)?;
        let output = run_checked("adb", &["connect".into(), address.clone()], CONNECT_TIMEOUT)?;
        let connected = list_android_devices_inner()?.into_iter().any(|device| {
            device.serial == address
                && matches!(device.state.as_str(), "device" | "recovery" | "sideload")
        });
        if !connected {
            return Err(ApiError::new(
                "adb_connect_failed",
                if output.stdout.is_empty() {
                    "The endpoint did not appear as a usable adb device after connecting"
                        .to_string()
                } else {
                    output.stdout.clone()
                },
            )
            .with_details(serde_json::to_value(&output).unwrap_or_default()));
        }
        Ok(output.into_operation(format!("Connected to {address}")))
    })
    .await
}

#[tauri::command]
pub async fn adb_pair(address: String, code: String) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::host_port(&address)?;
        if code.len() != 6 || !code.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(ApiError::new(
                "invalid_pairing_code",
                "ADB pairing code must contain exactly six digits",
            ));
        }
        // Supplying the secret over stdin keeps it out of the host process list.
        let args = vec!["pair".into(), address.clone()];
        let mut input = code.into_bytes();
        input.push(b'\n');
        let result = run_checked_with_stdin("adb", &args, CONNECT_TIMEOUT, &input);
        input.fill(0);
        let output = result?;
        if !output.stdout.to_ascii_lowercase().contains("success") {
            return Err(ApiError::new(
                "adb_pair_failed",
                if output.stdout.is_empty() {
                    "adb did not confirm pairing".to_string()
                } else {
                    output.stdout.clone()
                },
            )
            .with_details(serde_json::to_value(&output).unwrap_or_default()));
        }
        Ok(output.into_operation(format!("Paired with {address}")))
    })
    .await
}

#[tauri::command]
pub async fn run_device_shell(serial: String, command: String) -> ApiResult<OperationResult> {
    blocking_api(move || {
        validation::serial(&serial)?;
        validation::shell_command(&command)?;
        // The command is sent directly to adb as one argument. No host shell is ever started.
        // This is intentionally an advanced operation: adb executes it in the selected device's shell.
        let args = vec!["-s".into(), serial, "shell".into(), command];
        let output = run_checked("adb", &args, Duration::from_secs(30))?;
        Ok(output.into_operation("Device shell command completed"))
    })
    .await
}

fn list_android_devices_inner() -> AppResult<Vec<AndroidDevice>> {
    let output = run_checked("adb", &["devices".into(), "-l".into()], DEVICE_TIMEOUT)?;
    Ok(parse_adb_devices(&output.stdout))
}

fn parse_adb_devices(output: &str) -> Vec<AndroidDevice> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("List of devices") && !line.starts_with('*')
        })
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let serial = parts.next()?.to_string();
            let state = parts.next().unwrap_or("unknown").to_string();
            let attributes = parts
                .filter_map(|field| field.split_once(':'))
                .collect::<BTreeMap<_, _>>();
            let connection = if serial.starts_with("emulator-") {
                "emulator"
            } else if serial.contains(':') {
                "network"
            } else {
                "usb"
            };
            Some(AndroidDevice {
                serial,
                state,
                product: attributes.get("product").map(|value| (*value).to_string()),
                model: attributes.get("model").map(|value| (*value).to_string()),
                device: attributes.get("device").map(|value| (*value).to_string()),
                transport_id: attributes
                    .get("transport_id")
                    .map(|value| (*value).to_string()),
                connection: connection.to_string(),
            })
        })
        .collect()
}

fn enrich_android_device(device: AndroidDevice) -> Device {
    let properties = if device.state == "device" {
        android_properties(&device.serial).unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    let model = properties
        .get("ro.product.model")
        .cloned()
        .or_else(|| device.model.clone())
        .map(|value| value.replace('_', " "));
    let name = model.clone().unwrap_or_else(|| device.serial.clone());
    let state = match device.state.as_str() {
        "device" | "recovery" | "sideload" => "online",
        "unauthorized" => "unauthorized",
        "offline" => "offline",
        _ => "connecting",
    };
    let rooted = (device.state == "device")
        .then(|| android_root_available(&device.serial))
        .flatten();
    Device {
        id: device.serial.clone(),
        name,
        platform: "android".into(),
        os_version: properties
            .get("ro.build.version.release")
            .cloned()
            .unwrap_or_else(|| "Unknown".into()),
        state: state.into(),
        transport: match device.connection.as_str() {
            "network" => "wifi".into(),
            "emulator" => "emulator".into(),
            _ => "usb".into(),
        },
        address: device.serial.contains(':').then_some(device.serial.clone()),
        model,
        architecture: properties.get("ro.product.cpu.abi").cloned(),
        rooted,
        jailbroken: None,
        product: properties
            .get("ro.product.name")
            .cloned()
            .or(device.product),
    }
}

fn android_root_available(serial: &str) -> Option<bool> {
    let output = run_checked(
        "adb",
        &[
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            "if [ \"$(id -u)\" = 0 ] || command -v su >/dev/null 2>&1; then echo 1; else echo 0; fi"
                .into(),
        ],
        DEVICE_TIMEOUT,
    )
    .ok()?;
    Some(output.stdout.trim() == "1")
}

fn android_properties(serial: &str) -> AppResult<BTreeMap<String, String>> {
    validation::serial(serial)?;
    let output = run_checked(
        "adb",
        &[
            "-s".into(),
            serial.to_string(),
            "shell".into(),
            "getprop".into(),
        ],
        DEVICE_TIMEOUT,
    )?;
    Ok(output
        .stdout
        .lines()
        .filter_map(|line| line.strip_prefix('[')?.split_once("]: ["))
        .map(|(key, value)| (key.to_string(), value.trim_end_matches(']').to_string()))
        .collect())
}

fn list_ios_devices_inner() -> AppResult<Vec<IosDevice>> {
    let usb_output = run_checked("idevice_id", &["-l".into()], DEVICE_TIMEOUT)?;
    let network_output = run_checked("idevice_id", &["-n".into()], DEVICE_TIMEOUT).ok();
    let mut connections = BTreeMap::<String, &'static str>::new();
    if let Some(output) = network_output {
        for udid in ios_identifiers(&output.stdout) {
            connections.insert(udid.to_string(), "network");
        }
    }
    for udid in ios_identifiers(&usb_output.stdout) {
        // A device visible through both transports is represented once and USB wins.
        connections.insert(udid.to_string(), "usb");
    }
    let mut devices = Vec::new();
    for (udid, connection) in connections {
        validation::serial(&udid)?;
        match ios_device_info_for_transport(&udid, connection == "network") {
            Ok(info) => devices.push(IosDevice {
                udid: udid.clone(),
                name: info.properties.get("DeviceName").cloned(),
                product_type: info.properties.get("ProductType").cloned(),
                product_version: info.properties.get("ProductVersion").cloned(),
                build_version: info.properties.get("BuildVersion").cloned(),
                connection: connection.to_string(),
            }),
            Err(_) => devices.push(IosDevice {
                udid,
                name: None,
                product_type: None,
                product_version: None,
                build_version: None,
                connection: connection.to_string(),
            }),
        }
    }
    Ok(devices)
}

fn ios_device_info_inner(udid: &str) -> AppResult<IosDeviceInfo> {
    validation::serial(udid)?;
    match ios_device_info_for_transport(udid, false) {
        Ok(info) => Ok(info),
        Err(_) => ios_device_info_for_transport(udid, true),
    }
}

fn ios_device_info_for_transport(udid: &str, network: bool) -> AppResult<IosDeviceInfo> {
    validation::serial(udid)?;
    let mut args = vec!["-u".into(), udid.to_string()];
    if network {
        args.push("-n".into());
    }
    let output = run_checked("ideviceinfo", &args, DEVICE_TIMEOUT)?;
    let properties = output
        .stdout
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .filter(|(key, _)| !key.is_empty())
        .collect();
    Ok(IosDeviceInfo {
        udid: udid.to_string(),
        properties,
    })
}

fn ios_identifiers(output: &str) -> impl Iterator<Item = &str> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{ios_identifiers, parse_adb_devices};

    #[test]
    fn parses_usb_network_and_emulator_devices() {
        let output = "List of devices attached\nemulator-5554 device product:sdk model:Pixel_8 device:emu transport_id:1\n192.168.1.42:5555 unauthorized transport_id:2\nABCDEF offline usb:1-2\n";
        let devices = parse_adb_devices(output);
        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].connection, "emulator");
        assert_eq!(devices[0].model.as_deref(), Some("Pixel_8"));
        assert_eq!(devices[1].connection, "network");
        assert_eq!(devices[1].state, "unauthorized");
        assert_eq!(devices[2].connection, "usb");
    }

    #[test]
    fn parses_nonempty_ios_identifiers_without_guessing() {
        assert_eq!(
            ios_identifiers("\n00008020-ABCDEF\n  00008030-123456  \n").collect::<Vec<_>>(),
            vec!["00008020-ABCDEF", "00008030-123456"]
        );
    }
}
