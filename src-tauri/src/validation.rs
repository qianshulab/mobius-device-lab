use crate::models::{ApiError, AppResult};
use std::{net::Ipv4Addr, path::Path};

const MAX_SERIAL_LEN: usize = 128;
const MAX_REMOTE_PATH_LEN: usize = 1024;

pub(crate) fn serial(value: &str) -> AppResult<&str> {
    if value.is_empty() || value.len() > MAX_SERIAL_LEN {
        return Err(ApiError::new(
            "invalid_serial",
            "Device identifier must be between 1 and 128 bytes",
        ));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | ':' | '-' | '_'))
    {
        return Err(ApiError::new(
            "invalid_serial",
            "Device identifier contains unsupported characters",
        ));
    }
    Ok(value)
}

pub(crate) fn package_name(value: &str) -> AppResult<&str> {
    if value.is_empty() || value.len() > 255 || value.starts_with('.') || value.ends_with('.') {
        return Err(ApiError::new(
            "invalid_package_name",
            "Package identifier must be between 1 and 255 bytes",
        ));
    }
    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() < 2
        || segments.iter().any(|segment| {
            segment.is_empty()
                || !segment
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                || !segment
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        })
    {
        return Err(ApiError::new(
            "invalid_package_name",
            "Package identifier must contain dot-separated identifier segments",
        ));
    }
    Ok(value)
}

pub(crate) fn host_port(value: &str) -> AppResult<(&str, u16)> {
    if value.is_empty() || value.len() > 260 || value.chars().any(char::is_control) {
        return Err(ApiError::new("invalid_address", "Invalid host:port value"));
    }
    let (host, port_text) = if let Some(rest) = value.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| ApiError::new("invalid_address", "Invalid IPv6 address"))?;
        let host = &rest[..end];
        let suffix = &rest[end + 1..];
        let port = suffix
            .strip_prefix(':')
            .ok_or_else(|| ApiError::new("invalid_address", "Address must include a port"))?;
        (host, port)
    } else {
        value
            .rsplit_once(':')
            .ok_or_else(|| ApiError::new("invalid_address", "Address must be host:port"))?
    };
    if host.is_empty()
        || !host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '_'))
    {
        return Err(ApiError::new(
            "invalid_address",
            "Host contains unsupported characters",
        ));
    }
    let port = port_text
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| ApiError::new("invalid_port", "Port must be between 1 and 65535"))?;
    Ok((host, port))
}

pub(crate) fn host(value: &str) -> AppResult<&str> {
    if value.is_empty()
        || value.len() > 253
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '_'))
    {
        return Err(ApiError::new(
            "invalid_host",
            "Host contains unsupported characters",
        ));
    }
    Ok(value)
}

pub(crate) fn remote_path(value: &str) -> AppResult<&str> {
    if value.is_empty()
        || value.len() > MAX_REMOTE_PATH_LEN
        || !value.starts_with('/')
        || value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
    {
        return Err(ApiError::new(
            "invalid_remote_path",
            "Remote path must be an absolute path without control characters",
        ));
    }
    if value.contains("//") || value.split('/').any(|part| matches!(part, "." | "..")) {
        return Err(ApiError::new(
            "invalid_remote_path",
            "Remote path cannot contain empty, '.' or '..' segments",
        ));
    }
    Ok(value)
}

pub(crate) fn deletable_remote_path(value: &str) -> AppResult<&str> {
    let path = remote_path(value)?;
    let normalized = path.trim_end_matches('/');
    let within_sdcard = normalized
        .strip_prefix("/sdcard/")
        .is_some_and(|rest| !rest.is_empty());
    let within_tmp = normalized
        .strip_prefix("/data/local/tmp/")
        .is_some_and(|rest| !rest.is_empty());
    let within_emulated = normalized
        .strip_prefix("/storage/emulated/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(user, child)| {
            !user.is_empty() && user.chars().all(|ch| ch.is_ascii_digit()) && !child.is_empty()
        });
    if within_sdcard || within_tmp || within_emulated {
        Ok(path)
    } else {
        Err(ApiError::new(
            "protected_remote_path",
            "Delete is limited to children of /sdcard, /storage/emulated/<user>, or /data/local/tmp; use the advanced shell deliberately for other locations",
        ))
    }
}

pub(crate) fn local_existing_path(value: &str) -> AppResult<&Path> {
    let path = local_absolute_path(value)?;
    if !path.exists() {
        return Err(ApiError::new(
            "local_path_not_found",
            format!("Local path does not exist: {}", path.display()),
        ));
    }
    Ok(path)
}

