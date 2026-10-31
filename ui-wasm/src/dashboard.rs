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
    pub error: String,
    pub predictions: Vec<RelayPrediction>,
    pub interfaces: Vec<RouteCandidate>,
    pub allowed_interfaces: Vec<String>,
    pub mode: String,
    pub hy2_server: String,
    pub hy2_server_name: String,
    pub udp_associations: usize,
    pub udp_route: String,
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
pub struct Rate { pub exe: String, pub policy: String, pub bps: u64, pub confirmed: bool, pub conflict: bool }

const RELAY_MODES: [(&str, &str); 4] = [("direct", "Direto · rota do sistema"), ("local", "Local · Flow nas interfaces autorizadas"),
    ("hy2", "HY2 · sem fallback direto"), ("neural_auto", "Neural Auto · direto/HY2 nas novas conexões TCP")];

fn render_relay(html: &mut String, relay: &Relay) {
    let (class, status) = if relay.running { ("net-card-active", "Escutando") }
        else if relay.stopping { ("net-card-warn", "Encerrando conexões") }
        else { ("net-card-off", "Desativado") };
    write!(html, "<div class=\"net-card {class}\"><span class=\"net-card-label\">Relay local · SOCKS5 TCP/UDP</span><span class=\"net-card-value\" role=\"status\">{status}</span><span class=\"net-card-sub\">{}</span><label><input type=\"checkbox\" data-action=\"relay\"{}{}> Habilitar relay local</label><span class=\"net-card-sub\">{} conexões ativas ({} associações UDP, saída UDP fixa: {}) · {} aceitas<br>Enviado: {:.1} KiB · Recebido: {:.1} KiB</span>", escape(&relay.address), if relay.running { " checked" } else { "" }, if relay.stopping { " disabled" } else { "" }, relay.active_connections, relay.udp_associations, escape(&relay.udp_route), relay.total_connections, relay.bytes_tx as f64 / 1024.0, relay.bytes_rx as f64 / 1024.0).unwrap();
    html.push_str("<span class=\"net-disclaimer\">Configure SOCKS5 neste endereço no cliente compatível. TCP CONNECT e UDP ASSOCIATE; sem captura automática. Desabilitar encerra todas as sessões. AION 2: compatibilidade não comprovada — tráfego de teste não prova que o jogo aceita proxy.</span>");
    let mode = RELAY_MODES.iter().find(|(id, _)| *id == relay.mode).map_or("desconhecido", |(_, label)| label);
    write!(html, "<details><summary>Transporte · {mode}</summary><p class=\"net-card-sub\">HY2 configurado: {} · nome TLS: {}</p><fieldset id=\"relay-transport\"{}><legend>Configuração desta sessão</legend><label for=\"relay-mode\">Modo</label><select id=\"relay-mode\" data-network-draft=\"relay-mode\">", if relay.hy2_server.is_empty() { "nenhum".into() } else { escape(&relay.hy2_server) }, if relay.hy2_server_name.is_empty() { "host do servidor".into() } else { escape(&relay.hy2_server_name) }, if relay.running || relay.stopping { " disabled" } else { "" }).unwrap();
    for (id, label) in RELAY_MODES {
        write!(html, "<option value=\"{id}\"{}>{label}</option>", if relay.mode == id { " selected" } else { "" }).unwrap();
    }
    html.push_str("</select><label for=\"relay-server\">Servidor HY2 (host:porta)</label><input id=\"relay-server\" data-network-draft=\"relay-server\" maxlength=\"512\" placeholder=\"servidor:443\"><label for=\"relay-server-name\">Nome TLS (opcional)</label><input id=\"relay-server-name\" data-network-draft=\"relay-server-name\" maxlength=\"253\"><label for=\"relay-auth\">Autenticação HY2</label><input id=\"relay-auth\" type=\"password\" autocomplete=\"off\" data-network-draft=\"relay-auth\" maxlength=\"4096\"><label for=\"relay-ca\">CA PEM (opcional; substitui as raízes do Windows)</label><textarea id=\"relay-ca\" data-network-draft=\"relay-ca\" maxlength=\"65536\" rows=\"3\"></textarea><button type=\"button\" data-action=\"relay-configure\">Aplicar configuração</button></fieldset><p class=\"net-disclaimer\">Desabilite o relay antes de alterar. Reenvie todos os campos ao aplicar. Credencial somente em memória, não salva; TLS sempre validado. HY2 exige servidor remoto compatível. Neural Auto pode usar a saída direta e mudar o IP público de novas conexões; UDP mantém a saída escolhida durante a sessão, sem estimativa de RTT por envio.</p></details>");
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
        else if relay.predictions.is_empty() { ("net-card-warn", "Aguardando tráfego SOCKS5") }
        else { ("net-card-active", "Aprendendo com conexões reais") };
    write!(html, "<div class=\"net-card net-card-wide {class}\"><span class=\"net-card-label\">Modelo Neural · Relay</span><span class=\"net-card-value\">Flow 9→16→1 · {status}</span><span class=\"net-card-sub\">Estima o tempo de abertura TCP por destino e saída, não o ping da partida. Compara apenas as saídas do modo escolhido (interfaces autorizadas ou direto/HY2); não migra sessões abertas nem envia sondas extras. UDP não é medido.</span>").unwrap();
    if !relay.predictions.is_empty() {
        html.push_str("<div class=\"relay-predictions\"><table class=\"net-policy-table\"><thead><tr><th>Destino / saída</th><th>Medido</th><th>Previsão TCP</th><th>Amostras / falhas</th></tr></thead><tbody>");
        for prediction in &relay.predictions {
            let via = match prediction.gateway.as_str() { "hy2" => "túnel HY2".into(), "system" => "rota do sistema".into(), gateway => escape(gateway) };
            write!(html, "<tr><td>{}<br><span class=\"net-card-sub\">if {} · {} via {via}{}</span></td><td>", escape(&prediction.destination), prediction.interface, escape(&prediction.source), if prediction.selected { " · última saída escolhida" } else { "" }).unwrap();
            if let Some(ms) = prediction.observed_ms { write!(html, "{ms:.1} ms").unwrap(); } else { html.push_str("Falha de conexão"); }
            html.push_str("</td><td>");
            if let Some(ms) = prediction.predicted_ms { write!(html, "{ms:.1} ms<br><span class=\"net-card-sub\">Desvio/resíduo: {:.1} ms</span>", prediction.uncertainty_ms).unwrap(); } else { html.push_str("Aguardando respostas"); }
            write!(html, "</td><td>{} / {}</td></tr>", prediction.samples, prediction.failures).unwrap();
        }
        html.push_str("</tbody></table></div>");
    }
    html.push_str("<span class=\"net-disclaimer\">Previsão após 6 tentativas medidas e última conexão bem-sucedida. Desvio/resíduo é descritivo, não intervalo de confiança nem ganho confirmado. Somente uma saída disponível não permite comparar caminhos.</span></div>");
}

