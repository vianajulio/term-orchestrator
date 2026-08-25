# Spec de Implementação — `term-orchestrator` v1

> Fonte única de verdade do projeto. Decisões de design detalhadas por milestone vivem em `docs/superpowers/specs/`.

---

## 1. Visão geral

Aplicativo desktop Tauri (Rust + React) que roda em Windows e Linux, atuando como orquestrador de sessões tmux em máquinas remotas da LAN via SSH, com Wake-on-LAN opcional e camada MCP para orquestração por agente de IA.

---

## 2. Decisões assumidas

| # | Ponto | Decisão v1 |
|---|---|---|
| 1 | Preview de sessão | Sob demanda (botão de refresh) + polling leve de **status** (não de conteúdo) a cada 10s. `capture-pane` só quando o card da sessão está expandido. |
| 2 | MCP na v1? | **Sim**, entra na v1 (Fase 6). Mas o app é utilizável sem ele (Fases 0–5 são independentes). |
| 3 | Nome de sessão padrão | `main`, configurável por máquina no campo `default_session`. |
| 4 | Config por notebook | Independente. Cada instância mantém seu `machines.toml`. Sem sync na v1. |
| 5 | Retry pós-WoL | Envia magic packet → aguarda 20s → até 5 tentativas de handshake com intervalo de 15s (total ~95s) → desiste com erro `WakeTimeout`. |
| 6 | "Quem está no controle" | Apenas indicador visual (metadado), sem lock de sessão na v1. |
| 7 | Layout de código | Cargo workspace: `crates/core` (lib) + `crates/cli` (bin de debug) + `src-tauri` (M3+). Core não depende de Tauri. |

---

## 3. Estrutura do repositório

```
term-orchestrator/
├── Cargo.toml                   # [workspace] members = ["crates/*", "src-tauri"]
├── docs/
│   ├── SPEC.md                  # este arquivo
│   ├── SETUP-WORKER.md          # passos manuais no worker (fonte do wizard)
│   └── superpowers/
│       ├── specs/               # design docs por milestone
│       └── plans/               # planos de implementação
├── crates/
│   ├── core/                    # lib `term_orchestrator_core`
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── config.rs        # Machine, load/save machines.toml
│   │   │   ├── ssh.rs           # SshRunner trait + impl via binário ssh
│   │   │   ├── tmux.rs          # list/create/kill/capture-pane sobre ssh
│   │   │   ├── discovery.rs     # ping + ARP + banner SSH + reverse DNS
│   │   │   ├── wol.rs           # magic packet + connect_with_wake
│   │   │   ├── terminal.rs      # spawn de terminal nativo por SO
│   │   │   └── error.rs         # enum OrchestratorError
│   │   └── tests/fixtures/      # saídas de arp/tmux/ssh para testes
│   └── cli/                     # bin `torch` — CLI de debug sobre o core
├── src-tauri/                   # M3+: shell Tauri, commands.rs, mcp/
├── src/                         # M3+: frontend React
├── package.json
├── README.md
└── .github/workflows/ci.yml
```

---

## 4. Modelos de dados

### 4.1 `Machine` (`core/config.rs`)

```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Machine {
    pub name: String,
    pub host: String,            // IP ou hostname
    pub mac: Option<String>,     // None => WoL indisponível
    pub os: MachineOs,           // WindowsWsl | Linux | Unknown
    pub ssh_user: String,
    pub ssh_port: u16,           // default 22
    pub ssh_key: PathBuf,
    pub default_session: String, // default "main"
}
```

`mac: None` implementa a regra "WoL é opcional": sem MAC, o fluxo nunca tenta magic packet — falha de conexão vira `Unreachable` direto.

### 4.2 `MachineStatus`

```rust
pub enum MachineStatus { Online, Sleeping, Unreachable, Waking }
```

- `Sleeping`: inacessível MAS tem MAC (candidata a WoL).
- `Unreachable`: inacessível e sem MAC, ou WoL esgotou retries.

### 4.3 `SessionInfo`

```rust
pub struct SessionInfo {
    pub name: String,
    pub windows: u32,
    pub attached: bool,
    pub controller: Controller,  // Human | Agent | Unknown
}
```

### 4.4 `DiscoveryResult`

```rust
pub struct DiscoveryResult {
    pub reachable: bool,
    pub mac: Option<String>,        // normalizado "AA:BB:CC:DD:EE:FF"
    pub os_hint: MachineOs,
    pub hostname_hint: Option<String>,
    pub ssh_banner: Option<String>,
}
```

### 4.5 `machines.toml`

```toml
onboarding_done = true
mcp_port = 8321
mcp_auth_token = ""   # opcional; validado se presente

[[machine]]
name = "worker-1"
host = "192.168.1.50"
mac = "AA:BB:CC:DD:EE:FF"
os = "linux"
ssh_user = "julio"
ssh_port = 22
ssh_key = "C:/Users/julio/.ssh/id_ed25519"
default_session = "main"
```

