use serde::Deserialize;
use std::fmt::Write;
use wasm_bindgen::prelude::*;
use crate::escape;
use crate::profile::{ProfileSettings, render_settings};

#[derive(Deserialize)]
pub struct Dashboard {
    pub alerts: Vec<String>,
    pub subtitle: String,
    pub target: String,
    pub metrics: Vec<(String, String)>,
    pub cores: Vec<(usize, f32, bool)>,
    pub cache: Vec<String>,
    pub running: Vec<Process>,
    pub auto_yield: bool,
    pub windowed: bool,
    pub minimize_tray: bool,
    pub library_count: usize,
    pub aion2_status: String,
    pub pin_status: Option<String>,
    pub profile: Option<AppliedProfile>,
    pub power_plan: PowerPlanState,
    pub network_tweaks: Vec<String>,
    #[serde(default)]
    pub network_priority: bool,
    #[serde(default)]
    pub routes: Routes,
    #[serde(default)]
    pub route_endpoints: Vec<String>,
    #[serde(default)]
    pub network_target_pid: u32,
    #[serde(default)]
    pub relay: Relay,
    #[serde(default)]
    pub wfp: Wfp,
    #[serde(default)]
    pub engine_ini: EngineIni,
    #[serde(default)]
    pub vpn: crate::vpn::Vpn,
}

#[derive(Default, Deserialize)]
pub struct EngineIni {
    pub path: String,
    pub active: bool,
    pub applied: bool,
    #[serde(default)]
    pub has_backup: bool,
    pub options: Vec<EngineIniOption>,
    pub error: String,
}

#[derive(Deserialize)]
pub struct EngineIniOption {
    pub key: String,
    pub label: String,
    pub enabled: bool,
}

#[derive(Default, Deserialize)]
pub struct Wfp {
    pub enabled: bool,
    pub running: bool,
    pub target: String,
    pub error: String,
}

#[derive(Default, Deserialize)]
pub struct Relay {
    pub running: bool,
    pub stopping: bool,
    pub address: String,
    pub active_connections: usize,
    pub total_connections: u64,
    pub bytes_tx: u64,
    pub bytes_rx: u64,
    #[serde(default)]
    pub prewarm_hits: u64,
    #[serde(default)]
    pub prewarm_misses: u64,
    pub error: String,
    pub predictions: Vec<RelayPrediction>,
    pub interfaces: Vec<RouteCandidate>,
    pub allowed_interfaces: Vec<String>,
    pub mode: String,
    #[serde(default)]
    pub tunnel: String,
    pub hy2_server: String,
    pub hy2_server_name: String,
    #[serde(default)]
    pub hy2_connected: bool,
    #[serde(default)]
    pub hy2_error: String,
    /// The server sealed into the executable is in use; nothing was typed.
    #[serde(default)]
    pub hy2_builtin: bool,
    /// ICMP round trip to the HY2 server in ms; 0 while unmeasured or unanswered.
    #[serde(default)]
    pub hy2_ping_ms: u32,
    pub udp_associations: usize,
    pub udp_destinations: u64,
    pub udp_route: String,
    #[serde(default)]
    pub redundancy: Redundancy,
    #[serde(default)]
    pub redundancy_auto_echo: String,
    #[serde(default)]
    pub redundancy_manual: Redundancy,
    #[serde(default)]
    pub redundancy_learning: String,
}

#[derive(Default, Clone, Copy, Deserialize, PartialEq, Eq, Debug)]
pub struct Redundancy {
    pub enabled: bool,
    pub x: u32,
    pub y: u32,
}
#[derive(Deserialize)]
pub struct RelayPrediction {
    pub destination: String,
    pub interface: u32,
    pub source: String,
    pub gateway: String,
    pub samples: usize,
    pub failures: usize,
    pub observed_ms: Option<f64>,
    pub predicted_ms: Option<f64>,
    pub uncertainty_ms: f64,
    pub selected: bool,
}

#[derive(Deserialize)]
pub struct PowerPlan {
    pub name: String,
    pub guid: String,
}

/// Last plan read from Windows; `error` is set when the latest read failed.
#[derive(Deserialize)]
pub struct PowerPlanState {
    pub plan: Option<PowerPlan>,
    pub error: String,
}

#[derive(Deserialize)]
pub struct AppliedProfile {
    pub pid: u32,
    pub game: String,
    #[serde(default)]
    pub exe: String,
    pub source: String,
    pub configured: ProfileSettings,
    pub priority: bool,
    pub pinned_threads: usize,
    pub qos: bool,
}

#[derive(Deserialize)]
pub struct Process {
    pub pid: u32,
    pub game: String,
    pub exe: String,
    pub state: String,
    pub enabled: bool,
    pub auto: bool,
    pub qos: bool,
    pub priority: bool,
    pub can_toggle: bool,
}

#[derive(Default, Deserialize)]
pub struct Routes {
    pub candidates: Vec<RouteCandidate>,
    pub owned: Vec<OwnedRoute>,
    pub pending: bool,
    pub message: String,
    pub error: String,
    pub automatic: String,
    pub auto_active: bool,
    pub history: Vec<String>,
    pub rate: Option<Rate>,
}
#[derive(Deserialize)]
pub struct RouteCandidate { pub id: String, pub interface: String, pub index: u32, pub gateway: String, pub ipv6: bool }
#[derive(Deserialize)]
pub struct OwnedRoute { pub destination: String, pub interface: String, pub gateway: String, pub conflict: bool }
#[derive(Deserialize)]
pub struct Rate { pub exempt: Vec<String>, pub policy: String, pub bps: u64, pub confirmed: bool, pub conflict: bool }

/// File names of the exempt executables, e.g. `steam.exe, rintel-rapo.exe, AION2.exe`.
fn exempt_names(rate: &Rate) -> String {
    rate.exempt.iter().map(|path| escape(path.rsplit(['\\', '/']).next().unwrap_or(path))).collect::<Vec<_>>().join(", ")
}

const RELAY_MODES: [(&str, &str); 4] = [("direct", "Direto · rota do sistema"), ("local", "Local · Flow nas interfaces autorizadas"),
    ("hy2", "HY2 · sem fallback direto"), ("neural_auto", "Neural Auto · direto/HY2 nas novas conexões TCP")];

/// First `<number>%` in a host-formatted metric, e.g. `"18.2% · Sistema 22.0%"` -> 18.2.
fn first_percent(text: &str) -> Option<f32> {
    let number = &text[..text.find('%')?];
    let start = number.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.').len();
    number[start..].parse::<f32>().ok().filter(|p| p.is_finite())
}

/// 0–100% meter with the core color thresholds: green, amber above 50%, red above 85%.
fn usage_bar(html: &mut String, percent: f32, label: &str) {
    let percent = percent.clamp(0.0, 100.0);
    let color = if percent > 85.0 { "core-high" } else if percent > 50.0 { "core-med" } else { "core-low" };
    write!(html, "<div class=\"core-bar-bg\" role=\"meter\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-valuenow=\"{percent:.0}\" aria-label=\"{}\"><div class=\"core-bar-fill {color}\" style=\"width: {percent:.1}%\"></div></div>", escape(label)).unwrap();
}

fn render_engine_ini(html: &mut String, ini: &EngineIni) {
    write!(html, "<section class=\"power-plan\"><h2>ENGINE.INI DO AION 2</h2><label class=\"property-toggle\"><input type=\"checkbox\" data-action=\"engine-ini-active\"{}> <div class=\"property-text\"><span class=\"property-name\">{}</span><span class=\"property-desc\">{}</span></div></label><p><code>{}</code></p><h3>PERFIL SALVO</h3>",
        if ini.active { " checked" } else { "" },
        if ini.active { "ON · opções marcadas gravadas no Engine.ini" } else { "OFF · nenhuma linha do RAPO no Engine.ini" },
        if ini.applied { "Ajustes do RAPO ativos no arquivo agora" } else { "Arquivo sem linhas do RAPO" }, escape(&ini.path)).unwrap();
    for option in &ini.options {
        let key = escape(&option.key);
        write!(html, "<label class=\"network-choice\"><input type=\"checkbox\" data-action=\"engine-ini\" data-key=\"{key}\"{}><span>{}</span></label>",
            if option.enabled { " checked" } else { "" }, escape(&option.label)).unwrap();
    }
    let (bak_label, bak_disabled) = if ini.has_backup {
        ("Restaurar backup do Engine.ini", "")
    } else {
        ("Restaurar backup (nenhum backup salvo)", " disabled")
    };
    let bak_status = if ini.has_backup {
        "Backup salvo no disco (.rapo-bak)"
    } else {
        "Nenhum backup salvo ainda"
    };
    write!(html, "<fieldset class=\"engine-ini-backup-fs\"><legend>Backup e restauração do Engine.ini</legend><p class=\"net-card-sub\">{bak_status}</p><button type=\"button\" data-action=\"engine-ini-backup\">Criar backup do Engine.ini</button><button type=\"button\" data-action=\"engine-ini-restore\"{bak_disabled}>{bak_label}</button></fieldset>").unwrap();
    html.push_str("<details class=\"how-it-works\"><summary>Como funciona</summary><p>Escolha um preset (1 a 5), que é exclusivo, e marque os extras que quiser. A seleção e o ON/OFF ficam salvos e voltam ao abrir o RAPO. O jogo lê o arquivo ao iniciar: ligue antes de abrir o AION 2. Desativar (OFF), fechar o RAPO ou restaurar o backup removem as linhas e todos os cabeçalhos adicionados pelo RAPO ([/Script/Engine.Engine], [ConsoleVariables], [SystemSettings], [/Script/Engine.StreamingSettings], [/Script/Engine.GarbageCollectionSettings], [Core.Log]), restaurando o arquivo sem tags residuais.</p></details>");
    if !ini.error.is_empty() { write!(html, "<p class=\"alert\" role=\"alert\">{}</p>", escape(&ini.error)).unwrap(); }
    html.push_str("</section>");
}

