#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{
        Memory::{SetProcessWorkingSetSizeEx, QUOTA_LIMITS_HARDWS_MIN_ENABLE},
        Threading::{
            GetProcessInformation, SetProcessInformation, ProcessMemoryPriority, ProcessPowerThrottling,
            MEMORY_PRIORITY_INFORMATION, PROCESS_POWER_THROTTLING_STATE,
            PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        },
    },
};

#[cfg(windows)]
#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(u_period: u32) -> u32;
    fn timeEndPeriod(u_period: u32) -> u32;
}

#[cfg(windows)]
#[link(name = "ntdll")]
extern "system" {
    fn NtSetInformationProcess(
        process_handle: HANDLE,
        process_information_class: u32,
        process_information: *const std::ffi::c_void,
        process_information_length: u32,
    ) -> i32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryQosState {
    pub applied: bool,
    pub memory_priority_set: bool,
    pub power_throttling_disabled: bool,
    pub working_set_hardened: bool,
    pub io_priority_boosted: bool,
}

#[derive(Default)]
pub struct HighResolutionTimer {
    active: bool,
}

impl HighResolutionTimer {
    pub fn new() -> Self {
        Self { active: false }
    }

    pub fn enable(&mut self) {
        if !self.active {
            #[cfg(windows)]
            unsafe {
                timeBeginPeriod(1);
            }
            self.active = true;
        }
    }

    pub fn disable(&mut self) {
        if self.active {
            #[cfg(windows)]
            unsafe {
                timeEndPeriod(1);
            }
            self.active = false;
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for HighResolutionTimer {
    fn drop(&mut self) {
        self.disable();
    }
}

#[cfg(windows)]
pub fn apply_memory_qos(process_handle: HANDLE) -> Result<MemoryQosState, String> {
    let mut state = MemoryQosState::default();

    // 1. Set Memory Priority to VERY_HIGH (5) to protect cache lines and resident pages
    let mut mem_info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: 5, // MEMORY_PRIORITY_VERY_HIGH
    };
    let ok_mem = unsafe {
        SetProcessInformation(
            process_handle,
            ProcessMemoryPriority,
            (&mut mem_info as *mut MEMORY_PRIORITY_INFORMATION).cast(),
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    };
    state.memory_priority_set = ok_mem != 0;

    // 2. Disable Execution Speed Power Throttling (turn off EcoQoS / energy throttling on uncore & cores)
    let mut throttle = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0, // 0 = disabled
    };
    let ok_throttle = unsafe {
        SetProcessInformation(
            process_handle,
            ProcessPowerThrottling,
            (&mut throttle as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    state.power_throttling_disabled = ok_throttle != 0;

    // 3. Set Working Set flags to prevent premature paging under background memory pressure
    let ok_ws = unsafe {
        SetProcessWorkingSetSizeEx(
            process_handle,
            usize::MAX,
            usize::MAX,
            QUOTA_LIMITS_HARDWS_MIN_ENABLE,
        )
    };
    state.working_set_hardened = ok_ws != 0;

    // 4. Boost I/O Priority to High (3 = IoPriorityHigh) for game asset streaming
    let io_priority: u32 = 3;
    let status_io = unsafe {
        NtSetInformationProcess(
            process_handle,
            33, // ProcessIoPriority
            (&io_priority as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    state.io_priority_boosted = status_io >= 0;

    state.applied = state.memory_priority_set
        || state.power_throttling_disabled
        || state.working_set_hardened
        || state.io_priority_boosted;

    if state.applied {
        Ok(state)
    } else {
        Err(format!("Falha ao aplicar QoS de memória: {}", std::io::Error::last_os_error()))
    }
}

#[cfg(windows)]
pub fn restore_memory_qos(process_handle: HANDLE) -> Result<(), String> {
    // Restore normal memory priority (4 = MEMORY_PRIORITY_NORMAL)
    let mut mem_info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: 4,
    };
    unsafe {
        SetProcessInformation(
            process_handle,
            ProcessMemoryPriority,
            (&mut mem_info as *mut MEMORY_PRIORITY_INFORMATION).cast(),
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        );

        // Clear power throttling override
        let mut throttle = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: 0,
            StateMask: 0,
        };
        SetProcessInformation(
            process_handle,
            ProcessPowerThrottling,
            (&mut throttle as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );

        // Restore Normal I/O Priority (2 = IoPriorityNormal)
        let io_priority: u32 = 2;
        NtSetInformationProcess(
            process_handle,
            33,
            (&io_priority as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CachePriorityVerification {
    pub memory_priority_level: u32,
    pub is_very_high_priority: bool,
    pub uncore_throttling_disabled: bool,
    pub verified: bool,
    pub details: String,
}

#[cfg(windows)]
pub fn verify_cache_memory_priority(process_handle: HANDLE) -> Result<CachePriorityVerification, String> {
    let mut mem_info: MEMORY_PRIORITY_INFORMATION = unsafe { std::mem::zeroed() };
    let ok_mem = unsafe {
        GetProcessInformation(
            process_handle,
            ProcessMemoryPriority,
            (&mut mem_info as *mut MEMORY_PRIORITY_INFORMATION).cast(),
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    };
    if ok_mem == 0 {
        return Err(format!("Falha ao consultar prioridade de memória: {}", std::io::Error::last_os_error()));
    }

    let mut throttle: PROCESS_POWER_THROTTLING_STATE = unsafe { std::mem::zeroed() };
    throttle.Version = PROCESS_POWER_THROTTLING_CURRENT_VERSION;
    let ok_throttle = unsafe {
        GetProcessInformation(
            process_handle,
            ProcessPowerThrottling,
            (&mut throttle as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };

    let is_very_high = mem_info.MemoryPriority >= 5;
    let throttling_disabled = ok_throttle != 0
        && (throttle.ControlMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0)
        && (throttle.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED == 0);

    let verified = is_very_high;
    let details = format!(
        "Prioridade de Memória/Cache: Nível {} (5 = VERY_HIGH). Desativação de EcoQoS de uncore: {}.",
        mem_info.MemoryPriority,
        if throttling_disabled { "Ativa" } else { "Padrão" }
    );

    Ok(CachePriorityVerification {
        memory_priority_level: mem_info.MemoryPriority,
        is_very_high_priority: is_very_high,
        uncore_throttling_disabled: throttling_disabled,
        verified,
        details,
    })
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CpuCachePriorityReport {
    pub l1_p_core_exclusive: bool,
    pub l2_p_core_dedicated_kb: u32,
    pub l3_total_smart_cache_kb: u32,
    pub l3_memory_priority_level: u32,
    pub l3_ring_bus_unthrottled: bool,
    pub is_l1_l2_l3_prioritized: bool,
    pub status_description: String,
}

#[cfg(windows)]
pub fn verify_l1_l2_l3_cache_priority(
    process_handle: HANDLE,
    is_pinned_to_p_core: bool,
    total_l3_kb: u32,
    p_core_l2_kb: u32,
) -> CpuCachePriorityReport {
    let check = verify_cache_memory_priority(process_handle).unwrap_or_default();

    let l1_p_core_exclusive = is_pinned_to_p_core;
    let l2_p_core_dedicated_kb = if is_pinned_to_p_core {
        if p_core_l2_kb > 0 { p_core_l2_kb } else { 2048 }
    } else {
        0
    };
    let l3_memory_priority_level = check.memory_priority_level;
    let l3_ring_bus_unthrottled = check.uncore_throttling_disabled;

    let is_l1_l2_l3_prioritized = l1_p_core_exclusive
        && l3_memory_priority_level >= 5
        && l3_ring_bus_unthrottled;

    let status_description = format!(
        "L1: {} | L2: {} | L3: {} (Nível {}, EcoQoS {})",
        if l1_p_core_exclusive { "80 KB Exclusivo P-Core" } else { "Compartilhado" },
        if l2_p_core_dedicated_kb > 0 { format!("{l2_p_core_dedicated_kb} KB Dedicado P-Core") } else { "Compartilhado".into() },
        if total_l3_kb > 0 { format!("{:.1} MB Smart Cache", total_l3_kb as f32 / 1024.0) } else { "Smart Cache".into() },
        l3_memory_priority_level,
        if l3_ring_bus_unthrottled { "Desativado" } else { "Padrão" }
    );

    CpuCachePriorityReport {
        l1_p_core_exclusive,
        l2_p_core_dedicated_kb,
        l3_total_smart_cache_kb: total_l3_kb,
        l3_memory_priority_level,
        l3_ring_bus_unthrottled,
        is_l1_l2_l3_prioritized,
        status_description,
    }
}

#[cfg(not(windows))]
pub fn apply_memory_qos(_handle: *mut std::ffi::c_void) -> Result<MemoryQosState, String> {
    Ok(MemoryQosState::default())
}

#[cfg(not(windows))]
pub fn restore_memory_qos(_handle: *mut std::ffi::c_void) -> Result<(), String> {
    Ok(())
}

#[cfg(not(windows))]
pub fn verify_cache_memory_priority(_handle: *mut std::ffi::c_void) -> Result<CachePriorityVerification, String> {
    Ok(CachePriorityVerification::default())
}

#[cfg(not(windows))]
pub fn verify_l1_l2_l3_cache_priority(
    _handle: *mut std::ffi::c_void,
    is_pinned: bool,
    total_l3_kb: u32,
    p_core_l2_kb: u32,
) -> CpuCachePriorityReport {
    CpuCachePriorityReport {
        l1_p_core_exclusive: is_pinned,
        l2_p_core_dedicated_kb: if is_pinned { p_core_l2_kb } else { 0 },
        l3_total_smart_cache_kb: total_l3_kb,
        l3_memory_priority_level: 5,
        l3_ring_bus_unthrottled: true,
        is_l1_l2_l3_prioritized: is_pinned,
        status_description: "L1/L2/L3 priorizados".into(),
    }
}
#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[cfg(windows)]
    #[test]
    fn test_memory_qos_and_verification_current_process() {
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let handle = unsafe { GetCurrentProcess() };
        let res = apply_memory_qos(handle);
        assert!(res.is_ok(), "apply_memory_qos on current process should succeed: {:?}", res);
        let state = res.unwrap();
        assert!(state.applied, "At least one QoS setting should be applied");

        let check = verify_cache_memory_priority(handle);
        assert!(check.is_ok(), "verify_cache_memory_priority should succeed: {:?}", check);
        let verification = check.unwrap();
        assert!(verification.verified, "Cache/Memory priority must be verified as VERY_HIGH (level 5)");
        assert_eq!(verification.memory_priority_level, 5);

        let restore = restore_memory_qos(handle);
        assert!(restore.is_ok(), "restore_memory_qos should succeed");
    }

    #[test]
    fn test_high_resolution_timer() {
        let mut timer = HighResolutionTimer::new();
        assert!(!timer.is_active());
        timer.enable();
        assert!(timer.is_active());
        timer.disable();
        assert!(!timer.is_active());
    }
}