fn render_network(html: &mut String, view: &Dashboard) {
    let routes = &view.routes;
    let disabled = if routes.pending || routes.auto_active { " disabled" } else { "" };
    html.push_str("<section id=\"network-routes\"><h2>ROTAS POR IP · INTERFACE / GATEWAY</h2><p>Rota /32 ou /128 afeta TODOS os aplicativos e portas que acessam o IP, não somente o jogo. Pode desconectar sessões. Sem túnel/relay, driver, alteração de payload ou garantia de ping/anti-cheat.</p><p>Somente rotas criadas nesta sessão são removidas se inalteradas. Saída normal aguarda limpeza por até 30 s; falha/crash pode deixar rotas e políticas até reiniciar. Não há varredura insegura de sessões anteriores. Erros de saída: %TEMP%\\rintel-rapo-routes.log.</p>");
    if !routes.error.is_empty() { write!(html, "<p class=\"alert\" role=\"alert\">{}</p>", escape(&routes.error)).unwrap(); }
    write!(html, "<p role=\"status\">{}{}</p><p role=\"status\">{}</p>", if routes.pending { "Operação de rede pendente. " } else { "" }, escape(&routes.message), escape(&routes.automatic)).unwrap();
    write!(html, "<fieldset{disabled}><legend>Configuração explícita de rede</legend><button data-action=\"route-refresh\">Atualizar gateways/interfaces</button><label for=\"route-destination\">IP numérico de destino</label><input id=\"route-destination\" data-network-draft=\"destination\" type=\"text\" maxlength=\"64\" list=\"route-endpoints\" placeholder=\"192.0.2.42 ou 2001:db8::42\" autocomplete=\"off\"><datalist id=\"route-endpoints\">").unwrap();
    for endpoint in &view.route_endpoints { write!(html, "<option value=\"{}\"></option>", escape(endpoint)).unwrap(); }
    html.push_str("</datalist><p>As sugestões são IPs TCP observados, não servidores de partida confirmados. Aplicação manual também aceita outro IP numérico remoto.</p><label for=\"route-candidate\">Gateway/interface existentes</label><select id=\"route-candidate\" data-network-draft=\"candidate\"><option value=\"\">Selecione uma saída</option>");
    for candidate in &routes.candidates {
        write!(html, "<option value=\"{}\">{} · if {} · {} · {}</option>", escape(&candidate.id), escape(&candidate.interface), candidate.index, escape(&candidate.gateway), if candidate.ipv6 { "IPv6" } else { "IPv4" }).unwrap();
    }
    html.push_str("</select><button data-action=\"route-apply\">Aplicar rota manual</button><h3>GATEWAYS AUTORIZADOS PARA EXPLORAÇÃO ATIVA</h3>");
    if routes.candidates.len() < 2 { html.push_str("<p>Sem saída alternativa disponível da mesma família: atualize a lista. Automático não altera rotas com apenas um gateway.</p>"); }
    for (index, candidate) in routes.candidates.iter().enumerate() {
        write!(html, "<label class=\"network-choice\"><input id=\"route-authorize-{index}\" data-network-draft=\"allow:{}\" data-route-candidate=\"{}\" type=\"checkbox\"> {} · if {} · {}</label>", escape(&candidate.id), escape(&candidate.id), escape(&candidate.interface), candidate.index, escape(&candidate.gateway)).unwrap();
    }
    html.push_str("<label class=\"network-choice\"><input id=\"network-consent\" data-network-draft=\"consent\" type=\"checkbox\"> Autorizo testes ativos SOMENTE das saídas/taxas selecionadas; podem desconectar ou atrasar meu jogo. Nada será alterado apenas por detectar um jogo.</label>");
    write!(html, "<button data-action=\"route-learn\"{}>Aprender e aplicar · rotas autorizadas</button>", if routes.candidates.len() < 2 { " disabled" } else { "" }).unwrap();
    html.push_str("<h3>QOS DE SAÍDA POR EXECUTÁVEL · Mbps DECIMAIS</h3><p>Afeta a saída inteira do executável alvo, não o conteúdo nem somente este IP. Pode atrasar o próprio jogo. Não é pacing de pacotes, SQM/CAKE, controle do download ou da fila da operadora. Informe taxas autorizadas; velocidade da placa não é upload medido. DSCP e políticas anteriores são preservados; conflitos ambíguos recusam aplicação.</p>");
    write!(html, "<p>Alvo atual: PID {} (caminho completo revalidado pelo host).</p><label for=\"rate-mbps\">Limite manual em Mbps</label><input id=\"rate-mbps\" data-network-draft=\"rate\" type=\"text\" inputmode=\"decimal\" placeholder=\"1.25\"><button data-action=\"rate-apply\" data-pid=\"{}\"{}>Aplicar limite de saída</button><label for=\"rate-list\">Taxas autorizadas em Mbps, separadas por vírgula (mínimo duas)</label><input id=\"rate-list\" data-network-draft=\"rates\" type=\"text\" placeholder=\"1.25, 2.5\"><button data-action=\"rate-learn\" data-pid=\"{}\"{}>Aprender e aplicar · taxas autorizadas</button></fieldset>", view.network_target_pid, view.network_target_pid, if view.network_target_pid == 0 { " disabled" } else { "" }, view.network_target_pid, if view.network_target_pid == 0 { " disabled" } else { "" }).unwrap();
    write!(html, "<button data-action=\"network-stop\"{}>Parar aprendizado e restaurar referência</button><button data-action=\"route-remove-all\"{}>Remover todos os ajustes de rede desta sessão</button><button data-action=\"rate-remove\"{}>Remover limite de saída próprio</button>", if routes.pending || !routes.auto_active { " disabled" } else { "" }, if routes.pending { " disabled" } else { "" }, if routes.pending || routes.rate.is_none() { " disabled" } else { "" }).unwrap();
    if let Some(rate) = &routes.rate {
        write!(html, "<p class=\"{}\">Saída: {:.6} Mbps · {} · política {} · {}{}</p>", if rate.conflict || !rate.confirmed { "alert" } else { "" }, rate.bps as f64 / 1_000_000.0, escape(&rate.exe), escape(&rate.policy), if rate.confirmed { "configuração ActiveStore confirmada, não prova de pacotes" } else { "intent registrado; aplicação não confirmada, remoção ainda necessária" }, if rate.conflict { " · conflito/erro: não sobrescrever terceiros" } else { "" }).unwrap();
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

pub fn render(view: &Dashboard) -> String {
    let mut html = format!("<p>{}</p>", escape(&view.subtitle));
    for alert in &view.alerts {
        write!(html, "<p class=\"alert\" role=\"alert\">{}</p>", escape(alert)).unwrap();
    }
    if let Some(pin) = &view.pin_status { write!(html, "<p>{}</p>", escape(pin)).unwrap(); }
    html.push_str("<section class=\"network-optimization\"><h2>OTIMIZAÇÃO DE REDE</h2>");
    write!(html, "<label style=\"margin-bottom:8px;display:inline-flex;align-items:center;gap:8px;cursor:pointer\"><input type=\"checkbox\" data-action=\"network-priority\" style=\"accent-color:#46e0d5;width:16px;height:16px\"{}> Priorizar rede do jogo (DSCP 46)</label>", if view.network_priority { " checked" } else { "" }).unwrap();
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
    // Card 4: TCP RTT
    let tcp_entry = view.metrics.iter().find(|(l, _)| l.contains("RTT TCP"));
    if let Some((_, value)) = tcp_entry {
        let (primary, _) = value.split_once(" · ").unwrap_or((value, ""));
        write!(html, "<div class=\"net-card\"><span class=\"net-card-label\">RTT TCP externo</span><span class=\"net-card-value\">{}</span><span class=\"net-card-sub\">DNS público; não é ping do jogo</span></div>", escape(primary)).unwrap();
    }
    // Card 5 wide: ICMP destinations
    let icmp_entries: Vec<_> = view.metrics.iter().filter(|(l, _)| l.contains("ICMP") && l.contains("DESTINO")).collect();
    if !icmp_entries.is_empty() {
        html.push_str("<div class=\"net-card net-card-wide\"><span class=\"net-card-label\">ICMP · IPs TCP do processo</span><ul class=\"net-history\">");
        for (_, value) in &icmp_entries {
            let (rtt, ip) = value.split_once(" · ").unwrap_or((value, ""));
            write!(html, "<li><span>{}</span><span>{}</span></li>", escape(ip), escape(rtt)).unwrap();
        }
        html.push_str("</ul><span class=\"net-disclaimer\">Não identifica servidor de partida. ICMP pode usar rota diferente do tráfego do jogo.</span></div>");
    }
    render_relay(&mut html, &view.relay);
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
        write!(html, "<div class=\"net-card {rc}\"><span class=\"net-card-label\">Limite de saída</span><span class=\"net-card-value\">{:.2} Mbps</span><span class=\"net-card-sub\">{} · {}</span></div>", rate.bps as f64 / 1_000_000.0, escape(&rate.exe), if rate.confirmed { "✅ confirmado" } else { "⏳ pendente" }).unwrap();
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
    write!(html, "<section class=\"telemetry-section\"><h2>TELEMETRIA DO PROGRAMA / JOGO: {}</h2><div class=\"telemetry-grid\">", escape(&view.target)).unwrap();
    for (label, value) in &view.metrics {
        let (primary, sub) = value.split_once(" · ").unwrap_or((value.as_str(), ""));
        write!(html, "<div class=\"telemetry-box\"><span class=\"box-label\">{}</span><strong class=\"box-value\">{}</strong>", escape(label), escape(primary)).unwrap();
        if !sub.is_empty() {
            write!(html, "<span class=\"box-sub\">{}</span>", escape(sub)).unwrap();
        }
        html.push_str("</div>");
    }
    html.push_str("</div></section><section class=\"cores-section\"><h2>USO POR NÚCLEO LÓGICO</h2><p>TOPOLOGIA DETECTADA PELO WINDOWS</p><div class=\"core-grid\">");
    for (index, load, p_core) in &view.cores {
        let percent = load.clamp(0.0, 1.0) * 100.0;
        let color_class = if percent > 85.0 { "core-high" } else if percent > 50.0 { "core-med" } else { "core-low" };
        write!(html, "<div class=\"core-box\"><div class=\"core-header\"><span class=\"core-num\">{:02}</span><span class=\"badge {}\">{}</span><span class=\"core-pct\">{:.0}%</span></div><div class=\"core-bar-bg\"><div class=\"core-bar-fill {}\" style=\"width: {:.1}%\"></div></div></div>", index, if *p_core { "badge-accent" } else { "badge-muted" }, if *p_core { "P-core" } else { "E-core" }, percent, color_class, percent).unwrap();
    }
    html.push_str("</div><div class=\"cache-badges\">");
    for cache in &view.cache { write!(html, "<span class=\"badge badge-muted\">{}</span> ", escape(cache)).unwrap(); }
    html.push_str("</div></section><section class=\"properties-section\"><h2>OPÇÕES E DIRETRIZES DE AGENDAMENTO</h2><div class=\"property-grid\">");
    for (action, on, label, hint) in [
        ("auto-yield", view.auto_yield, "Auto-Yield (afinidade)", "Cede afinidade se outro programa mudar a máscara de uma thread já fixada"),
        ("windowed", view.windowed, "Manter ajustes em segundo plano", "Mantém prioridade e afinidade mesmo com o jogo fora de foco"),
        ("minimize-tray", view.minimize_tray, "Minimizar para a Barra de Ícones (Tray)", "Oculta da barra de tarefas; menu Start/Stop na bandeja"),
    ] {
        write!(html, "<div class=\"property-box\"><label class=\"property-toggle\"><input type=\"checkbox\" data-action=\"{action}\"{}> <div class=\"property-text\"><span class=\"property-name\">{}</span><span class=\"property-desc\">{}</span></div></label></div>", if on { " checked" } else { "" }, escape(label), escape(hint)).unwrap();
    }
    html.push_str("<div class=\"property-box property-box-action\"><div class=\"property-text\"><span class=\"property-name\">Privilégios de Administrador</span><span class=\"property-desc\">Solicita UAC; reinicia para otimizar processos comuns protegidos</span></div><button data-action=\"elevate\" class=\"btn-elevate\" title=\"Solicita UAC; as opções deverão ser selecionadas novamente. Processos protegidos ainda podem negar acesso.\">Reabrir como administrador</button></div>");
    html.push_str("</div></section>");
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
    write!(html, "</section><h2>BIBLIOTECA STEAM: {}</h2><p>{}</p>", view.library_count, escape(&view.aion2_status)).unwrap();
    html
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
            metrics: vec![("ICMP · DESTINO OBSERVADO".into(), "Timeout · <img src=x> & destino".into())], cores: vec![], cache: vec![],
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
        };
        view.routes = Routes {
            candidates: vec![RouteCandidate { id: "x\"<&>".into(), interface: "<img src=x>".into(), index: 2, gateway: "fe80::1%2".into(), ipv6: true }],
            owned: vec![OwnedRoute { destination: "2001:db8::42".into(), interface: "Ethernet".into(), gateway: "fe80::1%2".into(), conflict: false }],
            pending: true, auto_active: true, error: "<script>admin</script>".into(), message: String::new(), automatic: String::new(),
            history: vec!["<b>previsão não é ganho</b>".into()],
            rate: Some(Rate { exe: "C:\\game<&>.exe".into(), policy: "own\"policy".into(), bps: 1_250_000, confirmed: true, conflict: false }),
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
        assert!(html.contains("1.250000 Mbps"));
        assert!(html.contains("C:\\game&lt;&amp;&gt;.exe"));
        assert!(html.contains("&lt;b&gt;previsão não é ganho&lt;/b&gt;"));
        assert!(html.contains("&lt;script&gt;hy2&lt;/script&gt;:443") && html.contains("&lt;img src=udp&gt;"));
        assert!(html.contains("<fieldset id=\"relay-transport\" disabled>"), "transport settings are locked while the relay runs");
        assert!(html.contains("<option value=\"hy2\" selected>") && html.contains("Transporte · HY2"));
    }
}
