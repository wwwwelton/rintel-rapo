//! VPN profiles panel (Hysteria 2, WireGuard, OpenVPN). Pure HTML from the host snapshot.
//! The host never sends a secret value, only whether each one is set, so a secret input starts empty and an empty
//! input on save means "keep the stored one". Secret inputs carry no `data-network-draft`, so a typed secret is
//! never restored from the page state after a re-render.

use crate::escape;
use serde::Deserialize;
use std::{collections::BTreeMap, fmt::Write};

#[derive(Default, Deserialize)]
pub struct Vpn {
    pub rows: Vec<Row>,
    pub error: String,
    pub schemas: Vec<Schema>,
}

#[derive(Deserialize)]
pub struct Schema {
    pub id: String,
    pub label: String,
    pub fields: Vec<String>,
    pub secrets: Vec<String>,
    pub importable: bool,
}

#[derive(Deserialize)]
pub struct Row {
    pub name: String,
    pub protocol: String,
    pub fields: BTreeMap<String, String>,
    pub secrets_set: BTreeMap<String, bool>,
    pub active: bool,
}

/// Portuguese label for a field name; unknown names fall back to the name itself.
fn label(name: &str) -> &str {
    match name {
        "server" => "Servidor (host:porta)", "server_name" => "Nome TLS (opcional)", "auth" => "Autenticação", "ca_pem" => "CA PEM (opcional)",
        "address" => "Endereço do túnel", "dns" => "DNS", "mtu" => "MTU", "public_key" => "Chave pública do servidor", "endpoint" => "Endpoint (host:porta)",
        "allowed_ips" => "IPs permitidos", "keepalive" => "Keepalive (s)", "private_key" => "Chave privada", "preshared_key" => "Chave pré-compartilhada",
        "remote" => "Servidor", "port" => "Porta", "proto" => "Protocolo (udp/tcp)", "cipher" => "Cifra", "ca" => "CA (PEM)", "cert" => "Certificado (PEM)",
        "key" => "Chave privada (PEM)", "username" => "Usuário", "password" => "Senha", "tls_auth" => "tls-auth / tls-crypt",
        other => other,
    }
}

/// Multi-line values (PEM blocks) get a textarea; everything else a single-line input.
fn multiline(name: &str) -> bool { matches!(name, "ca_pem" | "ca" | "cert" | "key" | "tls_auth") }

fn input(html: &mut String, kind: &str, name: &str, secret: bool, value: &str, is_set: bool) {
    let id = format!("vpn-{kind}-{name}");
    let placeholder = if secret && is_set { "definido; deixe vazio para manter" } else { "" };
    // Readable fields keep a typed draft across the 1 s re-render; secrets never do.
    let draft = if secret { String::new() } else { format!(" data-network-draft=\"{id}\"") };
    write!(html, "<label for=\"{id}\">{}</label>", escape(label(name))).unwrap();
    if multiline(name) {
        write!(html, "<textarea id=\"{id}\"{draft} maxlength=\"65536\" rows=\"3\" autocomplete=\"off\" spellcheck=\"false\" placeholder=\"{}\">{}</textarea>", escape(placeholder), escape(value)).unwrap();
    } else {
        write!(html, "<input id=\"{id}\"{draft} maxlength=\"4096\" autocomplete=\"off\" spellcheck=\"false\" type=\"{}\" placeholder=\"{}\" value=\"{}\">",
            if secret { "password" } else { "text" }, escape(placeholder), escape(value)).unwrap();
    }
}