Caminho é injetado (`Config::load(path)`); Tauri passa `app_config_dir`, CLI usa `<config_dir>/term-orchestrator/machines.toml`. Escrita atômica (`.tmp` + rename).

---

## 5. Módulos core — contratos

### 5.1 `ssh.rs`

```rust
#[async_trait]
pub trait SshRunner: Send + Sync {
    async fn run(&self, m: &Machine, cmd: &str, timeout: Duration)
        -> Result<CmdOutput, OrchestratorError>;
}
pub struct SystemSsh;  // impl via tokio::process::Command("ssh")
```

- Flags fixas: `-i <key> -p <port> -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new <user>@<host> <cmd>`.
- `BatchMode=yes` garante que nunca trava esperando senha.
- `classify_failure(exit_code, stderr) -> OrchestratorError` mapeia stderr do OpenSSH (ver §10).
- Testes usam `MockSsh` que retorna saídas fixas.

### 5.2 `tmux.rs`

```rust
pub async fn list_sessions(ssh: &dyn SshRunner, m: &Machine) -> Result<Vec<SessionInfo>, OrchestratorError>;
pub async fn create_session(ssh, m, name) -> Result<(), OrchestratorError>;
pub async fn kill_session(ssh, m, name) -> Result<(), OrchestratorError>;
pub async fn capture_pane(ssh, m, name, lines: u32) -> Result<String, OrchestratorError>;
pub async fn send_keys(ssh, m, name, keys) -> Result<(), OrchestratorError>;  // usado pelo MCP
```

- `list_sessions`: `tmux ls -F "#{session_name}|#{session_windows}|#{session_attached}"`. Exit 1 com stderr `no server running` → lista vazia (não erro).
- Exit 127 / `command not found` → `TmuxMissing`.
- `capture_pane`: `tmux capture-pane -p -t <name> -S -<lines>`; texto cru.
- Nomes de sessão validados: `[A-Za-z0-9_-]+` (evita injeção via shell).

### 5.3 `discovery.rs`

```rust
pub async fn discover(ip: IpAddr) -> DiscoveryResult;
```

- Etapas em paralelo (`tokio::join!`), timeout 2s cada, best-effort: falha de uma não derruba as outras.
- Ping via binário `ping` do SO (sem raw socket / admin). ARP via binário `arp` (`arp -n <ip>` Linux, `arp -a <ip>` Windows), parse com `#[cfg(target_os)]`; MAC normalizado para `AA:BB:CC:DD:EE:FF`.
- Banner SSH: `TcpStream` porta 22, lê primeira linha. `os_hint` heurístico (`Ubuntu`/`Debian` → Linux; `Windows`/`OpenSSH_for_Windows` → WindowsWsl).
- Reverse DNS via `tokio::net::lookup_host` / `dns-lookup`.

### 5.4 `wol.rs`

```rust
pub fn magic_packet(mac: &str) -> Result<[u8; 102], OrchestratorError>;
pub async fn wake(mac: &str) -> Result<(), OrchestratorError>;   // UDP broadcast 255.255.255.255:9
pub struct WakePolicy { pub initial_wait: Duration, pub retry_interval: Duration, pub max_retries: u32 }
pub async fn connect_with_wake(
    ssh: &dyn SshRunner, m: &Machine, policy: &WakePolicy,
    on_status: &mut dyn FnMut(MachineStatus),
) -> Result<(), OrchestratorError>;
```

- Máquina de estados: handshake → ok → `Online`; falha + `mac == None` → `Unreachable`; falha + MAC → `Waking` → `wake` → `initial_wait` → loop `max_retries` { handshake ok → `Online`; sleep `retry_interval` } → `WakeTimeout` + `Unreachable`.
- `on_status` é callback puro; Tauri adapta para `emit("machine-status-changed")`, CLI imprime.
- `WakePolicy::default()` = 20s / 15s / 5.

### 5.5 `terminal.rs`

```rust
pub fn detect_terminal() -> Result<Terminal, OrchestratorError>;   // cacheado (OnceLock)
pub fn spawn_attach(m: &Machine, session: &str) -> Result<(), OrchestratorError>;
```

- Windows: `wt.exe` → fallback `cmd /c start ssh ...`. Linux: `gnome-terminal` → `konsole` → `xterm`.
- Comando: `ssh -i <key> -p <port> <user>@<host> -t "tmux new-session -A -s <session>"`.

### 5.6 `error.rs`

```rust
#[derive(Debug, thiserror::Error, Serialize)]
pub enum OrchestratorError {
    AuthFailed, HostUnreachable, WakeTimeout, TmuxMissing,
    SshClientMissing, TerminalMissing, InvalidSessionName(String),
    ConfigError(String), Io(String),
}
```

---

## 6. CLI de debug (`crates/cli`, bin `torch`)

