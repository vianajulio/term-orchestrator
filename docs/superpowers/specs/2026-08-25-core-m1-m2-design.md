# Design — Core Rust (M1–M2)

Data: 2026-08-25. Escopo: `crates/core` + `crates/cli`. Sem UI, sem Tauri, sem MCP.
Contratos completos em `docs/SPEC.md` §4–§6, §10–§11. Este doc registra só as decisões e o porquê.

## Objetivo

Lib Rust testável que sabe: ler/gravar `machines.toml`, rodar comandos remotos via `ssh`, operar tmux, descobrir máquina por IP, acordar via WoL com retry, abrir terminal nativo. CLI `torch` exercita tudo contra worker real.

## Decisões

### D1. Workspace, core fora do `src-tauri`
`crates/core` é lib pura (tokio + serde + toml + thiserror). `crates/cli` e, depois, `src-tauri` e MCP dependem dela. `cargo test -p term-orchestrator-core` não compila webview.

### D2. `SshRunner` como trait
Único ponto de I/O de rede nos módulos `tmux` e `wol`. `SystemSsh` (real) e `MockSsh` (testes, fila de respostas programadas). Permite testar parsing, classificação de erro e máquina de estados WoL sem rede.

### D3. Binários do SO, não crates de raw socket
`ssh`, `ping`, `arp` via `tokio::process`. Evita privilégio admin (ICMP raw), evita libssh/crypto em Rust, e reusa `~/.ssh/config` e agente do usuário. Custo: parse de saída por SO — coberto por fixtures.

### D4. `connect_with_wake` recebe callback + `WakePolicy`
Sem dependência de Tauri `emit`. Policy injetável permite teste da máquina de estados em milissegundos. Default 20s/15s/5.

### D5. Validação de nome de sessão
`[A-Za-z0-9_-]+`, comprimento ≤ 64. Comando remoto é montado por concatenação de string única passada ao `ssh`; sem validação seria injeção de shell. Retorna `InvalidSessionName`.

### D6. Config path injetado
`Config::load(&Path)` / `Config::save(&Path)`. Escrita atômica: grava `machines.toml.tmp`, `rename`. Arquivo ausente → `Config::default()` (`onboarding_done = false`, sem máquinas).

### D7. `terminal.rs` já em M1
Pequeno, sem I/O de rede, e `torch attach` é a validação mais direta do fluxo SSH+tmux end-to-end.

## Fluxo de dados

```
CLI/Tauri/MCP
   │ Machine (de Config)
   ▼
tmux.rs ──cmd string──▶ SshRunner ──argv──▶ ssh binário ──▶ worker
   ▲                        │
   └── CmdOutput {code, stdout, stderr} ◀──┘
             │ classify_failure
             ▼
       OrchestratorError
```

`wol.rs` usa `SshRunner` só para handshake (`tmux -V` ou `true`); magic packet vai por `UdpSocket` broadcast direto.

## Erros

Todos os módulos retornam `OrchestratorError`. Tabela de classificação em SPEC §10. `Serialize` no enum para atravessar Tauri sem conversão em M3.

## Testes

Unit por módulo com fixtures em `crates/core/tests/fixtures/`:
- `arp_linux.txt`, `arp_windows.txt`, `tmux_ls.txt`, `ssh_stderr_*.txt`.
- `wol::tests` com `MockSsh` que falha N vezes e depois sucede → verifica sequência de status emitida e resultado.

Integração manual (fim do plano): `torch discover <ip>`, `torch sessions <m>`, `torch wake <m>`, `torch attach <m>` contra worker do usuário.

## Fora de escopo

Tauri, React, MCP, docker sshd, `ts-rs`, lock de sessão, auth MCP.