pub fn render(html: &mut String, vpn: &Vpn) {
    html.push_str("<section class=\"vpn-profiles\"><h2>PERFIS DE VPN</h2><p class=\"net-card-sub\">Um perfil ativo por vez. Segredos são cifrados com o Windows (DPAPI) e nunca voltam para esta tela.</p>");
    if !vpn.error.is_empty() { write!(html, "<p class=\"alert\" role=\"alert\">{}</p>", escape(&vpn.error)).unwrap(); }
    if vpn.rows.is_empty() { html.push_str("<p class=\"net-card-sub\">Nenhum perfil salvo.</p>"); }
    html.push_str("<ul class=\"vpn-list\">");
    for row in &vpn.rows {
        let proto = vpn.schemas.iter().find(|s| s.id == row.protocol).map_or(row.protocol.as_str(), |s| s.label.as_str());
        let name = escape(&row.name);
        write!(html, "<li><span class=\"vpn-name\">{name}</span><span class=\"badge\">{}</span>{}", escape(proto), if row.active { "<span class=\"badge badge-accent\">ATIVO</span>" } else { "" }).unwrap();
        if row.active {
            write!(html, "<button type=\"button\" data-action=\"vpn-deactivate\">Desativar</button>").unwrap();
        } else {
            write!(html, "<button type=\"button\" data-action=\"vpn-activate\" data-name=\"{name}\">Ativar</button>").unwrap();
        }
        write!(html, "<button type=\"button\" data-action=\"vpn-remove\" data-name=\"{name}\">Remover</button></li>").unwrap();
    }
    html.push_str("</ul><details class=\"how-it-works\"><summary>Adicionar ou editar perfil</summary>");
    html.push_str("<fieldset id=\"vpn-editor\"><legend>Novo perfil</legend><label for=\"vpn-protocol\">Protocolo</label><select id=\"vpn-protocol\" data-network-draft=\"vpn-protocol\">");
    for schema in &vpn.schemas { write!(html, "<option value=\"{id}\">{}</option>", escape(&schema.label), id = escape(&schema.id)).unwrap(); }
    html.push_str("</select><label for=\"vpn-name\">Nome do perfil</label><input id=\"vpn-name\" data-network-draft=\"vpn-name\" maxlength=\"80\" autocomplete=\"off\" type=\"text\">");
    // One block per protocol; the page shows the one the select names. Editing an existing profile fills its fields.
    for schema in &vpn.schemas {
        let current = vpn.rows.iter().find(|r| r.protocol == schema.id && r.active);
        write!(html, "<div class=\"vpn-form\" data-vpn-form=\"{id}\" data-fields=\"{fields}\" data-secrets=\"{secrets}\" hidden>", id = escape(&schema.id),
            fields = escape(&schema.fields.join(",")), secrets = escape(&schema.secrets.join(","))).unwrap();
        for field in &schema.fields { input(html, &format!("{}-f", schema.id), field, false, current.and_then(|r| r.fields.get(field)).map_or("", String::as_str), false); }
        for secret in &schema.secrets { input(html, &format!("{}-s", schema.id), secret, true, "", current.and_then(|r| r.secrets_set.get(secret)).copied().unwrap_or(false)); }
        if schema.importable {
            write!(html, "<label for=\"vpn-file-{id}\">Importar arquivo ({})</label><input id=\"vpn-file-{id}\" type=\"file\" data-vpn-file=\"{id}\" accept=\"{}\">",
                if schema.id == "wireguard" { ".conf" } else { ".ovpn" }, if schema.id == "wireguard" { ".conf,text/plain" } else { ".ovpn,text/plain" }, id = escape(&schema.id)).unwrap();
        }
        html.push_str("</div>");
    }
    html.push_str("<button type=\"button\" data-action=\"vpn-save\">Salvar perfil</button></fieldset></details></section>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schemas() -> Vec<Schema> {
        vec![
            Schema { id: "hysteria2".into(), label: "Hysteria 2".into(), fields: vec!["server".into(), "ca_pem".into()], secrets: vec!["auth".into()], importable: false },
            Schema { id: "wireguard".into(), label: "WireGuard".into(), fields: vec!["endpoint".into()], secrets: vec!["private_key".into()], importable: true },
        ]
    }
    fn row(name: &str, protocol: &str, active: bool) -> Row {
        Row { name: name.into(), protocol: protocol.into(), fields: BTreeMap::from([("server".into(), "vps.example.com:443".into())]), secrets_set: BTreeMap::from([("auth".into(), true)]), active }
    }
    fn page(vpn: &Vpn) -> String { let mut html = String::new(); render(&mut html, vpn); html }

    #[test]
    fn only_the_active_profile_offers_deactivate_and_the_others_offer_activate() {
        let html = page(&Vpn { rows: vec![row("A", "hysteria2", true), row("B", "wireguard", false)], error: String::new(), schemas: schemas() });
        assert_eq!(html.matches("data-action=\"vpn-deactivate\"").count(), 1);
        assert_eq!(html.matches("data-action=\"vpn-activate\" data-name=\"B\"").count(), 1);
        assert_eq!(html.matches("data-action=\"vpn-remove\"").count(), 2);
        assert_eq!(html.matches("ATIVO").count(), 1, "exactly one profile is marked active");
    }

    #[test]
    fn untrusted_names_and_errors_are_escaped_and_a_secret_never_becomes_a_value() {
        let mut bad = row("<img src=x onerror=alert(1)>", "hysteria2", false);
        bad.fields.insert("server".into(), "\"><script>x</script>".into());
        let html = page(&Vpn { rows: vec![bad], error: "<b>falha</b>".into(), schemas: schemas() });
        assert!(!html.contains("<img") && !html.contains("<script>") && !html.contains("<b>falha"), "{html}");
        assert!(html.contains("&lt;img") && html.contains("&lt;b&gt;falha"));
    }

    #[test]
    fn secret_inputs_start_empty_say_when_set_and_never_keep_a_page_draft() {
        let html = page(&Vpn { rows: vec![row("A", "hysteria2", true)], error: String::new(), schemas: schemas() });
        let secret = &html[html.find("id=\"vpn-hysteria2-s-auth\"").unwrap()..];
        let secret = &secret[..secret.find('>').unwrap()];
        assert!(secret.contains("type=\"password\"") && secret.contains("value=\"\"") && secret.contains("deixe vazio para manter"), "{secret}");
        assert!(!secret.contains("data-network-draft"), "a typed secret must not be restored after a re-render");
        let readable = &html[html.find("id=\"vpn-hysteria2-f-server\"").unwrap()..];
        assert!(readable[..readable.find('>').unwrap()].contains("data-network-draft"), "a readable field keeps its draft");
    }

    #[test]
    fn every_protocol_gets_a_hidden_form_listing_its_fields_and_only_file_formats_offer_import() {
        let html = page(&Vpn { rows: Vec::new(), error: String::new(), schemas: schemas() });
        assert!(html.contains("Nenhum perfil salvo"));
        assert_eq!(html.matches("data-vpn-form=").count(), 2);
        assert!(html.contains("data-fields=\"server,ca_pem\"") && html.contains("data-secrets=\"private_key\""));
        assert!(html.contains("data-vpn-file=\"wireguard\"") && !html.contains("data-vpn-file=\"hysteria2\""), "Hysteria 2 has no file format");
        assert!(html.contains("<textarea id=\"vpn-hysteria2-f-ca_pem\"") , "PEM fields use a textarea");
    }
}
