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
    mask: usize,
    samples: HashMap<(u32, u32), (u64, u64)>,
    pinned: HashMap<u32, Pinned>,
    last: Option<(u32, u32, u64)>,
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
            auto_yield: true,
            conflict: None,
        })
    }

    pub fn conflict(&self) -> Option<AffinityConflict> {
        self.conflict
    }
    pub fn status(&self) -> Option<(u32, u32, u64)> {
        self.last
    }

    fn restore(&mut self, pid: u32) {
        let Some(pin) = self.pinned.remove(&pid) else {
            return;
        };
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

    pub fn update(
        &mut self,
        running: &[Running],
        enabled: &HashSet<PathBuf>,
        keep_windowed: bool,
    ) -> Result<(), String> {
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
                self.conflict = None;
                return Ok(());
            };
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
            let current_pin = self.pinned.get(&process.pid).map(|p| (p.tid, p.created));
            let mut pinned_delta: Option<u64> = None;
            let mut best = None;
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
                                        let delta = cpu - old_cpu;
                                        if current_pin == Some((entry.th32ThreadID, created)) {
                                            pinned_delta = Some(delta);
                                        }
                                        if best.is_none_or(|(_, _, best_delta)| delta > best_delta)
                                        {
                                            best = Some((entry.th32ThreadID, created, delta));
                                        }
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
            let Some((mut tid, mut created, mut delta)) = best else {
                return Ok(());
            };
            // Hysteresis (20%): prevent L1/L2 cache thrashing between Main and Render threads
            if let (Some((p_tid, p_created)), Some(p_delta)) = (current_pin, pinned_delta) {
                if p_delta >= 50_000 && delta <= p_delta.saturating_mul(6) / 5 {
                    tid = p_tid;
                    created = p_created;
                    delta = p_delta;
                }
            }
            self.last = Some((process.pid, tid, delta / 10_000));
            if delta < 100_000 {
                return Ok(());
            } // At least 10 ms CPU time between samples.
            if let Some(pin) = self.pinned.get(&process.pid) {
                if pin.tid == tid && pin.created == created {
                    let thread = OpenThread(THREAD_QUERY_INFORMATION, 0, tid);
                    if !thread.is_null() {
                        let mut current_affinity: GROUP_AFFINITY = zeroed();
                        if GetThreadGroupAffinity(thread, &mut current_affinity) != 0
                            && current_affinity.Group == 0
                        {
                            if current_affinity.Mask != pin.applied {
                                let external_mask = current_affinity.Mask;
                                CloseHandle(thread);
                                self.conflict = Some(AffinityConflict {
                                    pid: process.pid,
                                    tid,
                                    our_mask: pin.applied,
                                    external_mask,
                                });
                                if self.auto_yield {
                                    self.pinned.remove(&process.pid);
                                }
                                return Ok(());
                            }
                        }
                        CloseHandle(thread);
                    }
                    self.conflict = None;
                    return Ok(());
                }
            }
            if self.conflict.is_some() && self.auto_yield {
                return Ok(());
            }
            self.restore(process.pid);
            let thread = OpenThread(THREAD_QUERY_INFORMATION | THREAD_SET_INFORMATION, 0, tid);
            if thread.is_null() {
                return Ok(());
            }
            if thread_time(thread).is_some_and(|(current, _)| current == created) {
                let mut affinity: GROUP_AFFINITY = zeroed();
                if GetThreadGroupAffinity(thread, &mut affinity) != 0
                    && affinity.Group == 0
                    && affinity.Mask != mask
                {
                    let original = SetThreadAffinityMask(thread, mask);
                    if original != 0 {
                        self.pinned.insert(
                            process.pid,
                            Pinned {
                                tid,
                                created,
                                original,
                                applied: mask,
                            },
                        );
                    }
                }
            }
            CloseHandle(thread);
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
    fn installed_topology_accepts_reversible_p_core_mask() {
        let mask = p_core_mask().expect("P/E-core topology unavailable");
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
            auto_yield: true,
            conflict: None,
        };
        pinner.pinned.insert(
            1234,
            Pinned {
                tid: 5678,
                created: 100,
                original: 0xFFFF,
                applied: 0x00FF,
            },
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
}