pub(crate) fn local_absolute_path(value: &str) -> AppResult<&Path> {
    if value.is_empty() || value.len() > 4096 || value.chars().any(|ch| ch == '\0') {
        return Err(ApiError::new("invalid_local_path", "Invalid local path"));
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(ApiError::new(
            "invalid_local_path",
            "Local paths must be absolute",
        ));
    }
    Ok(path)
}

pub(crate) fn shell_command(value: &str) -> AppResult<&str> {
    if value.trim().is_empty()
        || value.len() > 8192
        || value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
    {
        return Err(ApiError::new(
            "invalid_shell_command",
            "Shell command must be a single non-empty line up to 8192 bytes",
        ));
    }
    Ok(value)
}

pub(crate) fn endpoint(value: &str) -> AppResult<&str> {
    if value.is_empty()
        || value.len() > 512
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || !value.contains(':')
    {
        return Err(ApiError::new(
            "invalid_endpoint",
            "ADB endpoint must use a supported kind:value form without whitespace",
        ));
    }
    if let Some(port) = value.strip_prefix("tcp:") {
        port.parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| ApiError::new("invalid_endpoint", "Invalid TCP endpoint port"))?;
        return Ok(value);
    }
    let name = ["localabstract:", "localreserved:", "localfilesystem:"]
        .into_iter()
        .find_map(|prefix| value.strip_prefix(prefix));
    let Some(name) = name else {
        return Err(ApiError::new(
            "invalid_endpoint",
            "Only tcp, localabstract, localreserved and localfilesystem endpoints are supported",
        ));
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '@' | '+' | '-'))
    {
        return Err(ApiError::new(
            "invalid_endpoint",
            "ADB local socket name contains unsupported characters",
        ));
    }
    Ok(value)
}

pub(crate) fn parse_private_cidr_24(value: &str) -> AppResult<Ipv4Addr> {
    let (address, prefix) = value
        .split_once('/')
        .ok_or_else(|| ApiError::new("invalid_cidr", "CIDR must be an IPv4 /24 network"))?;
    if prefix != "24" {
        return Err(ApiError::new(
            "invalid_cidr",
            "Only private IPv4 /24 networks are allowed",
        ));
    }
    let ip = address
        .parse::<Ipv4Addr>()
        .map_err(|_| ApiError::new("invalid_cidr", "CIDR contains an invalid IPv4 address"))?;
    if !ip.is_private() {
        return Err(ApiError::new(
            "invalid_cidr",
            "Only RFC1918 private IPv4 networks are allowed",
        ));
    }
    let octets = ip.octets();
    Ok(Ipv4Addr::new(octets[0], octets[1], octets[2], 0))
}

pub(crate) fn quote_remote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_private_24_networks() {
        assert_eq!(
            parse_private_cidr_24("192.168.8.42/24").expect("private network"),
            Ipv4Addr::new(192, 168, 8, 0)
        );
        assert!(parse_private_cidr_24("8.8.8.0/24").is_err());
        assert!(parse_private_cidr_24("10.0.0.0/16").is_err());
    }

    #[test]
    fn remote_quote_handles_single_quotes() {
        assert_eq!(quote_remote("/sdcard/it's.txt"), "'/sdcard/it'\\''s.txt'");
    }

    #[test]
    fn deletion_is_confined_to_file_manager_roots() {
        assert!(deletable_remote_path("/sdcard/Download/report.txt").is_ok());
        assert!(deletable_remote_path("/storage/emulated/0/DCIM/photo.jpg").is_ok());
        assert!(deletable_remote_path("/data/local/tmp/frida-server").is_ok());
        assert!(deletable_remote_path("/system/build.prop").is_err());
        assert!(deletable_remote_path("/sdcard/../system/build.prop").is_err());
        assert!(deletable_remote_path("/sdcard").is_err());
    }

    #[test]
    fn endpoint_parser_rejects_non_socket_services() {
        assert!(endpoint("tcp:8080").is_ok());
        assert!(endpoint("localabstract:mobius").is_ok());
        assert!(endpoint("jdwp:1234").is_err());
        assert!(endpoint("tcp:0").is_err());
    }

    #[test]
    fn validates_package_identifiers() {
        assert!(package_name("dev.mobius.device_lab").is_ok());
        assert!(package_name("dev.mobius;id").is_err());
        assert!(package_name("single").is_err());
        assert!(package_name("dev.42bad").is_err());
    }
}