/// Which HY2 server the relay uses: a typed one, the one sealed into the executable, or the loopback fallback.
fn hy2_server_label(relay: &Relay) -> String {
    if !relay.hy2_server.is_empty() { escape(&relay.hy2_server) }
    else if relay.hy2_builtin { "HY2 integrado ao RAPO".into() }
    else { "HY2 local embutido (loopback 127.0.0.1)".into() }
}

/// One-line HY2 state: off, connecting, on and working (QUIC session open) with ping and traffic, or the last connect error.
fn hy2_status(relay: &Relay) -> String {
    if !relay.tunnel.is_empty() {
        let name = escape(&relay.tunnel);
        let (class, status) = if !relay.running { ("hy2-off", "relay parado") }
            else if relay.hy2_connected { ("hy2-on", "sessão do túnel ativa") }
            else if !relay.hy2_error.is_empty() { return format!("<span class=\"hy2-status hy2-error\" role=\"status\">{name} · erro: {}</span>", escape(&relay.hy2_error)); }
            else { ("hy2-pending", "conectando") };
        return format!("<span class=\"hy2-status {class}\" role=\"status\">{name} · {status}</span>");
    }
    let server = hy2_server_label(relay);
    let (class, text) = if !matches!(relay.mode.as_str(), "hy2" | "neural_auto") {
        let mode = RELAY_MODES.iter().find(|(id, _)| *id == relay.mode).map_or("desconhecido", |(_, label)| label);
        ("hy2-off", format!("Hysteria 2 desligado · modo atual: {mode} · para usar, desabilite o relay, escolha HY2 em Transporte e aplique"))
    } else if !relay.running {
        ("hy2-off", format!("Hysteria 2 selecionado · relay parado · {server}"))
    } else if relay.hy2_connected {
        let ping = if relay.hy2_ping_ms == 0 { "ping sem resposta ICMP".to_string() } else { format!("ping {} ms", relay.hy2_ping_ms) };
        ("hy2-on", format!("✅ Hysteria 2 ligado e funcionando · sessão QUIC ativa · {server} · {ping} · {} conexões · ↑ {:.1} KiB ↓ {:.1} KiB", relay.active_connections, relay.bytes_tx as f64 / 1024.0, relay.bytes_rx as f64 / 1024.0))
    } else if !relay.hy2_error.is_empty() {
        ("hy2-error", format!("⚠️ Hysteria 2 com erro · {server} · {}", escape(&relay.hy2_error)))
    } else {
        ("hy2-pending", format!("Hysteria 2 ligado · conectando · {server}"))
    };
    format!("<span class=\"hy2-status {class}\" role=\"status\">{text}</span>")
}

fn render_relay(html: &mut String, relay: &Relay, wfp: &Wfp) {
    let (class, status) = if relay.running { ("net-card-active", "Escutando") }
        else if relay.stopping { ("net-card-warn", "Encerrando conexões") }
        else { ("net-card-off", "Desativado") };
    write!(html, "<div class=\"net-card relay-card {class}\"><span class=\"net-card-label\">Relay local · SOCKS5 TCP/UDP</span><span class=\"net-card-value\" role=\"status\">{status}</span><span class=\"net-card-sub\">{}</span><label><input type=\"checkbox\" data-action=\"relay\"{}{}> Habilitar relay local</label><span class=\"net-card-sub\">{} TCP ativas · {} associações UDP · {} destinos UDP ativos · saída UDP fixa: {} · {} TCP aceitas<br>Enviado: {:.1} KiB · Recebido: {:.1} KiB<br>Conexões pré-abertas: {} usadas · {} descartadas</span>", escape(&relay.address), if relay.running { " checked" } else { "" }, if relay.stopping { " disabled" } else { "" }, relay.active_connections, relay.udp_associations, relay.udp_destinations, escape(&relay.udp_route), relay.total_connections, relay.bytes_tx as f64 / 1024.0, relay.bytes_rx as f64 / 1024.0, relay.prewarm_hits, relay.prewarm_misses).unwrap();
    html.push_str(&hy2_status(relay));
    html.push_str("<span class=\"net-disclaimer\">Configure SOCKS5 neste endereço no cliente compatível. TCP CONNECT e UDP ASSOCIATE funcionam sem WFP; o tunelamento transparente do AION 2 exige o driver MUDWFP, solicitado somente ao ativar a opção abaixo.</span>");
    let (_wfp_class, wfp_status) = if wfp.running { ("net-card-active", format!("Driver e proxy ativos · alvo {}", escape(&wfp.target))) }
        else if wfp.enabled { ("net-card-warn", "Inativo · verifique o driver e as permissões".into()) }
        else { ("net-card-off", "Desativado".into()) };
    write!(html, "<hr><label><input type=\"checkbox\" data-action=\"wfp\"{}{}> Tunelar processo via WFP ({wfp_status})</label>", if wfp.enabled { " checked" } else { "" }, if relay.stopping { " disabled" } else { "" }).unwrap();
    if !wfp.error.is_empty() { write!(html, "<span class=\"alert\" role=\"alert\">{}</span>", escape(&wfp.error)).unwrap(); }
    {
        let eff = relay.redundancy;
        let eff_x = if eff.x > 0 { eff.x } else { 20 };
        let eff_y = if eff.y > 0 { eff.y } else { 10 };
        let overhead = if eff.enabled && eff_x > 0 { (eff_y as u64 * 100) / eff_x as u64 } else { 0 };

        let m = relay.redundancy_manual;
        let manual_x = if m.x > 0 { m.x } else if eff.x > 0 { eff.x } else { 20 };
        let manual_y = if m.y > 0 { m.y } else if eff.y > 0 { eff.y } else { 10 };
        let manual_enabled = m.enabled || (m.x == 0 && m.y == 0 && eff.enabled);

        let eff_status = if eff.enabled { format!("{eff_x}:{eff_y}") } else { "desativada".into() };

        write!(html, "<hr><details><summary>UDPspeeder · Redundância de pacotes UDP ({} · +{overhead}% tráfego)</summary><p class=\"net-card-sub\">Replica pacotes UDP no cliente (razão X:Y) para tolerar perda sem depender de software no servidor.<br>Razão manual: {manual_x}:{manual_y} · Razão efetiva: {eff_status}</p><fieldset id=\"udp-redundancy-fs\"><legend>Configuração manual (X:Y manual: {manual_x}:{manual_y})</legend><label><input id=\"udp-red-enabled\" type=\"checkbox\" data-action=\"udp-redundancy-toggle\"{}> Ativar redundância UDP (UDPspeeder)</label><label for=\"udp-red-x\">Pacotes originais (X)</label><input id=\"udp-red-x\" type=\"number\" min=\"1\" max=\"30\" value=\"{manual_x}\"><label for=\"udp-red-y\">Pacotes redundantes (Y)</label><input id=\"udp-red-y\" type=\"number\" min=\"1\" max=\"30\" value=\"{manual_y}\"><button type=\"button\" data-action=\"udp-redundancy-save\">Aplicar razão X:Y</button></fieldset></details>",
            if eff.enabled { "Ativo" } else { "Desativado" },
            if manual_enabled { " checked" } else { "" }).unwrap();
    }
    {
        let r = relay;
        let status = match r.redundancy_learning.trim() {
            "aprendendo" => "aprendendo",
            "ativo" => "ativo",
            "sem feedback" | "sem_feedback" => "sem feedback",
            _ => "desativado",
        };
        let eff = r.redundancy;
        let eff_x = if eff.x > 0 { eff.x } else { 20 };
        let eff_y = if eff.y > 0 { eff.y } else { 10 };
        let eff_status = if eff.enabled { format!("{eff_x}:{eff_y}") } else { "desativada".into() };
        write!(html, "<fieldset id=\"udp-redundancy-auto-fs\"><legend>Aprendizado da razão de pacotes (Estado: {status} · Razão efetiva: {eff_status})</legend><label><input id=\"udp-red-auto\" type=\"checkbox\" data-action=\"udp-redundancy-auto-toggle\"{}> Deixar o aprendizado controlar X:Y</label><p>Aprende com as últimas três rodadas do eco local. Não mede perda no caminho até o jogo. Pare o relay para alterar esta opção.</p><p class=\"net-card-sub\">Estado: {status} · Razão efetiva: {eff_status}</p><label for=\"udp-red-echo\">Eco UDP em loopback (IP:porta)</label><input id=\"udp-red-echo\" type=\"text\" value=\"{}\"><button type=\"button\" data-action=\"udp-redundancy-auto-save\">Aplicar eco</button></fieldset>", if r.redundancy_auto_echo.is_empty() { "" } else { " checked" }, escape(&r.redundancy_auto_echo)).unwrap();
    }
    // Without a remote server the relay uses the embedded loopback HY2 server, so HY2 is always configured.
    {
        let mode = RELAY_MODES.iter().find(|(id, _)| *id == relay.mode).map_or("desconhecido", |(_, label)| label);
        let server = hy2_server_label(relay);
        write!(html, "<details><summary>Transporte · {mode}</summary><p class=\"net-card-sub\">HY2 configurado: {server} · nome TLS: {}</p><fieldset id=\"relay-transport\"{}><legend>Configuração desta sessão</legend><label for=\"relay-mode\">Modo</label><select id=\"relay-mode\">", if relay.hy2_server_name.is_empty() { "host do servidor".into() } else { escape(&relay.hy2_server_name) }, if relay.running || relay.stopping { " disabled" } else { "" }).unwrap();
        for (id, label) in RELAY_MODES {
            write!(html, "<option value=\"{id}\"{}>{label}</option>", if relay.mode == id { " selected" } else { "" }).unwrap();
        }
        html.push_str("</select><label for=\"relay-server\">Servidor HY2 próprio (opcional; vazio = servidor integrado ao RAPO)</label><input id=\"relay-server\" maxlength=\"512\" placeholder=\"vazio usa o servidor integrado\"><label for=\"relay-server-name\">Nome TLS (opcional)</label><input id=\"relay-server-name\" maxlength=\"253\"><label for=\"relay-auth\">Autenticação HY2 (vazia com o servidor integrado)</label><input id=\"relay-auth\" type=\"password\" autocomplete=\"off\" maxlength=\"4096\"><label for=\"relay-ca\">CA PEM (opcional; substitui as raízes do Windows)</label><textarea id=\"relay-ca\" maxlength=\"65536\" rows=\"3\"></textarea><button type=\"button\" data-action=\"relay-configure\">Aplicar configuração</button></fieldset><p class=\"net-disclaimer\">Desabilite o relay antes de alterar. Escolha Hysteria 2 e aplique: o servidor integrado ao RAPO é usado sem digitar nada (credencial cifrada no executável, nunca exibida). Credencial digitada fica só em memória; TLS sempre validado. Uma versão sem servidor integrado usa o HY2 local embutido em loopback, que só acrescenta latência. Neural Auto pode usar a saída direta e mudar o IP público de novas conexões; UDP mantém a saída escolhida durante a sessão, sem estimativa de RTT por envio.</p></details>");
    }
    if !relay.error.is_empty() { write!(html, "<span class=\"alert\" role=\"alert\">{}</span>", escape(&relay.error)).unwrap(); }
    if !relay.interfaces.is_empty() {
        html.push_str("<details><summary>Autorizar outras saídas</summary><p>A saída atual do Windows sempre permanece elegível. Autorizar outra interface pode contornar sua VPN e mudar o IP público; afeta somente conexões SOCKS5 novas.</p>");
        for interface in &relay.interfaces {
            let id = escape(&interface.id);
            write!(html, "<label class=\"network-choice\"><input id=\"relay-interface-{id}\" type=\"checkbox\" data-action=\"relay-interfaces\" data-relay-candidate=\"{id}\"{}{}><span>{} · if {} · {} · {}</span></label>", if relay.allowed_interfaces.contains(&interface.id) { " checked" } else { "" }, if relay.running { "" } else { " disabled" }, escape(&interface.interface), interface.index, if interface.ipv6 { "IPv6" } else { "IPv4" }, escape(&interface.gateway)).unwrap();
        }
        html.push_str("<span class=\"net-disclaimer\">Reinicie o relay para atualizar a lista após trocar adaptadores.</span></details>");
    }
    html.push_str("</div>");
    let (class, status) = if !relay.running { ("net-card-off", "Parado") }
        else if relay.predictions.is_empty() { ("net-card-warn", "Aguardando conexões do jogo") }
        else { ("net-card-active", "Aprendendo com conexões reais") };
    write!(html, "<div class=\"net-card net-card-wide {class}\"><span class=\"net-card-label\">Modelo Neural · Relay</span><span class=\"net-card-value\">Flow 9→16→1 · {status}</span><span class=\"net-card-sub\">Compara o tempo de abertura TCP por destino e saída, não o ping da partida nem a latência de cada pacote.</span>").unwrap();
    if !relay.predictions.is_empty() {
        html.push_str("<div class=\"relay-predictions\"><table class=\"net-policy-table\"><thead><tr><th>Destino / saída</th><th>Medido</th><th>Previsão (TCP/UDP)</th><th>Amostras / falhas</th></tr></thead><tbody>");
        for prediction in &relay.predictions {
            let via = match prediction.gateway.as_str() { "hy2" => "túnel HY2".into(), "system" => "rota do sistema".into(), gateway => escape(gateway) };
            write!(html, "<tr><td>{}<br><span class=\"net-card-sub\">if {} · {} via {via}{}</span></td><td>", escape(&prediction.destination), prediction.interface, escape(&prediction.source), if prediction.selected { " · última saída escolhida" } else { "" }).unwrap();
            if let Some(ms) = prediction.observed_ms { write!(html, "{ms:.1} ms").unwrap(); } else { html.push_str("Falha de conexão"); }
            html.push_str("</td><td>");
            if let Some(ms) = prediction.predicted_ms { write!(html, "{ms:.1} ms<br><span class=\"net-card-sub\">Desvio/resíduo: {:.1} ms</span>", prediction.uncertainty_ms).unwrap(); } else { write!(html, "<span class=\"net-card-sub\">Treinando rede ({}/2)</span>", prediction.samples).unwrap(); }
            write!(html, "</td><td>{} / {}</td></tr>", prediction.samples, prediction.failures).unwrap();
        }
        html.push_str("</tbody></table></div>");
    }
    html.push_str("<span class=\"net-disclaimer\">Previsão de abertura TCP após poucas amostras; desvio/resíduo é descritivo, não intervalo de confiança nem ganho confirmado.</span></div>");
}

