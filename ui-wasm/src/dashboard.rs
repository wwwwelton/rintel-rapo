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

pub fn render(view: &Dashboard) -> String {
    let mut html = format!("<p>{}</p>", escape(&view.subtitle));
    for alert in &view.alerts {
        write!(html, "<p class=\"alert\" role=\"alert\">{}</p>", escape(alert)).unwrap();
    }
    if let Some(pin) = &view.pin_status { write!(html, "<p>{}</p>", escape(pin)).unwrap(); }
    html.push_str("<section><h2>TWEAK DE REDE · QoS / DSCP 46</h2>");
    if view.network_tweaks.is_empty() {
        html.push_str("<p>Nenhuma política de rede aplicada pelo RAPO.</p>");
    } else {
        html.push_str("<p role=\"status\">Política de rede aplicada para:</p><ul>");
        for exe in &view.network_tweaks {
            write!(html, "<li>{}</li>", escape(exe)).unwrap();
        }
        html.push_str("</ul><p>DSCP 46 configurado no Windows; não confirma marcação dos pacotes nem redução do ping. Depende do suporte do roteador e da operadora.</p>");
    }
    html.push_str("<p>Sem limitador de taxa configurado. QoS nativo limita somente saída com taxa explícita; não é SQM/CAKE e não controla filas da operadora. Nenhuma taxa ou interface é ajustada automaticamente.</p>");
    html.push_str("</section>");
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
        let view = Dashboard {
            alerts: vec!["<img src=x onerror=alert(1)>".into()], subtitle: String::new(), target: String::new(),
            metrics: vec![("ICMP · DESTINO OBSERVADO".into(), "Timeout · <img src=x> & destino".into())], cores: vec![], cache: vec![],
            running: vec![Process { pid: 42, game: "<script>".into(), exe: "a\"b".into(), state: String::new(), enabled: false, auto: false, qos: false, priority: false, can_toggle: false }],
            auto_yield: true, windowed: false, minimize_tray: false, library_count: 0, aion2_status: String::new(), pin_status: None,
            profile: None,
            network_tweaks: vec!["game<&>.exe".into()],
            power_plan: PowerPlanState { plan: Some(PowerPlan { name: "<script>energia</script>".into(), guid: "a\"b".into() }), error: "<b>falha</b>".into() },
        };
        let html = render(&view);
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(html.contains("data-pid=\"42\" disabled>Otimizar"));
        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(html.contains("&lt;script&gt;energia&lt;/script&gt;"), "a failed read must keep the last plan visible");
        assert!(html.contains("Última leitura mantida; a consulta atual falhou: &lt;b&gt;falha&lt;/b&gt;"));
        assert!(html.contains("<li>game&lt;&amp;&gt;.exe</li>"));
        assert!(html.contains("&lt;img src=x&gt; &amp; destino"));
        assert!(html.contains("Sem limitador de taxa configurado"));
    }
}
