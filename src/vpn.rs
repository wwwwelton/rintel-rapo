//! VPN profiles for the local relay: Hysteria 2, WireGuard and OpenVPN. Exactly one profile can be active at a time.
//! A profile keeps its non-secret `fields` readable (the UI lists them) and its `secrets` apart, so a secret is
//! sealed by `secret` before storage and never reaches `Debug`, logs or the GUI.
//! Imported files are untrusted input: size and character limits apply, and OpenVPN directives that run programs
//! are refused, so importing a `.ovpn` can never execute code.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Largest file accepted by an importer; real WireGuard/OpenVPN profiles are a few KiB.
pub const MAX_IMPORT: usize = 64 * 1024;
const MAX_FIELD: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol { Hysteria2, Wireguard, Openvpn }

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::Hysteria2, Protocol::Wireguard, Protocol::Openvpn];
    pub fn id(self) -> &'static str { match self { Self::Hysteria2 => "hysteria2", Self::Wireguard => "wireguard", Self::Openvpn => "openvpn" } }
    pub fn parse(id: &str) -> Option<Self> { Self::ALL.into_iter().find(|p| p.id() == id) }
    pub fn label(self) -> &'static str { match self { Self::Hysteria2 => "Hysteria 2", Self::Wireguard => "WireGuard", Self::Openvpn => "OpenVPN" } }
    /// Field names the editor offers by default, in display order. Names listed in `secret_fields` are sealed.
    pub fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Hysteria2 => &["server", "server_name", "ca_pem"],
            Self::Wireguard => &["address", "dns", "mtu", "public_key", "endpoint", "allowed_ips", "keepalive"],
            Self::Openvpn => &["remote", "port", "proto", "cipher", "auth", "ca", "cert", "key_direction"],
        }
    }
    pub fn secret_fields(self) -> &'static [&'static str] {
        match self {
            Self::Hysteria2 => &["auth"],
            Self::Wireguard => &["private_key", "preshared_key"],
            Self::Openvpn => &["key", "username", "password", "tls_auth", "tls_crypt", "tls_crypt_v2"],
        }
    }
    fn required(self) -> &'static [&'static str] {
        match self { Self::Hysteria2 => &["server"], Self::Wireguard => &["address", "public_key", "endpoint", "private_key"], Self::Openvpn => &["remote", "ca"] }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub protocol: Protocol,
    pub fields: BTreeMap<String, String>,
    pub secrets: BTreeMap<String, String>,
}

impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile").field("name", &self.name).field("protocol", &self.protocol).field("fields", &self.fields)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>()).finish()
    }
}

