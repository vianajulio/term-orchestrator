mod commands;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use term_orchestrator_core::MachineOs;

#[derive(Parser)]
#[command(name = "torch", version, about = "term-orchestrator debug CLI")]
struct Cli {
    /// Path to machines.toml (default: <config_dir>/term-orchestrator/machines.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage registered machines
    Machines {
        #[command(subcommand)]
        cmd: MachinesCmd,
    },
    /// List tmux sessions on a machine
    Sessions { machine: String },
    /// Create or kill a tmux session
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },
    /// Print the last N lines of a session pane
    Preview {
        machine: String,
        session: String,
        #[arg(long, default_value_t = 50)]
        lines: u32,
    },
    /// Open a native terminal attached to a session (attach-or-create)
    Attach {
        machine: String,
        session: Option<String>,
    },
    /// Probe an IP: ping, ARP (MAC), SSH banner, reverse DNS
    Discover { ip: std::net::IpAddr },
    /// Connect to a machine, sending Wake-on-LAN and retrying if it is asleep
    Wake { machine: String },
}

#[derive(Subcommand)]
enum MachinesCmd {
    List,
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        host: String,
        #[arg(long)]
        user: String,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        mac: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(long, value_parser = parse_os, default_value = "unknown")]
        os: MachineOs,
        #[arg(long, default_value = "main")]
        session: String,
    },
    Rm {
        name: String,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    New { machine: String, name: String },
    Kill { machine: String, name: String },
}

fn parse_os(s: &str) -> Result<MachineOs, String> {
    match s {
        "linux" => Ok(MachineOs::Linux),
        "windows_wsl" | "wsl" => Ok(MachineOs::WindowsWsl),
        "unknown" => Ok(MachineOs::Unknown),
        other => Err(format!("unknown os `{other}` (linux|windows_wsl|unknown)")),
    }
}

fn config_path(cli: &Cli) -> anyhow::Result<PathBuf> {
    if let Some(p) = &cli.config {
        return Ok(p.clone());
    }
    let base = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    Ok(base.join("term-orchestrator").join("machines.toml"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let path = config_path(&cli)?;
    match cli.cmd {
        Command::Machines { cmd } => match cmd {
            MachinesCmd::List => commands::machines_list(&path).await,
            MachinesCmd::Add {
                name,
                host,
                user,
                key,
                mac,
                port,
                os,
                session,
            } => commands::machines_add(&path, name, host, user, key, mac, port, os, session),
            MachinesCmd::Rm { name } => commands::machines_rm(&path, &name),
        },
        Command::Sessions { machine } => commands::sessions(&path, &machine).await,
        Command::Session { cmd } => match cmd {
            SessionCmd::New { machine, name } => {
                commands::session_new(&path, &machine, &name).await
            }
            SessionCmd::Kill { machine, name } => {
                commands::session_kill(&path, &machine, &name).await
            }
        },
        Command::Preview {
            machine,
            session,
            lines,
        } => commands::preview(&path, &machine, &session, lines).await,
        Command::Attach { machine, session } => {
            commands::attach(&path, &machine, session.as_deref())
        }
        Command::Discover { ip } => commands::discover(ip).await,
        Command::Wake { machine } => commands::wake(&path, &machine).await,
    }
}
