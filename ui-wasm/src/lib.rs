//! Header rendering for the webview UI. Pure: input JSON -> HTML string.
//! Compiled natively for tests and to wasm32 for the webview.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;

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
<button id="btn-refresh">{refresh}</button>
<button id="btn-scan-steam">{scan}</button>
<button id="btn-default-config"{default_disabled}>{default_cfg}</button>
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
    /// False while training runs; every control in the row is disabled then.
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
                disabled.trim(),
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
        assert_eq!(html.matches(" disabled").count(), 4);
        assert!(!html.contains("apo-profile"));
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
    fn default_button_disabled_when_games_running() {
        let html = render_header(&view(true, DttBadge::NotInstalled, false));
        assert!(html.contains(r#"id="btn-default-config" disabled"#));
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
