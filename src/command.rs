//! UI intents. The render only records them; `App::apply` runs the side effects
//! after the frame, so the render itself stays read-only.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Desired optimizer state, computed from the state the frame was drawn with.
    SetOptimizerRunning(bool),
    Refresh,
    /// Requests UAC elevation and closes this instance on success.
    RelaunchElevated,
    /// Library filter text, typed into the search box.
    SetSearch(String),
    ScanSteam,
    ResetNetQos,
    SetAutoYield(bool),
    SetMinimizeToTray(bool),
    SetQos { directory: PathBuf, on: bool },
    SetPin { directory: PathBuf, on: bool },
    SetAuto { directory: PathBuf, on: bool },
    SetWindowed(bool),
    /// Optimize or restore one running process, looked up by pid when applied.
    ToggleProcess { pid: u32 },
    /// `None` clears the RAPO game.
    SetRapo(Option<PathBuf>),
    ApplyAion2Profile { directory: PathBuf },
    /// Starts continuous learning on the selected process.
    StartTraining,
    /// Stops learning and keeps the last saved profile.
    StopTraining,
    /// Chooses the process the next session targets, looked up by pid when applied.
    SelectTrainingTarget { pid: u32 },
    ApplyLearnedProfile(PathBuf),
    DeleteLearnedProfile(PathBuf),
    RefreshRoutes,
    ApplyRoute { destination: String, candidate: String },
    RemoveRoute(String),
    RemoveAllRoutes,
    StartRouteLearning { destination: String, candidates: Vec<String> },
    StopNetworkLearning,
    /// System-wide outbound limit; `pid` names the game to exempt, if any.
    ApplyRate { pid: Option<u32>, bps: u64 },
    StartRateLearning { pid: u32, destination: String, rates: Vec<u64> },
    RemoveRate,
    SetNetworkPriority(bool),
    SetFocus(bool),
    SetRelay(bool),
    SetRelayInterfaces(Vec<String>),
    ConfigureRelay { mode: crate::relay::Mode, config: crate::relay::Hy2Config },
    SetWfp(bool),
    /// Enables or disables one `engine_ini::OPTIONS` entry (preset or extra) in AION 2's Engine.ini.
    SetEngineIniOption { index: usize, on: bool },
    /// On/Off switch of the saved Engine.ini profile; Off removes RAPO's lines from the file.
    SetEngineIniActive(bool),
    BackupEngineIni,
    RestoreEngineIni,
    /// Creates or updates a VPN profile. A secret left empty keeps the stored value, so the page never needs it back.
    SaveVpnProfile { protocol: crate::vpn::Protocol, name: String, fields: std::collections::BTreeMap<String, String>, secrets: std::collections::BTreeMap<String, String> },
    /// Imports the text of a wg-quick `.conf` or `.ovpn` file chosen in the page; the host never reads a path from the page.
    ImportVpnProfile { protocol: crate::vpn::Protocol, name: String, text: String },
    /// Makes one profile the only active one; `None` deactivates.
    ActivateVpnProfile(Option<String>),
    RemoveVpnProfile(String),
    SetUdpRedundancy(crate::relay::RedundancyConfig),
    SetUdpRedundancyAuto(Option<std::net::SocketAddr>),
}