fn clean(value: &str) -> bool { !value.contains(['\0', '\r']) && value.len() <= MAX_FIELD }
fn host_port(value: &str) -> bool {
    let Some((host, port)) = value.rsplit_once(':') else { return false };
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    !host.is_empty() && host.len() <= 253 && !host.contains(['\0', '\r', '\n', ' ', '/', '\\']) && port.parse::<u16>().is_ok_and(|p| p != 0)
}
/// WireGuard keys are 32 bytes in base64: 44 characters ending in `=`.
fn wg_key(value: &str) -> bool { value.len() == 44 && value.ends_with('=') && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')) }

impl Profile {
    pub fn new(protocol: Protocol, name: &str) -> Self {
        Self { name: name.trim().to_owned(), protocol, fields: BTreeMap::new(), secrets: BTreeMap::new() }
    }
    /// Checks a profile before it is stored or activated; the message names what to fix.
    pub fn validate(&self) -> Result<(), String> {
        let bad = |message: &str| Err(message.to_owned());
        if self.name.is_empty() || self.name.chars().count() > 80 || self.name.contains(['\0', '\r', '\n']) { return bad("Nome do perfil ausente ou inválido (até 80 caracteres)"); }
        let (allowed, secret) = (self.protocol.fields(), self.protocol.secret_fields());
        if let Some(key) = self.fields.keys().find(|k| !allowed.contains(&k.as_str())) { return Err(format!("Campo desconhecido: {key}")); }
        if let Some(key) = self.secrets.keys().find(|k| !secret.contains(&k.as_str())) { return Err(format!("Segredo desconhecido: {key}")); }
        if self.fields.values().chain(self.secrets.values()).any(|v| !clean(v)) { return bad("Valor inválido: caracteres de controle ou texto longo demais"); }
        let value = |name: &str| self.fields.get(name).or_else(|| self.secrets.get(name)).map_or("", String::as_str);
        for name in self.protocol.required() { if value(name).trim().is_empty() { return Err(format!("Campo obrigatório ausente: {name}")); } }
        match self.protocol {
            Protocol::Hysteria2 => {
                if !host_port(value("server")) { return bad("Servidor HY2 deve ser host:porta"); }
                if value("auth").is_empty() { return bad("Autenticação HY2 ausente"); }
                let ca = value("ca_pem");
                if !ca.is_empty() && !ca.contains("-----BEGIN CERTIFICATE-----") { return bad("CA PEM HY2 inválida"); }
            }
            Protocol::Wireguard => {
                if !wg_key(value("private_key")) || !wg_key(value("public_key")) { return bad("Chave WireGuard inválida (32 bytes em base64)"); }
                if !value("preshared_key").is_empty() && !wg_key(value("preshared_key")) { return bad("Chave pré-compartilhada WireGuard inválida"); }
                if !host_port(value("endpoint")) { return bad("Endpoint WireGuard deve ser host:porta"); }
                if let Some(mtu) = self.fields.get("mtu").filter(|m| !m.is_empty()) { if !mtu.parse::<u16>().is_ok_and(|m| (576..=9000).contains(&m)) { return bad("MTU WireGuard deve ficar entre 576 e 9000"); } }
                if let Some(keep) = self.fields.get("keepalive").filter(|k| !k.is_empty()) { if keep.parse::<u16>().is_err() { return bad("Keepalive WireGuard deve ser um número de segundos"); } }
            }
            Protocol::Openvpn => {
                if value("remote").contains([' ', '/', '\\']) { return bad("Remote OpenVPN deve ser um host"); }
                if let Some(port) = self.fields.get("port").filter(|p| !p.is_empty()) { if !port.parse::<u16>().is_ok_and(|p| p != 0) { return bad("Porta OpenVPN inválida"); } }
                if let Some(proto) = self.fields.get("proto").filter(|p| !p.is_empty()) { if !matches!(proto.as_str(), "udp" | "tcp" | "udp4" | "tcp4" | "udp6" | "tcp6") { return bad("Protocolo OpenVPN deve ser udp ou tcp"); } }
                if !value("ca").contains("-----BEGIN CERTIFICATE-----") { return bad("CA OpenVPN deve ser um certificado PEM"); }
                let static_keys = ["tls_auth", "tls_crypt", "tls_crypt_v2"].iter().filter(|k| !value(k).is_empty()).count();
                if static_keys > 1 { return bad("O perfil OpenVPN não pode ter mais de uma chave de controle (tls-auth, tls-crypt ou tls-crypt-v2)"); }
            }
        }
        Ok(())
    }
}

fn check_size(text: &str) -> Result<(), String> {
    if text.len() > MAX_IMPORT { return Err("Arquivo grande demais para um perfil de VPN".into()); }
    if text.contains('\0') { return Err("Arquivo inválido: contém bytes nulos".into()); }
    Ok(())
}

fn derive_name(given: &str, fallback: &str) -> String { if given.trim().is_empty() { fallback.to_owned() } else { given.trim().to_owned() } }

/// Imports a `wg-quick` file. Only the keys a userspace tunnel can honor are read; `PostUp`, `PreDown`, `Table` and
/// the like run programs or edit routes on the host, so they are refused instead of being silently ignored.
pub fn import_wireguard(name: &str, text: &str) -> Result<Profile, String> {
    check_size(text)?;
    let mut profile = Profile::new(Protocol::Wireguard, &derive_name(name, "WireGuard"));
    let (mut section, mut peers) = (String::new(), 0);
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() { continue; }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = header.trim().to_ascii_lowercase();
            if section == "peer" { peers += 1; if peers > 1 { return Err("Importe um perfil por vez: o arquivo tem mais de um [Peer]".into()); } }
            if section != "interface" && section != "peer" { return Err(format!("Seção desconhecida: [{header}]")); }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { return Err(format!("Linha inválida: {line}")) };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim().to_owned());
        let target = match (section.as_str(), key.as_str()) {
            ("interface", "privatekey") => ("private_key", true), ("interface", "address") => ("address", false),
            ("interface", "dns") => ("dns", false), ("interface", "mtu") => ("mtu", false),
            ("peer", "publickey") => ("public_key", false), ("peer", "presharedkey") => ("preshared_key", true),
            ("peer", "endpoint") => ("endpoint", false), ("peer", "allowedips") => ("allowed_ips", false),
            ("peer", "persistentkeepalive") => ("keepalive", false),
            ("interface", "listenport" | "fwmark") => continue,
            (_, "postup" | "postdown" | "preup" | "predown" | "table" | "saveconfig") => return Err(format!("Diretiva recusada (executa comandos ou altera rotas do host): {key}")),
            _ => return Err(format!("Chave não suportada: {key}")),
        };
        let map = if target.1 { &mut profile.secrets } else { &mut profile.fields };
        if map.insert(target.0.to_owned(), value).is_some() { return Err(format!("Chave repetida: {key}")); }
    }
    profile.validate()?;
    Ok(profile)
}

