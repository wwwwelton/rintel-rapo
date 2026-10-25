#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE},
    System::Threading::{
        GetProcessInformation, GetProcessTimes, SetProcessInformation, ProcessMemoryPriority,
        ProcessPowerThrottling, MEMORY_PRIORITY_INFORMATION, PROCESS_POWER_THROTTLING_STATE,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    },
};

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryQosState {
    original_memory_priority: u32,
    original_throttle_control: u32,
    original_throttle_state: u32,
    created: u64,
}


#[cfg(windows)]
fn process_created(handle: HANDLE) -> Result<u64, String> {
    let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
        unsafe { std::mem::zeroed() };
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(format!("Falha ao identificar processo: {}", std::io::Error::last_os_error()));
    }
    Ok(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

#[cfg(windows)]
pub fn matches_process(handle: HANDLE, state: &MemoryQosState) -> Result<bool, String> {
    Ok(process_created(handle)? == state.created)
}

#[cfg(windows)]
fn memory_priority(handle: HANDLE) -> Result<MEMORY_PRIORITY_INFORMATION, String> {
    let mut memory = MEMORY_PRIORITY_INFORMATION { MemoryPriority: 0 };
    if unsafe {
        GetProcessInformation(
            handle,
            ProcessMemoryPriority,
            (&mut memory as *mut MEMORY_PRIORITY_INFORMATION).cast(),
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    } == 0 {
        return Err(format!("Falha ao ler prioridade de memória: {}", std::io::Error::last_os_error()));
    }
    Ok(memory)
}

#[cfg(windows)]
fn power_throttling(handle: HANDLE) -> Result<PROCESS_POWER_THROTTLING_STATE, String> {
    let mut power = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    if unsafe {
        GetProcessInformation(
            handle,
            ProcessPowerThrottling,
            (&mut power as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    } == 0 {
        return Err(format!("Falha ao ler EcoQoS: {}", std::io::Error::last_os_error()));
    }
    Ok(power)
}

#[cfg(windows)]
pub fn apply_memory_qos(handle: HANDLE) -> Result<MemoryQosState, String> {
    let created = process_created(handle)?;
    let original_memory = memory_priority(handle)?;
    let original_power = power_throttling(handle)?;
    let mut memory = MEMORY_PRIORITY_INFORMATION { MemoryPriority: 5 };
    if original_memory.MemoryPriority != 5
        && unsafe {
            SetProcessInformation(
                handle, ProcessMemoryPriority, (&mut memory as *mut MEMORY_PRIORITY_INFORMATION).cast(),
                std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            )
        } == 0
    {
        return Err(format!("Falha ao ajustar memória: {}", std::io::Error::last_os_error()));
    }
    let mut power = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: original_power.ControlMask | PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: original_power.StateMask & !PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    if unsafe {
        SetProcessInformation(
            handle, ProcessPowerThrottling, (&mut power as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    } == 0 {
        let error = std::io::Error::last_os_error();
        if original_memory.MemoryPriority != 5 {
            let mut previous = original_memory;
            if unsafe {
                SetProcessInformation(
                    handle, ProcessMemoryPriority, (&mut previous as *mut MEMORY_PRIORITY_INFORMATION).cast(),
                    std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
                )
            } == 0 {
                return Err(format!("Falha ao ajustar EcoQoS ({error}) e restaurar memória: {}", std::io::Error::last_os_error()));
            }
        }
        return Err(format!("Falha ao ajustar EcoQoS: {error}"));
    }
    Ok(MemoryQosState {
        original_memory_priority: original_memory.MemoryPriority,
        original_throttle_control: original_power.ControlMask,
        original_throttle_state: original_power.StateMask,
        created,
    })
}

#[cfg(windows)]
pub fn restore_memory_qos(handle: HANDLE, state: &MemoryQosState) -> Result<(), String> {
    if !matches_process(handle, state)? {
        return Ok(()); // PID reused: never write to a different process.
    }
    let current_memory = memory_priority(handle)?;
    let current_power = power_throttling(handle)?;
    let mut error = None;
    if current_memory.MemoryPriority == 5 && state.original_memory_priority != 5 {
        let mut memory = MEMORY_PRIORITY_INFORMATION { MemoryPriority: state.original_memory_priority };
        if unsafe {
            SetProcessInformation(
                handle, ProcessMemoryPriority, (&mut memory as *mut MEMORY_PRIORITY_INFORMATION).cast(),
                std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            )
        } == 0 {
            error = Some(format!("Falha ao restaurar memória: {}", std::io::Error::last_os_error()));
        }
    }
    if current_power.ControlMask == state.original_throttle_control | PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        && current_power.StateMask == state.original_throttle_state & !PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        && (current_power.ControlMask != state.original_throttle_control
            || current_power.StateMask != state.original_throttle_state)
    {
        let mut power = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: state.original_throttle_control,
            StateMask: state.original_throttle_state,
        };
        if unsafe {
            SetProcessInformation(
                handle, ProcessPowerThrottling, (&mut power as *mut PROCESS_POWER_THROTTLING_STATE).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        } == 0 {
            error = Some(format!("Falha ao restaurar EcoQoS: {}", std::io::Error::last_os_error()));
        }
    }
    error.map_or(Ok(()), Err)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn test_memory_qos_and_verification_current_process() {
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let handle = unsafe { GetCurrentProcess() };
        let before = memory_priority(handle).unwrap().MemoryPriority;
        let power_before = power_throttling(handle).unwrap();
        let state = apply_memory_qos(handle).expect("apply_memory_qos on current process");
        assert_eq!(power_throttling(handle).unwrap().StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED, 0);
        let mut recycled = state;
        recycled.created += 1;
        assert!(!matches_process(handle, &recycled).unwrap());
        restore_memory_qos(handle, &recycled).unwrap();
        assert_eq!(memory_priority(handle).unwrap().MemoryPriority, 5);
        restore_memory_qos(handle, &state).expect("restore_memory_qos");
        assert_eq!(memory_priority(handle).unwrap().MemoryPriority, before);
        let power_after = power_throttling(handle).unwrap();
        assert_eq!((power_after.ControlMask, power_after.StateMask),
                   (power_before.ControlMask, power_before.StateMask));
    }

}
