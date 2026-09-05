use super::blocking_api;
use crate::{
    models::{
        ApiError, ApiResult, OperationResult, PortDirection, PortMapping, PortMappingRequest,
        RemovePortMappingRequest,
    },
    runner::run_checked,
    state::{AppState, ManagedPortMapping},
    validation,
};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::State;

const PORT_TIMEOUT: Duration = Duration::from_secs(10);

#[tauri::command]
pub async fn list_port_mappings(serial: String) -> ApiResult<Vec<PortMapping>> {
    blocking_api(move || {
        validation::serial(&serial)?;
        let mut mappings = Vec::new();
        let mut warnings = Vec::new();

        let forward_args = vec![
            "-s".into(),
            serial.clone(),
            "forward".into(),
            "--list".into(),
        ];
        match run_checked("adb", &forward_args, PORT_TIMEOUT) {
            Ok(output) => mappings.extend(parse_mapping_lines(
                &output.stdout,
                PortDirection::Forward,
                &serial,
            )),
            Err(error) => warnings.push(format!("Forward mappings: {}", error.message)),
        }

        let reverse_args = vec![
            "-s".into(),
            serial.clone(),
            "reverse".into(),
            "--list".into(),
        ];
        match run_checked("adb", &reverse_args, PORT_TIMEOUT) {
            Ok(output) => mappings.extend(parse_mapping_lines(
                &output.stdout,
                PortDirection::Reverse,
                &serial,
            )),
            Err(error) => warnings.push(format!("Reverse mappings: {}", error.message)),
        }

        if mappings.is_empty() && warnings.len() == 2 {
            return Err(ApiError::new(
                "mapping_list_failed",
                "Unable to list forward or reverse mappings",
            )
            .with_details(serde_json::json!({ "warnings": warnings })));
        }
        Ok(mappings)
    })
    .await
}

#[tauri::command]
pub async fn create_port_mapping(
    request: PortMappingRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        validation::serial(&request.serial)?;
        validation::endpoint(&request.local)?;
        validation::endpoint(&request.remote)?;
        let mut args = vec!["-s".into(), request.serial.clone()];
        match request.direction {
            PortDirection::Forward => {
                args.push("forward".into());
                if request.no_rebind {
                    args.push("--no-rebind".into());
                }
                args.push(request.local.clone());
                args.push(request.remote.clone());
            }
            PortDirection::Reverse => {
                args.push("reverse".into());
                if request.no_rebind {
                    args.push("--no-rebind".into());
                }
                // Public schema is intuitive: local is host-side and remote is device-side.
                args.push(request.remote.clone());
                args.push(request.local.clone());
            }
        }
        let output = run_checked("adb", &args, PORT_TIMEOUT)?;
        let (remove_endpoint, expected_remote) = match request.direction {
            PortDirection::Forward => (&request.local, &request.remote),
            PortDirection::Reverse => (&request.remote, &request.local),
        };
        if let Err(error) = remember_mapping(
            &state,
            &request.serial,
            request.direction,
            remove_endpoint,
            expected_remote,
            "user",
        ) {
            let direction = match request.direction {
                PortDirection::Forward => "forward",
                PortDirection::Reverse => "reverse",
            };
            let _ = run_checked(
                "adb",
                &[
                    "-s".into(),
                    request.serial.clone(),
                    direction.into(),
                    "--remove".into(),
                    remove_endpoint.clone(),
                ],
                PORT_TIMEOUT,
            );
            return Err(error);
        }
        Ok(output.into_operation("Port mapping created"))
    })
    .await)
}

#[tauri::command]
pub async fn remove_port_mapping(
    request: RemovePortMappingRequest,
    state: State<'_, AppState>,
) -> Result<ApiResult<OperationResult>, ApiError> {
    let state = state.inner().clone();
    Ok(blocking_api(move || {
        validation::serial(&request.serial)?;
        validation::endpoint(&request.local)?;
        let direction = match request.direction {
            PortDirection::Forward => "forward",
            PortDirection::Reverse => "reverse",
        };
        let args = vec![
            "-s".into(),
            request.serial.clone(),
            direction.into(),
            "--remove".into(),
            request.local.clone(),
        ];
        let output = run_checked("adb", &args, PORT_TIMEOUT)?;
        forget_mapping(&state, &request.serial, request.direction, &request.local)?;
        Ok(output.into_operation("Port mapping removed"))
    })
    .await)
}

pub(crate) fn remember_mapping(
    state: &AppState,
    serial: &str,
    direction: PortDirection,
    remove_endpoint: &str,
    expected_remote: &str,
    owner: &str,
) -> Result<(), ApiError> {
    if state.shutting_down.load(Ordering::Acquire) {
        return Err(ApiError::new(
            "app_shutting_down",
            "Mobius is exiting and cannot retain a new port mapping",
        ));
    }
    let mut mappings = state
        .port_mappings
        .lock()
        .map_err(|_| ApiError::new("state_error", "Port mapping registry lock was poisoned"))?;
    if state.shutting_down.load(Ordering::Acquire) {
        return Err(ApiError::new(
            "app_shutting_down",
            "Mobius is exiting and cannot retain a new port mapping",
        ));
    }
    mappings.retain(|mapping| {
        !(mapping.serial == serial
            && mapping.direction == direction
            && mapping.remove_endpoint == remove_endpoint)
    });
    mappings.push(ManagedPortMapping {
        serial: serial.into(),
        direction,
        remove_endpoint: remove_endpoint.into(),
        expected_remote: expected_remote.into(),
        owner: owner.into(),
    });
    Ok(())
}