/// Directives that execute programs, load plugins or rewrite host configuration: never imported.
const OVPN_REFUSED: [&str; 17] = ["up", "down", "route-up", "route-pre-down", "ipchange", "client-connect", "client-disconnect", "learn-address",
    "auth-user-pass-verify", "tls-verify", "plugin", "script-security", "setenv", "setenv-safe", "management", "cryptoapicert", "dhcp-option-exec"];

/// What the relay must do for the active profile. Pure, so the choice is testable without a relay or Windows.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// No active profile: the relay keeps its built-in behaviour.
    Default,
    /// Run the HY2 path against this server (the profile's secret is in `Profile::secrets`).
    Hysteria2,
    /// Run the embedded userspace WireGuard tunnel.
    Wireguard,
    /// Run the embedded userspace OpenVPN tunnel.
    Openvpn,
    Refused(&'static str),
}

pub fn plan(active: Option<&Profile>) -> Plan {
    match active.map(|p| p.protocol) {
        None => Plan::Default,
        Some(Protocol::Hysteria2) => Plan::Hysteria2,
        Some(Protocol::Wireguard) => Plan::Wireguard,
        Some(Protocol::Openvpn) => Plan::Openvpn,
    }
}


/// What the editor shows for one protocol: its id, label and the readable and secret field names, in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Schema {
    pub id: &'static str,
    pub label: &'static str,
    pub fields: &'static [&'static str],
    pub secrets: &'static [&'static str],
    /// Whether a file can be imported (`.conf` / `.ovpn`).
    pub importable: bool,
}

pub fn schemas() -> Vec<Schema> {
    Protocol::ALL.into_iter().map(|p| Schema { id: p.id(), label: p.label(), fields: p.fields(), secrets: p.secret_fields(), importable: p != Protocol::Hysteria2 }).collect()
}

/// One row for the UI: readable fields, and for each secret only *whether it is set*. The value never leaves the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub name: String,
    pub protocol: Protocol,
    pub fields: BTreeMap<String, String>,
    pub secrets_set: BTreeMap<String, bool>,
    pub active: bool,
}

pub fn rows(profiles: &[Profile], active: Option<&str>) -> Vec<Row> {
    profiles.iter().map(|p| Row {
        name: p.name.clone(), protocol: p.protocol, fields: p.fields.clone(), active: active == Some(p.name.as_str()),
        secrets_set: p.protocol.secret_fields().iter().map(|k| ((*k).to_owned(), p.secrets.get(*k).is_some_and(|v| !v.is_empty()))).collect(),
    }).collect()
}

/// Builds the profile for an import command. Hysteria 2 has no file format, so it is not importable.
pub fn import(protocol: Protocol, name: &str, text: &str) -> Result<Profile, String> {
    match protocol {
        Protocol::Wireguard => import_wireguard(name, text),
        Protocol::Openvpn => import_openvpn(name, text),
        Protocol::Hysteria2 => Err("Hysteria 2 não tem arquivo de perfil; preencha os campos".into()),
    }
}

