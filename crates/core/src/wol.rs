use std::net::IpAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;

use crate::config::Machine;
use crate::error::{OrchestratorError, Result};
use crate::ssh::SshRunner;
use crate::tmux;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineStatus {
    Online,
    Sleeping,
    Unreachable,
    Waking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakePolicy {
    pub initial_wait: Duration,
    pub retry_interval: Duration,
    pub max_retries: u32,
}

impl Default for WakePolicy {
    fn default() -> Self {
        Self {
            initial_wait: Duration::from_secs(20),
            retry_interval: Duration::from_secs(15),
            max_retries: 5,
        }
    }
}

pub fn parse_mac(mac: &str) -> Result<[u8; 6]> {
    let parts: Vec<&str> = mac.split([':', '-']).collect();
    if parts.len() != 6 || parts.iter().any(|p| p.len() != 2) {
        return Err(OrchestratorError::ConfigError(format!(
            "invalid MAC `{mac}`"
        )));
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16)
            .map_err(|_| OrchestratorError::ConfigError(format!("invalid MAC `{mac}`")))?;
    }
    Ok(out)
}

pub fn magic_packet(mac: &str) -> Result<[u8; 102]> {
    let bytes = parse_mac(mac)?;
    let mut pkt = [0xFFu8; 102];
    for i in 0..16 {
        pkt[6 + i * 6..12 + i * 6].copy_from_slice(&bytes);
    }
    Ok(pkt)
}

pub async fn wake(mac: &str) -> Result<()> {
    send_magic(mac, (std::net::Ipv4Addr::BROADCAST.into(), 9), true).await
}

async fn send_magic(mac: &str, target: (IpAddr, u16), broadcast: bool) -> Result<()> {
    let pkt = magic_packet(mac)?;
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.set_broadcast(broadcast)?;
    sock.send_to(&pkt, target).await?;
    Ok(())
}

