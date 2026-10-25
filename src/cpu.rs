use windows_sys::Win32::System::Threading::{GetActiveProcessorCount, ALL_PROCESSOR_GROUPS};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorTime {
    idle: i64,
    kernel: i64,
    user: i64,
    dpc: i64,
    interrupt: i64,
    interrupt_count: u32,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        class: u32,
        info: *mut ProcessorTime,
        size: u32,
        returned: *mut u32,
    ) -> i32;
}

fn usage(old: ProcessorTime, now: ProcessorTime) -> f32 {
    let elapsed = (now.kernel - old.kernel + now.user - old.user).max(0);
    let idle = (now.idle - old.idle).max(0);
    if elapsed == 0 {
        0.0
    } else {
        (1.0 - idle as f32 / elapsed as f32).clamp(0.0, 1.0)
    }
}

pub struct Monitor {
    previous: Vec<ProcessorTime>,
    pub load: Vec<f32>,
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            previous: Vec::new(),
            load: Vec::new(),
        }
    }

    pub fn refresh(&mut self) -> Result<(), String> {
        let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as usize;
        if count == 0 || count > 4096 {
            return Err("Contagem de processadores indisponível".into());
        }
        let mut now = vec![ProcessorTime::default(); count];
        let mut returned = 0;
        // SystemProcessorPerformanceInformation (8) reports one sample per logical processor.
        let status = unsafe {
            NtQuerySystemInformation(
                8,
                now.as_mut_ptr(),
                (count * std::mem::size_of::<ProcessorTime>()) as u32,
                &mut returned,
            )
        };
        if status < 0 || returned as usize != count * std::mem::size_of::<ProcessorTime>() {
            return Err(format!(
                "Falha ao consultar uso por núcleo lógico (NTSTATUS {status:#x})"
            ));
        }
        if self.previous.len() == count {
            self.load = self
                .previous
                .iter()
                .zip(&now)
                .map(|(&old, &new)| usage(old, new))
                .collect();
        } else {
            self.load = vec![0.0; count];
        }
        self.previous = now;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_and_busy_intervals() {
        let old = ProcessorTime {
            kernel: 100,
            user: 100,
            idle: 80,
            ..Default::default()
        };
        let half = ProcessorTime {
            kernel: 180,
            user: 120,
            idle: 130,
            ..Default::default()
        };
        let busy = ProcessorTime {
            kernel: 200,
            user: 200,
            idle: 80,
            ..Default::default()
        };
        assert!((usage(old, half) - 0.5).abs() < 0.001);
        assert_eq!(usage(old, busy), 1.0);
        assert_eq!(usage(old, old), 0.0);
    }
    #[test]
    fn samples_all_active_processors_on_windows() {
        let mut monitor = Monitor::new();
        monitor.refresh().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(60));
        monitor.refresh().unwrap();
        assert_eq!(
            monitor.load.len(),
            unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as usize
        );
        assert!(monitor.load.iter().all(|value| (0.0..=1.0).contains(value)));
    }
}
