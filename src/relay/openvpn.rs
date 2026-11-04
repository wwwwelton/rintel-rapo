//! The OpenVPN tunnel selected for one relay run. It is built from the active `vpn::Profile`, serialised to the
//! JSON `native/hy2/openvpn.go` parses, and handed to the embedded library. Secrets (private key, passwords,
//! static keys) live in this value for the length of the run: `Debug` and `Display` never print them, and they are
//! never written to disk or logs.

use crate::vpn::{Profile, Protocol};
use std::fmt;

pub struct Tunnel {
    json: Vec<u8>,
    /// `host:port` of the server, for the ICMP ping shown in the GUI. Not secret.
    pub endpoint: String,
}

impl fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tunnel").field("endpoint", &self.endpoint).field("json", &"<redacted>").finish()
    }
}

impl Tunnel {
    /// Validates the profile again (it may come from an older database) and builds the library JSON.
    pub fn from_profile(profile: &Profile) -> Result<Self, String> {
        if profile.protocol != Protocol::Openvpn { return Err("O perfil ativo não é OpenVPN".into()); }
        profile.validate()?;
        let mut object = serde_json::Map::new();
        for (key, value) in profile.fields.iter().chain(profile.secrets.iter()).filter(|(_, v)| !v.is_empty()) {
            object.insert(key.clone(), value.clone().into());
        }
        let json = serde_json::to_vec(&object).map_err(|e| e.to_string())?;
        let remote = profile.fields.get("remote").cloned().unwrap_or_default();
        let port = profile.fields.get("port").cloned().unwrap_or_else(|| "1194".into());
        let endpoint = format!("{remote}:{port}");
        Ok(Self { json, endpoint })
    }
    pub fn json(&self) -> &[u8] { &self.json }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CA: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    const KEY: &str = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----";

    fn profile() -> Profile {
        let mut p = Profile::new(Protocol::Openvpn, "Casa");
        for (k, v) in [("remote", "vpn.example.com"), ("port", "1194"), ("proto", "udp"), ("ca", CA)] { p.fields.insert(k.into(), v.into()); }
        p.secrets.insert("key".into(), KEY.into());
        p
    }

    #[test]
    fn the_library_json_carries_every_value_and_skips_empty_ones() {
        let tunnel = Tunnel::from_profile(&profile()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(tunnel.json()).unwrap();
        assert_eq!(json["key"], KEY, "the Go side needs the private key to authenticate the TLS session");
        assert_eq!(json["remote"], "vpn.example.com");
        assert_eq!(json["port"], "1194");
        assert_eq!(tunnel.endpoint, "vpn.example.com:1194");
    }

    #[test]
    fn the_private_key_never_appears_in_debug_output() {
        let tunnel = Tunnel::from_profile(&profile()).unwrap();
        let shown = format!("{tunnel:?} {:#?}", tunnel);
        assert!(!shown.contains(KEY) && !shown.contains("PRIVATE KEY"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn only_a_valid_openvpn_profile_becomes_a_tunnel() {
        let mut wg = Profile::new(Protocol::Wireguard, "WG");
        wg.fields.insert("endpoint".into(), "1.2.3.4:51820".into());
        assert!(Tunnel::from_profile(&wg).unwrap_err().contains("não é OpenVPN"));
        let mut broken = profile();
        broken.fields.insert("ca".into(), "not-a-certificate".into());
        assert!(Tunnel::from_profile(&broken).is_err(), "a stored profile is validated again before it is used");
    }
}
