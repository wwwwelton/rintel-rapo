# Rintel RAPO

Aplicativo Windows para detectar jogos Steam em execução e aplicar, **por opção do usuário**, ajustes de prioridade e afinidade. Não é o Intel Application Optimization (APO), não contém código do APO e não altera firmware, clock ou cache L1/L2/L3.

## Funcionalidade e limites

- **Auto Prioridade:** eleva a classe do processo para acima do normal enquanto o jogo está em foco; restaura a classe original ao parar. O modo «Manter ajustes em segundo plano» também o faz fora de foco e pode prejudicar outros aplicativos.
- **P-cores:** identifica os núcleos de maior classe de eficiência reportados pelo Windows e tenta fixar a thread de maior tempo de CPU recente neles. Não substitui afinidade já escolhida pelo jogo; ao detectar alteração externa em uma thread fixada, o Auto-Yield cede. Afinidade não garante melhora de desempenho.
- **Prioridade de memória / EcoQoS:** define prioridade de páginas de memória 5 e desativa a marca de execução EcoQoS para o processo selecionado. Salva e tenta restaurar os valores anteriores ao desativar ou fechar o aplicativo, sem sobrescrever alterações externas detectadas. Não controla o cache da CPU nem a fila NVMe.
- **Telemetria:** CPU, GPU, RAM e contadores de leitura I/O do processo selecionado; tráfego de rede e leitura do disco físico referem-se ao **sistema inteiro**. RTT é o tempo de conexão TCP a servidores DNS públicos na porta 53, não o ping do jogo. Valores podem estar indisponíveis quando o Windows não expõe contadores ou a rede bloqueia conexões de teste.
- **Bandeja:** permite iniciar/parar a otimização e minimizar a interface sem interromper o monitoramento.
- **Administrador (opcional):** em «Opções e diretrizes de agendamento», «Reabrir como administrador» solicita UAC e reinicia o aplicativo elevado. Se cancelar, a janela atual permanece aberta. Após reiniciar, selecione os ajustes novamente; elevar pode permitir acesso a processos comuns, mas não contorna proteções de anti-cheat.

O aplicativo não mede FPS nem realiza benchmarks do jogo; medições anteriores eram apenas a taxa de repintura da própria interface e foram removidas. Ganhos de FPS/latência precisam de medição externa no hardware e jogo reais. As APIs de agendamento usadas não constituem garantia de compatibilidade com anti-cheats; consulte a política do jogo.

Versões anteriores podiam alterar os valores globais do Windows `NetworkThrottlingIndex` e `SystemResponsiveness`. Esta versão **não os modifica nem os restaura automaticamente**: se você usou a opção antiga, inspecione sua configuração de sistema antes de ajustá-la manualmente, para não apagar personalizações suas ou de outros programas.

## Compilar e executar

No Windows, com Rust: `cargo run --release`. No Linux/WSL2, para gerar o executável Windows: `cargo build --release --target x86_64-pc-windows-gnu`. O aplicativo gráfico não abre um terminal ao ser executado no Windows.