fn render_network(html: &mut String, view: &Dashboard) {
    let routes = &view.routes;
    let disabled = if routes.pending || routes.auto_active { " disabled" } else { "" };
    html.push_str("<section id=\"network-routes\"><h2>ROTAS POR IP · INTERFACE / GATEWAY</h2><p class=\"route-warning\">Rota /32 ou /128 afeta TODOS os aplicativos e portas que acessam o IP, não somente o jogo. Pode desconectar sessões. Sem túnel/relay, driver, alteração de payload ou garantia de ping/anti-cheat.</p><details class=\"how-it-works\"><summary>Limpeza e registro de erros</summary><p>Somente rotas criadas nesta sessão são removidas se inalteradas. Saída normal aguarda limpeza por até 30 s; falha/crash pode deixar rotas e políticas até reiniciar. Não há varredura insegura de sessões anteriores. Erros de saída: %TEMP%\\rintel-rapo-routes.log.</p></details>");
    if !routes.error.is_empty() { write!(html, "<p class=\"alert\" role=\"alert\">{}</p>", escape(&routes.error)).unwrap(); }
    write!(html, "<p role=\"status\">{}{}</p><p role=\"status\">{}</p>", if routes.pending { "Operação de rede pendente. " } else { "" }, escape(&routes.message), escape(&routes.automatic)).unwrap();
    write!(html, "<fieldset{disabled}><legend>Configuração explícita de rede</legend><button data-action=\"route-refresh\">Atualizar gateways/interfaces</button><label for=\"route-destination\">IP numérico de destino</label><input id=\"route-destination\" type=\"text\" maxlength=\"64\" list=\"route-endpoints\" placeholder=\"192.0.2.42 ou 2001:db8::42\" autocomplete=\"off\"><datalist id=\"route-endpoints\">").unwrap();
    for endpoint in &view.route_endpoints { write!(html, "<option value=\"{}\"></option>", escape(endpoint)).unwrap(); }
    html.push_str("</datalist><p>As sugestões são IPs TCP observados, não servidores de partida confirmados. Aplicação manual também aceita outro IP numérico remoto.</p><label for=\"route-candidate\">Gateway/interface existentes</label><select id=\"route-candidate\"><option value=\"\">Selecione uma saída</option>");
    for candidate in &routes.candidates {
        write!(html, "<option value=\"{}\">{} · if {} · {} · {}</option>", escape(&candidate.id), escape(&candidate.interface), candidate.index, escape(&candidate.gateway), if candidate.ipv6 { "IPv6" } else { "IPv4" }).unwrap();
    }
    html.push_str("</select><button data-action=\"route-apply\">Aplicar rota manual</button><h3>GATEWAYS AUTORIZADOS PARA EXPLORAÇÃO ATIVA</h3>");
    if routes.candidates.len() < 2 { html.push_str("<p>Sem saída alternativa disponível da mesma família: atualize a lista. Automático não altera rotas com apenas um gateway.</p>"); }
    for (index, candidate) in routes.candidates.iter().enumerate() {
        write!(html, "<label class=\"network-choice\"><input id=\"route-authorize-{index}\" data-route-candidate=\"{}\" type=\"checkbox\"> {} · if {} · {}</label>", escape(&candidate.id), escape(&candidate.interface), candidate.index, escape(&candidate.gateway)).unwrap();
    }
    html.push_str("<label class=\"network-choice\"><input id=\"network-consent\" type=\"checkbox\"> Autorizo testes ativos SOMENTE das saídas/taxas selecionadas; podem desconectar ou atrasar meu jogo. Nada será alterado apenas por detectar um jogo.</label>");
    write!(html, "<button data-action=\"route-learn\"{}>Aprender e aplicar · rotas autorizadas</button>", if routes.candidates.len() < 2 { " disabled" } else { "" }).unwrap();
    html.push_str("<h3>QOS DE SAÍDA DO SISTEMA · MB/s DECIMAIS</h3><p>Limita o envio (upload) de todo o Windows; Steam, o próprio RAPO (que encaminha o tráfego do jogo pelo relay) e o jogo alvo ficam fora do limite. Download não é limitado. Não é pacing de pacotes, SQM/CAKE nem controle da fila da operadora. Informe taxas autorizadas; velocidade da placa não é upload medido. Outra política global existente recusa a aplicação; DSCP de políticas anteriores é preservado.</p>");
    write!(html, "<p>{}</p><label for=\"rate-mbs\">Limite do sistema em MB/s</label><input id=\"rate-mbs\" type=\"text\" inputmode=\"decimal\" placeholder=\"5\"><button data-action=\"rate-apply\" data-pid=\"{}\">Aplicar limite do sistema</button><label for=\"rate-list\">Taxas autorizadas em MB/s, separadas por vírgula (mínimo duas; exige jogo alvo)</label><input id=\"rate-list\" type=\"text\" placeholder=\"2, 5\"><button data-action=\"rate-learn\" data-pid=\"{}\"{}>Aprender e aplicar · taxas autorizadas</button></fieldset>",
        if view.network_target_pid == 0 { "Sem jogo alvo detectado: Steam e RAPO ficam isentos; o jogo só é isento quando estiver em execução.".to_string() } else { format!("Jogo alvo isento: {}. Todos os executáveis do jogo ficam fora do limite, identificados pelo caminho (os PIDs mudam a cada abertura) e revalidados pelo host.", escape(&view.target)) },
        view.network_target_pid, view.network_target_pid, if view.network_target_pid == 0 { " disabled" } else { "" }).unwrap();
    write!(html, "<button data-action=\"network-stop\"{}>Parar aprendizado e restaurar referência</button><button data-action=\"route-remove-all\"{}>Remover todos os ajustes de rede desta sessão</button><button data-action=\"rate-remove\"{}>Remover limite do sistema</button>", if routes.pending || !routes.auto_active { " disabled" } else { "" }, if routes.pending { " disabled" } else { "" }, if routes.pending || routes.rate.is_none() { " disabled" } else { "" }).unwrap();
    if let Some(rate) = &routes.rate {
        write!(html, "<p class=\"{}\">Sistema: {:.3} MB/s de envio · isentos: {} · política {} · {}{}</p>", if rate.conflict || !rate.confirmed { "alert" } else { "" }, rate.bps as f64 / 8_000_000.0, exempt_names(rate), escape(&rate.policy), if rate.confirmed { "configuração ActiveStore confirmada, não prova de pacotes" } else { "intent registrado; aplicação não confirmada, remoção ainda necessária" }, if rate.conflict { " · conflito/erro: não sobrescrever terceiros" } else { "" }).unwrap();
    }
    html.push_str("<h3>ROTAS REGISTRADAS NESTA SESSÃO</h3>");
    if routes.owned.is_empty() { html.push_str("<p>Nenhuma rota host própria registrada.</p>"); }
    for route in &routes.owned {
        write!(html, "<article>{} · {} · gateway {}{} <button data-action=\"route-remove\" data-destination=\"{}\"{}>Remover rota própria</button></article>", escape(&route.destination), escape(&route.interface), escape(&route.gateway), if route.conflict { " · ALTERAÇÃO EXTERNA: preservada, inspecione no Windows" } else { "" }, escape(&route.destination), if routes.pending || routes.auto_active || route.conflict { " disabled" } else { "" }).unwrap();
    }
    html.push_str("<h3>APRENDIZADO ONLINE · ICMP, NÃO LATÊNCIA DE ENVIO</h3><p>Flow 9→16→1 em memória por rota/taxa realmente medida. Previsões ordenam candidatos; nunca confirmam ganho. Sem previsão para gateways/taxas nunca medidos. Mesmos PIDs/IP, rota e origem confirmadas ao redor da sonda; timeout não é 0 ms. Três respostas por janela, referência antes/depois, melhora acima de 10% e 3 ms sem sobreposição e dois pares favoráveis antes de promover. Revalida continuamente; regressão/erro pede restauração. Incerteza é descritiva, não intervalo estatístico. Variações de cena/rede não provam causalidade. Rotas e taxas não são treinadas juntas.</p><ul>");
    for history in &routes.history { write!(html, "<li>{}</li>", escape(history)).unwrap(); }
    html.push_str("</ul></section>");
}
fn is_ping_reply(s: &str) -> bool {
    s.starts_with("<1 ms") || (s.chars().next().is_some_and(|c| c.is_ascii_digit()) && s.contains(" ms"))
}


