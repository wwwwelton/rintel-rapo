#[cfg(windows)]
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
    Arc,
};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct ProcessTelemetry {
    pub has_target: bool,
    pub target_name: String,
    pub target_pid: u32,
    pub cpu_usage_pct: f32,
    pub sys_cpu_pct: f32,
    pub gpu_usage_pct: f32,
    pub mem_used_mb: f32,
    pub mem_usage_pct: f32,
    pub disk_read_mbs: f32,
    pub disk_total_read_gb: f32,
    pub net_usage_kbs: f32,
    pub ping_ms: u32,
    pub process_ping: crate::process_ping::ProcessPing,
}

#[cfg(windows)]
mod win_ffi {
    use windows_sys::Win32::Foundation::{FILETIME, HANDLE};

    #[repr(C)]
    pub struct MemoryStatusEx {
        pub dw_length: u32,
        pub dw_memory_load: u32,
        pub ull_total_phys: u64,
        pub ull_avail_phys: u64,
        pub ull_total_page_file: u64,
        pub ull_avail_page_file: u64,
        pub ull_total_virtual: u64,
        pub ull_avail_virtual: u64,
        pub ull_avail_extended_virtual: u64,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct ProcessMemoryCountersEx {
        pub cb: u32,
        pub page_fault_count: u32,
        pub peak_working_set_size: usize,
        pub working_set_size: usize,
        pub quota_peak_paged_pool_usage: usize,
        pub quota_paged_pool_usage: usize,
        pub quota_peak_non_paged_pool_usage: usize,
        pub quota_non_paged_pool_usage: usize,
        pub pagefile_usage: usize,
        pub peak_pagefile_usage: usize,
        pub private_usage: usize,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct IoCounters {
        pub read_operation_count: u64,
        pub write_operation_count: u64,
        pub other_operation_count: u64,
        pub read_transfer_count: u64,
        pub write_transfer_count: u64,
        pub other_transfer_count: u64,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
        pub fn GetProcessIoCounters(h_process: HANDLE, lp_io_counters: *mut IoCounters) -> i32;
        pub fn GetProcessTimes(
            h_process: HANDLE,
            lp_creation_time: *mut FILETIME,
            lp_exit_time: *mut FILETIME,
            lp_kernel_time: *mut FILETIME,
            lp_user_time: *mut FILETIME,
        ) -> i32;
        pub fn K32GetProcessMemoryInfo(
            process: HANDLE,
            ppsmem_counters: *mut ProcessMemoryCountersEx,
            cb: u32,
        ) -> i32;
    }

    #[repr(C)]
    pub struct PdhFmtCountervalue {
        pub c_status: u32,
        pub double_value: f64,
    }

    #[repr(C)]
    pub struct PdhFmtCountervalueItemW {
        pub sz_name: *const u16,
        pub fmt_value: PdhFmtCountervalue,
    }

    pub const PDH_FMT_DOUBLE: u32 = 0x0000_0200;

    #[link(name = "pdh")]
    extern "system" {
        pub fn PdhOpenQueryW(
            sz_data_source: *const u16,
            dw_user_data: usize,
            ph_query: *mut isize,
        ) -> u32;
        pub fn PdhAddEnglishCounterW(
            h_query: isize,
            sz_full_counter_path: *const u16,
            dw_user_data: usize,
            ph_counter: *mut isize,
        ) -> u32;
        pub fn PdhCollectQueryData(h_query: isize) -> u32;
        pub fn PdhGetFormattedCounterValue(
            h_counter: isize,
            dw_format: u32,
            lpdw_type: *mut u32,
            p_value: *mut PdhFmtCountervalue,
        ) -> u32;
        pub fn PdhGetFormattedCounterArrayW(
            h_counter: isize,
            dw_format: u32,
            lpdw_buffer_size: *mut u32,
            lpdw_item_count: *mut u32,
            item_buffer: *mut u8,
        ) -> u32;
        pub fn PdhCloseQuery(h_query: isize) -> u32;
    }

}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn ft_to_u64(ft: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

#[cfg(windows)]
#[derive(Clone, Copy, Default)]
struct ProcSample {
    created: u64,
    cpu_ticks: u64,
    read_bytes: u64,
    timestamp: Option<Instant>,
}

#[cfg(any(test, windows))]
fn read_rate_mbs(previous: u64, current: u64, elapsed_secs: f64) -> f32 {
    current.checked_sub(previous).map_or(0.0, |bytes| {
        (bytes as f64 / (1024.0 * 1024.0) / elapsed_secs) as f32
    })
}

#[derive(Clone, PartialEq, Eq)]
struct Target {
    name: String,
    pids: Vec<u32>,
}

struct Request {
    generation: u64,
    target: Option<Target>,
    num_cpus: usize,
    sys_cpu: f32,
}

struct Snapshot {
    generation: u64,
    metrics: ProcessTelemetry,
}

// Each slot owns a Box; only the thread swapping a pointer out may reclaim it.
#[derive(Default)]
struct Mailbox {
    request: AtomicPtr<Request>,
    snapshot: AtomicPtr<Snapshot>,
}

impl Mailbox {
    fn request(&self, request: Request) {
        let previous = self.request.swap(Box::into_raw(Box::new(request)), Ordering::AcqRel);
        if !previous.is_null() {
            // A swap transfers exclusive ownership of the displaced allocation.
            drop(unsafe { Box::from_raw(previous) });
        }
    }

    fn take_request(&self) -> Option<Box<Request>> {
        let request = self.request.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if request.is_null() { None } else { Some(unsafe { Box::from_raw(request) }) }
    }

    fn snapshot(&self, snapshot: Snapshot) {
        let previous = self.snapshot.swap(Box::into_raw(Box::new(snapshot)), Ordering::AcqRel);
        if !previous.is_null() {
            drop(unsafe { Box::from_raw(previous) });
        }
    }

    fn take_snapshot(&self) -> Option<Box<Snapshot>> {
        let snapshot = self.snapshot.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if snapshot.is_null() { None } else { Some(unsafe { Box::from_raw(snapshot) }) }
    }
}

impl Drop for Mailbox {
    fn drop(&mut self) {
        drop(self.take_request());
        drop(self.take_snapshot());
    }
}

/// Hands off samples without waiting for native collection. Until a matching sample
/// completes, a changed target displays cleared global metrics rather than old game data.
/// One pending request and one completed snapshot are retained; newer values replace older ones.
/// Drop only signals cancellation; the worker closes PDH after any in-flight call returns.
pub struct TelemetryCollector {
    pub metrics: ProcessTelemetry,
    ping_ms: Arc<AtomicU32>,
    stop_flag: Arc<AtomicBool>,
    mailbox: Arc<Mailbox>,
    worker: std::thread::Thread,
    target: Option<Target>,
    generation: u64,
}

impl Default for TelemetryCollector {
    fn default() -> Self { Self::new() }
}

impl TelemetryCollector {
    pub fn new() -> Self {
        let ping_ms = Arc::new(AtomicU32::new(0));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let ping_clone = Arc::clone(&ping_ms);
        let stop_clone = Arc::clone(&stop_flag);

        std::thread::spawn(move || {
            let fallbacks: [std::net::SocketAddr; 2] = [
                "1.1.1.1:53".parse().unwrap(),
                "8.8.8.8:53".parse().unwrap(),
            ];
            while !stop_clone.load(Ordering::Relaxed) {
                let mut measured_ms = 0u32;
                for target in &fallbacks {
                    let start = Instant::now();
                    if std::net::TcpStream::connect_timeout(
                        target,
                        std::time::Duration::from_millis(400),
                    )
                    .is_ok()
                    {
                        measured_ms = (start.elapsed().as_millis() as u32).max(1);
                        break;
                    }
                }
                ping_clone.store(measured_ms, Ordering::Relaxed);

                for _ in 0..15 {
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });

        let mailbox = Arc::new(Mailbox::default());
        let worker_mailbox = Arc::clone(&mailbox);
        let worker_stop = Arc::clone(&stop_flag);
        let worker = std::thread::spawn(move || {
            // The worker alone constructs, uses, and closes the native query.
            let mut sampler = Sampler::new();
            let mut process_ping = crate::process_ping::ProcessPingSampler::default();
            let mut ping_generation = 0;
            while !worker_stop.load(Ordering::Relaxed) {
                let request = worker_mailbox.take_request();
                let Some(request) = request else {
                    std::thread::park();
                    continue;
                };
                if ping_generation != request.generation {
                    process_ping = crate::process_ping::ProcessPingSampler::default();
                    ping_generation = request.generation;
                }
                sampler.refresh(
                    request.num_cpus,
                    request.sys_cpu,
                    request.target.as_ref().map(|target| (target.name.as_str(), target.pids.as_slice())),
                );
                sampler.metrics.process_ping = process_ping.sample(
                    request.target.as_ref().filter(|_| sampler.metrics.has_target)
                        .map_or(&[][..], |target| target.pids.as_slice()),
                );
                if worker_stop.load(Ordering::Relaxed) {
                    break;
                }
                let snapshot = Snapshot {
                    generation: request.generation,
                    metrics: std::mem::take(&mut sampler.metrics),
                };
                worker_mailbox.snapshot(snapshot);
            }
        }).thread().clone();

        Self {
            metrics: ProcessTelemetry::default(),
            ping_ms,
            stop_flag,
            mailbox,
            worker,
            target: None,
            generation: 0,
        }
    }

    pub fn refresh(&mut self, cpu_loads: &[f32], target_game: Option<(&str, &[u32])>) {
        let target_game = target_game.filter(|(_, pids)| !pids.is_empty());
        let same_target = match (&self.target, target_game) {
            (Some(target), Some((name, pids))) => target.name == name && target.pids.as_slice() == pids,
            (None, None) => true,
            _ => false,
        };
        if !same_target {
            self.target = target_game.map(|(name, pids)| Target {
                name: name.to_owned(),
                pids: pids.to_vec(),
            });
            self.generation = self.generation.wrapping_add(1);
            self.metrics = ProcessTelemetry::default();
        }
        let sys_cpu = if cpu_loads.is_empty() {
            0.0
        } else {
            (cpu_loads.iter().sum::<f32>() / cpu_loads.len() as f32 * 100.0).clamp(0.0, 100.0)
        };
        let snapshot = self.mailbox.take_snapshot();
        self.mailbox.request(Request {
            generation: self.generation,
            target: self.target.clone(),
            num_cpus: cpu_loads.len().max(1),
            sys_cpu,
        });
        self.worker.unpark();
        if let Some(snapshot) = snapshot.filter(|snapshot| snapshot.generation == self.generation) {
            self.metrics = snapshot.metrics;
        }
        if !self.metrics.has_target {
            if self.metrics.target_name != "Sistema Global" {
                self.metrics.target_name = "Sistema Global".into();
            }
            self.metrics.cpu_usage_pct = sys_cpu;
        }
        self.metrics.sys_cpu_pct = sys_cpu;
        self.metrics.ping_ms = self.ping_ms.load(Ordering::Relaxed);
    }
}

impl Drop for TelemetryCollector {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.worker.unpark();
    }
}

struct Sampler {
    metrics: ProcessTelemetry,
    #[cfg(windows)]
    proc_samples: HashMap<u32, ProcSample>,
    #[cfg(windows)]
    pdh_query: isize,
    #[cfg(windows)]
    counter_gpu: isize,
    #[cfg(windows)]
    counter_disk_read: isize,
    #[cfg(windows)]
    counter_net_recv: isize,
    #[cfg(windows)]
    counter_net_sent: isize,
}

impl Sampler {
    fn new() -> Self {
        #[cfg(windows)]
        let (pdh_query, counter_gpu, counter_disk_read, counter_net_recv, counter_net_sent) = unsafe {
            let mut query: isize = 0;
            let mut c_gpu: isize = 0;
            let mut c_disk: isize = 0;
            let mut c_net_in: isize = 0;
            let mut c_net_out: isize = 0;

            if win_ffi::PdhOpenQueryW(std::ptr::null(), 0, &mut query) == 0 && query != 0 {
                let gpu_path = to_wide("\\GPU Engine(*)\\Utilization Percentage");
                let disk_path = to_wide("\\PhysicalDisk(_Total)\\Disk Read Bytes/sec");
                let net_in_path = to_wide("\\Network Interface(*)\\Bytes Received/sec");
                let net_out_path = to_wide("\\Network Interface(*)\\Bytes Sent/sec");

                let _ = win_ffi::PdhAddEnglishCounterW(query, gpu_path.as_ptr(), 0, &mut c_gpu);
                let _ = win_ffi::PdhAddEnglishCounterW(query, disk_path.as_ptr(), 0, &mut c_disk);
                let _ = win_ffi::PdhAddEnglishCounterW(query, net_in_path.as_ptr(), 0, &mut c_net_in);
                let _ = win_ffi::PdhAddEnglishCounterW(query, net_out_path.as_ptr(), 0, &mut c_net_out);
                let _ = win_ffi::PdhCollectQueryData(query);
            }
            (query, c_gpu, c_disk, c_net_in, c_net_out)
        };

        Self {
            metrics: ProcessTelemetry::default(),
            #[cfg(windows)]
            proc_samples: HashMap::new(),
            #[cfg(windows)]
            pdh_query,
            #[cfg(windows)]
            counter_gpu,
            #[cfg(windows)]
            counter_disk_read,
            #[cfg(windows)]
            counter_net_recv,
            #[cfg(windows)]
            counter_net_sent,
        }
    }

    #[cfg(windows)]
    unsafe fn read_gpu_usage_for_pids(counter: isize, target_pids: &[u32]) -> f32 {
        if counter == 0 {
            return 0.0;
        }
        let mut buf_size: u32 = 0;
        let mut item_count: u32 = 0;
        let _ = win_ffi::PdhGetFormattedCounterArrayW(
            counter,
            win_ffi::PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut item_count,
            std::ptr::null_mut(),
        );
        if buf_size == 0 || item_count == 0 {
            return 0.0;
        }
        let mut buffer = vec![0u8; buf_size as usize];
        if win_ffi::PdhGetFormattedCounterArrayW(
            counter,
            win_ffi::PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut item_count,
            buffer.as_mut_ptr(),
        ) != 0
        {
            return 0.0;
        }

        let items = std::slice::from_raw_parts(
            buffer.as_ptr().cast::<win_ffi::PdhFmtCountervalueItemW>(),
            item_count as usize,
        );

        let prefixes: Vec<String> = target_pids.iter().map(|pid| format!("pid_{pid}_")).collect();
        let mut pid_sum = 0.0f64;
        let mut total_sum = 0.0f64;

        for item in items {
            if item.fmt_value.c_status == 0 && item.fmt_value.double_value.is_finite() {
                let val = item.fmt_value.double_value;
                total_sum += val;

                if !prefixes.is_empty() && !item.sz_name.is_null() {
                    let mut len = 0usize;
                    while *item.sz_name.add(len) != 0 && len < 128 {
                        len += 1;
                    }
                    let name = String::from_utf16_lossy(std::slice::from_raw_parts(item.sz_name, len));
                    if prefixes.iter().any(|p| name.starts_with(p)) {
                        pid_sum += val;
                    }
                }
            }
        }

        if target_pids.is_empty() {
            (total_sum as f32).clamp(0.0, 100.0)
        } else {
            (pid_sum as f32).clamp(0.0, 100.0)
        }
    }

    #[cfg(windows)]
    unsafe fn read_pdh_array_sum(counter: isize) -> f64 {
        if counter == 0 {
            return 0.0;
        }
        let mut buf_size: u32 = 0;
        let mut item_count: u32 = 0;
        let _ = win_ffi::PdhGetFormattedCounterArrayW(
            counter,
            win_ffi::PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut item_count,
            std::ptr::null_mut(),
        );
        if buf_size == 0 || item_count == 0 {
            return 0.0;
        }
        let mut buffer = vec![0u8; buf_size as usize];
        if win_ffi::PdhGetFormattedCounterArrayW(
            counter,
            win_ffi::PDH_FMT_DOUBLE,
            &mut buf_size,
            &mut item_count,
            buffer.as_mut_ptr(),
        ) == 0
        {
            let items = std::slice::from_raw_parts(
                buffer.as_ptr().cast::<win_ffi::PdhFmtCountervalueItemW>(),
                item_count as usize,
            );
            items
                .iter()
                .filter(|item| item.fmt_value.c_status == 0 && item.fmt_value.double_value.is_finite())
                .map(|item| item.fmt_value.double_value)
                .sum()
        } else {
            0.0
        }
    }

    #[cfg(windows)]
    unsafe fn read_pdh_single(counter: isize) -> f64 {
        if counter == 0 {
            return 0.0;
        }
        let mut val: win_ffi::PdhFmtCountervalue = std::mem::zeroed();
        let mut counter_type: u32 = 0;
        if win_ffi::PdhGetFormattedCounterValue(
            counter,
            win_ffi::PDH_FMT_DOUBLE,
            &mut counter_type,
            &mut val,
        ) == 0
            && val.c_status == 0
            && val.double_value.is_finite()
        {
            val.double_value
        } else {
            0.0
        }
    }


    fn refresh(&mut self, num_cpus: usize, sys_cpu: f32, target_game: Option<(&str, &[u32])>) {
        let num_cpus = num_cpus as f64;
        self.metrics.sys_cpu_pct = sys_cpu;

        let target_pids: &[u32] = match target_game {
            Some((name, pids)) if !pids.is_empty() => {
                self.metrics.has_target = true;
                self.metrics.target_pid = pids[0];
                if self.metrics.target_name != name {
                    self.metrics.target_name = name.to_string();
                }
                pids
            }
            _ => {
                self.metrics.has_target = false;
                self.metrics.target_pid = 0;
                if self.metrics.target_name != "Sistema Global" {
                    self.metrics.target_name = "Sistema Global".into();
                }
                &[]
            }
        };
        #[cfg(not(windows))]
        let _ = target_pids;

        #[cfg(windows)]
        self.proc_samples.retain(|pid, _| target_pids.contains(pid));
        #[cfg(windows)]
        unsafe {
            let mut mem_status: win_ffi::MemoryStatusEx = std::mem::zeroed();
            mem_status.dw_length = std::mem::size_of::<win_ffi::MemoryStatusEx>() as u32;
            let total_phys_bytes = if win_ffi::GlobalMemoryStatusEx(&mut mem_status) != 0 {
                mem_status.ull_total_phys.max(1)
            } else {
                1
            };

            if self.pdh_query != 0 {
                let _ = win_ffi::PdhCollectQueryData(self.pdh_query);
            }

            self.metrics.gpu_usage_pct = Self::read_gpu_usage_for_pids(self.counter_gpu, target_pids);

            let sys_disk_mbs =
                (Self::read_pdh_single(self.counter_disk_read) / (1024.0 * 1024.0)).max(0.0) as f32;
            let sys_net_kbs = ((Self::read_pdh_array_sum(self.counter_net_recv)
                + Self::read_pdh_array_sum(self.counter_net_sent))
                / 1024.0)
                .max(0.0) as f32;
            if !target_pids.is_empty() {
                // Process counters remain scoped to the detected game's PIDs.
                let now = Instant::now();
                let mut total_cpu_pct = 0.0f32;
                let mut total_mem_bytes = 0usize;
                let mut total_disk_rate_mbs = 0.0f32;
                let mut total_disk_read_bytes = 0u64;
                let mut primary_pid = target_pids[0];
                let mut max_proc_mem = 0usize;
                let mut any_handle_opened = false;

                for &pid in target_pids {
                    let handle = windows_sys::Win32::System::Threading::OpenProcess(
                        windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
                        0,
                        pid,
                    );
                    if handle.is_null() {
                        continue;
                    }
                    any_handle_opened = true;

                    // 1. Process CPU %
                    let (mut created, mut exited, mut kernel, mut user) = (
                        std::mem::zeroed(),
                        std::mem::zeroed(),
                        std::mem::zeroed(),
                        std::mem::zeroed(),
                    );
                    let mut cpu_ticks = 0u64;
                    if win_ffi::GetProcessTimes(
                        handle,
                        &mut created,
                        &mut exited,
                        &mut kernel,
                        &mut user,
                    ) != 0
                    {
                        cpu_ticks = ft_to_u64(kernel) + ft_to_u64(user);
                    }

                    // 2. Process RAM (WorkingSetSize)
                    let mut pmc = win_ffi::ProcessMemoryCountersEx {
                        cb: std::mem::size_of::<win_ffi::ProcessMemoryCountersEx>() as u32,
                        ..Default::default()
                    };
                    if win_ffi::K32GetProcessMemoryInfo(handle, &mut pmc, pmc.cb) != 0 {
                        total_mem_bytes = total_mem_bytes.saturating_add(pmc.working_set_size);
                        if pmc.working_set_size > max_proc_mem {
                            max_proc_mem = pmc.working_set_size;
                            primary_pid = pid;
                        }
                    }

                    // GetProcessIoCounters reports process I/O, not storage-device or network traffic.
                    let mut io = win_ffi::IoCounters::default();
                    let read_ok = win_ffi::GetProcessIoCounters(handle, &mut io) != 0;
                    if read_ok {
                        total_disk_read_bytes = total_disk_read_bytes.saturating_add(io.read_transfer_count);
                    }
                    if let Some(prev) = self.proc_samples.get(&pid).filter(|prev| prev.created == ft_to_u64(created)) {
                        if let Some(prev_time) = prev.timestamp {
                            let elapsed_secs = now.duration_since(prev_time).as_secs_f64().max(0.05);

                            if cpu_ticks >= prev.cpu_ticks {
                                let delta_ticks = (cpu_ticks - prev.cpu_ticks) as f64;
                                let wall_ticks = elapsed_secs * 10_000_000.0 * num_cpus;
                                total_cpu_pct += ((delta_ticks / wall_ticks) * 100.0) as f32;
                            }

                            if read_ok {
                                total_disk_rate_mbs += read_rate_mbs(prev.read_bytes, io.read_transfer_count, elapsed_secs);
                            }
                        }
                    }

                    if read_ok {
                        self.proc_samples.insert(
                            pid,
                            ProcSample {
                                created: ft_to_u64(created),
                                cpu_ticks,
                                read_bytes: io.read_transfer_count,
                                timestamp: Some(now),
                            },
                        );
                    }

                    windows_sys::Win32::Foundation::CloseHandle(handle);
                }

                if any_handle_opened {
                    self.metrics.target_pid = primary_pid;
                    self.metrics.cpu_usage_pct = total_cpu_pct.clamp(0.0, 100.0);
                    self.metrics.mem_used_mb = total_mem_bytes as f32 / (1024.0 * 1024.0);
                    self.metrics.mem_usage_pct =
                        ((total_mem_bytes as f64 / total_phys_bytes as f64) * 100.0)
                            .clamp(0.0, 100.0) as f32;
                    self.metrics.disk_read_mbs = total_disk_rate_mbs;
                    self.metrics.disk_total_read_gb =
                        (total_disk_read_bytes as f64 / (1024.0 * 1024.0 * 1024.0)) as f32;
                    self.metrics.net_usage_kbs = sys_net_kbs;
                    return;
                }
            }

            // No accessible game process: report global metrics, not game metrics.
            self.metrics.target_name = "Sistema Global".into();
            self.metrics.has_target = false;
            self.metrics.target_pid = 0;
            self.metrics.gpu_usage_pct = Self::read_gpu_usage_for_pids(self.counter_gpu, &[]);
            self.metrics.cpu_usage_pct = sys_cpu;
            let used_bytes = total_phys_bytes.saturating_sub(mem_status.ull_avail_phys);
            self.metrics.mem_used_mb = used_bytes as f32 / (1024.0 * 1024.0);
            self.metrics.mem_usage_pct = mem_status.dw_memory_load as f32;
            self.metrics.disk_read_mbs = sys_disk_mbs;
            self.metrics.disk_total_read_gb = 0.0;
            self.metrics.net_usage_kbs = sys_net_kbs;
        }

        #[cfg(not(windows))]
        {
            let _ = num_cpus;
            self.metrics.cpu_usage_pct = sys_cpu;
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.pdh_query != 0 {
            unsafe {
                let _ = win_ffi::PdhCloseQuery(self.pdh_query);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_rate_handles_counter_resets() {
        assert_eq!(read_rate_mbs(2 * 1024 * 1024, 4 * 1024 * 1024, 2.0), 1.0);
        assert_eq!(read_rate_mbs(100, 0, 1.0), 0.0);
    }

    fn wait_for_metrics(
        collector: &mut TelemetryCollector,
        target: Option<(&str, &[u32])>,
        ready: impl Fn(&ProcessTelemetry) -> bool,
    ) {
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        loop {
            collector.refresh(&[0.25, 0.50], target);
            if ready(&collector.metrics) {
                return;
            }
            assert!(Instant::now() < deadline, "No completed telemetry sample: {:?}", collector.metrics);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn test_process_telemetry_collection() {
        let mut collector = TelemetryCollector::new();
        let pids = [std::process::id()];
        wait_for_metrics(&mut collector, Some(("TestGame.exe", &pids)), |metrics| {
            metrics.has_target && {
                #[cfg(windows)]
                { metrics.mem_used_mb > 0.0 }
                #[cfg(not(windows))]
                { true }
            }
        });
        assert_eq!(collector.metrics.target_pid, pids[0]);
        assert_eq!(collector.metrics.target_name, "TestGame.exe");
        assert_eq!(collector.metrics.sys_cpu_pct, 37.5);
    }

    #[test]
    fn mailbox_keeps_only_latest_request() {
        let mailbox = Mailbox::default();
        for generation in 0..100 {
            mailbox.request(Request { generation, target: None, num_cpus: 1, sys_cpu: 0.0 });
        }
        assert_eq!(mailbox.take_request().unwrap().generation, 99);
        assert!(mailbox.take_request().is_none());
    }

    #[test]
    fn changed_target_rejects_completed_old_snapshot() {
        let mut collector = TelemetryCollector::new();
        let pids = [std::process::id()];
        let deadline = Instant::now() + std::time::Duration::from_secs(20);
        loop {
            collector.refresh(&[0.25, 0.50], Some(("Old.exe", &pids)));
            std::thread::sleep(std::time::Duration::from_millis(20));
            if let Some(mut snapshot) = collector.mailbox.take_snapshot() {
                assert!(snapshot.metrics.has_target);
                #[cfg(windows)]
                assert!(snapshot.metrics.mem_used_mb > 0.0);
                snapshot.metrics.process_ping.endpoints.push(crate::process_ping::EndpointPing {
                    destination: "192.0.2.42".into(),
                    outcome: crate::process_ping::EchoOutcome::Reply(42),
                    route: None,
                });
                collector.mailbox.snapshot(*snapshot);
                break;
            }
            assert!(Instant::now() < deadline, "No completed old-target sample");
        }
        collector.refresh(&[0.25, 0.50], Some(("New.exe", &pids)));
        assert!(!collector.metrics.has_target);
        assert_eq!(collector.metrics.target_name, "Sistema Global");
        assert_eq!(collector.metrics.mem_used_mb, 0.0);
        assert_eq!(collector.metrics.disk_total_read_gb, 0.0);
        assert!(collector.metrics.process_ping.endpoints.is_empty());
        wait_for_metrics(&mut collector, Some(("New.exe", &pids)), |metrics| {
            metrics.has_target && {
                #[cfg(windows)]
                { metrics.mem_used_mb > 0.0 }
                #[cfg(not(windows))]
                { true }
            }
        });
        assert_eq!(collector.metrics.target_name, "New.exe");
        assert_eq!(collector.metrics.target_pid, pids[0]);
    }

    #[cfg(windows)]
    #[test]
    fn inaccessible_target_and_empty_target_complete_as_global() {
        let mut collector = TelemetryCollector::new();
        for target in [Some(("Unavailable.exe", &[0u32][..])), Some(("Empty.exe", &[][..])), None] {
            // A real completed sample must populate RAM; zeros before completion cannot pass.
            collector.metrics.mem_used_mb = 0.0;
            wait_for_metrics(&mut collector, target, |metrics| !metrics.has_target && metrics.mem_used_mb > 0.0);
            assert_eq!(collector.metrics.target_name, "Sistema Global");
            assert_eq!(collector.metrics.target_pid, 0);
            assert_eq!(collector.metrics.cpu_usage_pct, 37.5);
            assert_eq!(collector.metrics.disk_total_read_gb, 0.0);
            assert!(collector.metrics.process_ping.endpoints.is_empty());
        }
    }
}
