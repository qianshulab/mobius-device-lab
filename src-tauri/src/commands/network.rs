use super::blocking_api;
use crate::{
    models::{ApiError, ApiResult, AppResult, ScanEndpoint},
    validation,
};
use if_addrs::IfAddr;
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const DEFAULT_ADB_PORT: u16 = 5555;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(280);
const MAX_PORTS: usize = 16;
const MAX_WORKERS: usize = 64;
const MAX_AUTO_SUBNETS: usize = 4;
static SCAN_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Eq, PartialEq)]
struct InterfaceIpv4 {
    name: String,
    ip: Ipv4Addr,
    prefix_len: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScanSubnet {
    network: Ipv4Addr,
    local_ips: Vec<Ipv4Addr>,
    interface_name: String,
    priority: (u8, u8),
}

struct ScanGuard;

impl ScanGuard {
    fn acquire() -> AppResult<Self> {
        SCAN_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                ApiError::new(
                    "scan_already_running",
                    "A local ADB subnet scan is already running",
                )
            })?;
        Ok(Self)
    }
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        SCAN_ACTIVE.store(false, Ordering::Release);
    }
}

#[tauri::command]
pub async fn scan_adb_subnet(
    cidr: Option<String>,
    ports: Option<Vec<u16>>,
) -> ApiResult<Vec<ScanEndpoint>> {
    blocking_api(move || scan_inner(cidr.as_deref(), ports)).await
}

fn scan_inner(cidr: Option<&str>, ports: Option<Vec<u16>>) -> AppResult<Vec<ScanEndpoint>> {
    let _scan_guard = ScanGuard::acquire()?;
    let detected_subnets = detect_private_ipv4_subnets()?;
    let selected_subnets = match cidr {
        Some(value) => {
            let requested = validation::parse_private_cidr_24(value)?;
            let matching = detected_subnets
                .iter()
                .filter(|subnet| subnet.network == requested)
                .cloned()
                .collect::<Vec<_>>();
            if matching.is_empty() {
                let active = detected_subnets
                    .iter()
                    .map(|subnet| format!("{}/24", subnet.network))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ApiError::new(
                    "cidr_not_local",
                    format!(
                        "Requested subnet {requested}/24 is not an active local subnet ({active})"
                    ),
                ));
            }
            matching
        }
        None => detected_subnets
            .into_iter()
            .take(MAX_AUTO_SUBNETS)
            .collect(),
    };

    let mut ports = ports.unwrap_or_else(|| vec![DEFAULT_ADB_PORT]);
    ports.sort_unstable();
    ports.dedup();
    if ports.is_empty() || ports.len() > MAX_PORTS || ports.contains(&0) {
        return Err(ApiError::new(
            "invalid_scan_ports",
            format!("Provide between 1 and {MAX_PORTS} non-zero ports"),
        ));
    }

    let mut endpoints = Vec::new();
    for subnet in &selected_subnets {
        let subnet_endpoints = scan_subnet(subnet, &ports)?;
        let found_adb = subnet_endpoints
            .iter()
            .any(|endpoint| endpoint.state == "adb");
        endpoints.extend(subnet_endpoints);
        if cidr.is_some() || found_adb {
            break;
        }
    }

    endpoints.sort_by_key(|entry| (entry.address.parse::<Ipv4Addr>().ok(), entry.port));
    Ok(endpoints)
}

fn scan_subnet(subnet: &ScanSubnet, ports: &[u16]) -> AppResult<Vec<ScanEndpoint>> {
    let local_ips = subnet.local_ips.iter().copied().collect::<HashSet<_>>();
    let octets = subnet.network.octets();
    let mut pending = VecDeque::with_capacity(254 * ports.len());
    for host in 1..=254_u8 {
        let ip = Ipv4Addr::new(octets[0], octets[1], octets[2], host);
        if local_ips.contains(&ip) {
            continue;
        }
        for port in ports {
            pending.push_back((ip, *port));
        }
    }

    let pending = Arc::new(Mutex::new(pending));
    let found = Arc::new(Mutex::new(Vec::new()));
    let workers = MAX_WORKERS.min(pending.lock().map(|queue| queue.len()).unwrap_or_default());
    thread::scope(|scope| {
        for _ in 0..workers {
            let pending = Arc::clone(&pending);
            let found = Arc::clone(&found);
            scope.spawn(move || loop {
                let target = pending.lock().ok().and_then(|mut queue| queue.pop_front());
                let Some((ip, port)) = target else { break };
                let address = SocketAddr::new(IpAddr::V4(ip), port);
                if let Some((adb_detected, latency_ms)) = probe_endpoint(address) {
                    if let Ok(mut endpoints) = found.lock() {
                        endpoints.push(ScanEndpoint {
                            address: ip.to_string(),
                            port,
                            latency_ms,
                            state: if adb_detected { "adb" } else { "open" }.into(),
                        });
                    }
                }
            });
        }
    });

    let endpoints = found
        .lock()
        .map_err(|_| ApiError::new("scan_failed", "Scan result lock was poisoned"))?
        .clone();
    Ok(endpoints)
}