/// Metrics the host always emits, in the order shown in the always-visible strip.
const SUMMARY_METRICS: [(&str, &str); 5] = [("USO DE CPU TOTAL", "CPU"), ("USO DE GPU", "GPU"), ("USO DE MEMÓRIA", "Memória"),
    ("REDE TOTAL", "Rede"), ("PING DO JOGO", "Ping do jogo · RTT ao destino")];

/// Compact always-visible status: a few headline metrics plus relay state, so the common
/// question ("is it working, how is the load?") needs no scrolling or tab switch.
pub fn render_summary(view: &Dashboard) -> String {
    let mut html = String::new();
    for (prefix, name) in SUMMARY_METRICS {
        let Some((_, value)) = view.metrics.iter().find(|(label, _)| label.starts_with(prefix)) else { continue };
        let primary = value.split_once(" · ").map_or(value.as_str(), |(first, _)| first);
        write!(html, "<div class=\"summary-item\"><span class=\"summary-label\">{name}</span><strong class=\"summary-value\">{}</strong></div>", escape(primary)).unwrap();
    }
    let (class, text) = if view.relay.running && view.relay.hy2_connected && !view.relay.tunnel.is_empty() { ("summary-ok", format!("{} ativo", view.relay.tunnel)) }
        else if view.relay.running && view.relay.hy2_connected { ("summary-ok", "HY2 ativo".to_string()) }
        else if view.relay.running { ("summary-warn", "Relay ativo".to_string()) }
        else { ("summary-off", "Relay parado".to_string()) };
    write!(html, "<div class=\"summary-item\"><span class=\"summary-label\">Relay</span><strong class=\"summary-value {class}\">{}</strong></div>", escape(&text)).unwrap();
    if !view.target.is_empty() {
        write!(html, "<div class=\"summary-item summary-target\"><span class=\"summary-label\">Alvo</span><strong class=\"summary-value\">{}</strong></div>", escape(&view.target)).unwrap();
    }
    html
}

#[wasm_bindgen]
pub fn render_summary_json(json: &str) -> Result<String, JsValue> {
    serde_json::from_str::<Dashboard>(json).map(|view| render_summary(&view)).map_err(|e| JsValue::from_str(&e.to_string()))
}

pub fn render(view: &Dashboard) -> String {
    // Panes group existing sections by task; each section's own markup is unchanged.
    let mut html = String::from("<div class=\"tab-pane\" data-tab=\"overview\">");
    if !view.subtitle.is_empty() { write!(html, "<p class=\"page-subtitle\">{}</p>", escape(&view.subtitle)).unwrap(); }
    for alert in &view.alerts {
        write!(html, "<p class=\"alert\" role=\"alert\">{}</p>", escape(alert)).unwrap();
    }
    if let Some(pin) = &view.pin_status { write!(html, "<p>{}</p>", escape(pin)).unwrap(); }
    render_telemetry(&mut html, view);
    render_running(&mut html, view);
    html.push_str("</div><div class=\"tab-pane\" data-tab=\"network\" hidden>");
    html.push_str("<section class=\"network-optimization\"><h2>OTIMIZAÇÃO DE REDE</h2>");
    write!(html, "<label class=\"network-priority-toggle\"><input type=\"checkbox\" data-action=\"network-priority\"{}> Priorizar rede do jogo (DSCP 46)</label>", if view.network_priority { " checked" } else { "" }).unwrap();
    let has_policies = !view.network_tweaks.is_empty();
    html.push_str("<div class=\"net-grid\">");
    // Card 1: DSCP status
    let (card_class, value, sub) = if has_policies {
        ("net-card-active", "DSCP 46", "Expedited Forwarding ativo")
    } else if view.network_priority {
        ("net-card-warn", "Armado", "Aguardando jogo detectado")
    } else {
        ("net-card-off", "Desativado", "Sem prioridade de rede")
    };
    write!(html, "<div class=\"net-card {card_class}\"><span class=\"net-card-label\">Prioridade</span><span class=\"net-card-value\">{value}</span><span class=\"net-card-sub\">{sub}</span></div>").unwrap();
    // Card 2: Policy detail
    if has_policies {
        html.push_str("<div class=\"net-card net-card-active\"><span class=\"net-card-label\">Política ActiveStore</span>");
        for entry in &view.network_tweaks {
            let (dscp_part, exe_part) = entry.split_once(" · ").unwrap_or(("", entry));
            let short_exe = exe_part.rsplit_once('\\').map_or(exe_part, |(_, f)| f);
            write!(html, "<span class=\"net-card-value\">{}</span><span class=\"net-card-sub\">✅ {} · {}</span>", escape(short_exe), escape(dscp_part), escape(exe_part)).unwrap();
        }
        html.push_str("</div>");
    }
    // Card 3: Network traffic
    let net_entry = view.metrics.iter().find(|(l, _)| l.contains("REDE TOTAL"));
    if let Some((_, value)) = net_entry {
        let (primary, _) = value.split_once(" · ").unwrap_or((value, ""));
        write!(html, "<div class=\"net-card\"><span class=\"net-card-label\">Tráfego</span><span class=\"net-card-value\">{}</span><span class=\"net-card-sub\">Envio + recebimento do sistema</span></div>", escape(primary)).unwrap();
    }
    // Card 4: game ping. The caption is the host's own explanation, so the caveat can never disagree with the value.
    if let Some((_, value)) = view.metrics.iter().find(|(l, _)| l.starts_with("PING DO JOGO")) {
        let (primary, caption) = value.split_once(" · ").unwrap_or((value, ""));
        let measured = is_ping_reply(primary);
        write!(html, "<div class=\"net-card {}\"><span class=\"net-card-label\">Ping do jogo · RTT ao destino</span><span class=\"net-card-value\">{}</span><span class=\"net-card-sub\">{}</span></div>",
            if measured { "" } else { "net-card-warn" }, escape(primary), escape(caption)).unwrap();
    }
    // Card 5 wide: ICMP destinations
    let icmp_entries: Vec<_> = view.metrics.iter().filter(|(l, _)| l.contains("DESTINO") && !l.contains("ROTA") && !l.contains("IPs")).collect();
    if !icmp_entries.is_empty() {
        html.push_str("<div class=\"net-card net-card-wide\"><span class=\"net-card-label\">Destinos observados (TCP/UDP) · Traceroute / RTT ao destino</span><ul class=\"net-history\">");
        for (_, value) in &icmp_entries {
            let (rtt, ip) = value.split_once(" · ").unwrap_or((value, ""));
            // The host labels success as "<n> ms" / "<1 ms"; anything else is a timeout or an unavailable probe, never shown in the success color.
            write!(html, "<li><span>{}</span><span{}>{}</span></li>", escape(ip), if is_ping_reply(rtt) { "" } else { " class=\"icmp-fail\"" }, escape(rtt)).unwrap();
        }
        html.push_str("</ul><span class=\"net-disclaimer\">Não identifica servidor de partida. O RTT reflete o tempo de resposta medido até os destinos de rede do jogo.</span></div>");
    }
    render_relay(&mut html, &view.relay, &view.wfp);
    crate::vpn::render(&mut html, &view.vpn);
    let route_learning = view.routes.auto_active;
    if route_learning || !view.routes.history.is_empty() {
    let (model_class, model_val) = if route_learning { ("net-card-warn", "Aprendizado ICMP ativo") }
        else { ("net-card-off", "Histórico ICMP da sessão") };
    write!(html, "<div class=\"net-card {model_class}\"><span class=\"net-card-label\">Modelo de rotas · separado do relay</span><span class=\"net-card-value\">{model_val}</span>").unwrap();
    if !view.routes.history.is_empty() {
        html.push_str("<ul class=\"net-history\">");
        for entry in &view.routes.history { write!(html, "<li><span>{}</span></li>", escape(entry)).unwrap(); }
        html.push_str("</ul>");
    }
    html.push_str("</div>");
    }
    // Card 7: Rate limit (if any)
    if let Some(rate) = &view.routes.rate {
        let rc = if rate.confirmed { "net-card-active" } else { "net-card-warn" };
        write!(html, "<div class=\"net-card {rc}\"><span class=\"net-card-label\">Limite de envio do sistema</span><span class=\"net-card-value\">{:.2} MB/s</span><span class=\"net-card-sub\">Isentos: {} · {}</span></div>", rate.bps as f64 / 8_000_000.0, exempt_names(rate), if rate.confirmed { "✅ confirmado" } else { "⏳ pendente" }).unwrap();
    }
    // Card 8: Owned routes (if any)
    if !view.routes.owned.is_empty() {
        html.push_str("<div class=\"net-card\"><span class=\"net-card-label\">Rotas próprias</span>");
        for route in &view.routes.owned {
            write!(html, "<span class=\"net-card-sub\">{} → {} via {}{}</span>", escape(&route.destination), escape(&route.interface), escape(&route.gateway), if route.conflict { " ⚠️" } else { "" }).unwrap();
        }
        html.push_str("</div>");
    }
    html.push_str("</div></section>");
    render_network(&mut html, view);
    html.push_str("</div><div class=\"tab-pane\" data-tab=\"profile\" hidden>");
    if let Some(profile) = &view.profile {
        let active = profile.priority || profile.qos || profile.pinned_threads > 0;
        write!(html, "<section class=\"profile-summary {}\"><h2>PERFIL EM USO · {} · PID {}</h2><p class=\"profile-source\">{}</p><strong>{}</strong><div class=\"applied-settings\">", if active { "profile-applied" } else { "profile-standby" }, escape(&profile.game), profile.pid, escape(&profile.source), if active { "AJUSTES ATIVOS NESTE PROCESSO" } else { "SEM AJUSTES ATIVOS NESTE PROCESSO" }).unwrap();
        write!(html, "<span>Prioridade: <b>{}</b></span><span>Afinidade: <b>{} threads fixadas</b></span><span>Memória / EcoQoS: <b>{}</b></span></div>", if profile.priority { "Ativa" } else { "Não aplicada" }, profile.pinned_threads, if profile.qos { "Ativa" } else { "Não aplicada" }).unwrap();
        html.push_str("<h3>CONFIGURAÇÃO SELECIONADA</h3><p>Os campos abaixo são a configuração solicitada. O estado acima mostra os ajustes aplicados pelo RAPO; pausa, falta de foco ou acesso negado podem impedir a aplicação.</p>");
        render_settings(&mut html, &profile.configured);
        html.push_str("</section>");
    } else {
        html.push_str("<section class=\"profile-summary profile-standby\"><h2>PERFIL EM USO</h2><p>Nenhum jogo detectado; nenhum perfil aplicado pelo RAPO.</p></section>");
    }
    render_properties(&mut html, view);
    html.push_str("</div><div class=\"tab-pane\" data-tab=\"resources\" hidden>");
    render_resources(&mut html, view);
    html.push_str("</div><div class=\"tab-pane\" data-tab=\"system\" hidden>");
    html.push_str("<section class=\"power-plan\"><h2>PLANO DE ENERGIA ATIVO NO WINDOWS</h2>");
    match &view.power_plan.plan {
        Some(plan) => {
            write!(html, "<p class=\"profile-source\">{}</p><p>GUID: <code>{}</code></p>", escape(&plan.name), escape(&plan.guid)).unwrap();
            if view.power_plan.error.is_empty() {
                html.push_str("<p>Atualizado quando o Windows notifica troca de plano. Este bloco não altera o plano.</p>");
            } else {
                write!(html, "<p class=\"alert\" role=\"status\">Última leitura mantida; a consulta atual falhou: {}</p>", escape(&view.power_plan.error)).unwrap();
            }
        }
        None => { write!(html, "<p class=\"alert\" role=\"alert\">Plano indisponível: {}</p>", escape(&view.power_plan.error)).unwrap(); }
    }
    html.push_str("</section>");
    render_engine_ini(&mut html, &view.engine_ini);
    html.push_str("</div>");
    html
}

