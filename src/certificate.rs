//! First-run trust prompt for RAPO's self-signed Authenticode certificate. Asks once per certificate,
//! only when this `.exe` is signed by a self-signed certificate missing from the user's trusted roots.
//! Installing into `CurrentUser\Root` makes Windows show its own security confirmation as well.

use std::{ffi::c_void, os::windows::ffi::OsStrExt, path::Path, ptr::{null, null_mut}};
use windows_sys::Win32::{
    Security::Cryptography::*,
    UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONQUESTION, MB_ICONWARNING, MB_OK, MB_YESNO},
};

const ENCODING: CERT_QUERY_ENCODING_TYPE = X509_ASN_ENCODING | PKCS_7_ASN_ENCODING;
const SETTINGS_KEY: &str = r"Software\RintelRAPO";
const PROMPTED_VALUE: &str = "CertificatePrompted";

struct Cert(*mut CERT_CONTEXT);
impl Drop for Cert {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the context this wrapper owns and frees exactly once.
        unsafe { CertFreeCertificateContext(self.0); }
    }
}

struct Store(HCERTSTORE);
impl Drop for Store {
    // Flags 0 keeps contexts taken from the store valid until they are freed.
    fn drop(&mut self) {
        // SAFETY: `self.0` is the store handle this wrapper owns and closes exactly once.
        unsafe { CertCloseStore(self.0, 0); }
    }
}

fn wide(text: impl AsRef<std::ffi::OsStr>) -> Vec<u16> { text.as_ref().encode_wide().chain(Some(0)).collect() }

/// Certificate that signed `exe`, or `None` when the file has no embedded Authenticode signature.
fn signer(exe: &Path) -> Option<Cert> {
    let path = wide(exe);
    let (mut encoding, mut content, mut format) = (0, 0, 0);
    let (mut store, mut msg) = (null_mut(), null_mut());
    // SAFETY: `path` is NUL-terminated UTF-16 outliving the call; the returned store/msg are owned by the wrappers/closed below; every out-buffer is sized by the preceding size query.
    unsafe {
        if CryptQueryObject(CERT_QUERY_OBJECT_FILE, path.as_ptr().cast(), CERT_QUERY_CONTENT_FLAG_PKCS7_SIGNED_EMBED, CERT_QUERY_FORMAT_FLAG_BINARY,
            0, &mut encoding, &mut content, &mut format, &mut store, &mut msg, null_mut()) == 0 { return None; }
        let store = Store(store);
        let found = (|| {
            let mut size = 0u32;
            if CryptMsgGetParam(msg, CMSG_SIGNER_CERT_INFO_PARAM, 0, null_mut(), &mut size) == 0 { return None; }
            let mut info = vec![0u64; (size as usize).div_ceil(8)]; // CERT_INFO needs pointer alignment.
            if CryptMsgGetParam(msg, CMSG_SIGNER_CERT_INFO_PARAM, 0, info.as_mut_ptr().cast(), &mut size) == 0 { return None; }
            let cert = CertFindCertificateInStore(store.0, ENCODING, 0, CERT_FIND_SUBJECT_CERT, info.as_ptr().cast(), null());
            (!cert.is_null()).then_some(Cert(cert))
        })();
        CryptMsgClose(msg);
        found
    }
}

fn self_signed(cert: &Cert) -> bool {
    // SAFETY: `cert.0` is a live context for the lifetime of `cert`, so `pCertInfo` and the borrowed names are valid for the comparison.
    unsafe {
        let info = &*(*cert.0).pCertInfo;
        CertCompareCertificateName(X509_ASN_ENCODING, &info.Subject, &info.Issuer) != 0
    }
}

fn thumbprint(cert: &Cert) -> Option<String> {
    let (mut hash, mut size) = ([0u8; 20], 20u32);
    // SAFETY: `hash` is a 20-byte buffer and `size` states exactly that length.
    let ok = unsafe { CertGetCertificateContextProperty(cert.0, CERT_SHA1_HASH_PROP_ID, hash.as_mut_ptr().cast(), &mut size) } != 0;
    ok.then(|| hash[..size as usize].iter().map(|b| format!("{b:02X}")).collect())
}

fn open(name: &str, read_only: bool) -> Option<Store> {
    let name = wide(name);
    let flags = CERT_SYSTEM_STORE_CURRENT_USER | if read_only { CERT_STORE_READONLY_FLAG } else { 0 };
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call; the result is null-checked.
    let store = unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr() as *const c_void) };
    (!store.is_null()).then_some(Store(store))
}

/// The user's Root view also lists the machine's trusted roots.
fn trusted(cert: &Cert) -> bool {
    let Some(root) = open("Root", true) else { return false };
    // SAFETY: `root` and `cert` are live; the returned context (if any) is owned and released through `Cert`.
    let found = unsafe { CertFindCertificateInStore(root.0, ENCODING, 0, CERT_FIND_EXISTING, cert.0 as *const c_void, null()) };
    !found.is_null() && { drop(Cert(found)); true }
}

/// Root first: Windows asks for confirmation there, and a refusal leaves TrustedPublisher untouched.
fn install(cert: &Cert) -> Result<(), String> {
    for name in ["Root", "TrustedPublisher"] {
        let store = open(name, false).ok_or_else(|| format!("Abrir CurrentUser\\{name}: {}", std::io::Error::last_os_error()))?;
        // SAFETY: `store` and `cert` are live handles owned by their wrappers; the final out-pointer may be null.
        if unsafe { CertAddCertificateContextToStore(store.0, cert.0, CERT_STORE_ADD_USE_EXISTING, null_mut()) } == 0 {
            return Err(format!("CurrentUser\\{name}: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn message(text: &str, flags: u32) -> i32 {
    let (text, title) = (wide(text), wide("Rintel RAPO"));
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call; a null owner window is allowed.
    unsafe { MessageBoxW(null_mut(), text.as_ptr(), title.as_ptr(), flags) }
}

/// Called before the window opens. Unsigned builds, CA-issued signatures and trusted certificates never prompt.
pub fn prompt_once() {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let Ok(exe) = std::env::current_exe() else { return };
    let Some(cert) = signer(&exe).filter(self_signed) else { return };
    if trusted(&cert) { return; }
    let Some(print) = thumbprint(&cert) else { return };
    let Ok((settings, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(SETTINGS_KEY) else { return };
    if settings.get_value::<String, _>(PROMPTED_VALUE).is_ok_and(|seen| seen == print) { return; }
    // Recorded before asking, so a declined or interrupted prompt is not repeated for this certificate.
    let _ = settings.set_value(PROMPTED_VALUE, &print);
    let answer = message(&format!(
        "Este RAPO é assinado por um certificado próprio (autoassinado) que ainda não está instalado neste usuário do Windows.\n\n\
         Instalar o certificado (impressão digital {print}) em Autoridades de Certificação Raiz Confiáveis e Editores Confiáveis do usuário atual?\n\n\
         O Windows mostrará um aviso de segurança para confirmar. Confie somente se você baixou o RAPO da página oficial de releases: \
         qualquer programa assinado com esta chave passará a ser aceito neste usuário. Esta pergunta não será repetida para este certificado."),
        MB_YESNO | MB_ICONQUESTION);
    if answer == IDYES {
        if let Err(error) = install(&cert) {
            message(&format!("Certificado não instalado: {error}"), MB_OK | MB_ICONWARNING);
        }
    }
}