fn probe_endpoint(address: SocketAddr) -> Option<(bool, u64)> {
    let started = Instant::now();
    let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).ok()?;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(450)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(450)));

    const A_CNXN: u32 = 0x4e58_4e43;
    const A_AUTH: u32 = 0x4854_5541;
    const A_STLS: u32 = 0x534c_5453;
    let payload = b"host::mobius\0";
    let checksum = payload.iter().map(|byte| u32::from(*byte)).sum::<u32>();
    let mut packet = Vec::with_capacity(24 + payload.len());
    for value in [
        A_CNXN,
        0x0100_0000,
        4096,
        payload.len() as u32,
        checksum,
        A_CNXN ^ u32::MAX,
    ] {
        packet.extend_from_slice(&value.to_le_bytes());
    }
    packet.extend_from_slice(payload);
    if stream.write_all(&packet).is_err() {
        return Some((false, latency_ms));
    }
    let mut response = [0_u8; 24];
    if stream.read_exact(&mut response).is_err() {
        return Some((false, latency_ms));
    }
    let command = u32::from_le_bytes(response[0..4].try_into().ok()?);
    let magic = u32::from_le_bytes(response[20..24].try_into().ok()?);
    let adb_detected = magic == command ^ u32::MAX && matches!(command, A_CNXN | A_AUTH | A_STLS);
    Some((adb_detected, latency_ms))
}

fn detect_private_ipv4_subnets() -> AppResult<Vec<ScanSubnet>> {
    let interfaces = if_addrs::get_if_addrs().map_err(|error| {
        ApiError::new(
            "network_detection_failed",
            format!("Unable to enumerate local network interfaces: {error}"),
        )
    })?;

    let addresses = interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            IfAddr::V4(address) => Some(InterfaceIpv4 {
                name: interface.name,
                ip: address.ip,
                prefix_len: address.prefixlen,
            }),
            IfAddr::V6(_) => None,
        })
        .collect::<Vec<_>>();
    let subnets = select_private_ipv4_subnets(&addresses, route_ipv4_hint());
    if subnets.is_empty() {
        return Err(ApiError::new(
            "no_private_network",
            "No active RFC1918 IPv4 LAN or Wi-Fi interface was found",
        ));
    }
    Ok(subnets)
}

