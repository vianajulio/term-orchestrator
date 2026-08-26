use std::time::Duration;

use async_trait::async_trait;

use crate::config::Machine;
use crate::error::{OrchestratorError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait SshRunner: Send + Sync {
    /// Run `cmd` on `m` via ssh. `Ok` if ssh connected and the remote command
    /// returned (any exit code). `Err` only for transport/auth/client failures.
    async fn run(&self, m: &Machine, cmd: &str, timeout: Duration) -> Result<CmdOutput>;
}

pub fn build_args(m: &Machine, cmd: &str) -> Vec<String> {
    vec![
        "-i".into(),
        m.ssh_key.to_string_lossy().into_owned(),
        "-p".into(),
        m.ssh_port.to_string(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        format!("{}@{}", m.ssh_user, m.host),
        cmd.to_string(),
    ]
}

pub fn classify_failure(code: i32, stderr: &str) -> OrchestratorError {
    let s = stderr.to_ascii_lowercase();
    if s.contains("permission denied")
        || s.contains("no such identity")
        || s.contains("host key verification failed")
    {
        return OrchestratorError::AuthFailed;
    }
    if s.contains("connection timed out")
        || s.contains("connection refused")
        || s.contains("no route to host")
        || s.contains("could not resolve")
        || s.contains("network is unreachable")
    {
        return OrchestratorError::HostUnreachable;
    }
    if code == 127 || s.contains("command not found") || s.contains("tmux: not found") {
        return OrchestratorError::TmuxMissing;
    }
    OrchestratorError::Io(stderr.trim().to_string())
}

pub struct SystemSsh;

#[async_trait]
impl SshRunner for SystemSsh {
    async fn run(&self, m: &Machine, cmd: &str, timeout: Duration) -> Result<CmdOutput> {
        let mut child = tokio::process::Command::new("ssh");
        child.args(build_args(m, cmd));
        child.stdin(std::process::Stdio::null());
        child.kill_on_drop(true);
        let fut = child.output();
        let out = match tokio::time::timeout(timeout, fut).await {
            Err(_) => return Err(OrchestratorError::HostUnreachable),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(OrchestratorError::SshClientMissing)
            }
            Ok(Err(e)) => return Err(OrchestratorError::Io(e.to_string())),
            Ok(Ok(o)) => o,
        };
        let code = out.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // Exit 255 is ssh's own failure code (never the remote command's).
        if code == 255 {
            return Err(classify_failure(code, &stderr));
        }
        Ok(CmdOutput {
            code,
            stdout,
            stderr,
        })
    }
}

#[cfg(any(test, feature = "mock"))]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MockSsh {
        queue: Mutex<VecDeque<Result<CmdOutput>>>,
        calls: Mutex<Vec<String>>,
    }

    impl MockSsh {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn push_ok(&self, code: i32, stdout: &str, stderr: &str) -> &Self {
            self.queue.lock().unwrap().push_back(Ok(CmdOutput {
                code,
                stdout: stdout.to_string(),
                stderr: stderr.to_string(),
            }));
            self
        }
        pub fn push_err(&self, e: OrchestratorError) -> &Self {
            self.queue.lock().unwrap().push_back(Err(e));
            self
        }
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SshRunner for MockSsh {
        async fn run(&self, _m: &Machine, cmd: &str, _timeout: Duration) -> Result<CmdOutput> {
            self.calls.lock().unwrap().push(cmd.to_string());
            self.queue
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| panic!("MockSsh: no response queued for `{cmd}`"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MachineOs;
    use std::path::PathBuf;

    fn m() -> Machine {
        Machine {
            name: "w1".into(),
            host: "10.0.0.5".into(),
            mac: None,
            os: MachineOs::Linux,
            ssh_user: "julio".into(),
            ssh_port: 2222,
            ssh_key: PathBuf::from("/keys/id"),
            default_session: "main".into(),
        }
    }

    #[test]
    fn build_args_has_fixed_flags_target_and_command() {
        let args = build_args(&m(), "tmux -V");
        let expected: Vec<String> = [
            "-i",
            "/keys/id",
            "-p",
            "2222",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "julio@10.0.0.5",
            "tmux -V",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(args, expected);
    }

    #[test]
    fn classify_auth_failures() {
        assert_eq!(
            classify_failure(255, "julio@10.0.0.5: Permission denied (publickey)."),
            OrchestratorError::AuthFailed
        );
        assert_eq!(classify_failure(255, "Warning: Identity file /k not accessible: No such file or directory.\njulio@h: Permission denied (publickey)."), OrchestratorError::AuthFailed);
        assert_eq!(
            classify_failure(255, "Host key verification failed."),
            OrchestratorError::AuthFailed
        );
    }

    #[test]
    fn classify_unreachable() {
        assert_eq!(
            classify_failure(
                255,
                "ssh: connect to host 10.0.0.5 port 22: Connection timed out"
            ),
            OrchestratorError::HostUnreachable
        );
        assert_eq!(
            classify_failure(
                255,
                "ssh: connect to host 10.0.0.5 port 22: Connection refused"
            ),
            OrchestratorError::HostUnreachable
        );
        assert_eq!(
            classify_failure(
                255,
                "ssh: connect to host 10.0.0.5 port 22: No route to host"
            ),
            OrchestratorError::HostUnreachable
        );
        assert_eq!(
            classify_failure(
                255,
                "ssh: Could not resolve hostname foo: Name or service not known"
            ),
            OrchestratorError::HostUnreachable
        );
    }

    #[test]
    fn classify_tmux_missing() {
        assert_eq!(
            classify_failure(127, "bash: tmux: command not found"),
            OrchestratorError::TmuxMissing
        );
        assert_eq!(
            classify_failure(127, "sh: 1: tmux: not found"),
            OrchestratorError::TmuxMissing
        );
    }

    #[test]
    fn classify_unknown_is_io_with_stderr() {
        assert_eq!(
            classify_failure(1, "weird"),
            OrchestratorError::Io("weird".into())
        );
    }

    #[tokio::test]
    async fn mock_returns_queued_and_records_calls() {
        let mock = mock::MockSsh::new();
        mock.push_ok(0, "out", "")
            .push_err(OrchestratorError::HostUnreachable);
        let a = mock.run(&m(), "one", Duration::from_secs(1)).await.unwrap();
        assert_eq!(a.stdout, "out");
        let b = mock
            .run(&m(), "two", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert_eq!(b, OrchestratorError::HostUnreachable);
        assert_eq!(mock.calls(), vec!["one", "two"]);
    }

    /// Restores the previous `PATH` when dropped, even if the test body panics.
    struct PathGuard(Option<std::ffi::OsString>);

    impl Drop for PathGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[tokio::test]
    #[serial_test::serial(path_env)]
    async fn system_ssh_reports_client_missing_when_binary_absent() {
        // Point PATH at an empty dir so `ssh` cannot be found.
        let dir = tempfile::tempdir().unwrap();
        let _guard = PathGuard(std::env::var_os("PATH"));
        std::env::set_var("PATH", dir.path());
        let r = SystemSsh.run(&m(), "true", Duration::from_secs(2)).await;
        assert_eq!(r.unwrap_err(), OrchestratorError::SshClientMissing);
    }
}
