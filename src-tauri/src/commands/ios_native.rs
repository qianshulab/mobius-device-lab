use crate::{
    models::{ApiError, AppResult, IosDevice, IosDeviceInfo},
    runner::{resolve_tool, run_checked, run_process, ProcessOutput},
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path, time::Duration};

#[derive(Debug, Deserialize)]
struct RawDeviceList {
    #[serde(rename = "deviceList")]
    devices: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceList {
    #[serde(rename = "deviceList")]
    devices: Vec<DeviceEntry>,
}

#[derive(Debug, Deserialize)]
struct DeviceEntry {
    #[serde(rename = "Udid", alias = "UDID", alias = "udid")]
    udid: String,
    #[serde(rename = "ProductName", default)]
    product_name: Option<String>,
    #[serde(rename = "ProductType", default)]
    product_type: Option<String>,
    #[serde(rename = "ProductVersion", default)]
    product_version: Option<String>,
    #[serde(rename = "ConnectionType", default)]
    connection_type: Option<String>,
}

pub(crate) fn available() -> bool {
    resolve_tool("ios").is_ok()
}

pub(crate) fn list_devices(timeout: Duration) -> AppResult<Vec<IosDevice>> {
    // The detailed go-ios list aborts wholesale when any usbmux entry is
    // unpaired, locked, or stale. Start with the raw UDID list so a newly
    // attached device remains visible, then enrich every entry best-effort.
    let output = run_checked("ios", &["list".into()], timeout)?;
    let mut identifiers = parse_raw_device_list(&output.stdout)?;
    identifiers.sort();
    identifiers.dedup();

    let mut detailed = run_checked("ios", &["list".into(), "--details".into()], timeout)
        .ok()
        .and_then(|output| parse_device_list(&output.stdout).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|device| (device.udid.clone(), device))
        .collect::<BTreeMap<_, _>>();

    Ok(identifiers
        .into_iter()
        .map(|udid| {
            detailed.remove(&udid).unwrap_or_else(|| {
                device_info(&udid, timeout)
                    .map(|info| device_from_info(&udid, &info))
                    .unwrap_or_else(|_| IosDevice {
                        udid,
                        name: None,
                        product_type: None,
                        product_version: None,
                        build_version: None,
                        connection: "usb".into(),
                    })
            })
        })
        .collect())
}

pub(crate) fn device_info(udid: &str, timeout: Duration) -> AppResult<IosDeviceInfo> {
    let output = run_checked("ios", &target_args("info", udid, &[]), timeout)?;
    parse_device_info(udid, &output.stdout)
}

pub(crate) fn capture_screenshot(
    udid: &str,
    destination: &Path,
    timeout: Duration,
) -> AppResult<ProcessOutput> {
    run_checked(
        "ios",
        &target_args(
            "screenshot",
            udid,
            &[format!("--output={}", destination.to_string_lossy())],
        ),
        timeout,
    )
}

pub(crate) fn install(udid: &str, source: &Path, timeout: Duration) -> AppResult<ProcessOutput> {
    run_checked(
        "ios",
        &target_args(
            "install",
            udid,
            &[format!("--path={}", source.to_string_lossy())],
        ),
        timeout,
    )
}

pub(crate) fn run(
    command: &str,
    udid: &str,
    extra_args: &[String],
    timeout: Duration,
) -> AppResult<ProcessOutput> {
    run_process("ios", &target_args(command, udid, extra_args), timeout, &[])
}

fn target_args(command: &str, udid: &str, extra_args: &[String]) -> Vec<String> {
    let mut args = Vec::with_capacity(extra_args.len() + 2);
    args.push(command.to_string());
    args.push(format!("--udid={udid}"));
    args.extend_from_slice(extra_args);
    args
}

fn parse_device_list(output: &str) -> AppResult<Vec<IosDevice>> {
    let list: DeviceList = parse_json_output(output, "ios_device_list_invalid")?;
    let mut devices = list
        .devices
        .into_iter()
        .filter(|entry| !entry.udid.trim().is_empty())
        .map(|entry| IosDevice {
            udid: entry.udid,
            name: nonempty(entry.product_name),
            product_type: nonempty(entry.product_type),
            product_version: nonempty(entry.product_version),
            build_version: None,
            connection: connection_label(entry.connection_type.as_deref()).into(),
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        left.udid.cmp(&right.udid).then_with(|| {
            connection_priority(&left.connection).cmp(&connection_priority(&right.connection))
        })
    });
    devices.dedup_by(|left, right| left.udid == right.udid);
    Ok(devices)
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| (!value.trim().is_empty()).then_some(value))
}

