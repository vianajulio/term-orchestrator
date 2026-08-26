use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use term_orchestrator_core::{terminal, tmux, Config, Machine, MachineOs, SystemSsh};

fn load(path: &Path) -> anyhow::Result<Config> {
    Config::load(path).with_context(|| format!("loading {}", path.display()))
}

pub fn find_machine<'a>(cfg: &'a Config, name: &str) -> anyhow::Result<&'a Machine> {
    cfg.find(name)
        .ok_or_else(|| anyhow::anyhow!("machine `{name}` not found (see `torch machines list`)"))
}

pub async fn machines_list(path: &Path) -> anyhow::Result<()> {
    let cfg = load(path)?;
    if cfg.machine.is_empty() {
        println!("no machines registered ({})", path.display());
        return Ok(());
    }
    for m in &cfg.machine {
        let status = match tmux::handshake(&SystemSsh, m).await {
            Ok(()) => "online".to_string(),
            Err(e) if m.mac.is_some() => format!("sleeping? ({e})"),
            Err(e) => format!("unreachable ({e})"),
        };
        println!(
            "{:<12} {:<16} {:<6} {:<12} mac={:<17} {}",
            m.name,
            m.host,
            m.ssh_port,
            m.ssh_user,
            m.mac.as_deref().unwrap_or("-"),
            status
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn machines_add(
    path: &Path,
    name: String,
    host: String,
    user: String,
    key: PathBuf,
    mac: Option<String>,
    port: u16,
    os: MachineOs,
    session: String,
) -> anyhow::Result<()> {
    let mut cfg = load(path)?;
    cfg.upsert(Machine {
        name: name.clone(),
        host,
        mac,
        os,
        ssh_user: user,
        ssh_port: port,
        ssh_key: key,
        default_session: session,
    });
    cfg.save(path)?;
    println!("saved `{name}` to {}", path.display());
    Ok(())
}

pub fn machines_rm(path: &Path, name: &str) -> anyhow::Result<()> {
    let mut cfg = load(path)?;
    if !cfg.remove(name) {
        bail!("machine `{name}` not found");
    }
    cfg.save(path)?;
    println!("removed `{name}`");
    Ok(())
}

pub async fn sessions(path: &Path, machine: &str) -> anyhow::Result<()> {
    let cfg = load(path)?;
    let m = find_machine(&cfg, machine)?;
    let list = tmux::list_sessions(&SystemSsh, m).await?;
    if list.is_empty() {
        println!("no sessions");
    }
    for s in list {
        println!(
            "{:<20} windows={:<3} {}",
            s.name,
            s.windows,
            if s.attached { "attached" } else { "detached" }
        );
    }
    Ok(())
}

pub async fn session_new(path: &Path, machine: &str, name: &str) -> anyhow::Result<()> {
    let cfg = load(path)?;
    tmux::create_session(&SystemSsh, find_machine(&cfg, machine)?, name).await?;
    println!("created `{name}` on `{machine}`");
    Ok(())
}

pub async fn session_kill(path: &Path, machine: &str, name: &str) -> anyhow::Result<()> {
    let cfg = load(path)?;
    tmux::kill_session(&SystemSsh, find_machine(&cfg, machine)?, name).await?;
    println!("killed `{name}` on `{machine}`");
    Ok(())
}

pub async fn preview(path: &Path, machine: &str, session: &str, lines: u32) -> anyhow::Result<()> {
    let cfg = load(path)?;
    let text = tmux::capture_pane(&SystemSsh, find_machine(&cfg, machine)?, session, lines).await?;
    print!("{text}");
    Ok(())
}

pub fn attach(path: &Path, machine: &str, session: Option<&str>) -> anyhow::Result<()> {
    let cfg = load(path)?;
    let m = find_machine(&cfg, machine)?;
    let session = session.unwrap_or(&m.default_session);
    terminal::spawn_attach(m, session)?;
    println!("opened terminal for `{machine}:{session}`");
    Ok(())
}
