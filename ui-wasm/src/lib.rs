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
