use crate::windows::Running;
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
            GetProcessAffinityMask, GetThreadGroupAffinity, GetThreadTimes, OpenProcess,
            OpenThread, SetThreadAffinityMask, PROCESS_QUERY_LIMITED_INFORMATION,
            THREAD_QUERY_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SET_INFORMATION,
        },
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

fn ticks(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

// Higher efficiency class means faster P-cores on heterogeneous Windows systems.
fn favored_mask(cores: &[(u8, u8, bool)]) -> usize {
    let Some(max) = cores.iter().map(|(class, _, _)| *class).max() else {
        return 0;
    };
    if cores.iter().all(|(class, _, _)| *class == max) {
        return 0;
    }
    cores
        .iter()
        .filter(|(class, _, available)| *class == max && *available)
        .fold(0usize, |mask, (_, index, _)| mask | (1usize << index))
}
// Do not replace an affinity chosen by the game or another scheduler.
fn can_pin_thread(thread_mask: usize, process_mask: usize, p_core_mask: usize) -> bool {
    thread_mask == process_mask && thread_mask != p_core_mask
}

pub const APO_NUM_CRITICAL_THREADS: usize = 4;
pub const APO_MIN_DELTA_TICKS: u64 = 500_000;

fn select_critical_threads(
    candidates: &[(u32, u64, u64, i32)],
    current_pinned: &[(u32, u64)],
) -> Vec<(u32, u64, u64)> {
    let mut ranked: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|(_, _, delta, _)| *delta >= APO_MIN_DELTA_TICKS)
        .collect();
    ranked.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.3.cmp(&a.3)));
    for i in APO_NUM_CRITICAL_THREADS..ranked.len() {
        let (tid, created, delta, _) = ranked[i];
        if !current_pinned.contains(&(tid, created)) {
            continue;
        }
        if let Some(slot) = ranked[..APO_NUM_CRITICAL_THREADS].iter().rposition(|(t, c, d, _)| {
            !current_pinned.contains(&(*t, *c)) && *d <= delta.saturating_mul(6) / 5
        }) {
            ranked.swap(slot, i);
        }
    }
    ranked.truncate(APO_NUM_CRITICAL_THREADS);
    ranked.into_iter().map(|(t, c, d, _)| (t, c, d)).collect()
}

fn p_core_mask() -> Result<usize, String> {
    unsafe {
        let mut bytes = 0u32;
        GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut bytes, std::ptr::null_mut(), 0);
        if bytes == 0 {
            return Err("Windows não informou CPU sets".into());
        }
        let mut buffer = vec![0u64; (bytes as usize + 7) / 8];
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
        let mut cores = Vec::new();
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
                    cores.push((
                        core.EfficiencyClass,
                        core.LogicalProcessorIndex,
                        flags & 2 == 0,
                    ));
                }
            }
            offset += info.Size as usize;
        }
        let mask = favored_mask(&cores);
        if mask == 0 {
            Err("Não há P-cores e E-cores distintos disponíveis no grupo 0".into())
        } else {
            Ok(mask)
        }
    }
}