/// Metrics shown in the Resources tab instead of the Visão geral telemetry grid.
fn is_resource_metric(label: &str) -> bool {
    label.starts_with("USO DE") || label.contains("DISCO") || label.contains("LEITURA DE DISCO") || label.contains("I/O DE LEITURA") || label.contains("REDE TOTAL")
}

/// Model resources in use: (label, on). Shown as green when on, red when off.
fn model_states(profile: &AppliedProfile) -> Vec<(String, bool)> {
    let s = &profile.configured;
    vec![
        ("HT / SMT".to_string(), s.smt),
        ("Core parking".to_string(), s.park_cores),
        ("Só núcleos P".to_string(), s.p_only),
        ("Afinidade de CPU".to_string(), s.pin),
        ("Prioridade".to_string(), s.priority),
        ("Memória / EcoQoS".to_string(), s.qos),
    ]
}

/// One-line description of what the current profile does for a destination block.
fn dest_detail(label: &str, profile: &AppliedProfile) -> String {
    match label {
        "Afinidade de CPU" => format!("{} thread(s) fixadas", profile.pinned_threads),
        "Prioridade" => if profile.priority { "prioridade elevada".into() } else { "prioridade normal".into() },
        "Memória / EcoQoS" => if profile.qos { "EcoQoS aplicado".into() } else { "sem EcoQoS".into() },
        _ => profile.source.clone(),
    }
}

/// Resources tab: CPU, GPU, memory, disk and network from the same telemetry the Visão geral tab shows.
fn render_resources(html: &mut String, view: &Dashboard) {
    let metrics: Vec<&(String, String)> = view.metrics.iter().filter(|(l, _)| is_resource_metric(l)).collect();
    html.push_str("<section class=\"resources-section\"><h2>RECURSOS DO SISTEMA</h2>");
    // Flow in the reference style: sources (metrics) → central node → destinations (the RAPO actions on them).
    html.push_str("<svg class=\"resources-flow\" viewBox=\"0 0 960 420\" role=\"img\" aria-label=\"Fluxo de recursos do sistema\">");
    for (index, (label, value)) in metrics.iter().enumerate() {
        let (primary, sub) = value.split_once(" · ").unwrap_or((value.as_str(), ""));
        let y = 30.0 + index as f32 * 80.0;
        write!(html, "<path class=\"flow-link\" d=\"M220 {y:.0} C 330 {y:.0}, 340 210, 420 210\"/>").unwrap();
        write!(html, "<g class=\"flow-node\" transform=\"translate(20 {:.0})\"><rect width=\"200\" height=\"64\" rx=\"8\"/><text class=\"flow-label\" x=\"12\" y=\"24\">{}</text><text class=\"flow-value\" x=\"12\" y=\"48\">{}</text></g>", y - 32.0, escape(label), escape(primary)).unwrap();
        if label.starts_with("USO DE") { if let Some(percent) = first_percent(value) { write!(html, "<rect class=\"flow-usage\" role=\"meter\" aria-valuemin=\"0\" aria-valuemax=\"100\" aria-valuenow=\"{percent:.0}\" aria-label=\"{}\" x=\"12\" y=\"56\" width=\"{:.0}\" height=\"3\"/>", escape(label), 176.0 * percent.clamp(0.0, 100.0) / 100.0).unwrap(); } }
        if !sub.is_empty() {
            write!(html, "<title>{}</title>", escape(sub)).unwrap();
        }
    }
    let (hub_class, hub_text) = match view.profile.as_ref() {
        Some(profile) if profile.source.contains("aprendido salvo") => ("flow-hub flow-hub-trained", "MODELO TREINADO"),
        Some(_) => ("flow-hub flow-hub-on", "MODELO ATIVO"),
        None => ("flow-hub flow-hub-off", "SEM MODELO"),
    };
    let (first, second) = hub_text.split_once(' ').unwrap_or((hub_text, ""));
    let (action, dir) = match view.profile.as_ref() {
        Some(profile) if !profile.exe.is_empty() => ("profile-apply", escape(&profile.exe)),
        _ => ("start_training", String::new()),
    };
    let dir_attr = if dir.is_empty() { String::new() } else { format!(" data-dir=\"{dir}\"") };
    write!(html, "<g class=\"{hub_class}\" role=\"button\" tabindex=\"0\" data-action=\"{action}\"{dir_attr} transform=\"translate(420 150)\"><title>{}</title><rect width=\"120\" height=\"120\" rx=\"60\"/><text x=\"60\" y=\"54\" text-anchor=\"middle\" class=\"flow-hub-label\">{first}</text><text x=\"60\" y=\"72\" text-anchor=\"middle\" class=\"flow-hub-label\">{second}</text></g>", if action == "profile-apply" { "Aplicar o modelo a este jogo" } else { "Iniciar treinamento" }).unwrap();
    let states = view.profile.as_ref().map(model_states).unwrap_or_default();
    for (index, (label, on)) in states.iter().enumerate() {
        let y = 30.0 + index as f32 * 80.0;
        let class = if *on { "flow-state flow-on" } else { "flow-state flow-off" };
        write!(html, "<path class=\"flow-link flow-out {class}\" d=\"M540 210 C 620 210, 640 {y:.0}, 720 {y:.0}\"/>").unwrap();
        let detail = view.profile.as_ref().map(|p| dest_detail(label, p)).unwrap_or_default();
        write!(html, "<g class=\"flow-dest {class}\" transform=\"translate(720 {:.0})\"><rect width=\"220\" height=\"40\" rx=\"6\"/><text x=\"12\" y=\"17\">{}: {}</text><text class=\"flow-dest-detail\" x=\"12\" y=\"32\">{}</text></g>", y - 20.0, escape(label), if *on { "ligado" } else { "desligado" }, escape(&detail)).unwrap();
    }
    html.push_str("</svg>");
    html.push_str("</section>");
}

