use crate::learning::{Limits, Settings};
use crate::windows::{same_executable, Running};
use std::collections::{HashMap, HashSet};
use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        SystemInformation::{
            CpuSetInformation, GetSystemCpuSetInformation, GROUP_AFFINITY,
            SYSTEM_CPU_SET_INFORMATION,
        },
        Threading::{
            GetActiveProcessorGroupCount, GetProcessAffinityMask, GetProcessGroupAffinity,
            GetThreadGroupAffinity, GetThreadTimes, OpenProcess,
            OpenThread, SetProcessAffinityMask, SetThreadAffinityMask, SetThreadIdealProcessor,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
            THREAD_QUERY_LIMITED_INFORMATION, THREAD_SET_INFORMATION,
        },
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

fn ticks(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

// Do not replace an affinity chosen by the game or another scheduler.
fn can_pin_thread(thread_mask: usize, process_mask: usize, p_core_mask: usize) -> bool {
    (thread_mask == process_mask || (thread_mask & !p_core_mask) == 0) && thread_mask != p_core_mask
}

fn select_critical_threads(
    candidates: &[(u32, u64, u64, i32)],
    current_pinned: &[(u32, u64)],
    limit: usize,
    min_delta: u64,
) -> Vec<(u32, u64, u64)> {
    let mut ranked: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|(_, _, delta, _)| *delta >= min_delta)
        .collect();
    ranked.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.3.cmp(&a.3)));
    for i in limit..ranked.len() {
        let (tid, created, delta, _) = ranked[i];
        if !current_pinned.contains(&(tid, created)) {
            continue;
        }
        if let Some(slot) = ranked[..limit].iter().rposition(|(t, c, d, _)| {
            !current_pinned.contains(&(*t, *c)) && *d <= delta.saturating_mul(6) / 5
        }) {
            ranked.swap(slot, i);
        }
    }
    ranked.truncate(limit);
    ranked.into_iter().map(|(t, c, d, _)| (t, c, d)).collect()
}

#[derive(Clone, Copy)]
struct Core {
    index: u8,
    class: u8,
    mask: usize,
}

fn affinity_mask(cores: &[Core], settings: Settings, process_mask: usize) -> usize {
    let class = cores.iter().map(|core| core.class).max().unwrap_or(0);
    cores
        .iter()
        .filter(|core| !settings.p_only || core.class == class)
        .map(|core| core.mask & process_mask)
        .filter(|mask| *mask != 0)
        .take(usize::from(settings.cores))
        .fold(0, |mask, core| {
            mask | if settings.smt {
                core
            } else {
                core.isolate_lowest_one()
            }
        })
}

fn cpu_cores() -> Result<Vec<Core>, String> {
    unsafe {
        let mut bytes = 0u32;
        GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut bytes, std::ptr::null_mut(), 0);
        if bytes == 0 {
            return Err("Windows não informou CPU sets".into());
        }
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        if GetSystemCpuSetInformation(
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
            std::ptr::null_mut(),
            0,
        ) == 0
        {
            return Err(format!(
                "Falha ao obter topologia: {}",
                std::io::Error::last_os_error()
            ));
        }
        let mut cores: Vec<Core> = Vec::new();
        let mut offset = 0usize;
        while offset + size_of::<SYSTEM_CPU_SET_INFORMATION>() <= bytes as usize {
            let info = &*(buffer
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<SYSTEM_CPU_SET_INFORMATION>());
            if info.Size < size_of::<SYSTEM_CPU_SET_INFORMATION>() as u32
                || offset + info.Size as usize > bytes as usize
            {
                break;
            }
            if info.Type == CpuSetInformation {
                let core = info.Anonymous.CpuSet;
                if core.Group == 0 && core.LogicalProcessorIndex < usize::BITS as u8 {
                    let flags = core.Anonymous1.AllFlags;
                    if flags & 2 == 0 {
                        let mask = 1usize << core.LogicalProcessorIndex;
                        if let Some(physical) = cores.iter_mut().find(|c| c.index == core.CoreIndex)
                        {
                            physical.mask |= mask;
                        } else {
                            cores.push(Core {
                                index: core.CoreIndex,
                                class: core.EfficiencyClass,
                                mask,
                            });
                        }
                    }
                }
            }
            offset += info.Size as usize;
        }
        if cores.is_empty() {
            return Err("Nenhum CPU set disponível no grupo 0".into());
        }
        cores.sort_by_key(|core| core.index);
        Ok(cores)
    }
}

