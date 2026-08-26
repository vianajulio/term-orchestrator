use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

use crate::config::MachineOs;

pub const STEP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub reachable: bool,
    pub mac: Option<String>,
    pub os_hint: MachineOs,
    pub hostname_hint: Option<String>,
    pub ssh_banner: Option<String>,
}

pub fn normalize_mac(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.trim().split([':', '-']).collect();
    if parts.len() != 6
        || parts
            .iter()
            .any(|p| p.len() != 2 || u8::from_str_radix(p, 16).is_err())
    {
        return None;
    }
    Some(
        parts
            .iter()
            .map(|p| p.to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn find_mac_for_ip(out: &str, ip: IpAddr) -> Option<String> {
    let target = ip.to_string();
    out.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let addr = cols.next()?;
        if addr != target {
            return None;
        }
        cols.find_map(normalize_mac)
    })
}

pub fn parse_arp_linux(out: &str, ip: IpAddr) -> Option<String> {
    find_mac_for_ip(out, ip)
}

pub fn parse_arp_windows(out: &str, ip: IpAddr) -> Option<String> {
    find_mac_for_ip(out, ip)
}

pub fn os_hint_from_banner(banner: &str) -> MachineOs {
    let b = banner.to_ascii_lowercase();
    if b.contains("windows") {
        MachineOs::WindowsWsl
    } else if b.contains("ubuntu")
        || b.contains("debian")
        || b.contains("fedora")
        || b.contains("arch")
    {
        MachineOs::Linux
    } else {
        MachineOs::Unknown
    }
}

async fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(STEP_TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn ping(ip: IpAddr) -> bool {
    let target = ip.to_string();
    #[cfg(target_os = "windows")]
    let args = ["-n", "1", "-w", "1500", target.as_str()];
    #[cfg(not(target_os = "windows"))]
    let args = ["-c", "1", "-W", "1", target.as_str()];
    let mut cmd = tokio::process::Command::new("ping");
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    matches!(tokio::time::timeout(STEP_TIMEOUT, cmd.status()).await, Ok(Ok(s)) if s.success())
}

async fn arp_lookup(ip: IpAddr) -> Option<String> {
    let target = ip.to_string();
    #[cfg(target_os = "windows")]
    {
        let out = run_capture("arp", &["-a", &target]).await?;
        parse_arp_windows(&out, ip)
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(out) = run_capture("arp", &["-n", &target]).await {
            if let Some(mac) = parse_arp_linux(&out, ip) {
                return Some(mac);
            }
        }
        // Fallback: iproute2 (`ip neigh`) — format: "<ip> dev eth0 lladdr aa:bb:.. REACHABLE"
        let out = run_capture("ip", &["neigh", "show", &target]).await?;
        out.split_whitespace().find_map(normalize_mac)
    }
}

async fn ssh_banner(ip: IpAddr) -> Option<String> {
    let mut stream = tokio::time::timeout(STEP_TIMEOUT, TcpStream::connect((ip, 22)))
        .await
        .ok()?
        .ok()?;
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(STEP_TIMEOUT, stream.read(&mut buf))
        .await
        .ok()?
        .ok()?;
    let line = String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()?
        .trim()
        .to_string();
    if line.starts_with("SSH-") {
        Some(line)
    } else {
        None
    }
}

async fn reverse_dns(ip: IpAddr) -> Option<String> {
    let fut = tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&ip).ok());
    tokio::time::timeout(STEP_TIMEOUT, fut)
        .await
        .ok()?
        .ok()?
        .filter(|h| h != &ip.to_string())
}

pub async fn discover(ip: IpAddr) -> DiscoveryResult {
    let (pinged, banner, host) = tokio::join!(ping(ip), ssh_banner(ip), reverse_dns(ip));
    // ARP after ping so the cache is populated.
    let mac = arp_lookup(ip).await;
    DiscoveryResult {
        reachable: pinged || banner.is_some(),
        mac,
        os_hint: banner
            .as_deref()
            .map(os_hint_from_banner)
            .unwrap_or_default(),
        hostname_hint: host,
        ssh_banner: banner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = include_str!("../tests/fixtures/arp_linux.txt");
    const WINDOWS: &str = include_str!("../tests/fixtures/arp_windows.txt");

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn normalize_mac_forms() {
        assert_eq!(
            normalize_mac("aa-bb-cc-dd-ee-ff").as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(
            normalize_mac("aa:bb:cc:dd:ee:ff").as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(normalize_mac("(incomplete)"), None);
        assert_eq!(normalize_mac("aa:bb:cc"), None);
    }

    #[test]
    fn arp_linux_finds_ip() {
        assert_eq!(
            parse_arp_linux(LINUX, ip("192.168.1.50")).as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(parse_arp_linux(LINUX, ip("192.168.1.77")), None);
        assert_eq!(
            parse_arp_linux(LINUX, ip("192.168.1.5")),
            None,
            "must not prefix-match .50"
        );
    }

    #[test]
    fn arp_windows_finds_ip() {
        assert_eq!(
            parse_arp_windows(WINDOWS, ip("192.168.1.50")).as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(parse_arp_windows(WINDOWS, ip("192.168.1.5")), None);
    }

    #[test]
    fn os_hint_heuristics() {
        assert_eq!(
            os_hint_from_banner("SSH-2.0-OpenSSH_9.6p1 Ubuntu-3ubuntu13"),
            MachineOs::Linux
        );
        assert_eq!(
            os_hint_from_banner("SSH-2.0-OpenSSH_9.2p1 Debian-2"),
            MachineOs::Linux
        );
        assert_eq!(
            os_hint_from_banner("SSH-2.0-OpenSSH_for_Windows_9.5"),
            MachineOs::WindowsWsl
        );
        assert_eq!(
            os_hint_from_banner("SSH-2.0-OpenSSH_9.6"),
            MachineOs::Unknown
        );
    }

    #[tokio::test]
    #[serial_test::serial(path_env)]
    async fn discover_unroutable_ip_is_not_reachable_and_does_not_hang() {
        // 192.0.2.0/24 is TEST-NET-1, never routable.
        let start = std::time::Instant::now();
        let r = discover(ip("192.0.2.1")).await;
        assert!(!r.reachable);
        assert_eq!(r.ssh_banner, None);
        assert!(
            start.elapsed() < Duration::from_secs(6),
            "steps must run in parallel with 2s timeouts"
        );
    }
}