fn render_telemetry(html: &mut String, view: &Dashboard) {
    write!(html, "<section class=\"telemetry-section\"><h2>TELEMETRIA DO PROGRAMA / JOGO: {}</h2><div class=\"telemetry-grid\">", escape(&view.target)).unwrap();
    for (label, value) in view.metrics.iter().filter(|(l, _)| !is_resource_metric(l)) {
        let (primary, sub) = value.split_once(" · ").unwrap_or((value.as_str(), ""));
        // A figure ("34.5%", "2300.0 KB/s") is shown large; a sentence ("4 IP(s) TCP observado(s); …") is not a figure.
        let class = if primary.chars().count() <= 14 { "box-value" } else { "box-value box-value-text" };
        write!(html, "<div class=\"telemetry-box\"><span class=\"box-label\">{}</span><strong class=\"{class}\">{}</strong>", escape(label), escape(primary)).unwrap();
        if !sub.is_empty() {
            write!(html, "<span class=\"box-sub\">{}</span>", escape(sub)).unwrap();
        }
        // Bars only where the value is a share of a whole; FPS, MB/s and ms have no natural maximum.
        if let Some(percent) = label.starts_with("USO DE").then(|| first_percent(value)).flatten() {
            usage_bar(html, percent, label);
        }
        html.push_str("</div>");
    }
    html.push_str("</div></section><section class=\"cores-section\"><h2>USO POR NÚCLEO LÓGICO</h2><p>TOPOLOGIA DETECTADA PELO WINDOWS</p><div class=\"core-grid\">");
    for (index, load, p_core) in &view.cores {
        let percent = load.clamp(0.0, 1.0) * 100.0;
        write!(html, "<div class=\"core-box{}\"><div class=\"core-header\"><span class=\"core-num\">CPU {:02}</span><span class=\"badge {}\">{}</span></div><span class=\"core-pct\">{:.0}%</span>", if *p_core { " core-p" } else { "" }, index, if *p_core { "badge-accent" } else { "badge-muted" }, if *p_core { "P-core" } else { "E-core" }, percent).unwrap();
        usage_bar(html, percent, &format!("Uso do núcleo {index}"));
        html.push_str("</div>");
    }
    html.push_str("</div><div class=\"cache-badges\">");
    for cache in &view.cache { write!(html, "<span class=\"badge badge-muted\">{}</span> ", escape(cache)).unwrap(); }
    html.push_str("</div></section>");
}

fn render_properties(html: &mut String, view: &Dashboard) {
    html.push_str("<section class=\"properties-section\"><h2>OPÇÕES E DIRETRIZES DE AGENDAMENTO</h2><div class=\"property-grid\">");
    for (action, on, label, hint) in [
        ("auto-yield", view.auto_yield, "Auto-Yield (afinidade)", "Cede afinidade se outro programa mudar a máscara de uma thread já fixada"),
        ("windowed", view.windowed, "Manter ajustes em segundo plano", "Mantém prioridade e afinidade mesmo com o jogo fora de foco"),
        ("minimize-tray", view.minimize_tray, "Minimizar para a Barra de Ícones (Tray)", "Oculta da barra de tarefas; menu Start/Stop na bandeja"),
    ] {
        write!(html, "<div class=\"property-box\"><label class=\"property-toggle\"><input type=\"checkbox\" data-action=\"{action}\"{}> <div class=\"property-text\"><span class=\"property-name\">{}</span><span class=\"property-desc\">{}</span></div></label></div>", if on { " checked" } else { "" }, escape(label), escape(hint)).unwrap();
    }
    html.push_str("<div class=\"property-box property-box-action\"><div class=\"property-text\"><span class=\"property-name\">Privilégios de Administrador</span><span class=\"property-desc\">Solicita UAC; reinicia para otimizar processos comuns protegidos</span></div><button data-action=\"elevate\" class=\"btn-elevate\" title=\"Solicita UAC; as opções deverão ser selecionadas novamente. Processos protegidos ainda podem negar acesso.\">Reabrir como administrador</button></div>");
    html.push_str("</div></section>");
}

fn render_running(html: &mut String, view: &Dashboard) {
    write!(html, "<section><h2>JOGOS DETECTADOS EM EXECUÇÃO: {}</h2>", view.running.len()).unwrap();
    if view.running.is_empty() { html.push_str("<p>Nenhum processo dentro das pastas dos jogos Steam instalado em execução.</p>"); }
    for p in &view.running {
        write!(html, "<article><strong>{}</strong> · PID {} · {}<p>{}</p>", escape(&p.game), p.pid, escape(&p.state), escape(&p.exe)).unwrap();
        if p.auto { html.push_str("<span class=\"badge\">AUTO ATIVO</span>"); }
        else {
            write!(html, "<button data-action=\"toggle-process\" data-pid=\"{}\"{}>{}</button>", p.pid, if p.can_toggle { "" } else { " disabled" }, if p.enabled { "Restaurar" } else { "Otimizar" }).unwrap();
        }
        if p.qos { html.push_str("<span class=\"badge\">QOS MEMÓRIA</span>"); }
        if p.priority { html.push_str("<span class=\"badge\">PRIORIDADE ACIMA DO NORMAL</span>"); }
        html.push_str("</article>");
    }
    write!(html, "</section><p class=\"library-note\">Biblioteca Steam: {} · {}</p>", view.library_count, escape(&view.aion2_status)).unwrap();
}