pub async fn connect_with_wake(
    ssh: &dyn SshRunner,
    m: &Machine,
    policy: &WakePolicy,
    on_status: &mut dyn FnMut(MachineStatus),
) -> Result<()> {
    let mac = match (tmux::handshake(ssh, m).await, m.mac.as_deref()) {
        (Ok(()), _) => {
            on_status(MachineStatus::Online);
            return Ok(());
        }
        (Err(OrchestratorError::HostUnreachable), Some(mac)) => mac,
        (Err(e), _) => {
            on_status(MachineStatus::Unreachable);
            return Err(e);
        }
    };
    on_status(MachineStatus::Waking);
    if let Err(e) = wake(mac).await {
        on_status(MachineStatus::Unreachable);
        return Err(e);
    }
    tokio::time::sleep(policy.initial_wait).await;
    for attempt in 0..policy.max_retries {
        match tmux::handshake(ssh, m).await {
            Ok(()) => {
                on_status(MachineStatus::Online);
                return Ok(());
            }
            Err(OrchestratorError::HostUnreachable) => {}
            Err(e) => {
                on_status(MachineStatus::Unreachable);
                return Err(e);
            }
        }
        if attempt + 1 < policy.max_retries {
            tokio::time::sleep(policy.retry_interval).await;
        }
    }
    on_status(MachineStatus::Unreachable);
    Err(OrchestratorError::WakeTimeout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MachineOs;
    use crate::ssh::mock::MockSsh;
    use std::path::PathBuf;

    fn m(mac: Option<&str>) -> Machine {
        Machine {
            name: "w1".into(),
            host: "h".into(),
            mac: mac.map(String::from),
            os: MachineOs::Linux,
            ssh_user: "u".into(),
            ssh_port: 22,
            ssh_key: PathBuf::from("/k"),
            default_session: "main".into(),
        }
    }

    fn fast() -> WakePolicy {
        WakePolicy {
            initial_wait: Duration::from_millis(1),
            retry_interval: Duration::from_millis(1),
            max_retries: 3,
        }
    }

    #[test]
    fn parse_mac_accepts_colon_dash_and_case() {
        let want = [0xAA, 0xBB, 0xCC, 0x01, 0x02, 0x03];
        assert_eq!(parse_mac("AA:BB:CC:01:02:03").unwrap(), want);
        assert_eq!(parse_mac("aa-bb-cc-01-02-03").unwrap(), want);
    }

    #[test]
    fn parse_mac_rejects_garbage() {
        for bad in [
            "",
            "AA:BB:CC",
            "ZZ:BB:CC:01:02:03",
            "AABBCC010203",
            "A:B:C:1:2:3",
        ] {
            assert!(
                matches!(parse_mac(bad), Err(OrchestratorError::ConfigError(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn magic_packet_layout() {
        let p = magic_packet("AA:BB:CC:01:02:03").unwrap();
        assert!(p[..6].iter().all(|&b| b == 0xFF));
        for i in 0..16 {
            assert_eq!(
                &p[6 + i * 6..12 + i * 6],
                &[0xAA, 0xBB, 0xCC, 0x01, 0x02, 0x03]
            );
        }
    }

    #[tokio::test]
    async fn send_magic_delivers_102_byte_packet() {
        // Exercise the real send path without touching the LAN: bind a
        // loopback socket to receive on, then send through `send_magic`
        // (non-broadcast) to that socket's address.
        let recv_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target_addr = recv_sock.local_addr().unwrap();

        let mac = "AA:BB:CC:01:02:03";
        send_magic(mac, (target_addr.ip(), target_addr.port()), false)
            .await
            .unwrap();

        let mut buf = [0u8; 256];
        let (n, _) = recv_sock.recv_from(&mut buf).await.unwrap();
        assert_eq!(n, 102);
        assert_eq!(&buf[..n], &magic_packet(mac).unwrap()[..]);
    }

    #[tokio::test]
    async fn online_first_try_emits_only_online() {
        let ssh = MockSsh::new();
        ssh.push_ok(0, "tmux 3.4", "");
        let mut seen = vec![];
        connect_with_wake(&ssh, &m(Some("AA:BB:CC:01:02:03")), &fast(), &mut |s| {
            seen.push(s)
        })
        .await
        .unwrap();
        assert_eq!(seen, vec![MachineStatus::Online]);
        assert_eq!(ssh.calls().len(), 1);
    }

    #[tokio::test]
    async fn no_mac_goes_straight_to_unreachable() {
        let ssh = MockSsh::new();
        ssh.push_err(OrchestratorError::HostUnreachable);
        let mut seen = vec![];
        let r = connect_with_wake(&ssh, &m(None), &fast(), &mut |s| seen.push(s)).await;
        assert_eq!(r.unwrap_err(), OrchestratorError::HostUnreachable);
        assert_eq!(seen, vec![MachineStatus::Unreachable]);
        assert_eq!(ssh.calls().len(), 1);
    }

    #[tokio::test]
    async fn auth_failure_does_not_trigger_wake() {
        let ssh = MockSsh::new();
        ssh.push_err(OrchestratorError::AuthFailed);
        let mut seen = vec![];
        let r = connect_with_wake(&ssh, &m(Some("AA:BB:CC:01:02:03")), &fast(), &mut |s| {
            seen.push(s)
        })
        .await;
        assert_eq!(r.unwrap_err(), OrchestratorError::AuthFailed);
        assert_eq!(seen, vec![MachineStatus::Unreachable]);
    }

    #[tokio::test]
    async fn wakes_then_online_on_second_retry() {
        let ssh = MockSsh::new();
        ssh.push_err(OrchestratorError::HostUnreachable) // initial
            .push_err(OrchestratorError::HostUnreachable) // retry 1
            .push_ok(0, "tmux 3.4", ""); // retry 2
        let mut seen = vec![];
        connect_with_wake(&ssh, &m(Some("AA:BB:CC:01:02:03")), &fast(), &mut |s| {
            seen.push(s)
        })
        .await
        .unwrap();
        assert_eq!(seen, vec![MachineStatus::Waking, MachineStatus::Online]);
        assert_eq!(ssh.calls().len(), 3);
    }

    #[tokio::test]
    async fn exhausts_retries_into_wake_timeout() {
        let ssh = MockSsh::new();
        for _ in 0..4 {
            ssh.push_err(OrchestratorError::HostUnreachable);
        }
        let mut seen = vec![];
        let r = connect_with_wake(&ssh, &m(Some("AA:BB:CC:01:02:03")), &fast(), &mut |s| {
            seen.push(s)
        })
        .await;
        assert_eq!(r.unwrap_err(), OrchestratorError::WakeTimeout);
        assert_eq!(
            seen,
            vec![MachineStatus::Waking, MachineStatus::Unreachable]
        );
        assert_eq!(ssh.calls().len(), 4); // 1 initial + 3 retries
    }

    #[tokio::test]
    async fn invalid_mac_is_config_error_after_waking_status() {
        let ssh = MockSsh::new();
        ssh.push_err(OrchestratorError::HostUnreachable);
        let mut seen = vec![];
        let r = connect_with_wake(&ssh, &m(Some("nope")), &fast(), &mut |s| seen.push(s)).await;
        assert!(matches!(r.unwrap_err(), OrchestratorError::ConfigError(_)));
        assert_eq!(
            seen,
            vec![MachineStatus::Waking, MachineStatus::Unreachable]
        );
    }
}
