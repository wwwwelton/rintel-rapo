fn main() {
    for path in ["native", "vendor/presentmon", "ui.json", "ui_figma.svg"] {
        println!("cargo:rerun-if-changed={path}");
    }
    generate_figma_ui_and_tokens();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include("native")
        .include("vendor/presentmon/PresentData")
        .define("NDEBUG", None)
        .define("NOMINMAX", None)
        .define("WINVER", "0x0A00")
        .define("_WIN32_WINNT", "0x0A00")
        .define("UNICODE", None)
        .define("_UNICODE", None)
        .opt_level(2)
        .file("native/presentmon_capture.cpp");
    let gnu = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu");
    if gnu {
        build
            .flag("-include")
            .flag("windows_compat.hpp")
            .cpp_link_stdlib("stdc++")
            .cpp_link_stdlib_static(true);
        let output = build
            .get_compiler()
            .to_command()
            .arg("-print-file-name=libstdc++.a")
            .output()
            .expect("MinGW C++ é necessário para compilar Windows GNU");
        assert!(
            output.status.success(),
            "Não foi possível localizar o runtime C++ estático"
        );
        let library = String::from_utf8(output.stdout).expect("Caminho de biblioteca inválido");
        let path = std::path::Path::new(library.trim());
        assert!(
            path.is_file(),
            "Runtime C++ estático ausente: {}",
            path.display()
        );
        println!(
            "cargo:rustc-link-search=native={}",
            path.parent().unwrap().display()
        );
    } else {
        build.flag("/EHsc").flag("/FIwindows_compat.hpp");
    }
    for source in [
        "Debug.cpp",
        "GpuTrace.cpp",
        "PresentMonTraceConsumer.cpp",
        "PresentMonTraceSession.cpp",
        "TraceConsumer.cpp",
    ] {
        build.file(format!("vendor/presentmon/PresentData/{source}"));
    }
    build.compile("rapo_presentdata");
    if gnu {
        println!("cargo:rustc-link-lib=static=winpthread");
        println!("cargo:rustc-link-lib=static=gcc_eh");
    }
    for library in ["advapi32", "tdh", "version"] {
        println!("cargo:rustc-link-lib={library}");
    }
}

fn write_if_changed(path: &std::path::Path, content: &str) {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == content {
            return;
        }
    }
    let _ = std::fs::write(path, content);
}