#[wasm_bindgen]
pub fn render_dashboard_json(json: &str) -> Result<String, JsValue> {
    let view: Dashboard = serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(render(&view))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protected_process_controls_and_untrusted_text() {
        let mut view = Dashboard {
            alerts: vec!["<img src=x onerror=alert(1)>".into()], subtitle: String::new(), target: String::new(),
            metrics: vec![("ICMP · DESTINO OBSERVADO".into(), "Timeout · <img src=x> & destino".into()),
                ("USO DE CPU TOTAL".into(), "18.2% · Sistema 22.0%".into()),
                ("ICMP · ROTA OBSERVADA".into(), "2001:db8::42 · gateway fe80::1%2".into())], cores: vec![], cache: vec![],
            running: vec![Process { pid: 42, game: "<script>".into(), exe: "a\"b".into(), state: String::new(), enabled: false, auto: false, qos: false, priority: false, can_toggle: false }],
            auto_yield: true, windowed: false, minimize_tray: false, library_count: 0, aion2_status: String::new(), pin_status: None,
            profile: None,
            network_tweaks: vec!["DSCP 46 · game<&>.exe".into()], network_priority: true,
            power_plan: PowerPlanState { plan: Some(PowerPlan { name: "<script>energia</script>".into(), guid: "a\"b".into() }), error: "<b>falha</b>".into() },
            routes: Routes::default(), route_endpoints: vec![], network_target_pid: 0,
            relay: Relay { running: true, address: "127.0.0.1:1080".into(), error: "<script>relay</script>".into(),
                interfaces: vec![RouteCandidate { id: "relay\"<&>".into(), interface: "<img src=relay>".into(), index: 2, gateway: "192.0.2.1".into(), ipv6: false }],
                mode: "hy2".into(), hy2_server: "<script>hy2</script>:443".into(), udp_route: "<img src=udp>".into(),
                ..Relay::default() },
            wfp: Wfp::default(), engine_ini: EngineIni { path: "C:\\<x>".into(), active: true, applied: true, has_backup: false, options: vec![
                EngineIniOption { key: "very-low".into(), label: "1- Very Low".into(), enabled: true },
                EngineIniOption { key: "additions".into(), label: "Additions".into(), enabled: false }],
            error: String::new() },
            vpn: Default::default(),
        };
        view.routes = Routes {
            candidates: vec![RouteCandidate { id: "x\"<&>".into(), interface: "<img src=x>".into(), index: 2, gateway: "fe80::1%2".into(), ipv6: true }],
            owned: vec![OwnedRoute { destination: "2001:db8::42".into(), interface: "Ethernet".into(), gateway: "fe80::1%2".into(), conflict: false }],
            pending: true, auto_active: true, error: "<script>admin</script>".into(), message: String::new(), automatic: String::new(),
            history: vec!["<b>previsão não é ganho</b>".into()],
            rate: Some(Rate { exempt: vec!["C:\\Steam\\steam.exe".into(), "C:\\game<&>.exe".into()], policy: "own\"policy".into(), bps: 10_000_000, confirmed: true, conflict: false }),
        };
        let html = render(&view);
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(html.contains("data-pid=\"42\" disabled"));
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(html.contains("&lt;script&gt;energia&lt;/script&gt;"), "a failed read must keep the last plan visible");
        assert!(html.contains("&lt;b&gt;falha&lt;/b&gt;"));
        assert!(html.contains("game&lt;&amp;&gt;.exe"), "network tweak exe must be escaped");
        assert!(html.contains("&lt;img src=x&gt; &amp; destino"));
        assert!(html.contains("<fieldset disabled>"));
        assert!(html.contains("data-destination=\"2001:db8::42\" disabled>"));
        assert!(html.contains("Sistema: 1.250 MB/s de envio") && html.contains("1.25 MB/s"), "rates are shown in megabytes per second");
        assert!(html.contains("isentos: steam.exe, game&lt;&amp;&gt;.exe"), "exempt executables are listed by escaped file name");
        assert!(html.contains("&lt;b&gt;previsão não é ganho&lt;/b&gt;"));
        assert!(html.contains("&lt;script&gt;hy2&lt;/script&gt;:443") && html.contains("&lt;img src=udp&gt;"));
        assert!(html.contains("<fieldset id=\"relay-transport\" disabled>"), "transport settings are locked while the relay runs");
        assert!(html.contains("<option value=\"hy2\" selected>") && html.contains("Transporte · HY2"));
        assert!(html.contains("C:\\&lt;x&gt;"), "Engine.ini path is untrusted");
        assert!(html.contains("data-key=\"very-low\" checked>") && html.contains("data-key=\"additions\"><span>"), "each option shows its own state");
        assert!(html.contains("data-action=\"engine-ini-active\" checked>") && html.contains("ON · opções marcadas"), "the On/Off switch reflects the saved state");
        assert!(html.contains("data-action=\"engine-ini-backup\""));
        assert!(html.contains("data-action=\"engine-ini-restore\""));
        assert!(html.contains("class=\"flow-usage\"") && html.contains("aria-valuenow=\"18\" aria-label=\"USO DE CPU TOTAL\"") && html.contains("width=\"32\""), "usage metrics get a 0–100% bar in the flow node");
        assert_eq!(html.matches("role=\"meter\"").count(), 1, "metrics without a natural maximum (e.g. an IPv6 zone `%2`) get no bar");
    }
    #[test]
    fn hy2_status_tells_off_connecting_working_and_error_apart() {
        let relay = |mode: &str, running, connected, error: &str| Relay { mode: mode.into(), running, hy2_connected: connected, hy2_error: error.into(), ..Relay::default() };
        let off = hy2_status(&relay("local", true, false, ""));
        assert!(off.contains("Hysteria 2 desligado") && off.contains("modo atual: Local") && off.contains("escolha HY2 em Transporte"), "{off}");
        assert!(hy2_status(&relay("hy2", false, false, "")).contains("relay parado"));
        assert!(hy2_status(&relay("hy2", true, false, "")).contains("hy2-pending"));
        let working = hy2_status(&relay("neural_auto", true, true, ""));
        assert!(working.contains("ligado e funcionando") && working.contains("HY2 local embutido") && working.contains("role=\"status\""));
        let failed = hy2_status(&relay("hy2", true, false, "<b>auth</b>"));
        assert!(failed.contains("hy2-error") && failed.contains("&lt;b&gt;auth&lt;/b&gt;"), "connect errors are untrusted text");
        let remote = hy2_status(&Relay { hy2_server: "vps.example:443".into(), ..relay("hy2", true, true, "") });
        assert!(remote.contains("vps.example:443") && !remote.contains("embutido"));
    }

    #[test]
    fn wireguard_status_does_not_claim_a_hysteria_session() {
        let relay = Relay { mode: "hy2".into(), tunnel: "WireGuard".into(), running: true, hy2_connected: true, ..Relay::default() };
        let status = hy2_status(&relay);
        assert!(status.contains("WireGuard") && status.contains("sessão do túnel ativa"));
        assert!(!status.contains("Hysteria") && !status.contains("QUIC"));
    }
    #[test]
    fn integrated_hy2_status_shows_ping_connections_and_traffic() {
        let base = Relay { mode: "hy2".into(), running: true, hy2_connected: true, hy2_builtin: true, active_connections: 3, bytes_tx: 2048, bytes_rx: 4096, ..Relay::default() };
        let measured = hy2_status(&Relay { hy2_ping_ms: 21, ..base });
        assert!(measured.contains("HY2 integrado ao RAPO") && measured.contains("ping 21 ms") && measured.contains("3 conexões"), "{measured}");
        assert!(measured.contains("↑ 2.0 KiB") && measured.contains("↓ 4.0 KiB"), "{measured}");
        let unanswered = hy2_status(&Relay { mode: "hy2".into(), running: true, hy2_connected: true, hy2_builtin: true, ..Relay::default() });
        assert!(unanswered.contains("ping sem resposta ICMP") && !unanswered.contains("0 ms"), "an unmeasured ping is never shown as 0 ms: {unanswered}");
        let typed = hy2_status(&Relay { mode: "hy2".into(), hy2_server: "vps.example:443".into(), hy2_builtin: true, ..Relay::default() });
        assert!(typed.contains("vps.example:443") && !typed.contains("integrado"), "a typed server wins over the sealed one");
    }
    #[test]
    fn udp_destinations_are_visible_when_tcp_connections_are_zero() {
        let relay: Relay = serde_json::from_str(
            r#"{"running":true,"stopping":false,"address":"127.0.0.1:1080","active_connections":0,"total_connections":0,"bytes_tx":0,"bytes_rx":0,"prewarm_hits":0,"prewarm_misses":0,"error":"","predictions":[],"interfaces":[],"allowed_interfaces":[],"mode":"local","hy2_server":"","hy2_server_name":"","udp_associations":0,"udp_route":"direct","udp_destinations":2}"#,
        ).unwrap();
        let mut html = String::new();
        render_relay(&mut html, &relay, &Wfp::default());
        assert!(html.contains("0 TCP ativas · 0 associações UDP · 2 destinos UDP ativos"));
        assert!(html.contains("HY2 configurado: HY2 local embutido (loopback 127.0.0.1)") && html.contains("data-action=\"relay-configure\""),
            "without a remote server the transport block still offers HY2 through the embedded server");
    }
    /// Minimal valid dashboard; `overrides` are merged on top so a test never repeats a key.
    fn dashboard(overrides: serde_json::Value) -> Dashboard {
        let mut base = serde_json::json!({"alerts":[],"subtitle":"","target":"","metrics":[],"cores":[],"cache":[],"running":[],"auto_yield":false,"windowed":false,
            "minimize_tray":false,"library_count":0,"aion2_status":"","pin_status":null,"profile":null,"power_plan":{"plan":null,"error":""},"network_tweaks":[]});
        base.as_object_mut().unwrap().extend(overrides.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }
    #[test]
    fn summary_shows_headline_metrics_in_order_and_escapes_untrusted_text() {
        let view = dashboard(serde_json::json!({"target":"<b>jogo</b>","metrics":[["REDE TOTAL (SISTEMA)","1.0 KB/s · x"],["USO DE CPU TOTAL","<img src=x> · Sistema 9%"],["USO DE GPU","7.0% · y"]]}));
        let html = render_summary(&view);
        assert!(html.find("CPU").unwrap() < html.find("GPU").unwrap() && html.find("GPU").unwrap() < html.find("Rede").unwrap(), "fixed order, not host order: {html}");
        assert!(html.contains("&lt;img src=x&gt;") && html.contains("&lt;b&gt;jogo&lt;/b&gt;") && !html.contains("<img") && !html.contains("<b>"), "{html}");
        assert!(!html.contains("Sistema 9%"), "only the headline part of a metric is shown");
        assert!(!html.contains("Memória"), "a metric the host did not send is omitted, not invented");
        assert!(html.contains("summary-off") && html.contains("Relay parado"), "no relay state is never shown as working");
        assert_eq!(render_summary(&dashboard(serde_json::json!({}))).matches("summary-item").count(), 1, "with no metrics or target only the relay state remains");
    }
    #[test]
    fn summary_distinguishes_relay_with_and_without_hy2() {
        let relay = |connected: bool| render_summary(&dashboard(serde_json::json!({"relay":{"running":true,"stopping":false,"address":"","active_connections":0,"total_connections":0,
            "bytes_tx":0,"bytes_rx":0,"error":"","predictions":[],"interfaces":[],"allowed_interfaces":[],"mode":"hy2","hy2_server":"","hy2_server_name":"",
            "hy2_connected":connected,"udp_associations":0,"udp_route":"","udp_destinations":0}})));
        assert!(relay(true).contains("summary-ok") && relay(true).contains("HY2 ativo"));
        assert!(relay(false).contains("summary-warn") && relay(false).contains("Relay ativo") && !relay(false).contains("HY2 ativo"));
    }
    #[test]
    fn every_section_lives_in_exactly_one_named_pane_in_order() {
        let html = render(&dashboard(serde_json::json!({})));
        // Text before the first marker is empty; each later chunk is one pane's content.
        let marker = "<div class=\"tab-pane\" data-tab=\"";
        let panes: Vec<(&str, &str)> = html.split(marker).skip(1).map(|chunk| chunk.split_once('"').unwrap()).collect();
        assert_eq!(panes.iter().map(|(name, _)| *name).collect::<Vec<_>>(), ["overview", "network", "profile", "resources", "system"]);
        assert!(panes[0].1.starts_with('>') && panes[1..].iter().all(|(_, rest)| rest.starts_with(" hidden>")), "only the overview pane starts visible");
        assert!(html.contains("RECURSOS DO SISTEMA"), "resources tab section exists");
        for (name, needle) in [("overview", "TELEMETRIA DO PROGRAMA"), ("network", "OTIMIZAÇÃO DE REDE"), ("profile", "OPÇÕES E DIRETRIZES"), ("system", "PLANO DE ENERGIA")] {
            let hits: Vec<_> = panes.iter().filter(|(_, content)| content.contains(needle)).map(|(n, _)| *n).collect();
            assert_eq!(hits, [name], "{needle} must appear in the {name} pane only");
        }
        assert!(html.ends_with("</div>"));
    }
    #[test]
    fn icmp_timeouts_and_unavailable_probes_never_use_the_success_style() {
        let html = render(&dashboard(serde_json::json!({"metrics":[
            ["ICMP · DESTINO OBSERVADO","24 ms · 203.0.113.7"], ["ICMP · DESTINO OBSERVADO","<1 ms · 203.0.113.8"],
            ["ICMP · DESTINO OBSERVADO","Timeout (250 ms; ICMP pode estar bloqueado) · 203.0.113.9"], ["ICMP · DESTINO OBSERVADO","Indisponível (Windows/IP 11003) · 203.0.113.10"]]})));
        assert_eq!(html.matches("class=\"icmp-fail\"").count(), 2, "only the timeout and the unavailable probe are failures");
        assert!(html.contains("<span>24 ms</span>") && html.contains("<span>&lt;1 ms</span>"), "replies, including sub-millisecond, keep the plain style");
    }
    #[test]
    fn game_ping_replaces_the_external_rtt_and_a_non_measurement_never_looks_like_one() {
        let ping = |value: &str| dashboard(serde_json::json!({"metrics":[["PING DO JOGO (ICMP)", value],["RTT TCP (EXTERNO)","18 ms · TCP até DNS público"]]}));
        let ok = ping("12 ms · ICMP ao IP TCP do jogo 10.0.0.3 · 2 de 4 responderam; não prova ser o servidor da partida");
        let summary = render_summary(&ok);
        assert!(summary.contains("Ping do jogo") && summary.contains("RTT ao destino") && summary.contains(">12 ms<") && !summary.contains("RTT TCP (EXTERNO)"), "the strip shows the game ping, not the DNS probe: {summary}");
        let page = render(&ok);
        assert!(page.contains("Ping do jogo · RTT ao destino") && page.contains("não prova ser o servidor da partida"), "the host's caveat is the card caption");
        assert!(!page.contains("RTT TCP externo") && !page.contains("DNS público; não é ping"), "the external RTT card is gone");
        assert!(page.contains("<div class=\"net-card \"><span class=\"net-card-label\">Ping do jogo"), "a real measurement uses the neutral card");
        let silent = render(&ping("sem resposta · Os IPs TCP do jogo não responderam ao ICMP"));
        assert!(silent.contains("net-card-warn\"><span class=\"net-card-label\">Ping do jogo"), "no reply is shown as a warning, never as a number: {silent}");
        assert!(render_summary(&dashboard(serde_json::json!({}))).find("Ping do jogo").is_none(), "no metric, no invented ping");
    }
    #[test]
    fn game_ping_supports_tcp_and_udp_rtt_and_renders_measured_values() {
        let tcp_view = dashboard(serde_json::json!({"metrics":[["PING DO JOGO (ICMP)", "24 ms (TCP porta 7777) · Destino do jogo 203.0.113.1:7777 · 1 de 1 responderam"]]}));
        let tcp_page = render(&tcp_view);
        assert!(tcp_page.contains("24 ms (TCP porta 7777)"));
        assert!(tcp_page.contains("<div class=\"net-card \"><span class=\"net-card-label\">Ping do jogo"));
        let udp_view = dashboard(serde_json::json!({"metrics":[["PING DO JOGO (ICMP)", "18 ms (UDP porta 9000) · Destino do jogo 198.51.100.2:9000 · 1 de 1 responderam"]]}));
        let udp_page = render(&udp_view);
        assert!(udp_page.contains("18 ms (UDP porta 9000)"));
        assert!(udp_page.contains("<div class=\"net-card \"><span class=\"net-card-label\">Ping do jogo"));
    }
    #[test]
    fn qos_target_names_the_game_not_a_pid_and_escapes_it() {
        let page = render(&dashboard(serde_json::json!({"network_target_pid": 4812, "target": "AION <2> · 4 processos · 2 executáveis"})));
        assert!(page.contains("Jogo alvo isento: AION &lt;2&gt; · 4 processos · 2 executáveis."), "the game, escaped, is the identity");
        assert!(page.contains("Todos os executáveis do jogo ficam fora do limite") && !page.contains("Jogo alvo isento: PID"), "no single pid names the game");
        assert!(page.contains("data-action=\"rate-apply\" data-pid=\"4812\""), "the pid stays as the technical handle of the buttons");
        assert!(render(&dashboard(serde_json::json!({}))).contains("Sem jogo alvo detectado"), "no target keeps the honest no-game text");
    }
    #[test]
    fn resources_pane_shows_cpu_gpu_memory_disk_and_network_only_once() {
        let html = render(&dashboard(serde_json::json!({"metrics":[["USO DE CPU TOTAL","34.5% · Sistema 9%"],["USO DE GPU","7.0% · Engines PDH"],
            ["USO DE MEMÓRIA","12.1 GB · 48.0% da RAM"],["LEITURA DE DISCO (SISTEMA)","2.10 MB/s · Total 4.00 GB"],["REDE TOTAL (SISTEMA)","2300.0 KB/s · x"]]})));
        let resources = html.split("data-tab=\"resources\"").nth(1).unwrap().split("data-tab=\"system\"").next().unwrap();
        for label in ["USO DE CPU TOTAL", "USO DE GPU", "USO DE MEMÓRIA", "LEITURA DE DISCO (SISTEMA)", "REDE TOTAL (SISTEMA)"] {
            assert!(resources.contains(label), "{label} appears in the Resources tab");
        }
        let telemetry = html.split("TELEMETRIA DO PROGRAMA").nth(1).unwrap().split("USO POR NÚCLEO").next().unwrap();
        assert!(!telemetry.contains("USO DE CPU TOTAL") && !telemetry.contains("REDE TOTAL"), "resource metrics are not duplicated in Visão geral");
    }
    #[test]
    fn resources_flow_colors_model_states_and_trained_hub() {
        let mut view = dashboard(serde_json::json!({}));
        view.profile = Some(AppliedProfile {
            pid: 1, game: "g".into(), exe: "C:/Games/g.exe".into(), source: "RAPO · perfil aprendido salvo".into(),
            configured: ProfileSettings { priority: false, pin: true, qos: false, threads: 4, min_percent: 5, cores: 4, smt: true, p_only: false, park_cores: true },
            priority: false, pinned_threads: 0, qos: false,
        });
        let html = render(&view);
        assert!(html.contains("flow-hub-trained"), "a saved learned profile turns the hub blue");
        assert_eq!(html.matches("flow-dest flow-state flow-on").count(), 3, "HT, core parking and affinity are on: green");
        assert_eq!(html.matches("flow-dest flow-state flow-off").count(), 3, "P-only, priority and QoS are off: red");
    }
    #[test]
    fn hub_applies_the_profile_or_starts_training_when_there_is_none() {
        let mut view = dashboard(serde_json::json!({}));
        view.profile = Some(AppliedProfile {
            pid: 1, game: "g".into(), exe: "C:/Games/g.exe".into(), source: "RAPO · perfil aprendido salvo".into(),
            configured: ProfileSettings { priority: false, pin: true, qos: false, threads: 4, min_percent: 5, cores: 4, smt: true, p_only: false, park_cores: true },
            priority: false, pinned_threads: 2, qos: false,
        });
        let html = render(&view);
        assert!(html.contains("data-action=\"profile-apply\" data-dir=\"C:/Games/g.exe\""), "the hub applies the learned profile to its game");
        assert!(html.contains("2 thread(s) fixadas"), "the affinity block shows the pinned thread count");
        let empty = render(&dashboard(serde_json::json!({})));
        assert!(empty.contains("data-action=\"start_training\""), "with no model the hub starts training");
        assert!(!empty.contains("profile-apply"), "no profile means no apply action");
    }
    #[test]
    fn telemetry_figures_are_large_and_sentences_are_not() {
        let html = render(&dashboard(serde_json::json!({"metrics":[["USO DE CPU TOTAL","34.5% · Sistema 9%"],["REDE TOTAL (SISTEMA)","2300.0 KB/s · x"],
            ["ICMP · IPs TCP DO PROCESSO","4 IP(s) TCP observado(s); até 4 sondas, 250 ms cada · Não identifica servidor"]]})));
        assert_eq!(html.matches("<text class=\"flow-value\"").count(), 2, "short figures keep the big style in the Resources flow");
        assert_eq!(html.matches("<strong class=\"box-value box-value-text\">4 IP(s) TCP").count(), 1, "a sentence does not use the big-number style");
    }
    #[test]
    fn udp_redundancy_renders_controls_and_overhead_percentage() {
        let mut view = dashboard(serde_json::json!({}));
        view.relay.redundancy = Redundancy { enabled: true, x: 10, y: 5 };
        let html = render(&view);
        assert!(html.contains("UDPspeeder · Redundância de pacotes UDP (Ativo · +50% tráfego)"));
        assert!(html.contains("id=\"udp-red-enabled\" type=\"checkbox\" data-action=\"udp-redundancy-toggle\" checked"));
        assert!(html.contains("id=\"udp-red-x\" type=\"number\" min=\"1\" max=\"30\" value=\"10\""));
        assert!(html.contains("id=\"udp-red-y\" type=\"number\" min=\"1\" max=\"30\" value=\"5\""));
    }
    #[test]
    fn udp_redundancy_separates_manual_and_effective_ratios() {
        let mut view = dashboard(serde_json::json!({}));
        view.relay.redundancy_manual = Redundancy { enabled: true, x: 20, y: 10 };
        view.relay.redundancy = Redundancy { enabled: true, x: 4, y: 1 };
        view.relay.redundancy_learning = "ativo".into();
        let html = render(&view);
        assert!(html.contains("Razão manual: 20:10"), "must show manual ratio");
        assert!(html.contains("Razão efetiva: 4:1"), "must show effective ratio");
        assert!(html.contains("Estado: ativo"), "must show learning state");
        assert!(html.contains("id=\"udp-red-x\" type=\"number\" min=\"1\" max=\"30\" value=\"20\""), "manual x input keeps manual value");
        assert!(html.contains("id=\"udp-red-y\" type=\"number\" min=\"1\" max=\"30\" value=\"10\""), "manual y input keeps manual value");
        assert!(html.contains("+25% tráfego"), "overhead summary reflects effective ratio");
    }
    #[test]
    fn udp_redundancy_renders_all_learning_states() {
        let mut view = dashboard(serde_json::json!({}));
        view.relay.redundancy = Redundancy { enabled: true, x: 20, y: 10 };

        for (input_state, expected) in [
            ("aprendendo", "aprendendo"),
            ("ativo", "ativo"),
            ("sem feedback", "sem feedback"),
            ("sem_feedback", "sem feedback"),
            ("desativado", "desativado"),
            ("", "desativado"),
        ] {
            view.relay.redundancy_learning = input_state.into();
            let html = render(&view);
            assert!(
                html.contains(&format!("Estado: {expected}")),
                "expected 'Estado: {expected}' for input '{input_state}'"
            );
        }
    }
}
