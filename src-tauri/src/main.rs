#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write;
use zeroize::Zeroize;

fn main() {
    if let Some(success) = run_ssh_askpass_helper() {
        std::process::exit(if success { 0 } else { 1 });
    }
    mobius_device_lab_lib::run();
}

/// OpenSSH invokes this same executable as its askpass helper for password-mode
/// sessions. The secret arrives only in the child environment, is removed before
/// output, and its Rust allocation is wiped before this helper exits.
fn run_ssh_askpass_helper() -> Option<bool> {
    if std::env::var_os("MOBIUS_SSH_ASKPASS").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return None;
    }

    let mut password = std::env::var("MOBIUS_SSH_PASSWORD").unwrap_or_default();
    std::env::remove_var("MOBIUS_SSH_PASSWORD");
    std::env::remove_var("MOBIUS_SSH_ASKPASS");
    let result = std::io::stdout().write_all(password.as_bytes());
    password.zeroize();
    Some(result.is_ok())
}
