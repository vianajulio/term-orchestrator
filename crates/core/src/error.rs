use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "detail")]
pub enum OrchestratorError {
    #[error("SSH authentication failed (key rejected or missing)")]
    AuthFailed,
    #[error("host unreachable (timeout, refused or no route)")]
    HostUnreachable,
    #[error("machine did not come online after Wake-on-LAN")]
    WakeTimeout,
    #[error("tmux is not installed on the remote machine")]
    TmuxMissing,
    #[error("ssh client not found in PATH")]
    SshClientMissing,
    #[error("no supported terminal emulator found")]
    TerminalMissing,
    #[error("invalid session name: {0}")]
    InvalidSessionName(String),
    #[error("config error: {0}")]
    ConfigError(String),
    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for OrchestratorError {
    fn from(e: std::io::Error) -> Self {
        OrchestratorError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, OrchestratorError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_converts_to_io_variant() {
        let e: OrchestratorError = std::io::Error::other("boom").into();
        assert_eq!(e, OrchestratorError::Io("boom".to_string()));
    }

    #[test]
    fn serializes_with_kind_tag() {
        let json =
            serde_json::to_string(&OrchestratorError::InvalidSessionName("a b".into())).unwrap();
        assert_eq!(json, r#"{"kind":"InvalidSessionName","detail":"a b"}"#);
    }
}