fn default_ui_tokens() -> Vec<(&'static str, &'static str)> {
    vec![
        ("txt_app_title", "RINTEL RAPO"),
        ("badge_scheduling", "AGENDAMENTO WINDOWS"),
        ("badge_status_active", "🟢 ATIVO (START)"),
        ("badge_status_paused", "⏸ PAUSADO (STOP)"),
        ("badge_dtt_running", "INTEL DTT EM EXECUÇÃO"),
        ("badge_dtt_stopped", "INTEL DTT DETECTADO"),
        ("badge_dtt_none", "DTT NÃO DETECTADO"),
        ("btn_refresh", "Atualizar"),
        ("btn_scan_steam", "Reexaminar Steam"),
        ("btn_start", "▶ Start"),
        ("btn_stop", "⏹ Stop"),
        ("txt_app_subtitle", "APO e RAPO para jogos Steam: aprendizado em tempo real, núcleos físicos, afinidade, threads e SMT/Hyper-Threading via APIs públicas do Windows"),
        ("badge_conflict", "DISPUTA DE AFINIDADE DETECTADA"),
        ("badge_rapo_paused", "RAPO PAUSADO — candidato preservado"),
        ("badge_rapo_baseline", "RAPO ATIVO — medindo referência sem ajustes"),
        ("badge_rapo_candidate", "RAPO ATIVO — medindo candidato"),
        ("txt_rapo_paused_note", "Sem captura e sem ajustes de teste no alvo. Volte ao mesmo jogo e mantenha a otimização em Start para retomar este candidato."),
        ("badge_rapo_armed", "RAPO ARMADO — aguardando o jogo em foco"),
        ("badge_rapo_disabled", "RAPO DESATIVADO — nenhum perfil em teste"),
        ("txt_rapo_hint_running", "— Marque RAPO ao lado de Perfil APO na biblioteca para iniciar o aprendizado"),
        ("txt_rapo_hint_idle", "— Marque RAPO na biblioteca; o aprendizado aguardará o jogo em foco"),
        ("badge_optimizing", "🟢 PROCESSO EM OTIMIZAÇÃO ATIVA"),
        ("badge_priority_boost", "PRIORIDADE ACIMA DO NORMAL"),
        ("badge_qos_memory", "PRIORIDADE DE MEMÓRIA / ECOQOS"),
        ("badge_background_mode", "MODO SEGUNDO PLANO"),
        ("txt_learning_panel_title", "APRENDIZADO CONTÍNUO · FPS REAL"),
        ("btn_stop_training", "Parar aprendizado e manter último perfil salvo"),
        ("btn_disable_rapo", "Desativar RAPO"),
        ("btn_start_training", "Iniciar aprendizado contínuo"),
        ("txt_combobox_idle", "Abra um jogo Steam"),
        ("chk_rapo_library", "RAPO"),
        ("chk_rapo_tooltip", "Aprendizado adaptativo; pausa ao sair e retoma o mesmo candidato ao voltar. Um jogo por vez."),
        ("txt_telemetry_global", "TELEMETRIA DO PROGRAMA / JOGO (MODO GLOBAL — AGUARDANDO JOGO)"),
        ("badge_telemetry_global", "SISTEMA GLOBAL"),
        ("lbl_fps_ui", "FPS DA INTERFACE"),
        ("lbl_cpu_total", "USO DE CPU TOTAL"),
        ("lbl_gpu_usage", "USO DE GPU"),
        ("lbl_mem_usage", "USO DE MEMÓRIA"),
        ("lbl_disk_read", "LEITURA DE DISCO (SISTEMA)"),
        ("lbl_disk_read_proc", "I/O DE LEITURA (PROCESSO)"),
        ("lbl_net_total", "REDE TOTAL (SISTEMA)"),
        ("lbl_rtt_tcp", "RTT TCP (EXTERNO)"),
        ("txt_cpu_matrix_title", "USO POR NÚCLEO LÓGICO"),
        ("txt_cpu_matrix_sub", "TOPOLOGIA DETECTADA PELO WINDOWS"),
        ("txt_options_title", "OPÇÕES E DIRETRIZES DE AGENDAMENTO"),
        ("chk_auto_yield", "Auto-Yield (afinidade)"),
        ("chk_auto_yield_tooltip", "Cede afinidade se outro programa mudar a máscara de uma thread já fixada"),
        ("chk_windowed_mode", "Manter ajustes em segundo plano"),
        ("chk_windowed_mode_tooltip", "Mantém prioridade e afinidade do jogo mesmo fora de foco; pode prejudicar outros aplicativos"),
        ("chk_minimize_tray", "Minimizar para a Barra de Ícones (Tray)"),
        ("chk_minimize_tray_tooltip", "Ao minimizar a janela, oculta da barra de tarefas e mantém o ícone interativo na bandeja do relógio (com menu Start/Stop)"),
        ("btn_reopen_admin", "Reabrir como administrador"),
        ("btn_reopen_admin_tooltip", "Solicita UAC somente agora. As opções deverão ser selecionadas novamente; processos protegidos ainda podem negar acesso."),
        ("txt_running_games_title", "JOGOS DETECTADOS EM EXECUÇÃO"),
        ("txt_running_empty", "Nenhum processo dentro das pastas dos jogos Steam instalado em execução."),
        ("btn_proc_restore", "Restaurar"),
        ("btn_proc_optimize", "Otimizar"),
        ("txt_library_title", "BIBLIOTECA STEAM"),
        ("hint_search_game", "Buscar jogo..."),
        ("chk_game_pcores", "P-cores"),
        ("chk_game_auto_priority", "Auto Prioridade"),
        ("btn_apo_profile", "Perfil APO"),
        ("btn_apo_profile_tooltip", "Ativa Auto Prioridade, P-cores (4 threads críticas UE5, 5%, 10s), Prioridade de memória / EcoQoS e prefetch somente leitura"),
    ]
}