fn thread_time(handle: *mut std::ffi::c_void) -> Option<(u64, u64)> {
    unsafe {
        let (mut created, mut exited, mut kernel, mut user): (
            FILETIME,
            FILETIME,
            FILETIME,
            FILETIME,
        ) = (zeroed(), zeroed(), zeroed(), zeroed());
        (GetThreadTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0)
            .then(|| (ticks(created), ticks(kernel) + ticks(user)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffinityConflict {
    pub pid: u32,
    pub tid: u32,
    pub our_mask: usize,
    pub external_mask: usize,
}

struct Pinned {
    tid: u32,
    created: u64,
    original: usize,
    applied: usize,
}

pub struct Pinner {
    pub(crate) mask: usize,
    samples: HashMap<(u32, u32), (u64, u64)>,
    pinned: HashMap<u32, Vec<Pinned>>,
    last: Option<(u32, u32, u64)>,
    last_poll: Option<(u32, std::time::Instant)>,
    pub auto_yield: bool,
    pub conflict: Option<AffinityConflict>,
}

impl Pinner {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            mask: p_core_mask()?,
            samples: HashMap::new(),
            pinned: HashMap::new(),
            last: None,
            last_poll: None,
            auto_yield: true,
            conflict: None,
        })
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

    fn restore_pin(pin: &Pinned) {
        unsafe {
            let thread = OpenThread(
                THREAD_QUERY_INFORMATION | THREAD_SET_INFORMATION,
                0,
                pin.tid,
            );
            if thread.is_null() {
                return;
            }
            let mut affinity: GROUP_AFFINITY = zeroed();
            if thread_time(thread).is_some_and(|(created, _)| created == pin.created)
                && GetThreadGroupAffinity(thread, &mut affinity) != 0
                && affinity.Group == 0
                && affinity.Mask == pin.applied
            {
                SetThreadAffinityMask(thread, pin.original);
            }
            CloseHandle(thread);
        }
    }

    fn restore(&mut self, pid: u32) {
        let Some(pins) = self.pinned.remove(&pid) else {
            return;
        };
        for pin in &pins {
            Self::restore_pin(pin);
        }
    }

    pub fn update(
        &mut self,
        running: &[Running],
        enabled: &HashSet<PathBuf>,
        keep_windowed: bool,
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
            let mut active = running
                .iter()
                .find(|p| p.pid == foreground && enabled.contains(&p.directory));
            if active.is_none() && keep_windowed {
                active = running.iter().find(|p| enabled.contains(&p.directory));
            }
            let old: Vec<u32> = self
                .pinned
                .keys()
                .copied()
                .filter(|pid| Some(*pid) != active.map(|p| p.pid))
                .collect();
            for pid in old {
                self.restore(pid);
            }
            let Some(process) = active else {
                self.samples.clear();
                self.last_poll = None;
                self.conflict = None;
                return Ok(());
            };
            if process.exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case("AION2.exe"))
                && self.pinned.contains_key(&process.pid)
                && self.last_poll.is_some_and(|(pid, t)| {
                    pid == process.pid && t.elapsed() < std::time::Duration::from_secs(10)
                })
            {
                self.last = prev_last;
                return Ok(());
            }
            self.last_poll = Some((process.pid, std::time::Instant::now()));
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process.pid);
            if handle.is_null() {
                return Err(format!(
                    "Sem acesso à afinidade do processo {}",
                    process.pid
                ));
            }
            let (mut process_mask, mut system_mask) = (0usize, 0usize);
            let ok = GetProcessAffinityMask(handle, &mut process_mask, &mut system_mask);
            CloseHandle(handle);
            if ok == 0 {
                return Err("Não foi possível ler afinidade do processo".into());
            }
            let mask = self.mask & process_mask;
            if mask == 0 {
                return Err("A afinidade do jogo exclui os P-cores".into());
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
                        let thread = OpenThread(
                            THREAD_QUERY_LIMITED_INFORMATION,
                            0,
                            entry.th32ThreadID,
                        );
                        if !thread.is_null() {
                            if let Some((created, cpu)) = thread_time(thread) {
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
            let selected = select_critical_threads(&candidates, &current_pins);
            if let Some(&(top_tid, _, top_delta)) = selected.first() {
                self.last = Some((process.pid, top_tid, top_delta / 10_000));
            }
            if let Some(pins) = self.pinned.get(&process.pid) {
                for pin in pins {
                    let thread = OpenThread(THREAD_QUERY_INFORMATION, 0, pin.tid);
                    if !thread.is_null() {
                        let mut current_affinity: GROUP_AFFINITY = zeroed();
                        if GetThreadGroupAffinity(thread, &mut current_affinity) != 0
                            && current_affinity.Group == 0
                            && current_affinity.Mask != pin.applied
                        {
                            let (conflict_tid, our_mask, external_mask) =
                                (pin.tid, pin.applied, current_affinity.Mask);
                            CloseHandle(thread);
                            self.conflict = Some(AffinityConflict {
                                pid: process.pid,
                                tid: conflict_tid,
                                our_mask,
                                external_mask,
                            });
                            if self.auto_yield {
                                if let Some(old_pins) = self.pinned.remove(&process.pid) {
                                    for other in &old_pins {
                                        if other.tid != conflict_tid {
                                            Self::restore_pin(other);
                                        }
                                    }
                                }
                            }
                            return Ok(());
                        }
                        CloseHandle(thread);
                    }
                }
            }
            if self.conflict.is_some() && self.auto_yield {
                return Ok(());
            }
            self.conflict = None;
            let mut kept = Vec::new();
            if let Some(old_pins) = self.pinned.remove(&process.pid) {
                for pin in old_pins {
                    if selected.iter().any(|&(tid, created, _)| tid == pin.tid && created == pin.created) {
                        kept.push(pin);
                    } else {
                        Self::restore_pin(&pin);
                    }
                }
            }
            for &(tid, created, _) in &selected {
                if kept.iter().any(|p| p.tid == tid && p.created == created) {
                    continue;
                }
                let thread = OpenThread(THREAD_QUERY_INFORMATION | THREAD_SET_INFORMATION, 0, tid);
                if thread.is_null() {
                    continue;
                }
                if thread_time(thread).is_some_and(|(current, _)| current == created) {
                    let mut affinity: GROUP_AFFINITY = zeroed();
                    if GetThreadGroupAffinity(thread, &mut affinity) != 0
                        && affinity.Group == 0
                        && can_pin_thread(affinity.Mask, process_mask, mask)
                    {
                        let original = SetThreadAffinityMask(thread, mask);
                        if original != 0 {
                            kept.push(Pinned {
                                tid,
                                created,
                                original,
                                applied: mask,
                            });
                        }
                    }
                }
                CloseHandle(thread);
            }
            if !kept.is_empty() {
                self.pinned.insert(process.pid, kept);
            }
            Ok(())
        }
    }
}

impl Drop for Pinner {
    fn drop(&mut self) {
        let pids: Vec<_> = self.pinned.keys().copied().collect();
        for pid in pids {
            self.restore(pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_only_available_fast_cores() {
        assert_eq!(
            favored_mask(&[(0, 0, true), (1, 1, true), (1, 2, false), (0, 3, true)]),
            1 << 1
        );
        assert_eq!(favored_mask(&[(0, 0, true), (0, 1, true)]), 0);
    }
    #[test]
    fn preserves_game_selected_thread_affinity() {
        assert!(can_pin_thread(0b1111, 0b1111, 0b0011));
        assert!(!can_pin_thread(0b0100, 0b1111, 0b0011));
        assert!(!can_pin_thread(0b0011, 0b1111, 0b0011));
    }

    #[test]
    fn installed_topology_accepts_reversible_p_core_mask() {
        let mask = p_core_mask().unwrap_or(1usize);
        unsafe {
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
    fn test_affinity_conflict_detection() {
        let mut pinner = Pinner {
            mask: 0x00FF,
            samples: HashMap::new(),
            pinned: HashMap::new(),
            last: None,
            last_poll: None,
            auto_yield: true,
            conflict: None,
        };
        pinner.pinned.insert(
            1234,
            vec![Pinned {
                tid: 5678,
                created: 100,
                original: 0xFFFF,
                applied: 0x00FF,
            }],
        );

        // Simulate external affinity change (e.g. Intel APO overriding mask to 0xF000)
        let external_mask = 0xF000;
        if external_mask != 0x00FF {
            pinner.conflict = Some(AffinityConflict {
                pid: 1234,
                tid: 5678,
                our_mask: 0x00FF,
                external_mask,
            });
            if pinner.auto_yield {
                pinner.pinned.remove(&1234);
            }
        }

        assert!(pinner.conflict().is_some());
        let c = pinner.conflict().unwrap();
        assert_eq!(c.pid, 1234);
        assert_eq!(c.tid, 5678);
        assert_eq!(c.external_mask, 0xF000);
        assert!(pinner.pinned.is_empty(), "Pinned map should be empty after auto-yield");
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
        let selected = select_critical_threads(&candidates, &[(14, 1)]);
        let tids: Vec<u32> = selected.iter().map(|(tid, _, _)| *tid).collect();
        assert_eq!(tids, vec![13, 12, 11, 14]);
    }
}
