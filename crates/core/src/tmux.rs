use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Machine;
use crate::error::{OrchestratorError, Result};
use crate::ssh::{CmdOutput, SshRunner};

pub const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Controller {
    Human,
    Agent,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    pub controller: Controller,
}

pub fn validate_session_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(OrchestratorError::InvalidSessionName(name.to_string()))
    }
}

pub fn parse_ls(stdout: &str) -> Vec<SessionInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('|');
            let name = parts.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let windows: u32 = parts.next()?.trim().parse().ok()?;
            let attached: u32 = parts.next()?.trim().parse().ok()?;
            Some(SessionInfo {
                name: name.to_string(),
                windows,
                attached: attached > 0,
                controller: Controller::Unknown,
            })
        })
        .collect()
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn check_exit(out: &CmdOutput) -> Result<()> {
    if out.code == 0 {
        Ok(())
    } else {
        Err(crate::ssh::classify_failure(out.code, &out.stderr))
    }
}

pub async fn handshake(ssh: &dyn SshRunner, m: &Machine) -> Result<()> {
    let out = ssh.run(m, "tmux -V", TIMEOUT).await?;
    check_exit(&out)
}

pub async fn list_sessions(ssh: &dyn SshRunner, m: &Machine) -> Result<Vec<SessionInfo>> {
    let out = ssh
        .run(
            m,
            r##"tmux ls -F "#{session_name}|#{session_windows}|#{session_attached}""##,
            TIMEOUT,
        )
        .await?;
    if out.code == 0 {
        return Ok(parse_ls(&out.stdout));
    }
    if out.code == 1 && out.stderr.contains("no server running") {
        return Ok(Vec::new());
    }
    if out.code == 1 && out.stderr.contains("error connecting to") {
        return Ok(Vec::new());
    }
    Err(crate::ssh::classify_failure(out.code, &out.stderr))
}

pub async fn create_session(ssh: &dyn SshRunner, m: &Machine, name: &str) -> Result<()> {
    validate_session_name(name)?;
    let out = ssh
        .run(m, &format!("tmux new-session -d -s {name}"), TIMEOUT)
        .await?;
    check_exit(&out)
}

pub async fn kill_session(ssh: &dyn SshRunner, m: &Machine, name: &str) -> Result<()> {
    validate_session_name(name)?;
    let out = ssh
        .run(m, &format!("tmux kill-session -t {name}"), TIMEOUT)
        .await?;
    check_exit(&out)
}

pub async fn capture_pane(
    ssh: &dyn SshRunner,
    m: &Machine,
    name: &str,
    lines: u32,
) -> Result<String> {
    validate_session_name(name)?;
    let out = ssh
        .run(
            m,
            &format!("tmux capture-pane -p -t {name} -S -{lines}"),
            TIMEOUT,
        )
        .await?;
    check_exit(&out)?;
    Ok(out.stdout)
}