/// Maps a webview event (`data-action`, `data-dir`, checkbox state) to a command.
/// Unknown actions or missing fields yield `None`, so the event is ignored.
pub fn from_ui_event(action: &str, dir: Option<&str>, checked: Option<bool>) -> Option<Command> {
    let directory = || dir.map(PathBuf::from);
    match action {
        "start" => Some(Command::SetOptimizerRunning(true)),
        "stop" => Some(Command::SetOptimizerRunning(false)),
        "refresh" => Some(Command::Refresh),
        "scan" => Some(Command::ScanSteam),
        "reset" => Some(Command::ResetNetQos),
        "elevate" => Some(Command::RelaunchElevated),
        "auto-yield" => Some(Command::SetAutoYield(checked?)),
        "windowed" => Some(Command::SetWindowed(checked?)),
        "minimize-tray" => Some(Command::SetMinimizeToTray(checked?)),
        "qos" => Some(Command::SetQos { directory: directory()?, on: checked? }),
        "pcores" => Some(Command::SetPin { directory: directory()?, on: checked? }),
        "auto" => Some(Command::SetAuto { directory: directory()?, on: checked? }),
        "rapo" => Some(Command::SetRapo(if checked? { directory() } else { None })),
        "apo-profile" => Some(Command::ApplyAion2Profile { directory: directory()? }),
        "profile-apply" => Some(Command::ApplyLearnedProfile(directory()?)),
        "profile-delete" => Some(Command::DeleteLearnedProfile(directory()?)),
        "network-priority" => Some(Command::SetNetworkPriority(checked?)),
        "focus" => Some(Command::SetFocus(checked?)),
        "relay" => Some(Command::SetRelay(checked?)),
        "wfp" => Some(Command::SetWfp(checked?)),
        "engine-ini-active" => Some(Command::SetEngineIniActive(checked?)),
        "engine-ini-backup" => Some(Command::BackupEngineIni),
        "engine-ini-restore" => Some(Command::RestoreEngineIni),
        _ => None,
    }
}


