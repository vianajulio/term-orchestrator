# Setup do worker

Passos manuais para preparar uma máquina da LAN para ser controlada pelo `term-orchestrator`. Este conteúdo é a fonte do wizard de onboarding.

## Obrigatório

### 1. Servidor SSH

**Linux (Debian/Ubuntu):**
```bash
sudo apt install -y openssh-server tmux
sudo systemctl enable --now ssh
```

**Windows + WSL:** o orquestrador conecta no sshd **dentro do WSL**, não no OpenSSH do Windows.
```bash
# dentro do WSL
sudo apt install -y openssh-server tmux
sudo systemctl enable --now ssh      # requer systemd habilitado no WSL (/etc/wsl.conf: [boot] systemd=true)
```
Encaminhar a porta 22 do host Windows para o WSL (PowerShell como admin). IP do WSL: `wsl -- ip -4 addr show eth0` (campo `inet`; `hostname -I` não existe em todas as distros):
```powershell
netsh interface portproxy add v4tov4 listenport=22 listenaddress=0.0.0.0 connectport=22 connectaddress=<IP_WSL>
netsh advfirewall firewall add rule name="SSH WSL" dir=in action=allow protocol=TCP localport=22
```
O IP do WSL muda a cada boot e quebra o portproxy. Alternativa recomendada (Windows 11 22H2+): `%USERPROFILE%\.wslconfig` com
```
[wsl2]
networkingMode=mirrored
```
seguido de `wsl --shutdown`. O WSL passa a usar o IP do Windows; basta a regra de firewall, sem portproxy.

### 2. Chave SSH (sem senha)

No notebook orquestrador:
```bash
ssh-keygen -t ed25519 -f ~/.ssh/term-orch -N ""
ssh-copy-id -i ~/.ssh/term-orch.pub <user>@<ip-worker>
```
No Windows sem `ssh-copy-id`:
```powershell
type $env:USERPROFILE\.ssh\term-orch.pub | ssh <user>@<ip-worker> "mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys"
```

### 3. Verificar

```bash
ssh -i ~/.ssh/term-orch -o BatchMode=yes <user>@<ip-worker> "tmux -V"
```
Deve imprimir `tmux 3.x` sem pedir senha. Se pedir senha → chave não instalada. Se `command not found` → tmux ausente.

### 4. IP fixo

Reserve o IP do worker no DHCP do roteador (ou configure estático). Sem isso o cadastro quebra ao IP mudar.

## Opcional — Wake-on-LAN

Só necessário se o worker vai dormir/suspender.

1. **BIOS/UEFI:** habilitar "Wake on LAN" / "Power On by PCI-E". Desabilitar "ErP" / "Deep Sleep" se existir.
2. **Linux:** `sudo ethtool -s <iface> wol g` (persistir via systemd unit ou NetworkManager: `nmcli c modify <con> 802-3-ethernet.wake-on-lan magic`).
3. **Windows:** Gerenciador de Dispositivos → adaptador de rede → Gerenciamento de energia → "Permitir que este dispositivo acorde o computador". Desabilitar Fast Startup (Painel de Controle → Opções de Energia).
4. **Cabo:** WoL só funciona confiavelmente em Ethernet cabeada. Wi-Fi geralmente não.
5. **MAC:** anote o MAC da interface cabeada (`ip link` / `ipconfig /all`). O botão **Buscar** no cadastro tenta descobrir via ARP, mas só funciona se a máquina estiver ligada no momento.

Teste: suspenda o worker, rode `torch wake <nome>` (ou clique na máquina 🌙 no app).

## Checklist de validação (M1/M2)

- [ ] `torch discover <ip>` retorna `reachable: true` e MAC
- [ ] `torch sessions <nome>` lista (ou lista vazia sem erro)
- [ ] `torch session new <nome> teste` + `torch preview <nome> teste`
- [ ] `torch attach <nome> teste` abre terminal nativo já dentro do tmux
- [ ] `torch wake <nome>` com worker suspenso → `Waking` → `Online`