fn parse_raw_device_list(output: &str) -> AppResult<Vec<String>> {
    let list: RawDeviceList = parse_json_output(output, "ios_device_list_invalid")?;
    Ok(list
        .devices
        .into_iter()
        .map(|udid| udid.trim().to_string())
        .filter(|udid| !udid.is_empty())
        .collect())
}

fn device_from_info(udid: &str, info: &IosDeviceInfo) -> IosDevice {
    IosDevice {
        udid: udid.to_string(),
        name: info.properties.get("DeviceName").cloned(),
        product_type: info.properties.get("ProductType").cloned(),
        product_version: info.properties.get("ProductVersion").cloned(),
        build_version: info.properties.get("BuildVersion").cloned(),
        connection: connection_label(info.properties.get("ConnectionType").map(String::as_str))
            .into(),
    }
}

fn parse_device_info(udid: &str, output: &str) -> AppResult<IosDeviceInfo> {
    let value: Value = parse_json_output(output, "ios_device_info_invalid")?;
    let object = value.as_object().ok_or_else(|| {
        ApiError::new(
            "ios_device_info_invalid",
            "go-ios returned device information in an unexpected format",
        )
    })?;
    let properties = object
        .iter()
        .map(|(key, value)| (key.clone(), property_value(value)))
        .collect::<BTreeMap<_, _>>();
    Ok(IosDeviceInfo {
        udid: udid.to_string(),
        properties,
    })
}

fn parse_json_output<T: for<'de> Deserialize<'de>>(output: &str, code: &str) -> AppResult<T> {
    let trimmed = output.trim();
    serde_json::from_str(trimmed)
        .or_else(|_| {
            trimmed
                .lines()
                .rev()
                .find_map(|line| serde_json::from_str(line.trim()).ok())
                .ok_or_else(|| serde_json::Error::io(std::io::Error::other("no JSON document")))
        })
        .map_err(|error| {
            ApiError::new(
                code,
                format!("go-ios returned an unreadable JSON response: {error}"),
            )
        })
}

fn property_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn connection_label(value: Option<&str>) -> &'static str {
    if value.is_some_and(|value| value.eq_ignore_ascii_case("network")) {
        "network"
    } else {
        "usb"
    }
}

fn connection_priority(value: &str) -> u8 {
    if value == "usb" {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_detailed_device_list() {
        let devices = parse_device_list(
            r#"{"deviceList":[{"Udid":"00008101-ABC","ProductName":"iPhone","ProductType":"iPhone15,2","ProductVersion":"17.6","ConnectionType":"USB"},{"Udid":"wifi-device","ProductName":"iPad","ProductType":"iPad14,5","ProductVersion":"18.0","ConnectionType":"Network"}]}"#,
        )
        .expect("valid go-ios list");
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].connection, "usb");
        assert_eq!(devices[1].connection, "network");
    }

    #[test]
    fn raw_device_list_keeps_untrusted_devices_visible() {
        let devices = parse_raw_device_list(
            "a warning line\n{\"deviceList\":[\" 00008101-ABC \",\"\",\"wifi-device\"]}",
        )
        .expect("raw go-ios list");
        assert_eq!(devices, ["00008101-ABC", "wifi-device"]);
    }

    #[test]
    fn partial_detailed_entries_do_not_render_blank_identity() {
        let devices = parse_device_list(
            r#"{"deviceList":[{"Udid":"new-device","ProductName":"","ProductType":" ","ProductVersion":"","ConnectionType":"USB"}]}"#,
        )
        .expect("partial detailed list");
        assert_eq!(devices.len(), 1);
        assert!(devices[0].name.is_none());
        assert!(devices[0].product_type.is_none());
        assert!(devices[0].product_version.is_none());
    }

    #[test]
    fn usb_wins_when_go_ios_reports_both_transports() {
        let devices = parse_device_list(
            r#"{"deviceList":[{"Udid":"same-device","ConnectionType":"Network"},{"Udid":"same-device","ConnectionType":"USB"}]}"#,
        )
        .expect("valid duplicate transport list");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].connection, "usb");
    }

    #[test]
    fn accepts_a_log_line_before_compact_json() {
        let devices = parse_device_list("warning written by a wrapped build\n{\"deviceList\":[]}")
            .expect("last compact JSON line should be accepted");
        assert!(devices.is_empty());
    }

    #[test]
    fn converts_scalar_and_nested_device_properties() {
        let info = parse_device_info(
            "device-1",
            r#"{"DeviceName":"Lab iPhone","ProductVersion":"17.5","ProductionSOC":true,"Nested":{"Value":1}}"#,
        )
        .expect("valid go-ios info");
        assert_eq!(
            info.properties.get("DeviceName").map(String::as_str),
            Some("Lab iPhone")
        );
        assert_eq!(
            info.properties.get("ProductionSOC").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            info.properties.get("Nested").map(String::as_str),
            Some("{\"Value\":1}")
        );
    }
}
