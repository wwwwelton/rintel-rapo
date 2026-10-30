//! Header rendering for the webview UI. Pure: input JSON -> HTML string.
//! Compiled natively for tests and to wasm32 for the webview.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

mod dashboard;
pub use dashboard::render_dashboard_json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DttBadge {
    Running,
    Stopped,
    NotInstalled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderView {
    pub optimizer_running: bool,
    pub dtt: DttBadge,
    pub default_config_enabled: bool,
    /// Translated strings keyed by ui.json key; missing keys render the key itself.
    #[serde(default)]
    pub strings: HashMap<String, String>,
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn label<'a>(view: &'a HeaderView, key: &'a str) -> &'a str {
    view.strings.get(key).map(String::as_str).unwrap_or(key)
}

fn dtt_badge_html(view: &HeaderView) -> String {
    let (key, class) = match view.dtt {
        DttBadge::Running => ("badge_dtt_running", "badge-warning"),
        DttBadge::Stopped => ("badge_dtt_stopped", "badge-muted"),
        DttBadge::NotInstalled => ("badge_dtt_none", "badge-accent"),
    };
    format!(r#"<span class="badge {class}">{}</span>"#, escape(label(view, key)))
}

/// Renders the header bar as HTML. Button ids are stable hooks for the webview host.
pub fn render_header(view: &HeaderView) -> String {
    let (status_key, status_class) = if view.optimizer_running {
        ("badge_status_active", "badge-accent")
    } else {
        ("badge_status_paused", "badge-warning")
    };
    let (toggle_key, toggle_action) = if view.optimizer_running {
        ("btn_stop", "stop")
    } else {
        ("btn_start", "start")
    };
    let default_disabled = if view.default_config_enabled { "" } else { " disabled" };

    format!(
        r#"<header class="rapo-header">
<h1>{title}</h1>
<span class="badge badge-accent">{scheduling}</span>
<span class="badge {status_class}">{status}</span>
<div class="actions">
{dtt}
<button id="btn-refresh" data-action="refresh">{refresh}</button>
<button id="btn-scan-steam" data-action="scan">{scan}</button>
<button id="btn-default-config" data-action="reset"{default_disabled}>{default_cfg}</button>
<button id="btn-toggle" data-action="{toggle_action}">{toggle}</button>
</div>
</header>"#,
        title = escape(label(view, "txt_app_title")),
        scheduling = escape(label(view, "badge_scheduling")),
        status = escape(label(view, status_key)),
        dtt = dtt_badge_html(view),
        refresh = escape(label(view, "btn_refresh")),
        scan = escape(label(view, "btn_scan_steam")),
        default_cfg = escape(label(view, "btn_default_config")),
        toggle = escape(label(view, toggle_key)),
    )
}

/// JS entry point: takes the header model as JSON, returns HTML or an error string.
#[wasm_bindgen]
pub fn render_header_json(json: &str) -> Result<String, JsValue> {
    let view: HeaderView = serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(render_header(&view))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameView {
    pub name: String,
    pub directory: String,
    pub is_aion2: bool,
    pub auto_priority: bool,
    pub pcores: bool,
    pub qos: bool,
    pub rapo: bool,
    /// False during training; RAPO selection remains available to stop or switch games.
    pub controls_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GamesView {
    pub games: Vec<GameView>,
    #[serde(default)]
    pub strings: HashMap<String, String>,
}

fn games_label<'a>(view: &'a GamesView, key: &'a str) -> &'a str {
    view.strings.get(key).map(String::as_str).unwrap_or(key)
}

/// Renders the game list as HTML. Controls carry `data-action` and `data-dir`
/// so the host can map a click to the matching `Command`.
pub fn render_games(view: &GamesView) -> String {
    let mut html = String::from("<ul class=\"games\">");
    for game in &view.games {
        let disabled = if game.controls_enabled { "" } else { " disabled" };
        let dir = escape(&game.directory);
        let checkbox = |action: &str, checked: bool, text: &str| {
            format!(
                "<label><input type=\"checkbox\" data-action=\"{action}\" data-dir=\"{dir}\"{} {}> {}</label>",
                if checked { " checked" } else { "" },
                if action == "rapo" { "" } else { disabled.trim() },
                escape(text),
            )
        };
        html.push_str("<li class=\"game\"><span class=\"name\">");
        html.push_str(&escape(&game.name));
        html.push_str("</span> ");
        html.push_str(&checkbox("qos", game.qos, "Prioridade de memória / EcoQoS"));
        html.push(' ');
        html.push_str(&checkbox("pcores", game.pcores, games_label(view, "chk_game_pcores")));
        html.push(' ');
        html.push_str(&checkbox("auto", game.auto_priority, games_label(view, "chk_game_auto_priority")));
        html.push(' ');
        html.push_str(&checkbox("rapo", game.rapo, games_label(view, "chk_rapo_library")));
        if game.is_aion2 {
            html.push_str(&format!(
                " <button type=\"button\" data-action=\"apo-profile\" data-dir=\"{dir}\"{disabled}>{}</button>",
                escape(games_label(view, "btn_apo_profile")),
            ));
        }
        html.push_str("</li>");
    }
    html.push_str("</ul>");
    html
}

/// JS entry point for the game list, mirroring `render_header_json`.
#[wasm_bindgen]
pub fn render_games_json(json: &str) -> Result<String, JsValue> {
    let view: GamesView = serde_json::from_str(json).map_err(|error| JsValue::from_str(&error.to_string()))?;
    Ok(render_games(&view))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetView {
    pub pid: u32,
    pub label: String,
    pub hint: String,
    pub selected: bool,
}

/// Mirrors the serialized `snapshot::TrainingView`; labels are pre-formatted on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrainingView {
    pub cpu_line: Option<String>,
    pub active: bool,
    pub headline: Option<String>,
    pub target_line: Option<String>,
    pub progress_line: Option<String>,
    pub candidate_line: Option<String>,
    pub best_line: Option<String>,
    pub armed_line: Option<String>,
    pub targets: Vec<TargetView>,
    pub selected_label: Option<String>,
    pub can_start: bool,
    pub message: String,
}

/// Renders the training panel as HTML. Buttons carry `data-action` (and `data-pid`
/// for target picks) so the host maps them with `from_training_event`.
pub fn render_training(view: &TrainingView) -> String {
    let mut html = String::from("<section class=\"training\"><h2>Avatarnoia Flow Engine 2.0 · FPS REAL</h2>");
    let paragraph = |text: &str| format!("<p>{}</p>", escape(text));
    if let Some(cpu) = &view.cpu_line {
        html.push_str(&paragraph(cpu));
    }
    if view.active {
        for line in [&view.headline, &view.target_line, &view.progress_line, &view.candidate_line, &view.best_line].into_iter().flatten() {
            html.push_str(&paragraph(line));
        }
        html.push_str(&paragraph("Testa cores, afinidade, threads críticas e irmãos SMT/Hyper-Threading conforme a topologia real do grupo 0. Sair pausa e descarta a janela parcial; voltar reinicia a medição do mesmo candidato. Não altera BIOS nem desativa SMT globalmente."));
        html.push_str("<button type=\"button\" data-action=\"stop_training\">Parar aprendizado e manter último perfil salvo</button>");
    } else {
        if let Some(armed) = &view.armed_line {
            html.push_str(&paragraph(armed));
            html.push_str("<button type=\"button\" data-action=\"disable_rapo\">Desativar RAPO</button>");
        }
        html.push_str(&paragraph(view.selected_label.as_deref().unwrap_or("Abra um jogo Steam")));
        html.push_str(&paragraph("RAPO aprende prioridade, memória/EcoQoS, quantidade de núcleos físicos, afinidade P/all, threads críticas e irmãos SMT/Hyper-Threading disponíveis. Um jogo por vez; pausa ao sair e retoma o mesmo candidato."));
        html.push_str(&paragraph("ETW de apresentação, sem rastrear entrada ou execução na GPU, sem injeção, driver ou leitura da memória do jogo. Acesso negado interrompe a captura, sem contornar proteções."));
        html.push_str("<ul class=\"targets\">");
        for target in &view.targets {
            html.push_str(&format!(
                "<li><button type=\"button\" data-action=\"select_training_target\" data-pid=\"{}\" aria-pressed=\"{}\" title=\"{}\">{}</button></li>",
                target.pid,
                target.selected,
                escape(&target.hint),
                escape(&target.label),
            ));
        }
        html.push_str("</ul>");
        let disabled = if view.can_start { "" } else { " disabled" };
        html.push_str(&format!("<button type=\"button\" data-action=\"start_training\"{disabled}>Iniciar aprendizado contínuo</button>"));
    }
    html.push_str(&paragraph("O resultado é o melhor perfil medido neste cenário, não garantia de ganho ou de compatibilidade com anti-cheat. Use uma cena repetível."));
    html.push_str("<details><summary id=\"collector-license\">Licença do coletor integrado</summary><p>PresentData / PresentMon 2.3.0 · Intel · MIT</p><pre>");
    html.push_str(&escape(include_str!("../../vendor/presentmon/LICENSE.txt")));
    html.push_str("</pre></details>");
    if !view.message.is_empty() {
        html.push_str(&paragraph(&view.message));
    }
    html.push_str("</section>");
    html
}

/// JS entry point for the training panel, mirroring `render_games_json`.
#[wasm_bindgen]
pub fn render_training_json(json: &str) -> Result<String, JsValue> {
    let view: TrainingView = serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(render_training(&view))
}

#[cfg(test)]
mod games_tests {
    use super::*;

    fn game(name: &str, dir: &str, aion2: bool, controls_enabled: bool) -> GameView {
        GameView {
            name: name.into(),
            directory: dir.into(),
            is_aion2: aion2,
            auto_priority: true,
            pcores: false,
            qos: false,
            rapo: false,
            controls_enabled,
        }
    }

    #[test]
    fn escapes_names_and_disables_controls_during_training() {
        let view = GamesView {
            games: vec![game("<b>X</b>", "C:\\a\"b", false, false)],
            strings: HashMap::new(),
        };
        let html = render_games(&view);
        assert!(!html.contains("<b>X</b>"));
        assert!(html.contains("&quot;"));
        assert!(!html.contains("apo-profile"));
    }

    #[test]
    fn rapo_remains_selectable_while_manual_settings_are_locked() {
        let html = render_games(&GamesView {
            games: vec![game("Game", "/g", false, false)],
            strings: HashMap::new(),
        });
        for (action, disabled) in [("rapo", false), ("qos", true), ("pcores", true), ("auto", true)] {
            let marker = format!("data-action=\"{action}\"");
            let attributes = html.split_once(&marker).unwrap().1.split_once('>').unwrap().0;
            assert_eq!(attributes.split_whitespace().any(|a| a == "disabled"), disabled, "{action}");
        }
    }

    #[test]
    fn aion2_row_gets_profile_button_only_when_enabled() {
        let view = GamesView {
            games: vec![game("AION 2", "/g/aion", true, true)],
            strings: HashMap::new(),
        };
        let html = render_games(&view);
        assert!(html.contains("data-action=\"apo-profile\""));
        assert!(!html.contains("disabled>"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(running: bool, dtt: DttBadge, enabled: bool) -> HeaderView {
        HeaderView {
            optimizer_running: running,
            dtt,
            default_config_enabled: enabled,
            strings: HashMap::new(),
        }
    }


    #[test]
    fn toggle_reflects_optimizer_state() {
        assert!(render_header(&view(true, DttBadge::NotInstalled, true)).contains(r#"data-action="stop""#));
        assert!(render_header(&view(false, DttBadge::NotInstalled, true)).contains(r#"data-action="start""#));
    }

    #[test]
    fn dtt_badge_class_per_status() {
        assert!(render_header(&view(true, DttBadge::Running, true)).contains("badge-warning\">badge_dtt_running"));
        assert!(render_header(&view(true, DttBadge::Stopped, true)).contains("badge-muted\">badge_dtt_stopped"));
        assert!(render_header(&view(true, DttBadge::NotInstalled, true)).contains("badge-accent\">badge_dtt_none"));
    }

    #[test]
    fn translated_strings_are_escaped() {
        let mut v = view(true, DttBadge::Stopped, true);
        v.strings.insert("btn_refresh".into(), "<b>Atualizar & ir</b>".into());
        let html = render_header(&v);
        assert!(html.contains("&lt;b&gt;Atualizar &amp; ir&lt;/b&gt;"));
        assert!(!html.contains("<b>Atualizar"));
    }

    #[test]
    fn json_entry_point_parses_model() {
        let json = r#"{"optimizer_running":false,"dtt":"not_installed","default_config_enabled":true}"#;
        let html = render_header_json(json).unwrap();
        assert!(html.contains("data-action=\"start\""));
    }
}

#[cfg(test)]
mod training_tests {
    use super::*;

    fn view(active: bool, can_start: bool) -> TrainingView {
        TrainingView {
            cpu_line: None,
            active,
            headline: active.then(|| "RAPO ATIVO · <medindo>".to_string()),
            target_line: None,
            progress_line: None,
            candidate_line: None,
            best_line: None,
            armed_line: None,
            targets: vec![TargetView { pid: 7, label: "AION 2 · aion.exe · PID 7".into(), hint: "C:/aion.exe".into(), selected: true }],
            selected_label: None,
            can_start,
            message: String::new(),
        }
    }

    #[test]
    fn idle_renders_start_enabled_only_when_allowed() {
        let html = render_training(&view(false, true));
        assert!(html.contains("data-action=\"start_training\">Iniciar aprendizado contínuo</button>"));
        assert!(render_training(&view(false, false)).contains("data-action=\"start_training\" disabled>"));
    }

    #[test]
    fn target_buttons_carry_pid_and_selection() {
        let html = render_training(&view(false, true));
        assert!(html.contains("data-action=\"select_training_target\" data-pid=\"7\" aria-pressed=\"true\""));
    }

    #[test]
    fn active_session_shows_stop_and_escapes_text() {
        let html = render_training(&view(true, true));
        assert!(html.contains("data-action=\"stop_training\""));
        assert!(!html.contains("start_training"));
        assert!(html.contains("RAPO ATIVO · &lt;medindo&gt;"));
    }

    #[test]
    fn parses_snapshot_json_shape() {
        let json = r#"{"cpu_line":null,"active":false,"headline":null,"target_line":null,"progress_line":null,"candidate_line":null,"best_line":null,"armed_line":null,"targets":[],"selected_label":null,"can_start":false,"message":""}"#;
        let html = render_training_json(json).unwrap();
        assert!(html.contains("disabled"));
    }
}