fn route_ipv4_hint() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect(("1.1.1.1", 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

fn select_private_ipv4_subnets(
    interfaces: &[InterfaceIpv4],
    route_hint: Option<Ipv4Addr>,
) -> Vec<ScanSubnet> {
    let mut subnets = BTreeMap::<Ipv4Addr, ScanSubnet>::new();
    for interface in interfaces {
        if !is_rfc1918(interface.ip)
            || !(1..=30).contains(&interface.prefix_len)
            || is_virtual_interface(&interface.name)
        {
            continue;
        }

        let octets = interface.ip.octets();
        let network = Ipv4Addr::new(octets[0], octets[1], octets[2], 0);
        let physical_priority = physical_interface_priority(&interface.name);
        let trusted_route_match = physical_priority < 2 && route_hint == Some(interface.ip);
        let priority = (u8::from(!trusted_route_match), physical_priority);
        let subnet = subnets.entry(network).or_insert_with(|| ScanSubnet {
            network,
            local_ips: Vec::new(),
            interface_name: interface.name.clone(),
            priority,
        });
        if !subnet.local_ips.contains(&interface.ip) {
            subnet.local_ips.push(interface.ip);
        }
        if priority < subnet.priority {
            subnet.interface_name.clone_from(&interface.name);
            subnet.priority = priority;
        }
    }

    let mut subnets = subnets.into_values().collect::<Vec<_>>();
    for subnet in &mut subnets {
        subnet.local_ips.sort_unstable();
    }
    subnets.sort_by_key(|subnet| (subnet.priority, subnet.network));
    subnets
}

fn is_rfc1918(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
}

fn physical_interface_priority(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    if name == "en0"
        || name.starts_with("wl")
        || name.contains("wi-fi")
        || name.contains("wifi")
        || name.contains("wireless")
        || name == "wlan"
        || name.contains("无线")
    {
        return 0;
    }
    if name.starts_with("en")
        || name.starts_with("eth")
        || name.contains("ethernet")
        || name.contains("local area connection")
        || name.contains("以太网")
        || name.contains("本地连接")
    {
        return 1;
    }
    2
}

fn is_virtual_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let exact_or_prefix = [
        "utun",
        "tun",
        "tap",
        "ppp",
        "ipsec",
        "wg",
        "docker",
        "veth",
        "virbr",
        "vmnet",
        "vbox",
        "br-",
        "bridge",
        "podman",
        "cni",
        "flannel",
        "awdl",
        "llw",
        "anpi",
        "gif",
        "stf",
        "tailscale",
        "zerotier",
        "zt",
        "ham",
        "dummy",
        "ifb",
    ];
    if name == "lo"
        || name == "lo0"
        || exact_or_prefix
            .iter()
            .any(|prefix| name.starts_with(prefix))
    {
        return true;
    }

    [
        "loopback",
        "virtual",
        "hyper-v",
        "vethernet",
        "vmware",
        "virtualbox",
        "wsl",
        "vpn",
        "wireguard",
        "tunnel",
        "tap-windows",
        "tailscale",
        "zerotier",
        "bluetooth",
    ]
    .iter()
    .any(|keyword| name.contains(keyword))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(name: &str, ip: [u8; 4], prefix_len: u8) -> InterfaceIpv4 {
        InterfaceIpv4 {
            name: name.into(),
            ip: Ipv4Addr::from(ip),
            prefix_len,
        }
    }

    #[test]
    fn accepts_only_rfc1918_addresses() {
        assert!(is_rfc1918(Ipv4Addr::new(10, 20, 30, 40)));
        assert!(is_rfc1918(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_rfc1918(Ipv4Addr::new(172, 31, 255, 254)));
        assert!(is_rfc1918(Ipv4Addr::new(192, 168, 100, 2)));
        assert!(!is_rfc1918(Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!is_rfc1918(Ipv4Addr::new(198, 18, 0, 1)));
        assert!(!is_rfc1918(Ipv4Addr::new(198, 19, 255, 254)));
    }

    #[test]
    fn ignores_benchmark_tunnel_and_virtual_interfaces() {
        let interfaces = vec![
            interface("utun4", [198, 18, 0, 1], 15),
            interface("Tailscale0", [100, 100, 2, 3], 32),
            interface("Docker Desktop", [192, 168, 65, 1], 24),
            interface("vEthernet (WSL)", [172, 25, 16, 1], 20),
            interface("bridge100", [10, 211, 55, 2], 24),
            interface("bridge101", [10, 37, 129, 2], 24),
            interface("en0", [192, 168, 100, 23], 24),
        ];

        let subnets = select_private_ipv4_subnets(&interfaces, Some(Ipv4Addr::new(198, 18, 0, 1)));
        assert_eq!(subnets.len(), 1);
        assert_eq!(subnets[0].network, Ipv4Addr::new(192, 168, 100, 0));
        assert_eq!(subnets[0].interface_name, "en0");
    }

    #[test]
    fn returns_multiple_physical_subnets_in_stable_priority_order() {
        let interfaces = vec![
            interface("Ethernet", [10, 20, 30, 8], 24),
            interface("en7", [192, 168, 50, 9], 24),
            interface("Wi-Fi", [192, 168, 100, 23], 24),
            interface("Wi-Fi", [192, 168, 100, 24], 24),
        ];

        let subnets =
            select_private_ipv4_subnets(&interfaces, Some(Ipv4Addr::new(192, 168, 100, 23)));
        assert_eq!(subnets.len(), 3);
        assert_eq!(subnets[0].network, Ipv4Addr::new(192, 168, 100, 0));
        assert_eq!(
            subnets[0].local_ips,
            vec![
                Ipv4Addr::new(192, 168, 100, 23),
                Ipv4Addr::new(192, 168, 100, 24)
            ]
        );
        assert_eq!(subnets[1].network, Ipv4Addr::new(10, 20, 30, 0));
        assert_eq!(subnets[2].network, Ipv4Addr::new(192, 168, 50, 0));
    }

    #[test]
    fn trusted_active_ethernet_route_beats_an_inactive_wifi_address() {
        let interfaces = vec![
            interface("Wi-Fi", [192, 168, 20, 8], 24),
            interface("Ethernet", [192, 168, 100, 56], 24),
        ];

        let subnets =
            select_private_ipv4_subnets(&interfaces, Some(Ipv4Addr::new(192, 168, 100, 56)));
        assert_eq!(subnets[0].network, Ipv4Addr::new(192, 168, 100, 0));
    }

    #[test]
    fn drops_point_to_point_prefixes_used_by_tunnels() {
        let interfaces = vec![
            interface("mystery0", [10, 0, 0, 2], 32),
            interface("en0", [192, 168, 1, 20], 24),
        ];

        let subnets = select_private_ipv4_subnets(&interfaces, None);
        assert_eq!(subnets.len(), 1);
        assert_eq!(subnets[0].network, Ipv4Addr::new(192, 168, 1, 0));
    }

    #[test]
    #[ignore = "requires an explicitly authorized live Android TCP device"]
    fn live_auto_scan_finds_the_authorized_android_device() {
        let serial = std::env::var("MOBIUS_LIVE_ANDROID_SERIAL")
            .expect("set MOBIUS_LIVE_ANDROID_SERIAL to the authorized host:port");
        let (address, port) = serial
            .rsplit_once(':')
            .expect("authorized serial must be an IPv4 host:port");
        let expected_ip = address
            .parse::<Ipv4Addr>()
            .expect("authorized serial must contain an IPv4 address");
        assert!(is_rfc1918(expected_ip));
        let port = port
            .parse::<u16>()
            .expect("authorized serial must contain a TCP port");

        let endpoints = scan_inner(None, Some(vec![port])).expect("automatic LAN scan");
        assert!(endpoints.iter().any(|endpoint| {
            endpoint.address == expected_ip.to_string()
                && endpoint.port == port
                && endpoint.state == "adb"
        }));
    }
}