/// Imports a `.ovpn` file with inline `<ca>`, `<cert>` and `<key>` blocks. External file references (`ca ca.crt`) are
/// refused: the profile must be self-contained, otherwise it would read arbitrary files from the importing machine.
pub fn import_openvpn(name: &str, text: &str) -> Result<Profile, String> {
    check_size(text)?;
    let mut profile = Profile::new(Protocol::Openvpn, &derive_name(name, "OpenVPN"));
    let mut open = None::<(String, Vec<&str>)>;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some((tag, body)) = &mut open {
            if line == format!("</{tag}>") {
                let (tag, body) = open.take().expect("checked just above");
                let (field, secret) = match tag.as_str() { "ca" => ("ca", false), "cert" => ("cert", false), "key" => ("key", true), "tls-auth" => ("tls_auth", true), "tls-crypt" => ("tls_crypt", true), "tls-crypt-v2" => ("tls_crypt_v2", true), _ => continue };
                let map = if secret { &mut profile.secrets } else { &mut profile.fields };
                // Two top-level blocks for one field would make "which one is trusted" ambiguous: refuse.
                if map.insert(field.to_owned(), body.join("\n")).is_some() { return Err(format!("Bloco <{tag}> repetido")); }
            } else { body.push(line); }
            continue;
        }
        if let Some(tag) = line.strip_prefix('<').and_then(|l| l.strip_suffix('>')).filter(|t| !t.starts_with('/')) { open = Some((tag.to_owned(), Vec::new())); continue; }
        let line = line.split(['#', ';']).next().unwrap_or("").trim();
        if line.is_empty() { continue; }
        let mut words = line.split_whitespace();
        let directive = words.next().unwrap_or("").to_ascii_lowercase();
        let args: Vec<&str> = words.collect();
        if OVPN_REFUSED.contains(&directive.as_str()) { return Err(format!("Diretiva recusada (executa programas ou altera o host): {directive}")); }
        match directive.as_str() {
            "client" | "dev" | "nobind" | "persist-key" | "persist-tun" | "resolv-retry" | "verb" | "mute" | "remote-cert-tls" | "float" | "tun-mtu" | "mssfix" | "comp-lzo" | "compress" | "auth-nocache" | "pull" | "tls-client" | "tls-version-min" | "reneg-sec" | "keepalive" | "ping" | "ping-restart" | "explicit-exit-notify" | "redirect-gateway" | "block-outside-dns" | "route" | "route-nopull" | "dev-type" | "setenv-safe-ok" | "data-ciphers" | "data-ciphers-fallback" | "ncp-ciphers" | "tls-cipher" | "tls-ciphersuites" | "ignore-unknown-option" | "lport" | "rport" | "nice" | "connect-retry" | "connect-timeout" => {}
            "key-direction" => { if let Some(dir) = args.first() { profile.fields.insert("key_direction".into(), (*dir).to_owned()); } }
            "remote" => {
                if profile.fields.contains_key("remote") { continue; } // the first server; failover entries are not used
                let host = args.first().ok_or("remote sem servidor")?;
                profile.fields.insert("remote".into(), (*host).to_owned());
                if let Some(port) = args.get(1) { profile.fields.insert("port".into(), (*port).to_owned()); }
                if let Some(proto) = args.get(2) { profile.fields.insert("proto".into(), (*proto).to_owned()); }
            }
            "proto" => { profile.fields.insert("proto".into(), args.first().unwrap_or(&"").to_string()); }
            "port" => { profile.fields.insert("port".into(), args.first().unwrap_or(&"").to_string()); }
            "cipher" => { profile.fields.insert("cipher".into(), args.first().unwrap_or(&"").to_string()); }
            "auth" => { profile.fields.insert("auth".into(), args.first().unwrap_or(&"").to_string()); }
            "auth-user-pass" => { if !args.is_empty() { return Err("Arquivo externo não aceito (auth-user-pass): informe usuário e senha no perfil".into()); } } // bare form: the user types the credentials in the editor
            "ca" | "cert" | "key" | "tls-auth" | "tls-crypt" | "pkcs12" | "secret" | "crl-verify" | "dh" => return Err(format!("Arquivo externo não aceito ({directive}); use um perfil com blocos embutidos <ca>, <cert> e <key>")),
            other => return Err(format!("Diretiva não suportada: {other}")),
        }
    }
    if let Some((tag, _)) = open { return Err(format!("Bloco <{tag}> sem fechamento")); }
    profile.validate()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "YAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=";
    const KEY_B: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";
    const WG: &str = "[Interface]\nPrivateKey = YAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=\nAddress = 10.0.0.2/32\nDNS = 1.1.1.1\nMTU = 1380\n\n[Peer]\nPublicKey = xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=\nEndpoint = vpn.example.com:51820\nAllowedIPs = 0.0.0.0/0, ::/0\nPersistentKeepalive = 25 # keep NAT open\n";
    const CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    fn ovpn(extra: &str) -> String {
        format!("client\ndev tun\nproto udp\nremote vpn.example.com 1194\ncipher AES-256-GCM\nauth SHA256\nverb 3\n{extra}\n<ca>\n{CERT}\n</ca>\n<key>\n-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n</key>\n")
    }

    #[test]
    fn wireguard_file_splits_secrets_from_readable_fields() {
        let profile = import_wireguard("", WG).unwrap();
        assert_eq!(profile.name, "WireGuard");
        assert_eq!(profile.fields["endpoint"], "vpn.example.com:51820");
        assert_eq!(profile.fields["keepalive"], "25", "a trailing comment is not part of the value");
        assert_eq!(profile.secrets["private_key"], KEY_A);
        assert!(!profile.fields.values().any(|v| v == KEY_A), "the private key is never a readable field");
        assert!(!format!("{profile:?}").contains(KEY_A), "Debug must never print a secret");
        assert_eq!(profile.fields["public_key"], KEY_B);
    }

    #[test]
    fn wireguard_import_refuses_what_runs_programs_and_what_it_cannot_honor() {
        for line in ["PostUp = powershell -c calc", "PostDown = x", "PreUp = x", "PreDown = x", "Table = off", "SaveConfig = true"] {
            let error = import_wireguard("x", &WG.replace("MTU = 1380", line)).unwrap_err();
            assert!(error.contains("recusada"), "{line}: {error}");
        }
        assert!(import_wireguard("x", &WG.replace("MTU = 1380", "Bogus = 1")).unwrap_err().contains("não suportada"));
        assert!(import_wireguard("x", &format!("{WG}\n[Peer]\nPublicKey = {KEY_B}\nEndpoint = a:1\n")).unwrap_err().contains("mais de um"));
        assert!(import_wireguard("x", &WG.replace(KEY_A, "short")).is_err());
        assert!(import_wireguard("x", &WG.replace("51820", "0")).is_err());
        assert!(import_wireguard("x", &"a".repeat(MAX_IMPORT + 1)).unwrap_err().contains("grande demais"));
        assert!(import_wireguard("x", "[Interface]\nPrivateKey = a\0b").is_err());
    }

    #[test]
    fn openvpn_file_keeps_inline_blocks_and_seals_the_key() {
        let profile = import_openvpn("Casa", &ovpn("auth-user-pass")).unwrap();
        assert_eq!((profile.name.as_str(), profile.fields["remote"].as_str(), profile.fields["port"].as_str(), profile.fields["proto"].as_str()), ("Casa", "vpn.example.com", "1194", "udp"));
        assert_eq!(profile.fields["cipher"], "AES-256-GCM");
        assert!(profile.fields["ca"].contains("BEGIN CERTIFICATE"));
        assert!(profile.secrets["key"].contains("PRIVATE KEY"), "the private key is a secret, not a readable field");
        assert!(!profile.fields.contains_key("key"));
    }

    #[test]
    fn openvpn_import_refuses_every_directive_that_executes_code_or_reads_files() {
        for line in ["up /tmp/x.sh", "down x", "route-up x", "script-security 2", "plugin libx.so", "client-connect x", "tls-verify x", "auth-user-pass-verify x via-env", "setenv A B", "management 127.0.0.1 7505"] {
            let error = import_openvpn("x", &ovpn(line)).unwrap_err();
            assert!(error.contains("recusada"), "{line}: {error}");
        }
        for line in ["ca /etc/passwd", "cert user.crt", "key user.key", "tls-auth ta.key 1", "pkcs12 a.p12", "auth-user-pass creds.txt"] {
            assert!(import_openvpn("x", &ovpn(line)).unwrap_err().contains("não aceit"), "{line}");
        }
        assert!(import_openvpn("x", &ovpn("frobnicate 1")).unwrap_err().contains("não suportada"));
        assert!(import_openvpn("x", "client\nremote a 1\n<ca>\nx").unwrap_err().contains("sem fechamento"));
        assert!(import_openvpn("x", &ovpn("")).is_ok(), "the clean profile imports");
    }

    #[test]
    fn openvpn_directive_inside_an_inline_block_is_data_not_a_directive() {
        let text = ovpn("").replace("AAAA", "up /bin/sh\nscript-security 3");
        let profile = import_openvpn("x", &text).unwrap();
        assert!(profile.secrets["key"].contains("script-security 3"), "block contents are carried verbatim, never interpreted");
    }

    #[test]
    fn a_block_tag_inside_another_block_cannot_replace_the_real_ca() {
        let forged = "-----BEGIN CERTIFICATE-----\nATTACKER\n-----END CERTIFICATE-----";
        let text = format!("client\nremote vpn.example.com 1194\n<key>\n<ca>\n{forged}\n</ca>\n</key>\n<ca>\n{CERT}\n</ca>\n");
        let profile = import_openvpn("x", &text).unwrap();
        assert_eq!(profile.fields["ca"], CERT, "only a top-level <ca> block is the CA");
        assert!(profile.secrets["key"].contains("ATTACKER"), "the nested text stays inside the key body");
    }

    #[test]
    fn two_top_level_blocks_for_the_same_field_are_refused_as_ambiguous() {
        let text = format!("client\nremote vpn.example.com 1194\n<ca>\n{CERT}\n</ca>\n<ca>\n{CERT}\n</ca>\n");
        assert!(import_openvpn("x", &text).unwrap_err().contains("repetido"));
    }

    #[test]
    fn validation_names_the_field_to_fix_per_protocol() {
        let mut hy2 = Profile::new(Protocol::Hysteria2, "HY2");
        assert!(hy2.validate().unwrap_err().contains("server"));
        hy2.fields.insert("server".into(), "vps.example.com:443".into());
        assert!(hy2.validate().unwrap_err().contains("Autenticação"));
        hy2.secrets.insert("auth".into(), "s3cret".into());
        hy2.validate().unwrap();
        hy2.fields.insert("server".into(), "no-port".into());
        assert!(hy2.validate().is_err());
        hy2.fields.insert("server".into(), "vps.example.com:443".into());
        hy2.fields.insert("bogus".into(), "x".into());
        assert!(hy2.validate().unwrap_err().contains("desconhecido"));
        hy2.fields.remove("bogus");
        hy2.secrets.insert("private_key".into(), "x".into());
        assert!(hy2.validate().unwrap_err().contains("Segredo desconhecido"), "a secret of another protocol is rejected");
        assert!(Profile::new(Protocol::Hysteria2, "  ").validate().unwrap_err().contains("Nome"));
        assert!(Profile::new(Protocol::Hysteria2, &"n".repeat(81)).validate().is_err());
    }

    #[test]
    fn protocols_round_trip_through_their_id_and_every_secret_is_a_known_field() {
        for protocol in Protocol::ALL {
            assert_eq!(Protocol::parse(protocol.id()), Some(protocol));
            assert!(protocol.fields().iter().all(|f| !protocol.secret_fields().contains(f)), "{protocol:?}: a field is either readable or secret");
        }
        assert_eq!(Protocol::parse("ipsec"), None);
    }

    #[test]
    fn ui_rows_never_carry_a_secret_value_only_whether_it_is_set() {
        let profile = import_wireguard("Casa", WG).unwrap();
        let rows = rows(&[profile.clone(), Profile::new(Protocol::Hysteria2, "vazio")], Some("Casa"));
        let json = serde_json::to_string(&rows).unwrap();
        assert!(!json.contains(KEY_A), "the private key must not reach the page: {json}");
        assert!(rows[0].active && !rows[1].active);
        assert!(rows[0].secrets_set["private_key"]);
        assert!(!rows[0].secrets_set["preshared_key"]);
        assert!(!rows[1].secrets_set["auth"]);
    }

    #[test]
    fn import_dispatches_by_protocol_and_hysteria2_has_no_file_format() {
        assert_eq!(import(Protocol::Wireguard, "", WG).unwrap().protocol, Protocol::Wireguard);
        assert_eq!(import(Protocol::Openvpn, "", &ovpn("")).unwrap().protocol, Protocol::Openvpn);
        assert!(import(Protocol::Hysteria2, "", WG).is_err());
        assert!(import(Protocol::Openvpn, "", WG).is_err(), "a WireGuard file is not an OpenVPN profile");
        assert!(import(Protocol::Wireguard, "", &ovpn("")).is_err(), "an OpenVPN file is not a WireGuard profile");
    }

    #[test]
    fn the_active_profile_decides_the_exit_and_an_unsupported_one_is_refused_not_replaced() {
        assert_eq!(plan(None), Plan::Default);
        assert_eq!(plan(Some(&import_wireguard("x", WG).unwrap())), Plan::Wireguard);
        assert_eq!(plan(Some(&Profile::new(Protocol::Hysteria2, "h"))), Plan::Hysteria2);
        assert_eq!(plan(Some(&import_openvpn("x", &ovpn("")).unwrap())), Plan::Openvpn);
    }
}
