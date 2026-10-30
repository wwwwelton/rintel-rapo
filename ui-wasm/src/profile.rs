use std::fmt::{Display, Write};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSettings {
    pub priority: bool,
    pub pin: bool,
    pub qos: bool,
    pub threads: u8,
    pub min_percent: u8,
    pub cores: u8,
    pub smt: bool,
    pub p_only: bool,
    pub park_cores: bool,
}

fn field(html: &mut String, label: &str, value: impl Display, relevant: bool) {
    write!(html, "<div class=\"parameter {}\"><dt>{label}</dt><dd>{value}</dd>{}</div>", if relevant { "parameter-on" } else { "parameter-off" }, if relevant { "" } else { "<small>Sem efeito neste perfil</small>" }).unwrap();
}

pub fn render_settings(html: &mut String, settings: &ProfileSettings) {
    let on = |value| if value { "Ativado" } else { "Desativado" };
    html.push_str("<dl class=\"parameters\">");
    field(html, "Prioridade do processo", on(settings.priority), settings.priority);
    field(html, "Afinidade de CPU", on(settings.pin), settings.pin);
    field(html, "Memória / EcoQoS", on(settings.qos), settings.qos);
    field(html, "Threads críticas (limite)", settings.threads, settings.pin);
    if settings.cores == 64 {
        field(html, "Núcleos físicos (limite)", "Todos disponíveis", settings.pin);
    } else {
        field(html, "Núcleos físicos (limite)", settings.cores, settings.pin);
    }
    field(html, "Limiar de CPU (%)", settings.min_percent, settings.pin);
    field(html, "Núcleos elegíveis", if settings.p_only { "Somente P-cores" } else { "Todos os núcleos" }, settings.pin);
    field(html, "Permitir irmãos SMT / HT", on(settings.smt), settings.pin);
    field(html, "Desligar núcleos ociosos (reserva de cache)", on(settings.park_cores), settings.pin);
    html.push_str("</dl>");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_park_cores_parameter_card_active_and_inactive() {
        let mut html_active = String::new();
        let settings_active = ProfileSettings {
            priority: true,
            pin: true,
            qos: false,
            threads: 4,
            min_percent: 5,
            cores: 8,
            smt: true,
            p_only: true,
            park_cores: true,
        };
        render_settings(&mut html_active, &settings_active);
        assert!(html_active.contains("<div class=\"parameter parameter-on\"><dt>Desligar núcleos ociosos (reserva de cache)</dt><dd>Ativado</dd></div>"));

        let mut html_standby = String::new();
        let settings_standby = ProfileSettings {
            pin: false,
            park_cores: false,
            ..settings_active
        };
        render_settings(&mut html_standby, &settings_standby);
        assert!(html_standby.contains("<div class=\"parameter parameter-off\"><dt>Desligar núcleos ociosos (reserva de cache)</dt><dd>Desativado</dd><small>Sem efeito neste perfil</small></div>"));
    }
}