| Comando | Ação |
|---|---|
| `torch machines list` | lista máquinas + status (handshake) |
| `torch machines add --name --host --user --key [--mac] [--port] [--os]` | grava no TOML |
| `torch machines rm <name>` | remove |
| `torch discover <ip>` | imprime `DiscoveryResult` |
| `torch sessions <machine>` | `list_sessions` |
| `torch session new/kill <machine> <name>` | cria/mata |
| `torch preview <machine> <session> [--lines N]` | `capture_pane` |
| `torch wake <machine>` | `connect_with_wake` imprimindo transições |
| `torch attach <machine> [session]` | `spawn_attach` |

---

## 7. Comandos Tauri (M3+)

Camada fina sobre o core:

| Comando | Assinatura TS |
|---|---|
| `list_machines` | `() => Machine[]` |
| `save_machine` | `(m: Machine) => void` |
| `delete_machine` | `(name: string) => void` |
| `discover_machine` | `(ip: string) => DiscoveryResult` |
| `test_connection` | `(m: Machine) => SessionInfo[]` |
| `get_status` | `(name: string) => MachineStatus` |
| `connect_machine` | `(name: string) => void` (progresso via evento) |
| `list_sessions` | `(name: string) => SessionInfo[]` |
| `create_session` | `(name: string, session: string) => void` |
| `kill_session` | `(name: string, session: string) => void` |
| `get_preview` | `(name: string, session: string, lines: number) => string` |
| `attach_session` | `(name: string, session: string) => void` |
| `check_prerequisites` | `() => PrereqReport` |

Evento: `machine-status-changed { name, status }`.

---

## 8. MCP Server (M6)

- Crate `rmcp`, HTTP/SSE, `0.0.0.0:<mcp_port>` (default 8321). Toggle na UI.
- Tools: `list_machines`, `wake_machine`, `list_sessions`, `create_session`, `run_command` (send-keys + captura após N ms), `get_session_output`.
- Tools que modificam estado marcam `controller = Agent`; `attach_session` via UI marca `Human`.
- Sem auth na v1; `mcp_auth_token` validado se presente.

---

## 9. Frontend (M3–M5)

### Onboarding (`/onboarding`)
Exibido quando `machines.toml` não existe ou `onboarding_done = false`. Passos: boas-vindas → `check_prerequisites` → `SETUP-WORKER.md` renderizado → seção opcional WoL → cadastrar primeira máquina. Botão "pular" sempre visível.

### Admin (`/admin`)
`MachineCard` com status (🟢/🌙/🔴/⏳). `MachineForm`: IP + **Buscar** (discovery pré-preenche), user/chave manuais, **Testar conexão**, aviso inline sem MAC.

### Dashboard (`/`)
Sidebar de máquinas; painel de `SessionCard` (nome, janelas, 🧑/🤖, preview expandido). Ações: Nova sessão, Attach, Encerrar (confirmação).

---

## 10. Classificação de stderr SSH → erro

| Padrão em stderr / exit | Erro |
|---|---|
| `Permission denied`, `no such identity`, `Host key verification failed` | `AuthFailed` |
| `Connection timed out`, `Connection refused`, `No route to host`, `Could not resolve` | `HostUnreachable` |
| exit 127, `command not found`, `tmux: not found` | `TmuxMissing` |
| binário ssh ausente localmente (spawn `NotFound`) | `SshClientMissing` |
| demais | `Io(stderr)` |

---

## 11. Testes

- **Unit (core):** parsers `tmux ls`, ARP Linux/Windows (fixtures), normalização MAC, montagem de args SSH, `classify_failure`, `magic_packet` bytes, `connect_with_wake` com `MockSsh` + `WakePolicy` em ms, validação de nome de sessão.
- **Integração manual:** CLI `torch` contra worker real (checklist em `docs/SETUP-WORKER.md`).
- **Manual por release:** WoL físico; attach em wt.exe + 3 terminais Linux; wizard em máquina limpa.

---

## 12. Milestones

| M | Entrega | Aceite |
|---|---|---|
| M1 | Core: error + config + ssh + tmux + terminal + CLI | `cargo test` verde; `torch sessions` lista sessões de worker real |
| M2 | Discovery + WoL | `torch discover` preenche MAC/os_hint; `torch wake` acorda máquina física |
| M3 | Tauri shell + Admin | CRUD + testar pela UI; TOML persistido |
| M4 | Dashboard + Attach | listar/criar sessões; attach abre terminal nos 2 SOs |
| M5 | Onboarding | wizard funcional; checagens locais ok |
| M6 | MCP server | agente externo lista máquinas, cria sessão, roda comando |
| M7 | Polimento | todos `OrchestratorError` tratados na UI; README completo |

---

## 13. Convenções

- Branches: `main`; feature branches `feat/m<N>-<desc>` quando útil; commits diretos em `main` aceitos na v1.
- Commits: Conventional Commits.
- Rust: `cargo fmt` + `clippy -D warnings` no CI. Edição 2021, MSRV = stable atual.
- Frontend: TypeScript estrito; `lib/types.ts` espelha structs Rust.
- CI: `cargo fmt --check`, `clippy`, `cargo test` em `ubuntu-latest` e `windows-latest`; `npm run build` a partir de M3.