fn thread_time(handle: *mut std::ffi::c_void) -> Option<(u64, u64, bool)> {
    unsafe {
        let (mut created, mut exited, mut kernel, mut user): (
            FILETIME,
            FILETIME,
            FILETIME,
            FILETIME,
        ) = (zeroed(), zeroed(), zeroed(), zeroed());
        (GetThreadTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0)
            .then(|| (ticks(created), ticks(kernel) + ticks(user), ticks(exited) != 0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffinityConflict {
    pub pid: u32,
    pub tid: u32,
    pub our_mask: usize,
    pub external_mask: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum RestoreOutcome {
    Restored,
    Terminated,
    ExternallyChanged(usize),
}

// A primary-group mask cannot describe Windows 11's default all-group eligibility.
fn reversible_group_affinity(system_groups: u16, process_groups: &[u16]) -> bool {
    system_groups == 1 && process_groups == [0]
}

struct Pinned {
    tid: u32,
    created: u64,
    original: usize,
    applied: usize,
    original_ideal: Option<u32>,
}

pub struct Pinner {
    pub(crate) mask: usize,
    cores: Vec<Core>,
    last_settings: Option<Settings>,
    samples: HashMap<(u32, u32), (u64, u64)>,
    pinned: HashMap<u32, Vec<Pinned>>,
    last: Option<(u32, u32, u64)>,
    last_poll: Option<(u32, std::time::Instant)>,
    pub auto_yield: bool,
    pub conflict: Option<AffinityConflict>,
    process_masks: HashMap<u32, usize>,
}

impl Pinner {
    pub fn new() -> Result<Self, String> {
        let cores = cpu_cores()?;
        let class = cores.iter().map(|core| core.class).max().unwrap_or(0);
        let mask = cores
            .iter()
            .filter(|core| core.class == class)
            .fold(0, |mask, core| mask | core.mask);
        Ok(Self {
            mask,
            cores,
            last_settings: None,
            samples: HashMap::new(),
            pinned: HashMap::new(),
            last: None,
            last_poll: None,
            auto_yield: true,
            conflict: None,
            process_masks: HashMap::new(),
        })
    }
    pub fn limits(&self) -> Limits {
        let all = self
            .cores
            .iter()
            .fold(0usize, |mask, core| mask | core.mask);
        let class = self.cores.iter().map(|core| core.class).max().unwrap_or(0);
        Limits {
            threads: all.count_ones() as u8,
            cores: self.cores.len() as u8,
            p_cores: self.cores.iter().filter(|core| core.class == class).count() as u8,
            smt: self.cores.iter().any(|core| core.mask.count_ones() > 1),
        }
    }

    pub fn conflict(&self) -> Option<AffinityConflict> {
        self.conflict
    }
    pub fn status(&self) -> Option<(u32, u32, u64)> {
        self.last.filter(|(pid, tid, _)| {
            self.pinned
                .get(pid)
                .is_some_and(|pins| pins.iter().any(|pin| pin.tid == *tid))
        })
    }
    pub fn pinned_count(&self, pid: u32) -> usize {
        self.pinned.get(&pid).map_or(0, Vec::len)
    }

    fn restore_pin(pin: &Pinned) -> Result<RestoreOutcome, String> {
        unsafe {
            let thread = OpenThread(
                THREAD_QUERY_LIMITED_INFORMATION | THREAD_SET_INFORMATION,
                0,
                pin.tid,
            );
            if thread.is_null() {
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(87) {
                    Ok(RestoreOutcome::Terminated)
                } else {
                    Err(format!("Thread {}: falha ao abrir para restaurar: {error}", pin.tid))
                };
            }
            let result = (|| {
                let (created, _, exited) = thread_time(thread).ok_or_else(|| {
                    format!("Thread {}: falha ao verificar identidade: {}", pin.tid, std::io::Error::last_os_error())
                })?;
                if exited || created != pin.created { return Ok(RestoreOutcome::Terminated); }
                let mut affinity: GROUP_AFFINITY = zeroed();
                if GetThreadGroupAffinity(thread, &mut affinity) == 0 {
                    return Err(format!("Thread {}: falha ao ler afinidade: {}", pin.tid, std::io::Error::last_os_error()));
                }
                if affinity.Group != 0 || affinity.Mask != pin.applied {
                    return Ok(RestoreOutcome::ExternallyChanged(affinity.Mask));
                }
                if SetThreadAffinityMask(thread, pin.original) == 0 {
                    return Err(format!("Thread {}: falha ao restaurar afinidade: {}", pin.tid, std::io::Error::last_os_error()));
                }
                if let Some(ideal) = pin.original_ideal {
                    SetThreadIdealProcessor(thread, ideal);
                }
                Ok(RestoreOutcome::Restored)
            })();
            CloseHandle(thread);
            result
        }
    }

    fn restore_pins(
        pins: &mut Vec<Pinned>,
        mut restore: impl FnMut(&Pinned) -> Result<RestoreOutcome, String>,
    ) -> Result<(), String> {
        let mut errors = String::new();
        pins.retain(|pin| match restore(pin) {
            Ok(_) => false,
            Err(error) => {
                if !errors.is_empty() { errors.push_str("; "); }
                errors.push_str(&error);
                true
            }
        });
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    fn restore(&mut self, pid: u32) -> Result<(), String> {
        if let Some(original_mask) = self.process_masks.remove(&pid) {
            unsafe {
                let handle = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
                if !handle.is_null() {
                    SetProcessAffinityMask(handle, original_mask);
                    CloseHandle(handle);
                }
            }
        }
        let pins_result = if let Some(pins) = self.pinned.get_mut(&pid) {
            let res = Self::restore_pins(pins, Self::restore_pin);
            if pins.is_empty() { self.pinned.remove(&pid); }
            res
        } else {
            Ok(())
        };
        pins_result
    }

    pub fn update(
        &mut self,
        running: &[Running],
        enabled: &HashSet<PathBuf>,
        keep_windowed: bool,
        profiles: &HashMap<PathBuf, crate::learning::Settings>,
    ) -> Result<(), String> {
        let prev_last = self.last;
        self.last = None;
        unsafe {
            let hwnd = GetForegroundWindow();
            let mut foreground = 0;
            GetWindowThreadProcessId(hwnd, &mut foreground);
            if foreground == 0 && !hwnd.is_null() {
                let root = windows_sys::Win32::UI::WindowsAndMessaging::GetAncestor(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::GA_ROOTOWNER,
                );
                if !root.is_null() {
                    GetWindowThreadProcessId(root, &mut foreground);
                }
            }
            let profile = |p: &Running| profiles.get(&p.exe).or_else(|| {
                profiles.iter().find(|(exe, _)| same_executable(exe, &p.exe)).map(|(_, settings)| settings)
            }).copied();
            let pin_enabled = |p: &Running| {
                profile(p).map_or_else(|| enabled.iter().any(|dir| same_executable(dir, &p.directory)), |s| s.pin)
            };
            let mut active = running
                .iter()
                .find(|p| p.pid == foreground && pin_enabled(p));
            if active.is_none() && keep_windowed {
                active = running.iter().find(|p| pin_enabled(p));
            }
            let old: Vec<u32> = self
                .pinned
                .keys()
                .copied()
                .filter(|pid| Some(*pid) != active.map(|p| p.pid))
                .collect();
            let mut restore_error = None;
            for pid in old {
                if let Err(error) = self.restore(pid) { restore_error.get_or_insert(error); }
            }
            if let Some(error) = restore_error { return Err(error); }
            let Some(process) = active else {
                self.samples.clear();
                self.last_poll = None;
                self.conflict = None;
                return Ok(());
            };
            if process
                .exe
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("AION2.exe"))
                && self.last_settings
                    == profile(process).or(Some(Settings::default()))
                && self.pinned.contains_key(&process.pid)
                && self.last_poll.is_some_and(|(pid, t)| {
                    pid == process.pid && t.elapsed() < std::time::Duration::from_secs(10)
                })
            {
                self.last = prev_last;
                return Ok(());
            }
            let settings = profile(process).unwrap_or_default();
            // Failed changes must retry instead of entering AION2's successful-update throttle.
            self.last_settings = None;
            let elapsed = self
                .last_poll
                .filter(|(pid, _)| *pid == process.pid)
                .map_or(std::time::Duration::from_secs(1), |(_, time)| {
                    time.elapsed()
                });
            self.last_poll = Some((process.pid, std::time::Instant::now()));
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process.pid);
            if handle.is_null() {
                return Err(format!(
                    "Sem acesso à afinidade do processo {}",
                    process.pid
                ));
            }
            let mut group = 0u16;
            let mut group_count = 1u16;
            let groups_ok = GetProcessGroupAffinity(handle, &mut group_count, &mut group);
            let system_groups = GetActiveProcessorGroupCount();
            if groups_ok == 0 || group_count != 1 || !reversible_group_affinity(system_groups, &[group]) {
                CloseHandle(handle);
                // Refuse hard affinity only; priority, memory and other adjustments remain available.
                self.restore(process.pid)?;
                return Err("Afinidade rígida indisponível: não é possível restaurar com segurança a elegibilidade entre grupos de processadores".into());
            }
            let (mut process_mask, mut system_mask) = (0usize, 0usize);
            let ok = GetProcessAffinityMask(handle, &mut process_mask, &mut system_mask);
            CloseHandle(handle);
            if ok == 0 {
                return Err("Não foi possível ler afinidade do processo".into());
            }
            let base_process_mask = self.process_masks.get(&process.pid).copied().unwrap_or(process_mask);
            let mask = affinity_mask(&self.cores, settings, base_process_mask);
            if mask == 0 {
                return Err("Nenhum núcleo elegível na afinidade permitida pelo jogo".into());
            }
            if settings.park_cores {
                let set_handle = OpenProcess(PROCESS_SET_INFORMATION, 0, process.pid);
                if !set_handle.is_null() {
                    self.process_masks.entry(process.pid).or_insert(process_mask);
                    SetProcessAffinityMask(set_handle, mask);
                    CloseHandle(set_handle);
                }
            } else if let Some(original_mask) = self.process_masks.remove(&process.pid) {
                let set_handle = OpenProcess(PROCESS_SET_INFORMATION, 0, process.pid);
                if !set_handle.is_null() {
                    SetProcessAffinityMask(set_handle, original_mask);
                    CloseHandle(set_handle);
                }
            }
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err("Não foi possível listar threads".into());
            }
            let mut entry: THREADENTRY32 = zeroed();
            entry.dwSize = size_of::<THREADENTRY32>() as u32;
            let current_pins: Vec<(u32, u64)> = self
                .pinned
                .get(&process.pid)
                .map(|pins| pins.iter().map(|p| (p.tid, p.created)).collect())
                .unwrap_or_default();
            let mut candidates = Vec::new();
            let mut seen = HashSet::new();
            if Thread32First(snapshot, &mut entry) != 0 {
                loop {
                    if entry.th32OwnerProcessID == process.pid {
                        let thread =
                            OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, entry.th32ThreadID);
                        if !thread.is_null() {
                            if let Some((created, cpu, false)) = thread_time(thread) {
                                let key = (process.pid, entry.th32ThreadID);
                                seen.insert(key);
                                if let Some((old_created, old_cpu)) =
                                    self.samples.insert(key, (created, cpu))
                                {
                                    if created == old_created && cpu >= old_cpu {
                                        candidates.push((
                                            entry.th32ThreadID,
                                            created,
                                            cpu - old_cpu,
                                            entry.tpBasePri,
                                        ));
                                    }
                                }
                            }
                            CloseHandle(thread);
                        }
                    }
                    if Thread32Next(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
            self.samples.retain(|key, _| seen.contains(key));
            let (threads, percent) = (settings.threads, settings.min_percent);
            let limit = usize::from(threads.max(1)).min(mask.count_ones() as usize);
            let min_delta = (elapsed.as_secs_f64() * 10_000_000.0 * f64::from(percent.clamp(1, 20))
                / 100.0) as u64;
            let selected = select_critical_threads(&candidates, &current_pins, limit, min_delta);
            if let Some(&(top_tid, _, top_delta)) = selected.first() {
                self.last = Some((process.pid, top_tid, top_delta / 10_000));
            }
            let mut detected_conflict = None;
            if let Some(pins) = self.pinned.get(&process.pid) {
                for pin in pins {
                    let thread = OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, pin.tid);
                    if thread.is_null() {
                        let error = std::io::Error::last_os_error();
                        if error.raw_os_error() == Some(87) { continue; }
                        return Err(format!("Thread {}: falha ao verificar afinidade: {error}", pin.tid));
                    }
                    let Some((created, _, exited)) = thread_time(thread) else {
                        let error = std::io::Error::last_os_error();
                        CloseHandle(thread);
                        return Err(format!("Thread {}: falha ao verificar identidade: {error}", pin.tid));
                    };
                    if exited || created != pin.created { CloseHandle(thread); continue; }
                    let mut current_affinity: GROUP_AFFINITY = zeroed();
                    let ok = GetThreadGroupAffinity(thread, &mut current_affinity);
                    let error = std::io::Error::last_os_error();
                    CloseHandle(thread);
                    if ok == 0 { return Err(format!("Thread {}: falha ao ler afinidade: {error}", pin.tid)); }
                    if current_affinity.Group != 0 || current_affinity.Mask != pin.applied {
                        detected_conflict = Some(AffinityConflict {
                            pid: process.pid,
                            tid: pin.tid,
                            our_mask: pin.applied,
                            external_mask: current_affinity.Mask,
                        });
                        break;
                    }
                }
            }
            if let Some(conflict) = detected_conflict {
                self.conflict = Some(conflict);
                if self.auto_yield { self.restore(process.pid)?; }
                return Ok(());
            }
            if self.conflict.is_some() && self.auto_yield {
                return Ok(());
            }
            self.conflict = None;
            let mut kept = Vec::new();
            let mut restore_error = None;
            let mut restoration_changed = false;
            if let Some(old_pins) = self.pinned.remove(&process.pid) {
                for pin in old_pins {
                    if pin.applied == mask
                        && selected
                            .iter()
                            .any(|&(tid, created, _)| tid == pin.tid && created == pin.created)
                    {
                        kept.push(pin);
                    } else {
                        match Self::restore_pin(&pin) {
                            Err(error) => {
                                restore_error.get_or_insert(error);
                                kept.push(pin);
                            }
                            Ok(RestoreOutcome::ExternallyChanged(external_mask)) => {
                                restoration_changed = true;
                                self.conflict = Some(AffinityConflict {
                                    pid: process.pid,
                                    tid: pin.tid,
                                    our_mask: pin.applied,
                                    external_mask,
                                });
                            }
                            Ok(_) => (),
                        }
                    }
                }
            }
            if restore_error.is_some() || restoration_changed {
                if !kept.is_empty() { self.pinned.insert(process.pid, kept); }
                return restore_error.map_or(Ok(()), Err);
            }
            let eligible_cores: Vec<u32> = (0..usize::BITS).filter(|bit| (mask >> bit) & 1 == 1).collect();
            let mut apply_error = None;
            for (idx, &(tid, created, _)) in selected.iter().enumerate() {
                if kept.iter().any(|p| p.tid == tid && p.created == created) {
                    continue;
                }
                let thread = OpenThread(
                    THREAD_QUERY_LIMITED_INFORMATION | THREAD_SET_INFORMATION,
                    0,
                    tid,
                );
                if thread.is_null() {
                    continue;
                }
                if thread_time(thread).is_some_and(|(current, _, exited)| current == created && !exited) {
                    let mut affinity: GROUP_AFFINITY = zeroed();
                    if GetThreadGroupAffinity(thread, &mut affinity) == 0 {
                        apply_error.get_or_insert_with(|| format!("Thread {tid}: falha ao ler afinidade: {}", std::io::Error::last_os_error()));
                    } else if affinity.Group == 0 && can_pin_thread(affinity.Mask, process_mask, mask) {
                        let original = SetThreadAffinityMask(thread, mask);
                        if original != 0 {
                            let mut original_ideal = None;
                            if let Some(&ideal_core) = eligible_cores.get(idx % eligible_cores.len().max(1)) {
                                let prev = SetThreadIdealProcessor(thread, ideal_core);
                                if prev != u32::MAX {
                                    original_ideal = Some(prev);
                                }
                            }
                            kept.push(Pinned {
                                tid,
                                created,
                                original,
                                applied: mask,
                                original_ideal,
                            });
                        } else {
                            apply_error.get_or_insert_with(|| format!("Thread {tid}: falha ao aplicar afinidade: {}", std::io::Error::last_os_error()));
                        }
                    }
                }
                CloseHandle(thread);
            }
            if !kept.is_empty() {
                self.pinned.insert(process.pid, kept);
            }
            if let Some(error) = apply_error {
                Err(error)
            } else {
                self.last_settings = Some(settings);
                Ok(())
            }
        }
    }
}

impl Drop for Pinner {
    fn drop(&mut self) {
        let pids: Vec<_> = self.pinned.keys().copied().chain(self.process_masks.keys().copied()).collect();
        for pid in pids {
            if let Err(error) = self.restore(pid) { eprintln!("{error}"); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_physical_cores_smt_siblings_and_allowed_affinity() {
        let cores = [
            Core {
                index: 0,
                class: 1,
                mask: 0b010001,
            },
            Core {
                index: 1,
                class: 1,
                mask: 0b100010,
            },
            Core {
                index: 2,
                class: 0,
                mask: 0b000100,
            },
        ];
        let mut settings = Settings {
            pin: true,
            ..Settings::default()
        };
        assert_eq!(affinity_mask(&cores, settings, usize::MAX), 0b110011);
        settings.smt = false;
        assert_eq!(affinity_mask(&cores, settings, usize::MAX), 0b000011);
        settings.cores = 1;
        assert_eq!(affinity_mask(&cores, settings, usize::MAX), 0b000001);
        assert_eq!(affinity_mask(&cores, settings, 0b110000), 0b010000);
        settings.cores = 64;
        settings.p_only = false;
        assert_eq!(affinity_mask(&cores, settings, usize::MAX), 0b000111);
        assert_eq!(affinity_mask(&cores, settings, 0b000100), 0b000100);
    }
    #[test]
    fn preserves_game_selected_thread_affinity() {
        assert!(can_pin_thread(0b1111, 0b1111, 0b0011));
        assert!(!can_pin_thread(0b0100, 0b1111, 0b0011));
        assert!(!can_pin_thread(0b0011, 0b1111, 0b0011));
    }

    #[test]
    fn failed_restore_retains_original_mask_until_retry_succeeds() {
        let mut pins = vec![
            Pinned { tid: 1, created: 10, original: 0xff, applied: 3, original_ideal: None },
            Pinned { tid: 2, created: 20, original: 0xf0, applied: 0x30, original_ideal: None },
            Pinned { tid: 3, created: 30, original: 0x0f, applied: 3, original_ideal: None },
        ];
        let error = Pinner::restore_pins(&mut pins, |pin| match pin.tid {
            1 => Err("access denied".into()),
            2 => Ok(RestoreOutcome::Terminated),
            _ => Ok(RestoreOutcome::ExternallyChanged(4)),
        }).unwrap_err();
        assert_eq!(error, "access denied");
        assert_eq!(pins.len(), 1);
        assert_eq!((pins[0].tid, pins[0].original, pins[0].applied), (1, 0xff, 3));
        Pinner::restore_pins(&mut pins, |_| Ok(RestoreOutcome::Restored)).unwrap();
        assert!(pins.is_empty());
    }

    #[test]
    fn primary_group_zero_does_not_prove_reversible_all_group_affinity() {
        assert!(reversible_group_affinity(1, &[0]));
        assert!(!reversible_group_affinity(2, &[0]));
        assert!(!reversible_group_affinity(2, &[0, 1]));
        assert!(!reversible_group_affinity(1, &[1]));
        assert!(!reversible_group_affinity(0, &[]));
    }

    #[test]
    fn installed_topology_accepts_reversible_p_core_mask() {
        let mask = Pinner::new().unwrap().mask;
        unsafe {
            if GetActiveProcessorGroupCount() != 1 { return; }
            let thread = windows_sys::Win32::System::Threading::GetCurrentThread();
            let previous = SetThreadAffinityMask(thread, mask);
            if previous != 0 {
                SetThreadAffinityMask(thread, previous);
            }
            assert_ne!(
                previous,
                0,
                "Windows rejected P-core affinity: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    #[test]
    fn selects_up_to_four_critical_threads_by_utilization_then_priority() {
        let candidates = [
            (10, 1, 400_000, 10),
            (11, 1, 800_000, 8),
            (12, 1, 800_000, 10),
            (13, 1, 900_000, 7),
            (14, 1, 700_000, 9),
            (15, 1, 750_000, 6),
        ];
        let selected = select_critical_threads(&candidates, &[(14, 1)], 4, 500_000);
        let tids: Vec<u32> = selected.iter().map(|(tid, _, _)| *tid).collect();
        assert_eq!(tids, vec![13, 12, 11, 14]);
    }
    #[test]
    fn learned_limits_change_eligible_thread_selection() {
        let candidates = [
            (10, 1, 400_000, 10),
            (11, 1, 800_000, 8),
            (12, 1, 900_000, 10),
        ];
        assert_eq!(
            select_critical_threads(&candidates, &[], 1, 500_000),
            vec![(12, 1, 900_000)]
        );
        assert_eq!(
            select_critical_threads(&candidates, &[], 3, 850_000),
            vec![(12, 1, 900_000)]
        );
        assert_eq!(
            select_critical_threads(&candidates, &[], 3, 1_000_000),
            vec![]
        );
    }
}
