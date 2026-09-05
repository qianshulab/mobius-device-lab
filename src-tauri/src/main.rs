#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    io::{Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream},
    time::Duration,
};
use zeroize::Zeroize;

fn main() {
    if let Some(success) = run_ssh_askpass_helper() {
        std::process::exit(if success { 0 } else { 1 });
    }
    mobius_device_lab_lib::run();
}

/// An explicitly configured OpenSSH-compatible fallback invokes this executable
/// as its askpass helper. The default bundled SSH client reads the same one-time
/// loopback broker directly. Neither path puts the password in arguments or the
/// long-lived child environment.
fn run_ssh_askpass_helper() -> Option<bool> {
    if std::env::var_os("MOBIUS_SSH_ASKPASS").as_deref() != Some(std::ffi::OsStr::new("1")) {
        return None;
    }

    let mut port = std::env::var("MOBIUS_SSH_ASKPASS_PORT").unwrap_or_default();
    let mut token = std::env::var("MOBIUS_SSH_ASKPASS_TOKEN").unwrap_or_default();
    std::env::remove_var("MOBIUS_SSH_ASKPASS_PORT");
    std::env::remove_var("MOBIUS_SSH_ASKPASS_ENDPOINT");
    std::env::remove_var("MOBIUS_SSH_ASKPASS_TOKEN");
    std::env::remove_var("MOBIUS_SSH_ASKPASS");
    let result = (|| {
        let address = loopback_broker_address(&port)?;
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok()?;
        stream.write_all(token.as_bytes()).ok()?;
        stream.write_all(b"\n").ok()?;
        stream.shutdown(Shutdown::Write).ok()?;
        let mut password = Vec::with_capacity(1024);
        let mut limited = (&mut stream).take(1025);
        limited.read_to_end(&mut password).ok()?;
        if password.is_empty() || password.len() > 1024 {
            password.zeroize();
            return None;
        }
        let output = std::io::stdout().write_all(&password).ok();
        password.zeroize();
        output
    })();
    port.zeroize();
    token.zeroize();
    Some(result.is_some())
}

fn loopback_broker_address(port: &str) -> Option<SocketAddr> {
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
    Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_broker_accepts_only_a_decimal_port_on_ipv4_loopback() {
        let address = loopback_broker_address("49152").expect("valid broker port");
        assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
        assert_eq!(address.port(), 49_152);
        for invalid in ["", "0", "65536", "127.0.0.1:49152", "-1", "+22", " 22"] {
            assert!(loopback_broker_address(invalid).is_none(), "{invalid}");
        }
    }
}
