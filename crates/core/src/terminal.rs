use std::process::{Command, Stdio};
use std::sync::OnceLock;

use serde::Serialize;

use crate::config::Machine;
use crate::error::{OrchestratorError, Result};
use crate::tmux::validate_session_name;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Terminal {
    WindowsTerminal,
    Cmd,
    GnomeTerminal,
    Konsole,
    Xterm,
}

pub fn ssh_attach_args(m: &Machine, session: &str) -> Vec<String> {
    vec![
        "-i".into(),
        m.ssh_key.to_string_lossy().into_owned(),
        "-p".into(),
        m.ssh_port.to_string(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        format!("{}@{}", m.ssh_user, m.host),
        "-t".into(),
        format!("tmux new-session -A -s {session}"),
    ]
}

pub fn detect_terminal_in(
    candidates: &[(&str, Terminal)],
    exists: &dyn Fn(&str) -> bool,
) -> Result<Terminal> {
    candidates
        .iter()
        .find(|(bin, _)| exists(bin))
        .map(|(_, t)| *t)
        .ok_or(OrchestratorError::TerminalMissing)
}

fn binary_in_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(name).is_file())
}

#[cfg(target_os = "windows")]
const CANDIDATES: &[(&str, Terminal)] = &[
    ("wt.exe", Terminal::WindowsTerminal),
    ("cmd.exe", Terminal::Cmd),
];
#[cfg(not(target_os = "windows"))]
const CANDIDATES: &[(&str, Terminal)] = &[
    ("gnome-terminal", Terminal::GnomeTerminal),
    ("konsole", Terminal::Konsole),
    ("xterm", Terminal::Xterm),
];

pub fn detect_terminal() -> Result<Terminal> {
    // A negative result (`TerminalMissing`) is cached for the lifetime of the
    // process, same as a positive one: `OnceLock::get_or_init` only ever runs
    // the closure once, regardless of whether it returned `Ok` or `Err`. That
    // is fine for this one-shot CLI, but a long-lived host (e.g. the M3 Tauri
    // app) must not rely on this cache to notice a terminal installed after
    // the first call — it will keep returning the original `Err` forever.
    static CACHE: OnceLock<Result<Terminal>> = OnceLock::new();
    CACHE
        .get_or_init(|| detect_terminal_in(CANDIDATES, &binary_in_path))
        .clone()
}

/// Detects a terminal and spawns it attached to `session` on `m`.
///
/// This function is synchronous and blocking: `detect_terminal` performs
/// filesystem `stat` calls across every directory in `PATH`, and spawning the
/// terminal process is itself a blocking syscall. Async callers must not call
/// this directly on an async task — wrap it in `tokio::task::spawn_blocking`
/// to avoid stalling the executor.
pub fn spawn_attach(m: &Machine, session: &str) -> Result<()> {
    validate_session_name(session)?;
    let term = detect_terminal()?;
    let ssh_args = ssh_attach_args(m, session);
    let title = format!("{}:{}", m.name, session);
    let mut cmd = match term {
        Terminal::WindowsTerminal => {
            let mut c = Command::new("wt.exe");
            c.args(["new-tab", "--title", &title, "ssh"])
                .args(&ssh_args);
            c
        }
        Terminal::Cmd => {
            let mut c = Command::new("cmd.exe");
            c.args(["/c", "start", &title, "ssh"]).args(&ssh_args);
            c
        }
        Terminal::GnomeTerminal => {
            let mut c = Command::new("gnome-terminal");
            c.args(["--title", &title, "--", "ssh"]).args(&ssh_args);
            c
        }
        Terminal::Konsole => {
            let mut c = Command::new("konsole");
            c.args(["-p", &format!("tabtitle={title}"), "-e", "ssh"])
                .args(&ssh_args);
            c
        }
        Terminal::Xterm => {
            let mut c = Command::new("xterm");
            c.args(["-T", &title, "-e", "ssh"]).args(&ssh_args);
            c
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => OrchestratorError::TerminalMissing,
        _ => OrchestratorError::Io(e.to_string()),
    })?;
    Ok(())
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
            ssh_port: 22,
            ssh_key: PathBuf::from("/keys/id"),
            default_session: "main".into(),
        }
    }

    #[test]
    fn attach_args_use_new_session_attach_or_create() {
        let args = ssh_attach_args(&m(), "main");
        assert_eq!(
            args,
            vec![
                "-i",
                "/keys/id",
                "-p",
                "22",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "julio@10.0.0.5",
                "-t",
                "tmux new-session -A -s main",
            ]
        );
    }

    #[test]
    fn detect_picks_first_existing_candidate() {
        let c = [
            ("wt.exe", Terminal::WindowsTerminal),
            ("cmd.exe", Terminal::Cmd),
        ];
        let t = detect_terminal_in(&c, &|n| n == "cmd.exe").unwrap();
        assert_eq!(t, Terminal::Cmd);
        let t = detect_terminal_in(&c, &|_| true).unwrap();
        assert_eq!(t, Terminal::WindowsTerminal);
    }

    #[test]
    fn detect_none_is_terminal_missing() {
        let c = [("xterm", Terminal::Xterm)];
        assert_eq!(
            detect_terminal_in(&c, &|_| false).unwrap_err(),
            OrchestratorError::TerminalMissing
        );
    }

    #[test]
    fn spawn_rejects_bad_session_name() {
        assert!(matches!(
            spawn_attach(&m(), "a b"),
            Err(OrchestratorError::InvalidSessionName(_))
        ));
    }
}
