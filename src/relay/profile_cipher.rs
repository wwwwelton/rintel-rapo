//! Seals the built-in HY2 profile (server, secret, CA) so it is not readable as plain text in the `.exe`.
//! Shared by `build.rs` (seal) and the relay (open) through `include!`/`#[path]`.
//! shortcut: the key is derived from the nonce stored beside the ciphertext, so this defeats `strings`
//! and tampering, not a determined reverse engineer; a client can never hide a secret it must use.
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 5] = b"RH2P1";
const NONCE: usize = 16;
const TAG: usize = 16;

fn key(nonce: &[u8]) -> [u8; 32] {
    Sha256::new().chain_update(b"rintel-rapo/hy2-profile/v1").chain_update(nonce).finalize().into()
}

fn apply_keystream(key: &[u8; 32], nonce: &[u8], data: &mut [u8]) {
    for (counter, chunk) in data.chunks_mut(32).enumerate() {
        let block = Sha256::new().chain_update(key).chain_update(nonce).chain_update((counter as u64).to_le_bytes()).finalize();
        chunk.iter_mut().zip(block).for_each(|(byte, mask)| *byte ^= mask);
    }
}

fn tag(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> [u8; TAG] {
    let digest = Sha256::new().chain_update(b"tag").chain_update(key).chain_update(nonce).chain_update(ciphertext).finalize();
    digest[..TAG].try_into().unwrap()
}

/// `nonce` must differ for every sealed profile.
#[allow(dead_code)] // used by build.rs and tests; the shipped executable only opens.
pub fn seal(plain: &[u8], nonce: [u8; NONCE]) -> Vec<u8> {
    let key = key(&nonce);
    let mut body = plain.to_vec();
    apply_keystream(&key, &nonce, &mut body);
    let mut out = [MAGIC.as_slice(), &nonce, &body].concat();
    out.extend_from_slice(&tag(&key, &nonce, &body));
    out
}

/// `None` for an empty, truncated or modified blob.
pub fn open(blob: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < MAGIC.len() + NONCE + TAG || !blob.starts_with(MAGIC) { return None; }
    let (nonce, rest) = blob[MAGIC.len()..].split_at(NONCE);
    let (body, found) = rest.split_at(rest.len() - TAG);
    let key = key(nonce);
    if tag(&key, nonce, body) != found { return None; }
    let mut plain = body.to_vec();
    apply_keystream(&key, nonce, &mut plain);
    Some(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_profile_hides_text_and_rejects_tampering() {
        let plain = b"{\"server\":\"203.0.113.7:443\",\"auth\":\"s3cret-token-0123456789abcdef0123\"}";
        let blob = seal(plain, [7; NONCE]);
        assert!(!blob.windows(6).any(|w| w == b"s3cret"), "the secret must not appear in clear");
        assert_eq!(open(&blob).as_deref(), Some(plain.as_slice()));
        let mut changed = blob.clone();
        changed[MAGIC.len() + NONCE + 3] ^= 1;
        assert_eq!(open(&changed), None);
        assert_eq!(open(&blob[..blob.len() - 1]), None);
        assert_eq!(open(b""), None);
        assert_ne!(seal(plain, [8; NONCE]), blob, "a new nonce changes the whole blob");
    }
}
