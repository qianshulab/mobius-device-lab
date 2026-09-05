mod android_apps;
mod devices;
mod files;
mod frida;
mod ios_apps;
mod ios_diagnostics;
mod ios_frida;
mod ios_host_tools;
mod ios_ports;
mod ios_ssh;
mod media;
mod network;
mod packages;
mod ports;
mod scrcpy;
mod tools;

pub use android_apps::*;
pub use devices::*;
pub use files::*;
pub use frida::*;
pub use ios_apps::*;
pub use ios_diagnostics::*;
pub use ios_frida::*;
pub use ios_host_tools::*;
pub use ios_ports::*;
pub use ios_ssh::*;
pub use media::*;
pub use network::*;
pub use packages::*;
pub use ports::*;
pub use scrcpy::*;
pub use tools::*;

use crate::{
    models::{ApiError, ApiResult, AppResult},
    runner::elapsed_ms,
};
use std::time::Instant;

async fn blocking_api<T, F>(operation: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> AppResult<T> + Send + 'static,
{
    let started = Instant::now();
    let result = match tauri::async_runtime::spawn_blocking(operation).await {
        Ok(result) => result,
        Err(error) => Err(ApiError::new(
            "task_join_error",
            format!("Background operation failed: {error}"),
        )),
    };
    ApiResult::from_result(result, elapsed_ms(started))
}
