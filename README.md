# Rintel RAPO

Aplicativo Windows para detectar jogos Steam em execução e aplicar, **por opção do usuário**, ajustes de prioridade e afinidade. Não é o Intel Application Optimization (APO), não contém código do APO e não altera firmware, clock ou cache L1/L2/L3.

## Funcionalidade e limites

- **Auto Prioridade:** eleva a classe do processo para acima do normal enquanto o jogo está em foco; restaura a classe original ao parar. O modo «Manter ajustes em segundo plano» também o faz fora de foco e pode prejudicar outros aplicativos.
- **P-cores (`CriticalThreadsAffinitize`):** identifica os núcleos de maior classe de eficiência reportados pelo Windows e fixa até 4 threads críticas (`delta >= 5%`, ordenadas por utilização e prioridade, com janela de estabilidade de 20% e intervalo de 10 s para `AION2.exe`, mantendo E-cores livres para as demais threads). Não substitui afinidade já escolhida pelo jogo; ao detectar alteração externa em uma thread fixada, o Auto-Yield cede e restaura as demais.
- **Prioridade de memória / EcoQoS:** define prioridade de páginas de memória 5 e desativa a marca de execução EcoQoS para o processo selecionado. Salva e tenta restaurar os valores anteriores ao desativar ou fechar o aplicativo, sem sobrescrever alterações externas detectadas. Não controla o cache da CPU nem a fila NVMe.
- **Perfil APO UE5 (AION 2):** integrado diretamente às opções **Auto Prioridade**, **P-cores** e **Prioridade de memória / EcoQoS** (ou pelo atalho **Perfil APO** na linha do jogo). Com o jogo fechado, lê `AION2.exe` e índices `.utoc`; se ainda houver orçamento, aquece contêineres `.ucas` em ordem de tamanho. O orçamento é recalculado em tempo real como 25% da RAM física disponível, limitado a 512 MiB–4 GiB, e cai quando a memória fica pressionada. A leitura para ao iniciar o jogo ou desativar o perfil. O cache é compartilhado e evictável pelo Windows; não é uma cópia privada nem uma garantia de permanência em RAM. Em execução, aplica `CriticalThreadsAffinitize=4` (limiar 5%, intervalo 10 s, E-cores livres), sem hook, sem DLL e sem escrita em MSR.
- **Telemetria:** CPU, GPU, RAM e contadores de leitura I/O do processo selecionado; tráfego de rede e leitura do disco físico referem-se ao **sistema inteiro**. RTT é o tempo de conexão TCP a servidores DNS públicos na porta 53, não o ping do jogo. Valores podem estar indisponíveis quando o Windows não expõe contadores ou a rede bloqueia conexões de teste.
- **Bandeja:** permite iniciar/parar a otimização e minimizar a interface sem interromper o monitoramento.
- **Administrador (opcional):** em «Opções e diretrizes de agendamento», «Reabrir como administrador» solicita UAC e reinicia o aplicativo elevado. Se cancelar, a janela atual permanece aberta. Após reiniciar, selecione os ajustes novamente; elevar pode permitir acesso a processos comuns, mas não contorna proteções de anti-cheat.

O aplicativo exibe o FPS/frame pacing da própria interface quando há um processo em execução; esse valor **não mede o FPS do jogo**. Ganhos de FPS/latência precisam de medição externa no hardware e jogo reais. As APIs de agendamento usadas não constituem garantia de compatibilidade com anti-cheats; consulte a política do jogo.

Versões anteriores podiam alterar os valores globais do Windows `NetworkThrottlingIndex` e `SystemResponsiveness`. Esta versão **não os modifica nem os restaura automaticamente**: se você usou a opção antiga, inspecione sua configuração de sistema antes de ajustá-la manualmente, para não apagar personalizações suas ou de outros programas.

## Compilar e executar

No Windows, com Rust: `cargo run --release`. No Linux/WSL2, para gerar o executável Windows: `cargo build --release --target x86_64-pc-windows-gnu`. O aplicativo gráfico não abre um terminal ao ser executado no Windows.
