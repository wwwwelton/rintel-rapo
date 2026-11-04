//! The WireGuard tunnel selected for one relay run. It is built from the active `vpn::Profile`, serialised to the
//! JSON `native/hy2/wireguard.go` parses, and then only handed to the embedded library. The private key lives in this
//! value for the length of the run: `Debug` and `Display` never print it, and it is never written to disk or logs.

use crate::vpn::{Profile, Protocol};
use std::fmt;

pub struct Tunnel {
    json: Vec<u8>,
    /// `host:port` of the peer, for the ICMP ping shown in the GUI. Not secret.
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
        if profile.protocol != Protocol::Wireguard { return Err("O perfil ativo não é WireGuard".into()); }
        profile.validate()?;
        let mut object = serde_json::Map::new();
        for (key, value) in profile.fields.iter().chain(profile.secrets.iter()).filter(|(_, v)| !v.is_empty()) {
            object.insert(key.clone(), value.clone().into());
        }
        let json = serde_json::to_vec(&object).map_err(|e| e.to_string())?;
        let endpoint = profile.fields.get("endpoint").cloned().unwrap_or_default();
        Ok(Self { json, endpoint })
    }
    pub fn json(&self) -> &[u8] { &self.json }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "YAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=";
    const KEY_B: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";

    fn profile() -> Profile {
        let mut p = Profile::new(Protocol::Wireguard, "Casa");
        for (k, v) in [("address", "10.0.0.2/32"), ("public_key", KEY_B), ("endpoint", "vpn.example.com:51820"), ("dns", ""), ("mtu", "1380")] { p.fields.insert(k.into(), v.into()); }
        p.secrets.insert("private_key".into(), KEY_A.into());
        p
    }

    #[test]
    fn the_library_json_carries_every_value_and_skips_empty_ones() {
        let tunnel = Tunnel::from_profile(&profile()).unwrap();
        let json: serde_json::Value = serde_json::from_slice(tunnel.json()).unwrap();
        assert_eq!(json["private_key"], KEY_A, "the Go side needs the key to build the tunnel");
        assert_eq!((json["endpoint"].as_str(), json["mtu"].as_str(), json["address"].as_str()), (Some("vpn.example.com:51820"), Some("1380"), Some("10.0.0.2/32")));
        assert!(json.get("dns").is_none(), "an empty optional field is omitted, not sent as an empty string");
        assert_eq!(tunnel.endpoint, "vpn.example.com:51820");
    }

    #[test]
    fn the_private_key_never_appears_in_debug_output() {
        let tunnel = Tunnel::from_profile(&profile()).unwrap();
        let shown = format!("{tunnel:?} {:#?}", tunnel);
        assert!(!shown.contains(KEY_A) && !shown.contains("private_key"), "{shown}");
        assert!(shown.contains("redacted"));
    }

    #[test]
    fn only_a_valid_wireguard_profile_becomes_a_tunnel() {
        let mut hy2 = Profile::new(Protocol::Hysteria2, "HY2");
        hy2.fields.insert("server".into(), "vps.example.com:443".into());
        hy2.secrets.insert("auth".into(), "x".into());
        assert!(Tunnel::from_profile(&hy2).unwrap_err().contains("não é WireGuard"));
        let mut broken = profile();
        broken.secrets.insert("private_key".into(), "short".into());
        assert!(Tunnel::from_profile(&broken).is_err(), "a stored profile is validated again before it is used");
    }
}