pub(crate) fn forget_mapping(
    state: &AppState,
    serial: &str,
    direction: PortDirection,
    remove_endpoint: &str,
) -> Result<(), ApiError> {
    let mut mappings = state
        .port_mappings
        .lock()
        .map_err(|_| ApiError::new("state_error", "Port mapping registry lock was poisoned"))?;
    mappings.retain(|mapping| {
        !(mapping.serial == serial
            && mapping.direction == direction
            && mapping.remove_endpoint == remove_endpoint)
    });
    Ok(())
}

pub(crate) fn cleanup_managed_mappings(state: &AppState) {
    let mappings = match state.port_mappings.lock() {
        Ok(registry) => registry.clone(),
        Err(_) => {
            eprintln!("Mobius cleanup: port mapping registry lock was poisoned");
            return;
        }
    };
    for mapping in mappings {
        match managed_mapping_still_matches(&mapping) {
            Ok(true) => {
                let direction = match mapping.direction {
                    PortDirection::Forward => "forward",
                    PortDirection::Reverse => "reverse",
                };
                if let Err(error) = run_checked(
                    "adb",
                    &[
                        "-s".into(),
                        mapping.serial.clone(),
                        direction.into(),
                        "--remove".into(),
                        mapping.remove_endpoint.clone(),
                    ],
                    PORT_TIMEOUT,
                ) {
                    eprintln!(
                        "Mobius cleanup: could not remove {} mapping {}: {}",
                        mapping.owner, mapping.remove_endpoint, error.message
                    );
                } else if let Err(error) = forget_mapping(
                    state,
                    &mapping.serial,
                    mapping.direction,
                    &mapping.remove_endpoint,
                ) {
                    eprintln!(
                        "Mobius cleanup: removed mapping but could not update its registry: {}",
                        error.message
                    );
                }
            }
            Ok(false) => {
                eprintln!(
                    "Mobius cleanup: {} mapping {} changed externally and was left untouched",
                    mapping.owner, mapping.remove_endpoint
                );
                if let Err(error) = forget_mapping(
                    state,
                    &mapping.serial,
                    mapping.direction,
                    &mapping.remove_endpoint,
                ) {
                    eprintln!(
                        "Mobius cleanup: could not update mapping registry: {}",
                        error.message
                    );
                }
            }
            Err(error) => eprintln!(
                "Mobius cleanup: could not verify {} mapping {}: {}",
                mapping.owner, mapping.remove_endpoint, error.message
            ),
        }
    }
}

fn managed_mapping_still_matches(mapping: &ManagedPortMapping) -> Result<bool, ApiError> {
    let direction = match mapping.direction {
        PortDirection::Forward => "forward",
        PortDirection::Reverse => "reverse",
    };
    let output = run_checked(
        "adb",
        &[
            "-s".into(),
            mapping.serial.clone(),
            direction.into(),
            "--list".into(),
        ],
        PORT_TIMEOUT,
    )?;
    Ok(
        parse_mapping_lines(&output.stdout, mapping.direction, &mapping.serial)
            .iter()
            .any(|candidate| {
                candidate.remove_endpoint == mapping.remove_endpoint
                    && match mapping.direction {
                        PortDirection::Forward => candidate.remote == mapping.expected_remote,
                        PortDirection::Reverse => candidate.local == mapping.expected_remote,
                    }
            }),
    )
}

fn parse_mapping_lines(
    output: &str,
    direction: PortDirection,
    selected_serial: &str,
) -> Vec<PortMapping> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let (serial, first, second) = match fields.as_slice() {
                [serial, first, second, ..] => (*serial, *first, *second),
                [first, second] => (selected_serial, *first, *second),
                _ => return None,
            };
            if serial != selected_serial {
                return None;
            }
            let (local, remote, remove_endpoint) = match direction {
                PortDirection::Forward => (first, second, first),
                // adb reverse lists CLI order: device endpoint, then host endpoint.
                PortDirection::Reverse => (second, first, first),
            };
            Some(PortMapping {
                serial: serial.to_string(),
                direction,
                local: local.to_string(),
                remote: remote.to_string(),
                remove_endpoint: remove_endpoint.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_reverse_remove_endpoint() {
        let mappings = parse_mapping_lines(
            "device-1 tcp:9000 tcp:8000",
            PortDirection::Reverse,
            "device-1",
        );
        assert_eq!(mappings[0].local, "tcp:8000");
        assert_eq!(mappings[0].remote, "tcp:9000");
        assert_eq!(mappings[0].remove_endpoint, "tcp:9000");
    }

    #[test]
    fn ignores_other_device_mappings() {
        let mappings = parse_mapping_lines(
            "other-device tcp:7000 tcp:7001",
            PortDirection::Forward,
            "device-1",
        );
        assert!(mappings.is_empty());
    }
}