fn generate_figma_ui_and_tokens() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let tokens = default_ui_tokens();

    // 1. Generate or synchronize ui.json
    let json_path = root.join("ui.json");
    let mut current_map = std::collections::BTreeMap::new();
    if let Ok(content) = std::fs::read_to_string(&json_path) {
        for line in content.lines() {
            let line = line.trim().trim_end_matches(',');
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().trim_matches('"');
                let v = v.trim().trim_matches('"');
                if !k.is_empty() && !v.is_empty() {
                    current_map.insert(k.to_string(), v.replace("\\n", "\n").replace("\\\"", "\""));
                }
            }
        }
    }
    for (k, v) in &tokens {
        current_map.entry(k.to_string()).or_insert_with(|| v.to_string());
    }
    let mut json_str = String::from("{\n");
    let entries: Vec<_> = current_map.iter().collect();
    for (idx, (k, v)) in entries.iter().enumerate() {
        let comma = if idx + 1 < entries.len() { "," } else { "" };
        json_str.push_str(&format!("  \"{}\": \"{}\"{}\n", k, v.replace('"', "\\\""), comma));
    }
    json_str.push_str("}\n");
    write_if_changed(&json_path, &json_str);

    // Seed editable Figma source once; rebuilds embed the current file unchanged.
    let generated_svg = r##"<svg width="1180" height="1520" viewBox="0 0 1180 1520" fill="none" xmlns="http://www.w3.org/2000/svg">
  <style>
    text { font-family: system-ui, -apple-system, Segoe UI, sans-serif; }
    .text-title { font-size: 24px; font-weight: 700; fill: #F3F8FB; }
    .text-sub { font-size: 12px; fill: #8DA9BB; }
    .text-card-title { font-size: 14px; font-weight: 700; fill: #F3F8FB; }
    .text-badge { font-size: 11px; font-weight: 700; }
    .text-metric-val { font-size: 22px; font-weight: 700; fill: #F3F8FB; }
    .text-metric-lbl { font-size: 10px; font-weight: 700; fill: #8DA9BB; }
    .text-btn { font-size: 12px; font-weight: 600; fill: #F3F8FB; }
  </style>
  <rect width="1180" height="1520" fill="#050C13"/>

  <g id="AppHeader">
    <text id="txt_app_title" x="36" y="54" class="text-title">RINTEL RAPO</text>
    <g id="badge_scheduling">
      <rect x="200" y="36" width="168" height="24" rx="5" fill="#46E0D5" fill-opacity="0.15" stroke="#46E0D5"/>
      <text x="212" y="52" class="text-badge" fill="#46E0D5">AGENDAMENTO WINDOWS</text>
    </g>
    <g id="badge_status">
      <rect x="376" y="36" width="130" height="24" rx="5" fill="#46E0D5" fill-opacity="0.15" stroke="#46E0D5"/>
      <text id="txt_badge_status" x="388" y="52" class="text-badge" fill="#46E0D5">🟢 ATIVO (START)</text>
    </g>
    <g id="badge_dtt">
      <rect x="514" y="36" width="155" height="24" rx="5" fill="#46E0D5" fill-opacity="0.15" stroke="#46E0D5"/>
      <text id="txt_badge_dtt" x="526" y="52" class="text-badge" fill="#46E0D5">DTT NÃO DETECTADO</text>
    </g>
    <g id="btn_refresh">
      <rect x="880" y="34" width="76" height="28" rx="6" fill="#0C1A27" stroke="#1E394C"/>
      <text id="txt_btn_refresh" x="896" y="52" class="text-btn">Atualizar</text>
    </g>
    <g id="btn_scan_steam">
      <rect x="964" y="34" width="128" height="28" rx="6" fill="#0C1A27" stroke="#1E394C"/>
      <text id="txt_btn_scan_steam" x="976" y="52" class="text-btn">Reexaminar Steam</text>
    </g>
    <g id="btn_toggle_optimizer">
      <rect x="1100" y="34" width="44" height="28" rx="6" fill="#46E0D5"/>
      <text id="txt_btn_toggle" x="1110" y="52" class="text-btn" fill="#050C13">⏹ Stop</text>
    </g>
    <text id="txt_app_subtitle" x="36" y="82" class="text-sub">APO e RAPO para jogos Steam: aprendizado em tempo real, núcleos físicos, afinidade, threads e SMT/Hyper-Threading via APIs públicas do Windows</text>
  </g>

  <g id="Card_ContinuousLearning">
    <rect x="36" y="100" width="1108" height="150" rx="10" fill="#091520" stroke="#1E394C"/>
    <text id="txt_learning_panel_title" x="52" y="130" class="text-card-title">APRENDIZADO CONTÍNUO · FPS REAL</text>
    <text id="txt_cpu_detected" x="52" y="152" font-size="12px" fill="#F3F8FB">CPU detectada: 8 núcleos físicos · 16 processadores lógicos · 4 núcleos de maior desempenho · SMT disponível true</text>
    <text id="txt_rapo_running_info" x="52" y="174" font-size="12px" fill="#46E0D5">RAPO ATIVO · medindo o candidato atual — Aion2.exe · PID 1240 · 01:45 · 4 medições</text>
    <g id="btn_stop_training">
      <rect x="52" y="196" width="310" height="32" rx="6" fill="#0C1A27" stroke="#1E394C"/>
      <text id="txt_btn_stop_training" x="66" y="216" class="text-btn">Parar aprendizado e manter último perfil salvo</text>
    </g>
    <g id="btn_start_training">
      <rect x="372" y="196" width="200" height="32" rx="6" fill="#46E0D5"/>
      <text id="txt_btn_start_training" x="390" y="216" class="text-btn" fill="#050C13">Iniciar aprendizado contínuo</text>
    </g>
  </g>

  <g id="Card_HeroRapo">
    <rect x="36" y="265" width="1108" height="130" rx="12" fill="#0C1A27" stroke="#46E0D5" stroke-width="1.5"/>
    <g id="badge_rapo_state">
      <rect x="52" y="282" width="225" height="24" rx="5" fill="#46E0D5" fill-opacity="0.15" stroke="#46E0D5"/>
      <text id="txt_rapo_state" x="64" y="298" class="text-badge" fill="#46E0D5">RAPO ATIVO — medindo candidato</text>
    </g>
    <text id="txt_rapo_game_title" x="52" y="330" font-size="14px" font-weight="700" fill="#F3F8FB">Aion2.exe · PID 1240 · 4 medições confirmadas</text>
    <text id="txt_rapo_candidate_specs" x="52" y="354" font-size="12px" fill="#8DA9BB">Prioridade true · afinidade true · até 8 núcleos físicos · até 4 threads críticas · SMT true · somente P-cores true · memória/EcoQoS true</text>
    <text id="txt_rapo_paused_note" x="52" y="378" font-size="11px" fill="#8DA9BB">Sem captura e sem ajustes de teste no alvo. Volte ao mesmo jogo e mantenha a otimização em Start para retomar este candidato.</text>
  </g>

  <g id="Card_Telemetry">
    <rect x="36" y="410" width="1108" height="225" rx="10" fill="#091520" stroke="#1E394C"/>
    <text id="txt_telemetry_title" x="52" y="438" class="text-card-title">TELEMETRIA DO PROGRAMA / JOGO EM EXECUÇÃO: AION 2</text>
    <g id="badge_telemetry_target">
      <rect x="470" y="422" width="76" height="22" rx="5" fill="#46E0D5" fill-opacity="0.15" stroke="#46E0D5"/>
      <text x="484" y="437" class="text-badge" fill="#46E0D5">PID 1240</text>
    </g>
    <g id="metric_fps_ui">
      <rect x="52" y="456" width="145" height="70" rx="8" fill="#0C1A27" stroke="#46E0D5"/>
      <text id="lbl_fps_ui" x="64" y="474" class="text-metric-lbl" fill="#46E0D5">FPS DA INTERFACE</text>
      <text id="val_fps_ui" x="64" y="502" class="text-metric-val">60 FPS</text>
    </g>
    <g id="metric_cpu_total">
      <rect x="207" y="456" width="145" height="70" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <text id="lbl_cpu_total" x="219" y="474" class="text-metric-lbl">USO DE CPU TOTAL</text>
      <text id="val_cpu_total" x="219" y="502" class="text-metric-val" fill="#46E0D5">34.2%</text>
    </g>
    <g id="metric_gpu_usage">
      <rect x="362" y="456" width="145" height="70" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <text id="lbl_gpu_usage" x="374" y="474" class="text-metric-lbl">USO DE GPU</text>
      <text id="val_gpu_usage" x="374" y="502" class="text-metric-val" fill="#55D7FF">68.5%</text>
    </g>
    <g id="metric_mem_usage">
      <rect x="517" y="456" width="145" height="70" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <text id="lbl_mem_usage" x="529" y="474" class="text-metric-lbl">USO DE MEMÓRIA</text>
      <text id="val_mem_usage" x="529" y="502" class="text-metric-val">3.45 GB</text>
    </g>
    <g id="metric_disk_read">
      <rect x="52" y="536" width="220" height="70" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <text id="lbl_disk_read_proc" x="64" y="554" class="text-metric-lbl">I/O DE LEITURA (PROCESSO)</text>
      <text id="val_disk_read" x="64" y="582" class="text-metric-val" fill="#FFD96B">12.4 MB/s</text>
    </g>
    <g id="metric_net_total">
      <rect x="282" y="536" width="220" height="70" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <text id="lbl_net_total" x="294" y="554" class="text-metric-lbl">REDE TOTAL (SISTEMA)</text>
      <text id="val_net_total" x="294" y="582" class="text-metric-val" fill="#55D7FF">45.0 KB/s</text>
    </g>
    <g id="metric_rtt_tcp">
      <rect x="512" y="536" width="150" height="70" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <text id="lbl_rtt_tcp" x="524" y="554" class="text-metric-lbl">RTT TCP (EXTERNO)</text>
      <text id="val_rtt_tcp" x="524" y="582" class="text-metric-val" fill="#46E0D5">18 ms</text>
    </g>
  </g>

  <g id="Card_CpuMatrix">
    <rect x="36" y="650" width="1108" height="150" rx="10" fill="#091520" stroke="#1E394C"/>
    <text id="txt_cpu_matrix_title" x="52" y="678" class="text-card-title">USO POR NÚCLEO LÓGICO</text>
    <text id="txt_cpu_matrix_sub" x="240" y="678" font-size="11px" fill="#8DA9BB">TOPOLOGIA DETECTADA PELO WINDOWS</text>
  </g>

  <g id="Card_Options">
    <rect x="36" y="815" width="1108" height="85" rx="10" fill="#091520" stroke="#1E394C"/>
    <text id="txt_options_title" x="52" y="842" class="text-card-title">OPÇÕES E DIRETRIZES DE AGENDAMENTO</text>
    <g id="chk_auto_yield">
      <rect x="52" y="856" width="16" height="16" rx="3" fill="#46E0D5"/>
      <text id="txt_chk_auto_yield" x="76" y="869" font-size="12px" fill="#F3F8FB">Auto-Yield (afinidade)</text>
    </g>
    <g id="chk_windowed_mode">
      <rect x="236" y="856" width="16" height="16" rx="3" fill="#0C1A27" stroke="#1E394C"/>
      <text id="txt_chk_windowed_mode" x="260" y="869" font-size="12px" fill="#F3F8FB">Manter ajustes em segundo plano</text>
    </g>
    <g id="chk_minimize_tray">
      <rect x="500" y="856" width="16" height="16" rx="3" fill="#46E0D5"/>
      <text id="txt_chk_minimize_tray" x="524" y="869" font-size="12px" fill="#F3F8FB">Minimizar para a Barra de Ícones (Tray)</text>
    </g>
    <g id="btn_reopen_admin">
      <rect x="780" y="850" width="190" height="28" rx="6" fill="#0C1A27" stroke="#1E394C"/>
      <text id="txt_btn_reopen_admin" x="794" y="868" class="text-btn">Reabrir como administrador</text>
    </g>
  </g>

  <g id="Card_RunningGames">
    <rect x="36" y="915" width="1108" height="135" rx="10" fill="#091520" stroke="#1E394C"/>
    <text id="txt_running_games_title" x="52" y="942" class="text-card-title">JOGOS DETECTADOS EM EXECUÇÃO (1)</text>
    <g id="item_proc_0">
      <rect x="52" y="956" width="1076" height="70" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <g id="badge_optimizing">
        <rect x="66" y="972" width="145" height="22" rx="5" fill="#46E0D5" fill-opacity="0.15" stroke="#46E0D5"/>
        <text x="78" y="987" class="text-badge" fill="#46E0D5">OTIMIZANDO AGORA</text>
      </g>
      <text id="txt_proc_name" x="220" y="988" font-size="15px" font-weight="700" fill="#F3F8FB">AION 2</text>
      <g id="badge_proc_pid">
        <rect x="290" y="972" width="70" height="22" rx="5" fill="#55D7FF" fill-opacity="0.15" stroke="#55D7FF"/>
        <text x="302" y="987" class="text-badge" fill="#55D7FF">PID 1240</text>
      </g>
      <g id="btn_proc_toggle">
        <rect x="1030" y="970" width="80" height="26" rx="6" fill="#0C1A27" stroke="#1E394C"/>
        <text id="txt_btn_proc_toggle" x="1046" y="987" class="text-btn">Restaurar</text>
      </g>
      <text id="txt_proc_path" x="66" y="1014" font-size="11px" fill="#68869A">C:\Steam\steamapps\common\AION2\Aion2.exe</text>
    </g>
  </g>

  <g id="Card_SteamLibrary">
    <rect x="36" y="1065" width="1108" height="240" rx="10" fill="#091520" stroke="#1E394C"/>
    <text id="txt_library_title" x="52" y="1092" class="text-card-title">BIBLIOTECA STEAM (1)</text>
    <g id="input_search_game">
      <rect x="890" y="1076" width="220" height="26" rx="5" fill="#050C13" stroke="#1E394C"/>
      <text id="hint_search_game" x="904" y="1093" font-size="11px" fill="#8DA9BB">Buscar jogo...</text>
    </g>
    <g id="item_lib_0">
      <rect x="52" y="1116" width="1076" height="60" rx="8" fill="#0C1A27" stroke="#142A39"/>
      <text id="txt_lib_game_name" x="66" y="1150" font-size="15px" font-weight="700" fill="#F3F8FB">AION 2</text>
      <g id="chk_game_pcores">
        <rect x="680" y="1136" width="16" height="16" rx="3" fill="#46E0D5"/>
        <text id="txt_chk_game_pcores" x="704" y="1149" font-size="12px" fill="#F3F8FB">P-cores</text>
      </g>
      <g id="chk_game_auto_priority">
        <rect x="780" y="1136" width="16" height="16" rx="3" fill="#46E0D5"/>
        <text id="txt_chk_game_auto_priority" x="804" y="1149" font-size="12px" fill="#F3F8FB">Auto Prioridade</text>
      </g>
      <g id="chk_game_rapo">
        <rect x="910" y="1136" width="16" height="16" rx="3" fill="#46E0D5"/>
        <text id="txt_chk_game_rapo" x="934" y="1149" font-size="12px" fill="#F3F8FB">RAPO</text>
      </g>
      <g id="btn_apo_profile">
        <rect x="1000" y="1130" width="90" height="28" rx="6" fill="#0C1A27" stroke="#1E394C"/>
        <text id="txt_btn_apo_profile" x="1016" y="1148" class="text-btn">Perfil APO</text>
      </g>
    </g>
  </g>
</svg>
"##;
    let svg_path = root.join("ui_figma.svg");
    let svg_content = std::fs::read_to_string(&svg_path).unwrap_or_else(|_| generated_svg.to_string());
    write_if_changed(&svg_path, &svg_content);
}