/// Decodes the JSON IPC boundary; invalid fields never become commands.
pub fn from_json(body: &str) -> Option<Command> {
    let event: serde_json::Value = serde_json::from_str(body).ok()?;
    let action = event.get("action")?.as_str()?;
    if action == "search" {
        return Some(Command::SetSearch(event.get("text")?.as_str()?.to_owned()));
    }
    let dir = event.get("dir").and_then(|v| v.as_str());
    let checked = event.get("checked").and_then(|v| v.as_bool());
    let pid = event.get("pid").and_then(|v| v.as_u64()).and_then(|p| u32::try_from(p).ok());
    let text = |key| event.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty() && s.len() <= 512).map(str::to_owned);
    let bool_or = |key, default| match event.get(key) {
        None | Some(serde_json::Value::Null) => Some(default),
        Some(v) => v.as_bool(),
    };
    let u32_or = |key, default| match event.get(key) {
        None | Some(serde_json::Value::Null) => Some(default),
        Some(v) => v.as_u64().and_then(|n| u32::try_from(n).ok()),
    };
    match action {
        "relay-configure" => {
            let mode = serde_json::from_value(event.get("mode")?.clone()).ok()?;
            let bounded = |key, limit| event.get(key)?.as_str().filter(|value| value.len() <= limit).map(str::to_owned);
            let config = crate::relay::Hy2Config {
                server: bounded("server", 512)?,
                auth: bounded("auth", 4096)?,
                server_name: bounded("server_name", 253)?,
                ca_pem: bounded("ca_pem", 65_536)?,
            };
            return Some(Command::ConfigureRelay { mode, config });
        }
        "relay-interfaces" => {
            let candidates = event.get("candidates")?.as_array()?;
            if candidates.len() > 64 { return None; }
            let candidates = candidates.iter().map(|v| v.as_str().filter(|id| !id.is_empty() && id.len() <= 512).map(str::to_owned)).collect::<Option<Vec<_>>>()?;
            return Some(Command::SetRelayInterfaces(candidates));
        }
        "engine-ini" => {
            let key = event.get("key")?.as_str()?;
            let index = crate::engine_ini::OPTIONS.iter().position(|a| a.key == key)?;
            return Some(Command::SetEngineIniOption { index, on: checked? });
        }
        "engine-ini-backup" => return Some(Command::BackupEngineIni),
        "engine-ini-restore" => return Some(Command::RestoreEngineIni),
        "vpn-save" => {
            let protocol = crate::vpn::Protocol::parse(event.get("protocol")?.as_str()?)?;
            let map = |key| -> Option<std::collections::BTreeMap<String, String>> {
                let object = event.get(key)?.as_object()?;
                if object.len() > 16 { return None; }
                object.iter().map(|(k, v)| Some((k.clone(), v.as_str().filter(|s| s.len() <= crate::vpn::MAX_IMPORT)?.to_owned()))).collect()
            };
            return Some(Command::SaveVpnProfile { protocol, name: event.get("name")?.as_str().filter(|s| s.len() <= 320)?.to_owned(), fields: map("fields")?, secrets: map("secrets")? });
        }
        "vpn-import" => {
            let protocol = crate::vpn::Protocol::parse(event.get("protocol")?.as_str()?)?;
            if protocol == crate::vpn::Protocol::Hysteria2 { return None; } // HY2 has no file format
            let file = event.get("text")?.as_str().filter(|s| s.len() <= crate::vpn::MAX_IMPORT)?.to_owned();
            return Some(Command::ImportVpnProfile { protocol, name: event.get("name").and_then(|v| v.as_str()).unwrap_or("").to_owned(), text: file });
        }
        "vpn-activate" => return Some(Command::ActivateVpnProfile(Some(text("name")?))),
        "vpn-deactivate" => return Some(Command::ActivateVpnProfile(None)),
        "vpn-remove" => return Some(Command::RemoveVpnProfile(text("name")?)),
        "udp-redundancy" => {
            let enabled = bool_or("enabled", false)?;
            let x = u32_or("x", 20)?;
            let y = u32_or("y", 10)?;
            let config = crate::relay::RedundancyConfig::new(enabled, x, y).ok()?;
            return Some(Command::SetUdpRedundancy(config));
        }
        "udp-redundancy-auto" => {
            let enabled = bool_or("enabled", false)?;
            let echo = if enabled {
                let raw = text("echo")?;
                Some(raw.parse::<std::net::SocketAddr>().ok()?)
            } else {
                None
            };
            return Some(Command::SetUdpRedundancyAuto(echo));
        }
        "route-refresh" => return Some(Command::RefreshRoutes),
        "route-apply" => return Some(Command::ApplyRoute { destination: text("destination")?, candidate: text("candidate")? }),
        "route-remove" => return Some(Command::RemoveRoute(text("destination")?)),
        "route-remove-all" => return Some(Command::RemoveAllRoutes),
        "network-stop" => return Some(Command::StopNetworkLearning),
        "rate-remove" => return Some(Command::RemoveRate),
        "route-learn" => {
            if !event.get("consent")?.as_bool()? { return None; }
            let candidates = event.get("candidates")?.as_array()?.iter().map(|v| v.as_str().filter(|s| !s.is_empty() && s.len() <= 512).map(str::to_owned)).collect::<Option<Vec<_>>>()?;
            return Some(Command::StartRouteLearning { destination: text("destination")?, candidates });
        }
        "rate-apply" => return Some(Command::ApplyRate { pid: pid.filter(|p| *p != 0), bps: crate::network_qos::megabytes_per_second(&text("mb_s")?).ok()? }),
        "rate-learn" => {
            if !event.get("consent")?.as_bool()? { return None; }
            let rates = event.get("rates")?.as_array()?.iter().map(|v| crate::network_qos::megabytes_per_second(v.as_str()?).ok()).collect::<Option<Vec<_>>>()?;
            return Some(Command::StartRateLearning { pid: pid.filter(|p| *p != 0)?, destination: text("destination")?, rates });
        }
        _ => (),
    }
    from_ui_event(action, dir, checked).or_else(|| from_training_event(action, pid))
}
/// Maps training panel events (buttons and target picks by `pid`) to commands.
pub fn from_training_event(action: &str, pid: Option<u32>) -> Option<Command> {
    match action {
        "start_training" => Some(Command::StartTraining),
        "stop_training" => Some(Command::StopTraining),
        "disable_rapo" => Some(Command::SetRapo(None)),
        "toggle-process" => Some(Command::ToggleProcess { pid: pid? }),
        "select_training_target" => Some(Command::SelectTrainingTarget { pid: pid? }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_checkbox_events_with_directory_and_state() {
        assert_eq!(
            from_ui_event("qos", Some("C:/g"), Some(true)),
            Some(Command::SetQos { directory: "C:/g".into(), on: true })
        );
        assert_eq!(
            from_ui_event("pcores", Some("C:/g"), Some(false)),
            Some(Command::SetPin { directory: "C:/g".into(), on: false })
        );
    }

    #[test]
    fn rapo_checkbox_selects_or_clears_the_game() {
        assert_eq!(from_ui_event("rapo", Some("C:/g"), Some(true)), Some(Command::SetRapo(Some("C:/g".into()))));
        assert_eq!(from_ui_event("rapo", Some("C:/g"), Some(false)), Some(Command::SetRapo(None)));
    }

    #[test]
    fn ignores_unknown_actions_and_missing_fields() {
        assert_eq!(from_ui_event("nope", Some("C:/g"), Some(true)), None);
        assert_eq!(from_ui_event("qos", None, Some(true)), None);
        assert_eq!(from_ui_event("qos", Some("C:/g"), None), None);
    }

    #[test]
    fn training_events_map_to_commands() {
        assert_eq!(from_training_event("start_training", None), Some(Command::StartTraining));
        assert_eq!(from_training_event("stop_training", None), Some(Command::StopTraining));
        assert_eq!(from_training_event("disable_rapo", None), Some(Command::SetRapo(None)));
        assert_eq!(from_training_event("select_training_target", Some(7)), Some(Command::SelectTrainingTarget { pid: 7 }));
        assert_eq!(from_training_event("select_training_target", None), None);
        assert_eq!(from_training_event("qos", None), None);
        assert_eq!(from_ui_event("profile-apply", Some("C:/g.exe"), None), Some(Command::ApplyLearnedProfile("C:/g.exe".into())));
        assert_eq!(from_ui_event("profile-delete", Some("C:/g.exe"), None), Some(Command::DeleteLearnedProfile("C:/g.exe".into())));
        assert_eq!(from_ui_event("profile-delete", None, None), None);
    }

    #[test]
    fn ipc_rejects_invalid_process_ids_and_missing_checkbox_state() {
        for pid in ["-1", "4294967296", "\"7\"", "null", "7.5"] {
            assert_eq!(from_json(&format!(r#"{{"action":"toggle-process","pid":{pid}}}"#)), None);
        }
        assert_eq!(from_json(r#"{"action":"toggle-process","pid":7}"#), Some(Command::ToggleProcess { pid: 7 }));
        assert_eq!(from_json(r#"{"action":"windowed"}"#), None);
        assert_eq!(from_json(r#"{"action":"windowed","checked":false}"#), Some(Command::SetWindowed(false)));
        assert_eq!(from_json(r#"{"action":"search","text":"AION 2"}"#), Some(Command::SetSearch("AION 2".into())));
        assert_eq!(from_json("not JSON"), None);
        assert_eq!(from_json(r#"{"action":"engine-ini","key":"very-low","checked":true}"#), Some(Command::SetEngineIniOption { index: 0, on: true }));
        for body in [r#"{"action":"engine-ini","key":"t.OverrideFPS","checked":true}"#, r#"{"action":"engine-ini","key":"t.MaxFPS"}"#] {
            assert_eq!(from_json(body), None, "{body}");
        }
        for body in [r#"{"action":"relay"}"#, r#"{"action":"relay","checked":"true"}"#,
            r#"{"action":"relay-interfaces","candidates":[2]}"#, r#"{"action":"relay-interfaces","candidates":[""]}"#] {
            assert_eq!(from_json(body), None);
        }
    }

    #[test]
    fn relay_configuration_rejects_unknown_modes_untyped_and_oversized_secrets() {
        let valid = serde_json::json!({
            "action": "relay-configure", "mode": "hy2",
            "server": "localhost:443", "server_name": "localhost",
            "auth": "test-secret", "ca_pem": "",
        });
        assert!(matches!(from_json(&valid.to_string()), Some(Command::ConfigureRelay { mode: crate::relay::Mode::Hy2, .. })));
        for (key, value) in [
            ("mode", serde_json::json!("transparent")),
            ("auth", serde_json::json!(42)),
            ("auth", serde_json::json!("x".repeat(4097))),
            ("server", serde_json::json!("x".repeat(513))),
            ("ca_pem", serde_json::json!("x".repeat(65_537))),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert_eq!(from_json(&invalid.to_string()), None, "{key}");
        }
    }

    #[test]
    fn route_and_rate_ipc_requires_typed_fields_and_explicit_learning_consent() {
        assert_eq!(from_json(r#"{"action":"route-apply","destination":"192.0.2.42","candidate":"7:2:192.0.2.1%0"}"#),
            Some(Command::ApplyRoute { destination: "192.0.2.42".into(), candidate: "7:2:192.0.2.1%0".into() }));
        for body in [r#"{"action":"route-apply","destination":42,"candidate":"x"}"#,
            r#"{"action":"route-learn","destination":"192.0.2.42","candidates":["a","b"]}"#,
            r#"{"action":"route-learn","destination":"192.0.2.42","candidates":[3],"consent":true}"#,
            r#"{"action":"rate-apply","pid":7,"mb_s":"NaN"}"#,
            r#"{"action":"rate-apply","pid":7,"mbps":"1"}"#,
            r#"{"action":"rate-learn","pid":7,"destination":"192.0.2.42","rates":["1","2"],"consent":false}"#] { assert_eq!(from_json(body), None, "{body}"); }
        assert_eq!(from_json(r#"{"action":"rate-apply","pid":7,"mb_s":"1.25"}"#), Some(Command::ApplyRate { pid: Some(7), bps: 10_000_000 }));
        assert_eq!(from_json(r#"{"action":"rate-apply","pid":0,"mb_s":"2"}"#), Some(Command::ApplyRate { pid: None, bps: 16_000_000 }), "the system limit needs no running game");
    }

    /// The page always sends `dir`, `checked` and `pid` (null when absent) plus the fields its handler adds.
    /// These are the exact payloads `ui-wasm/index.html` builds for each control, so a renamed field or a changed
    /// type on either side turns a working button into a silent no-op here instead of in the field.
    #[test]
    fn every_control_the_page_can_send_reaches_its_command() {
        let page = |action: &str, extra: serde_json::Value| {
            let mut event = serde_json::json!({ "action": action, "dir": null, "checked": null, "pid": null });
            for (key, value) in extra.as_object().unwrap() { event[key] = value.clone(); }
            from_json(&event.to_string())
        };
        let none = serde_json::json!({});
        let dir = "C:\\Steam\\AION2";
        let route = |action: &str| page(action, serde_json::json!({ "destination": "203.0.113.9", "candidate": "c1", "consent": true, "candidates": ["c1", "c2"], "mb_s": "5", "rates": ["2", "5"] }));
        for (action, expected) in [
            ("start", Command::SetOptimizerRunning(true)), ("stop", Command::SetOptimizerRunning(false)),
            ("refresh", Command::Refresh), ("scan", Command::ScanSteam), ("reset", Command::ResetNetQos),
            ("elevate", Command::RelaunchElevated), ("start_training", Command::StartTraining),
            ("stop_training", Command::StopTraining), ("disable_rapo", Command::SetRapo(None)),
            ("route-refresh", Command::RefreshRoutes), ("route-remove-all", Command::RemoveAllRoutes),
            ("network-stop", Command::StopNetworkLearning), ("rate-remove", Command::RemoveRate),
        ] { assert_eq!(page(action, none.clone()), Some(expected), "{action}"); }
        for (action, expected) in [
            ("auto-yield", Command::SetAutoYield(true)), ("windowed", Command::SetWindowed(true)),
            ("minimize-tray", Command::SetMinimizeToTray(true)), ("network-priority", Command::SetNetworkPriority(true)), ("focus", Command::SetFocus(true)),
            ("relay", Command::SetRelay(true)), ("wfp", Command::SetWfp(true)), ("engine-ini-active", Command::SetEngineIniActive(true)),
            ("engine-ini-backup", Command::BackupEngineIni), ("engine-ini-restore", Command::RestoreEngineIni),
        ] { assert_eq!(page(action, serde_json::json!({ "checked": true })), Some(expected), "{action}"); }
        for (action, on) in [("qos", true), ("pcores", false), ("auto", true)] {
            let expected = match action { "qos" => Command::SetQos { directory: dir.into(), on }, "pcores" => Command::SetPin { directory: dir.into(), on }, _ => Command::SetAuto { directory: dir.into(), on } };
            assert_eq!(page(action, serde_json::json!({ "dir": dir, "checked": on })), Some(expected), "{action}");
        }
        assert_eq!(page("rapo", serde_json::json!({ "dir": dir, "checked": true })), Some(Command::SetRapo(Some(dir.into()))));
        assert_eq!(page("rapo", serde_json::json!({ "dir": dir, "checked": false })), Some(Command::SetRapo(None)));
        assert_eq!(page("apo-profile", serde_json::json!({ "dir": dir })), Some(Command::ApplyAion2Profile { directory: dir.into() }));
        assert_eq!(page("toggle-process", serde_json::json!({ "pid": 54732 })), Some(Command::ToggleProcess { pid: 54732 }));
        assert_eq!(page("select_training_target", serde_json::json!({ "pid": 58152 })), Some(Command::SelectTrainingTarget { pid: 58152 }));
        assert_eq!(page("engine-ini", serde_json::json!({ "checked": true, "key": "very-low" })), Some(Command::SetEngineIniOption { index: 0, on: true }));
        assert_eq!(page("relay-interfaces", serde_json::json!({ "checked": false, "candidates": ["c2"] })), Some(Command::SetRelayInterfaces(vec!["c2".into()])));
        assert_eq!(page("route-apply", serde_json::json!({ "destination": "203.0.113.9", "candidate": "c1" })), Some(Command::ApplyRoute { destination: "203.0.113.9".into(), candidate: "c1".into() }));
        assert_eq!(route("route-remove"), Some(Command::RemoveRoute("203.0.113.9".into())), "the remove button names the route by data-destination");
        assert_eq!(route("route-learn"), Some(Command::StartRouteLearning { destination: "203.0.113.9".into(), candidates: vec!["c1".into(), "c2".into()] }));
        assert_eq!(page("rate-apply", serde_json::json!({ "pid": 54732, "mb_s": "5" })), Some(Command::ApplyRate { pid: Some(54732), bps: 40_000_000 }));
        assert_eq!(page("rate-learn", serde_json::json!({ "pid": 54732, "destination": "203.0.113.9", "consent": true, "rates": ["2", "5"] })),
            Some(Command::StartRateLearning { pid: 54732, destination: "203.0.113.9".into(), rates: vec![16_000_000, 40_000_000] }));
        assert!(matches!(page("relay-configure", serde_json::json!({ "mode": "hy2", "server": "", "server_name": "", "auth": "", "ca_pem": "" })),
            Some(Command::ConfigureRelay { mode: crate::relay::Mode::Hy2, .. })), "an empty server and secret select the integrated HY2 server");
        assert_eq!(from_json(r#"{"action":"search","text":"aion"}"#), Some(Command::SetSearch("aion".into())));
        assert_eq!(page("udp-redundancy", serde_json::json!({ "enabled": true, "x": 10, "y": 5 })),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: true, x: 10, y: 5 })));
        assert_eq!(page("udp-redundancy-auto", serde_json::json!({ "enabled": true, "echo": "127.0.0.1:9" })),
            Some(Command::SetUdpRedundancyAuto(Some("127.0.0.1:9".parse().unwrap()))));
        assert_eq!(page("udp-redundancy-auto", serde_json::json!({ "enabled": false, "echo": "" })),
            Some(Command::SetUdpRedundancyAuto(None)));
    }

    #[test]
    fn vpn_ipc_is_typed_bounded_and_never_reads_a_path_from_the_page() {
        let page = |action: &str, extra: serde_json::Value| {
            let mut event = serde_json::json!({ "action": action, "dir": null, "checked": null, "pid": null });
            for (key, value) in extra.as_object().unwrap() { event[key] = value.clone(); }
            from_json(&event.to_string())
        };
        let saved = page("vpn-save", serde_json::json!({ "protocol": "hysteria2", "name": "HY2", "fields": { "server": "vps.example.com:443" }, "secrets": { "auth": "x" } }));
        let Some(Command::SaveVpnProfile { protocol, name, fields, secrets }) = saved else { panic!("vpn-save must decode: {saved:?}") };
        assert_eq!((protocol, name.as_str(), fields["server"].as_str(), secrets["auth"].as_str()), (crate::vpn::Protocol::Hysteria2, "HY2", "vps.example.com:443", "x"));
        assert_eq!(page("vpn-import", serde_json::json!({ "protocol": "wireguard", "name": "", "text": "[Interface]" })),
            Some(Command::ImportVpnProfile { protocol: crate::vpn::Protocol::Wireguard, name: String::new(), text: "[Interface]".into() }), "the page sends the file text, never a path");
        assert_eq!(page("vpn-activate", serde_json::json!({ "name": "Casa" })), Some(Command::ActivateVpnProfile(Some("Casa".into()))));
        assert_eq!(page("vpn-deactivate", serde_json::json!({})), Some(Command::ActivateVpnProfile(None)));
        assert_eq!(page("vpn-remove", serde_json::json!({ "name": "Casa" })), Some(Command::RemoveVpnProfile("Casa".into())));
        for (action, extra) in [
            ("vpn-save", serde_json::json!({ "protocol": "ipsec", "name": "x", "fields": {}, "secrets": {} })),
            ("vpn-save", serde_json::json!({ "protocol": "hysteria2", "name": "x", "fields": { "server": 7 }, "secrets": {} })),
            ("vpn-save", serde_json::json!({ "protocol": "hysteria2", "name": "x", "fields": {} })),
            ("vpn-import", serde_json::json!({ "protocol": "hysteria2", "text": "x" })),
            ("vpn-import", serde_json::json!({ "protocol": "wireguard", "text": "a".repeat(crate::vpn::MAX_IMPORT + 1) })),
            ("vpn-activate", serde_json::json!({ "name": "" })),
            ("vpn-remove", serde_json::json!({})),
        ] { assert_eq!(page(action, extra.clone()), None, "{action} {extra}"); }
        let many: serde_json::Map<String, serde_json::Value> = (0..17).map(|i| (format!("k{i}"), "v".into())).collect();
        assert_eq!(page("vpn-save", serde_json::json!({ "protocol": "hysteria2", "name": "x", "fields": many, "secrets": {} })), None, "field count is bounded");
    }

    #[test]
    fn udp_redundancy_ipc_parses_and_validates_bounds() {
        // Valid custom ratio
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy","enabled":true,"x":10,"y":5}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: true, x: 10, y: 5 }))
        );
        // Valid ratio boundary edges (1..=30)
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy","enabled":true,"x":1,"y":1}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: true, x: 1, y: 1 }))
        );
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy","enabled":true,"x":30,"y":30}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: true, x: 30, y: 30 }))
        );
        // Default fallbacks for omitted and null fields
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy"}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: false, x: 20, y: 10 }))
        );
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy","enabled":true}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: true, x: 20, y: 10 }))
        );
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy","enabled":null,"x":null,"y":null}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: false, x: 20, y: 10 }))
        );
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy","enabled":true,"x":15}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: true, x: 15, y: 10 }))
        );
        assert_eq!(
            from_json(r#"{"action":"udp-redundancy","enabled":true,"y":5}"#),
            Some(Command::SetUdpRedundancy(crate::relay::RedundancyConfig { enabled: true, x: 20, y: 5 }))
        );
        // Out-of-bounds ratios rejected
        for body in [
            r#"{"action":"udp-redundancy","enabled":true,"x":0,"y":5}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":5,"y":0}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":31,"y":5}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":5,"y":31}"#,
            r#"{"action":"udp-redundancy","enabled":false,"x":0,"y":10}"#,
        ] {
            assert_eq!(from_json(body), None, "{body}");
        }
        // Malformed / invalid types for ratio and enabled rejected
        for body in [
            r#"{"action":"udp-redundancy","enabled":true,"x":-1,"y":5}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":5,"y":-1}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":4294967296,"y":5}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":"10","y":5}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":10,"y":"5"}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":10.5,"y":5}"#,
            r#"{"action":"udp-redundancy","enabled":true,"x":true,"y":5}"#,
            r#"{"action":"udp-redundancy","enabled":"true","x":10,"y":5}"#,
            r#"{"action":"udp-redundancy","enabled":1,"x":10,"y":5}"#,
        ] {
            assert_eq!(from_json(body), None, "{body}");
        }

        // Auto redundancy valid parsing
        assert_eq!(from_json(r#"{"action":"udp-redundancy-auto","enabled":true,"echo":"127.0.0.1:9"}"#),
            Some(Command::SetUdpRedundancyAuto(Some("127.0.0.1:9".parse().unwrap()))));
        assert_eq!(from_json(r#"{"action":"udp-redundancy-auto","enabled":true,"echo":"[::1]:9"}"#),
            Some(Command::SetUdpRedundancyAuto(Some("[::1]:9".parse().unwrap()))));
        assert_eq!(from_json(r#"{"action":"udp-redundancy-auto","enabled":false}"#), Some(Command::SetUdpRedundancyAuto(None)));
        assert_eq!(from_json(r#"{"action":"udp-redundancy-auto","enabled":false,"echo":"127.0.0.1:9"}"#), Some(Command::SetUdpRedundancyAuto(None)));
        assert_eq!(from_json(r#"{"action":"udp-redundancy-auto","enabled":false,"echo":"qualquer-coisa"}"#), Some(Command::SetUdpRedundancyAuto(None)));
        assert_eq!(from_json(r#"{"action":"udp-redundancy-auto"}"#), Some(Command::SetUdpRedundancyAuto(None)));
        assert_eq!(from_json(r#"{"action":"udp-redundancy-auto","enabled":null}"#), Some(Command::SetUdpRedundancyAuto(None)));

        // Auto redundancy invalid or malformed rejected when enabled
        for body in [
            r#"{"action":"udp-redundancy-auto","enabled":true}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":null}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":""}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":"nao-e-ip"}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":"127.0.0.1"}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":":9"}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":"127.0.0.1:99999"}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":"127.0.0.1:-1"}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":"localhost:9"}"#,
            r#"{"action":"udp-redundancy-auto","enabled":true,"echo":12345}"#,
            r#"{"action":"udp-redundancy-auto","enabled":"true","echo":"127.0.0.1:9"}"#,
            r#"{"action":"udp-redundancy-auto","enabled":1,"echo":"127.0.0.1:9"}"#,
        ] {
            assert_eq!(from_json(body), None, "{body}");
        }
        let oversized = format!(r#"{{"action":"udp-redundancy-auto","enabled":true,"echo":"{}"}}"#, "1".repeat(513));
        assert_eq!(from_json(&oversized), None, "oversized echo rejected");
    }
}
