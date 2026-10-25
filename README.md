# Rintel RAPO

Otimizador de jogos em tempo real para processadores Intel Core híbridos (P-cores / E-cores). **Não é o Intel Application Optimization (APO)**: utiliza estritamente contratos públicos de agendamento do Windows em user-mode, sem modificar arquivos binários ou tocar na memória dos jogos.

O programa é compilado nativamente para Windows com `#![windows_subsystem = "windows"]`, **sem abrir janela de terminal**, exibindo apenas a interface gráfica.

---

## 🛡️ Segurança Anti-Cheat (Steam VAC, NCSoft GameGuard, Easy Anti-Cheat, BattlEye)

**100% livre de banimento.** O Rintel RAPO respeita rigorosamente as regras de integridade de plataformas como Valve Steam (VAC) e NCSoft:
- **Zero Leitura ou Gravação de Memória:** Não utiliza `ReadProcessMemory`, `WriteProcessMemory`, `VirtualAllocEx` ou `PROCESS_VM_READ/WRITE`.
- **Zero Injeção de Código:** Não injeta DLLs nem cria threads remotas (`CreateRemoteThread`).
- **Zero Interceptação de Pacotes:** Não altera nem inspeciona o conteúdo de pacotes de rede.
- **Apenas APIs Oficiais de Sistema Operacional:** Utiliza `SetThreadAffinityMask`, `SetPriorityClass`, `ProcessMemoryPriority` e flags oficiais da API Win32 do Windows.

---

## Recursos Principais

### 1. Priorização de Cache Interno da CPU (L1, L2 e L3)
- **Cache L1 (80 KB por P-core: 48 KB L1D + 32 KB L1I):** Ao fixar a thread crítica nos P-cores, o pipeline de renderização obtém acesso exclusivo sem disputa de cache por threads de fundo.
- **Cache L2 (2048 KB dedicado por P-core):** No i9-14900K, cada P-core possui 2 MB de L2 exclusivo. A afixação impede a degradação que ocorre ao compartilhar o L2 de 4 MB de clusters de E-cores.
- **Cache L3 (36 MB Smart Cache):**
  - Desativação do EcoQoS (`PROCESS_POWER_THROTTLING_EXECUTION_SPEED = 0`) para manter o barramento *Ring Bus / Uncore* em frequência máxima, minimizando a latência de acesso ao L3.
  - `ProcessMemoryPriority` em nível `VERY_HIGH` (5) com retenção de working set, prevenindo evicções e TLB shootdowns.

### 2. Suporte Completo a Jogos em Modo Janela / Borderless
- Suporta jogos executados em tela cheia, modo janela e janela sem bordas (*borderless windowed*).
- Opção **Modo Janela / Borderless**: mantém a otimização de prioridade e agendamento ativa mesmo ao alternar janelas secundárias ou em configurações com múltiplos monitores.

### 3. Prioridade de Leitura e Armazenamento (Disco onde o Jogo está Instalado)
- Identifica automaticamente a unidade de armazenamento do jogo (ex: `C:`, `D:`, etc.).
- Aplica `IoPriorityHigh` (nível 3 de I/O) para o processo do jogo, garantindo fila prioritária no controlador NVMe/SSD e acelerando o carregamento de texturas, shaders e assets do disco de instalação.

### 4. QoS de Internet com Preservação de Redutores de Ping (ExitLag, Mudfish, etc.)
- Desativa o limitador interno do Windows (`NetworkThrottlingIndex = 0xFFFFFFFF` e `SystemResponsiveness = 0`), eliminando o enfileiramento artificial de pacotes de rede.
- **Compatibilidade com Redutores de Ping:** Detecta ativamente processos como `ExitLag`, `Mudfish`, `NoPing`, `WTFast`, `GearUP Booster` e preserva suas rotas e túneis UDP sem qualquer interferência ou estrangulamento.
- Opção com botão de ligar/desligar na interface.

### 5. Indicador em Tempo Real do Processo em Otimização
- Banner visual no topo da tela destacando exatamente qual processo está sendo otimizado no momento (`🟢 OTIMIZANDO AGORA: Jogo (PID)`) com chips para Prioridade, P-core afixado, Cache L1/L2/L3, QoS de Disco e Rede.
- Status individual por jogo na lista de processos ativos (`OTIMIZANDO AGORA`, `STANDBY` ou `INATIVO`).

### 6. Proteção contra Conflitos com o Intel APO (Auto-Yield)
- Detecta o serviço Intel DTT (`ipfsvc`) no Windows.
- Monitora em tempo real se o Intel APO sobrescrever a afinidade da thread e cede automaticamente o controle (*Auto-Yield*), prevenindo oscilações e micro-stutters.

---

## Compilar e Executar

No Windows, com Rust instalado:

```powershell
cargo run --release
```

Ou compilação para Windows via Linux / WSL2:

```bash
cargo build --release --target x86_64-pc-windows-gnu
```

O binário final gerado é `rintel-rapo.exe`. Ao ser executado, ele abre diretamente a janela gráfica sem qualquer terminal.
