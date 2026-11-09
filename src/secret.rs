//! Encrypts VPN secrets (HY2 password, WireGuard private key, OpenVPN credentials) before they reach SQLite.
//! Windows DPAPI binds the blob to the current user account: another user or machine cannot decrypt it, and a
//! program of the same user must also know the application entropy. Anywhere else sealing fails closed, so no
//! code path can ever store a secret in clear text.
//! shortcut: DPAPI protects against other accounts and offline copies of the database, not against malware
//! already running as this user; a client that must use a key can never hide it from its own user.

/// Mixed into the DPAPI key so a blob is only readable by code that supplies it.
#[cfg(windows)]
const ENTROPY: &[u8] = b"rintel-rapo/vpn-secret/v1";
/// Marks sealed blobs so an unsealed or foreign value is rejected instead of being returned as a secret.
#[cfg(windows)]
const MAGIC: &[u8; 5] = b"RV1D\x01";

#[cfg(windows)]
pub fn seal(plain: &[u8]) -> Result<Vec<u8>, String> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB}};
    let input = CRYPT_INTEGER_BLOB { cbData: u32::try_from(plain.len()).map_err(|_| "Segredo grande demais")?, pbData: plain.as_ptr().cast_mut() };
    let entropy = CRYPT_INTEGER_BLOB { cbData: ENTROPY.len() as u32, pbData: ENTROPY.as_ptr().cast_mut() };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
    // SAFETY: `input`/`entropy` blobs borrow live slices for the call; on success DPAPI returns a LocalAlloc buffer of `cbData` bytes that is copied then freed once.
    unsafe {
        if CryptProtectData(&input, null(), &entropy, null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output) == 0 {
            return Err(format!("DPAPI não cifrou o segredo: {}", std::io::Error::last_os_error()));
        }
        let sealed = [MAGIC.as_slice(), std::slice::from_raw_parts(output.pbData, output.cbData as usize)].concat();
        LocalFree(output.pbData.cast());
        Ok(sealed)
    }
}

/// `None` for an empty, foreign or tampered blob, or one sealed by another Windows account.
#[cfg(windows)]
pub fn open(blob: &[u8]) -> Option<Vec<u8>> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB}};
    let body = blob.strip_prefix(MAGIC.as_slice())?;
    let input = CRYPT_INTEGER_BLOB { cbData: u32::try_from(body.len()).ok()?, pbData: body.as_ptr().cast_mut() };
    let entropy = CRYPT_INTEGER_BLOB { cbData: ENTROPY.len() as u32, pbData: ENTROPY.as_ptr().cast_mut() };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: null_mut() };
    // SAFETY: `input`/`entropy` blobs borrow live slices for the call; on success DPAPI returns a LocalAlloc buffer of `cbData` bytes that is copied then freed once.
    unsafe {
        if CryptUnprotectData(&input, null_mut(), &entropy, null(), null(), CRYPTPROTECT_UI_FORBIDDEN, &mut output) == 0 { return None; }
        let plain = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData.cast());
        Some(plain)
    }
}

#[cfg(not(windows))]
pub fn seal(_: &[u8]) -> Result<Vec<u8>, String> { Err("Cifrar segredos exige o DPAPI do Windows; nada foi gravado.".into()) }
#[cfg(not(windows))]
pub fn open(_: &[u8]) -> Option<Vec<u8>> { None }

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn sealed_secret_hides_its_text_round_trips_and_rejects_tampering() {
        let secret = b"WG-PRIVATE-KEY-0123456789abcdefghijklmnopqrstuvwxyz=";
        let blob = seal(secret).unwrap();
        assert!(blob.starts_with(MAGIC));
        assert!(!blob.windows(12).any(|w| w == &secret[..12]), "the secret must not appear in clear");
        assert_eq!(open(&blob).as_deref(), Some(secret.as_slice()));
        assert_ne!(seal(secret).unwrap(), blob, "DPAPI salts every blob");
        let mut changed = blob.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert_eq!(open(&changed), None, "a modified blob is rejected");
        assert_eq!(open(&blob[..blob.len() - 1]), None);
        assert_eq!(open(b""), None);
        assert_eq!(open(secret), None, "a clear value is never returned as a secret");
        assert_eq!(open(&[MAGIC.as_slice(), b"not dpapi"].concat()), None);
    }

    #[test]
    fn empty_secret_round_trips() {
        assert_eq!(open(&seal(b"").unwrap()).as_deref(), Some(&b""[..]));
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn sealing_fails_closed_instead_of_storing_clear_text() {
        assert!(super::seal(b"secret").is_err());
        assert_eq!(super::open(b"anything"), None);
    }
}