pub async fn send_keys(ssh: &dyn SshRunner, m: &Machine, name: &str, keys: &str) -> Result<()> {
    validate_session_name(name)?;
    let cmd = format!(
        "tmux send-keys -t {name} {} Enter",
        shell_single_quote(keys)
    );
    let out = ssh.run(m, &cmd, TIMEOUT).await?;
    check_exit(&out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MachineOs;
    use crate::ssh::mock::MockSsh;
    use std::path::PathBuf;

    fn m() -> Machine {
        Machine {
            name: "w1".into(),
            host: "h".into(),
            mac: None,
            os: MachineOs::Linux,
            ssh_user: "u".into(),
            ssh_port: 22,
            ssh_key: PathBuf::from("/k"),
            default_session: "main".into(),
        }
    }

    #[test]
    fn valid_names_pass() {
        for n in ["main", "dev-1", "a_b", "X"] {
            assert!(validate_session_name(n).is_ok(), "{n}");
        }
    }

    #[test]
    fn invalid_names_fail() {
        for n in ["", "a b", "x;rm -rf /", "$(id)", "ção", &"a".repeat(65)] {
            assert!(
                matches!(
                    validate_session_name(n),
                    Err(OrchestratorError::InvalidSessionName(_))
                ),
                "{n}"
            );
        }
    }

    #[test]
    fn parse_ls_lines() {
        let out = "main|3|1\ndev|1|0\n";
        let s = parse_ls(out);
        assert_eq!(
            s,
            vec![
                SessionInfo {
                    name: "main".into(),
                    windows: 3,
                    attached: true,
                    controller: Controller::Unknown
                },
                SessionInfo {
                    name: "dev".into(),
                    windows: 1,
                    attached: false,
                    controller: Controller::Unknown
                },
            ]
        );
    }

    #[test]
    fn parse_ls_skips_malformed() {
        assert_eq!(parse_ls("garbage\n|||\nok|2|0\n").len(), 1);
        assert!(parse_ls("").is_empty());
    }

    #[tokio::test]
    async fn list_sessions_no_server_is_empty() {
        let ssh = MockSsh::new();
        ssh.push_ok(1, "", "no server running on /tmp/tmux-1000/default");
        assert_eq!(list_sessions(&ssh, &m()).await.unwrap(), vec![]);
        assert_eq!(
            ssh.calls(),
            vec![r##"tmux ls -F "#{session_name}|#{session_windows}|#{session_attached}""##]
        );
    }

    #[tokio::test]
    async fn list_sessions_tmux_missing() {
        let ssh = MockSsh::new();
        ssh.push_ok(127, "", "bash: tmux: command not found");
        assert_eq!(
            list_sessions(&ssh, &m()).await.unwrap_err(),
            OrchestratorError::TmuxMissing
        );
    }

    #[tokio::test]
    async fn list_sessions_propagates_transport_error() {
        let ssh = MockSsh::new();
        ssh.push_err(OrchestratorError::AuthFailed);
        assert_eq!(
            list_sessions(&ssh, &m()).await.unwrap_err(),
            OrchestratorError::AuthFailed
        );
    }

    #[tokio::test]
    async fn create_kill_capture_send_build_commands() {
        let ssh = MockSsh::new();
        ssh.push_ok(0, "", "")
            .push_ok(0, "", "")
            .push_ok(0, "line1\nline2\n", "")
            .push_ok(0, "", "");
        create_session(&ssh, &m(), "dev").await.unwrap();
        kill_session(&ssh, &m(), "dev").await.unwrap();
        let text = capture_pane(&ssh, &m(), "dev", 50).await.unwrap();
        assert_eq!(text, "line1\nline2\n");
        send_keys(&ssh, &m(), "dev", "ls -la").await.unwrap();
        assert_eq!(
            ssh.calls(),
            vec![
                "tmux new-session -d -s dev",
                "tmux kill-session -t dev",
                "tmux capture-pane -p -t dev -S -50",
                "tmux send-keys -t dev 'ls -la' Enter",
            ]
        );
    }

    #[tokio::test]
    async fn send_keys_escapes_single_quotes() {
        let ssh = MockSsh::new();
        ssh.push_ok(0, "", "");
        send_keys(&ssh, &m(), "dev", "echo 'hi'").await.unwrap();
        assert_eq!(
            ssh.calls()[0],
            r#"tmux send-keys -t dev 'echo '\''hi'\''' Enter"#
        );
    }

    #[tokio::test]
    async fn ops_reject_bad_session_name_before_ssh() {
        let ssh = MockSsh::new();
        assert!(matches!(
            create_session(&ssh, &m(), "a b").await,
            Err(OrchestratorError::InvalidSessionName(_))
        ));
        assert!(ssh.calls().is_empty());
    }

    #[tokio::test]
    async fn nonzero_exit_becomes_io_error() {
        let ssh = MockSsh::new();
        ssh.push_ok(1, "", "can't find session: dev");
        assert_eq!(
            kill_session(&ssh, &m(), "dev").await.unwrap_err(),
            OrchestratorError::Io("can't find session: dev".into())
        );
    }

    #[tokio::test]
    async fn handshake_ok_and_missing() {
        let ssh = MockSsh::new();
        ssh.push_ok(0, "tmux 3.4\n", "")
            .push_ok(127, "", "tmux: not found");
        handshake(&ssh, &m()).await.unwrap();
        assert_eq!(
            handshake(&ssh, &m()).await.unwrap_err(),
            OrchestratorError::TmuxMissing
        );
        assert_eq!(ssh.calls()[0], "tmux -V");
    }
}
