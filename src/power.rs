use std::ptr;
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Guid { a: u32, b: u16, c: u16, d: [u8; 8] }
const PERFORMANCE: Guid = Guid { a: 0x8c5e7fda, b: 0xe8bf, c: 0x4a96, d: [0x9a, 0x85, 0xa6, 0xe2, 0x3a, 0x8c, 0x63, 0x5c] };
const ULTIMATE: Guid = Guid { a: 0xe9a42b02, b: 0xd5df, c: 0x448d, d: [0xaa, 0x00, 0x03, 0xf1, 0x47, 0x49, 0xeb, 0x61] };
#[link(name = "powrprof")]
extern "system" {
    fn PowerGetActiveScheme(root: *mut std::ffi::c_void, scheme: *mut *mut Guid) -> u32;
    fn PowerSetActiveScheme(root: *mut std::ffi::c_void, scheme: *const Guid) -> u32;
    fn PowerEnumerate(root: *mut std::ffi::c_void, scheme: *const Guid, subgroup: *const Guid, access: u32, index: u32, buffer: *mut u8, size: *mut u32) -> u32;
    fn PowerReadFriendlyName(root: *mut std::ffi::c_void, scheme: *const Guid, subgroup: *const Guid, setting: *const Guid, buffer: *mut u8, size: *mut u32) -> u32;
}
#[link(name = "kernel32")]
extern "system" { fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void; }
fn active() -> Result<Guid, String> {
    let mut scheme = ptr::null_mut();
    let error = unsafe { PowerGetActiveScheme(ptr::null_mut(), &mut scheme) };
    if error != 0 { return Err(format!("Leitura do plano de energia: {error}")); }
    if scheme.is_null() { return Err("Plano de energia ausente".into()); }
    let value = unsafe { *scheme };
    unsafe { LocalFree(scheme.cast()); }
    Ok(value)
}

#[derive(serde::Serialize)]
pub struct ActivePlan {
    pub name: String,
    pub guid: String,
}

pub fn active_plan() -> Result<ActivePlan, String> {
    let scheme = active()?;
    let mut bytes = 0u32;
    let error = unsafe {
        PowerReadFriendlyName(ptr::null_mut(), &scheme, ptr::null(), ptr::null(), ptr::null_mut(), &mut bytes)
    };
    if error != 0 && error != 234 {
        return Err(format!("Leitura do nome do plano de energia: {error}"));
    }
    if bytes == 0 || bytes % 2 != 0 {
        return Err("Nome do plano de energia inválido".into());
    }
    let mut name = vec![0u16; bytes as usize / 2];
    let error = unsafe {
        PowerReadFriendlyName(ptr::null_mut(), &scheme, ptr::null(), ptr::null(), name.as_mut_ptr().cast(), &mut bytes)
    };
    if error != 0 {
        return Err(format!("Leitura do nome do plano de energia: {error}"));
    }
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    let guid = format!("{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        scheme.a, scheme.b, scheme.c, scheme.d[0], scheme.d[1], scheme.d[2], scheme.d[3], scheme.d[4], scheme.d[5], scheme.d[6], scheme.d[7]);
    Ok(ActivePlan { name: String::from_utf16_lossy(&name[..end]), guid })
}

fn rank(scheme: Guid, name: &[u16]) -> u8 {
    let matches = |expected: &str| {
        char::decode_utf16(name.iter().copied().take_while(|c| *c != 0))
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .flat_map(char::to_lowercase).eq(expected.chars())
    };
    if scheme == ULTIMATE || matches("ultimate performance") || matches("desempenho máximo") {
        2
    } else if scheme == PERFORMANCE || matches("high performance") || matches("alto desempenho") {
        1
    } else { 0 }
}
fn preferred() -> Result<Guid, String> {
    let mut best = None;
    let mut best_rank = 0;
    for index in 0.. {
        let mut scheme = PERFORMANCE;
        let mut bytes = std::mem::size_of::<Guid>() as u32;
        let error = unsafe {
            PowerEnumerate(ptr::null_mut(), ptr::null(), ptr::null(), 16, index,
                (&mut scheme as *mut Guid).cast(), &mut bytes)
        };
        if error == 259 { break; }
        if error != 0 { return Err(format!("Enumeração dos planos de energia: {error}")); }
        let mut name = [0u16; 256];
        let mut bytes = std::mem::size_of_val(&name) as u32;
        let error = unsafe {
            PowerReadFriendlyName(ptr::null_mut(), &scheme, ptr::null(), ptr::null(),
                name.as_mut_ptr().cast(), &mut bytes)
        };
        let score = rank(scheme, if error == 0 { &name } else { &[] });
        if score > best_rank {
            best = Some(scheme);
            best_rank = score;
            if score == 2 { break; }
        }
    }
    best.ok_or_else(|| "Não há plano Alto desempenho ou Desempenho Máximo instalado".into())
}
#[derive(Default)]
pub struct Power { applied: Option<(Guid, Guid)> }
impl Power {
    pub fn update(&mut self, focused: bool) -> Result<(), String> {
        if focused && self.applied.is_none() {
            let original = active()?;
            let selected = preferred()?;
            let error = unsafe { PowerSetActiveScheme(ptr::null_mut(), &selected) };
            if error != 0 { return Err(format!("Ativação do plano de desempenho: {error}")); }
            self.applied = Some((original, selected));
        } else if !focused {
            if let Some((original, selected)) = self.applied {
                // Do not overwrite a plan chosen by the user or another controller.
                if active()? == selected {
                    let error = unsafe { PowerSetActiveScheme(ptr::null_mut(), &original) };
                    if error != 0 { return Err(format!("Restauração do plano de energia: {error}")); }
                }
                self.applied = None;
            }
        }
        Ok(())
    }
}
impl Drop for Power {
    fn drop(&mut self) { if let Err(error) = self.update(false) { eprintln!("{error}"); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefers_ultimate_including_cloned_localized_plans() {
        let clone = Guid { a: 1, ..PERFORMANCE };
        let ultimate: Vec<u16> = "DESEMPENHO MÁXIMO".encode_utf16().collect();
        let high: Vec<u16> = "High performance".encode_utf16().collect();
        assert!(rank(clone, &ultimate) > rank(PERFORMANCE, &[]));
        assert_eq!(rank(clone, &high), rank(PERFORMANCE, &[]));
        assert_eq!(rank(ULTIMATE, &[]), rank(clone, &ultimate));
        assert_eq!(rank(clone, &"Balanced".encode_utf16().collect::<Vec<_>>()), 0);
    }
}
