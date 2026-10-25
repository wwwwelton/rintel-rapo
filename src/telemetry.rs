use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct ProcessTelemetry {
    pub has_target: bool,
    pub target_name: String,
    pub target_pid: u32,
    pub cpu_usage_pct: f32,
    pub gpu_usage_pct: f32,
    pub mem_used_mb: f32,
    pub mem_usage_pct: f32,
    pub disk_read_mbs: f32,
    pub net_usage_kbs: f32,
    pub ping_ms: u32,
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

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct MibTcpRowOwnerPid {
        pub dw_state: u32,
        pub dw_local_addr: u32,
        pub dw_local_port: u32,
        pub dw_remote_addr: u32,
        pub dw_remote_port: u32,
        pub dw_owning_pid: u32,
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        pub fn GetExtendedTcpTable(
            p_tcp_table: *mut u8,
            pdw_size: *mut u32,
            b_order: i32,
            ul_af: u32,
            table_class: u32,
            reserved: u32,
        ) -> u32;
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

#[derive(Clone, Copy, Default)]
struct ProcSample {
    cpu_ticks: u64,
    read_bytes: u64,
    other_bytes: u64,
    timestamp: Option<Instant>,
}

pub struct TelemetryCollector {
    pub metrics: ProcessTelemetry,
    ping_ms: Arc<AtomicU32>,
    target_remote_ip: Arc<AtomicU32>,
    stop_flag: Arc<AtomicBool>,
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

impl TelemetryCollector {
    pub fn new() -> Self {
        let ping_ms = Arc::new(AtomicU32::new(0));
        let target_remote_ip = Arc::new(AtomicU32::new(0));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let ping_clone = Arc::clone(&ping_ms);
        let remote_ip_clone = Arc::clone(&target_remote_ip);
        let stop_clone = Arc::clone(&stop_flag);

        // Background thread to measure game server ping (or gateway DNS fallback) without blocking UI
        std::thread::spawn(move || {
            let fallbacks: [std::net::SocketAddr; 2] = [
                "1.1.1.1:53".parse().unwrap(),
                "8.8.8.8:53".parse().unwrap(),
            ];
            while !stop_clone.load(Ordering::Relaxed) {
                let mut measured_ms = 0u32;

                let raw_ip = remote_ip_clone.load(Ordering::Relaxed);
                if raw_ip != 0 {
                    let ip = std::net::Ipv4Addr::from(u32::from_be(raw_ip));
                    for port in [443u16, 80] {
                        let addr = std::net::SocketAddr::from((ip, port));
                        let start = Instant::now();
                        if std::net::TcpStream::connect_timeout(
                            &addr,
                            std::time::Duration::from_millis(250),
                        )
                        .is_ok()
                        {
                            measured_ms = (start.elapsed().as_millis() as u32).max(1);
                            break;
                        }
                    }
                }

                if measured_ms == 0 {
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
                }

                if measured_ms > 0 {
                    ping_clone.store(measured_ms, Ordering::Relaxed);
                }

                for _ in 0..15 {
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });

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
            ping_ms,
            target_remote_ip,
            stop_flag,
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
    unsafe fn read_gpu_usage_for_pid(counter: isize, target_pid: Option<u32>) -> f32 {
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

        let pid_prefix = target_pid.map(|pid| format!("pid_{pid}_"));
        let mut pid_sum = 0.0f64;
        let mut total_sum = 0.0f64;

        for item in items {
            if item.fmt_value.c_status == 0 && item.fmt_value.double_value.is_finite() {
                let val = item.fmt_value.double_value;
                total_sum += val;

                if let Some(prefix) = &pid_prefix {
                    if !item.sz_name.is_null() {
                        let mut len = 0usize;
                        while *item.sz_name.add(len) != 0 && len < 128 {
                            len += 1;
                        }
                        let name = String::from_utf16_lossy(std::slice::from_raw_parts(item.sz_name, len));
                        if name.starts_with(prefix) {
                            pid_sum += val;
                        }
                    }
                }
            }
        }

        if target_pid.is_some() && pid_sum > 0.0 {
            (pid_sum as f32).clamp(0.0, 100.0)
        } else {
            (total_sum as f32).clamp(0.0, 100.0)
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

    #[cfg(windows)]
    unsafe fn find_remote_ip_for_pid(pid: u32) -> Option<u32> {
        let mut size: u32 = 0;
        let _ = win_ffi::GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, 2, 4, 0);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        if win_ffi::GetExtendedTcpTable(buf.as_mut_ptr(), &mut size, 0, 2, 4, 0) != 0 {
            return None;
        }
        if buf.len() < 4 {
            return None;
        }
        let num_entries = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let rows_ptr = buf.as_ptr().add(4).cast::<win_ffi::MibTcpRowOwnerPid>();
        let max_rows = (buf.len() - 4) / std::mem::size_of::<win_ffi::MibTcpRowOwnerPid>();
        let rows = std::slice::from_raw_parts(rows_ptr, num_entries.min(max_rows));

        for row in rows {
            if row.dw_owning_pid == pid && row.dw_state == 5 {
                let ip = std::net::Ipv4Addr::from(u32::from_be(row.dw_remote_addr));
                if !ip.is_loopback() && !ip.is_private() && !ip.is_unspecified() {
                    return Some(row.dw_remote_addr);
                }
            }
        }
        None
    }

    pub fn refresh(&mut self, cpu_loads: &[f32], target_game: Option<(u32, &str)>) {
        let num_cpus = cpu_loads.len().max(1) as f64;
        self.metrics.ping_ms = self.ping_ms.load(Ordering::Relaxed);

        let prev_pid = self.metrics.target_pid;
        match target_game {
            Some((pid, name)) => {
                self.metrics.has_target = true;
                self.metrics.target_pid = pid;
                if self.metrics.target_name != name {
                    self.metrics.target_name = name.to_string();
                }
            }
            None => {
                self.metrics.has_target = false;
                self.metrics.target_pid = 0;
                if self.metrics.target_name != "Sistema Global" {
                    self.metrics.target_name = "Sistema Global".into();
                }
            }
        }

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

            let target_pid = target_game.map(|(pid, _)| pid);
            self.metrics.gpu_usage_pct = Self::read_gpu_usage_for_pid(self.counter_gpu, target_pid);

            let sys_disk_mbs =
                (Self::read_pdh_single(self.counter_disk_read) / (1024.0 * 1024.0)).max(0.0) as f32;
            let sys_net_kbs = ((Self::read_pdh_array_sum(self.counter_net_recv)
                + Self::read_pdh_array_sum(self.counter_net_sent))
                / 1024.0)
                .max(0.0) as f32;

            if let Some(pid) = target_pid {
                if pid != prev_pid || self.target_remote_ip.load(Ordering::Relaxed) == 0 {
                    if let Some(remote_ip) = Self::find_remote_ip_for_pid(pid) {
                        self.target_remote_ip.store(remote_ip, Ordering::Relaxed);
                    }
                }

                let handle = windows_sys::Win32::System::Threading::OpenProcess(
                    windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
                    0,
                    pid,
                );

                if !handle.is_null() {
                    let now = Instant::now();

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
                        self.metrics.mem_used_mb = pmc.working_set_size as f32 / (1024.0 * 1024.0);
                        self.metrics.mem_usage_pct =
                            ((pmc.working_set_size as f64 / total_phys_bytes as f64) * 100.0)
                                .clamp(0.0, 100.0) as f32;
                    }

                    // 3. Process Disk Read (MB/s) & Network/Socket I/O (KB/s)
                    let mut io = win_ffi::IoCounters::default();
                    let _ = win_ffi::GetProcessIoCounters(handle, &mut io);

                    if let Some(prev) = self.proc_samples.get(&pid) {
                        if let Some(prev_time) = prev.timestamp {
                            let elapsed_secs = now.duration_since(prev_time).as_secs_f64().max(0.05);

                            if cpu_ticks >= prev.cpu_ticks {
                                let delta_ticks = (cpu_ticks - prev.cpu_ticks) as f64;
                                let wall_ticks = elapsed_secs * 10_000_000.0 * num_cpus;
                                self.metrics.cpu_usage_pct =
                                    ((delta_ticks / wall_ticks) * 100.0).clamp(0.0, 100.0) as f32;
                            }

                            if io.read_transfer_count >= prev.read_bytes {
                                let delta_read = (io.read_transfer_count - prev.read_bytes) as f64;
                                self.metrics.disk_read_mbs =
                                    (delta_read / (1024.0 * 1024.0) / elapsed_secs) as f32;
                            }

                            if io.other_transfer_count >= prev.other_bytes {
                                let delta_other = (io.other_transfer_count - prev.other_bytes) as f64;
                                let proc_net_kbs = (delta_other / 1024.0 / elapsed_secs) as f32;
                                self.metrics.net_usage_kbs = if proc_net_kbs > 0.1 {
                                    proc_net_kbs
                                } else {
                                    sys_net_kbs
                                };
                            } else {
                                self.metrics.net_usage_kbs = sys_net_kbs;
                            }
                        }
                    } else {
                        self.metrics.net_usage_kbs = sys_net_kbs;
                    }

                    self.proc_samples.insert(
                        pid,
                        ProcSample {
                            cpu_ticks,
                            read_bytes: io.read_transfer_count,
                            other_bytes: io.other_transfer_count,
                            timestamp: Some(now),
                        },
                    );

                    windows_sys::Win32::Foundation::CloseHandle(handle);
                    return;
                }
            }

            // Fallback when no game is running: show global system telemetry
            self.target_remote_ip.store(0, Ordering::Relaxed);
            self.metrics.cpu_usage_pct = if cpu_loads.is_empty() {
                0.0
            } else {
                (cpu_loads.iter().sum::<f32>() / cpu_loads.len() as f32 * 100.0).clamp(0.0, 100.0)
            };
            let used_bytes = total_phys_bytes.saturating_sub(mem_status.ull_avail_phys);
            self.metrics.mem_used_mb = used_bytes as f32 / (1024.0 * 1024.0);
            self.metrics.mem_usage_pct = mem_status.dw_memory_load as f32;
            self.metrics.disk_read_mbs = sys_disk_mbs;
            self.metrics.net_usage_kbs = sys_net_kbs;
        }

        #[cfg(not(windows))]
        {
            let _ = num_cpus;
            self.metrics.cpu_usage_pct = if cpu_loads.is_empty() {
                0.0
            } else {
                (cpu_loads.iter().sum::<f32>() / cpu_loads.len() as f32 * 100.0).clamp(0.0, 100.0)
            };
        }
    }
}

impl Drop for TelemetryCollector {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
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
    fn test_process_telemetry_collection() {
        let mut collector = TelemetryCollector::new();
        let pid = std::process::id();
        collector.refresh(&[0.25, 0.50], Some((pid, "TestGame.exe")));
        std::thread::sleep(std::time::Duration::from_millis(200));
        collector.refresh(&[0.25, 0.50], Some((pid, "TestGame.exe")));
        println!("Collected Process Telemetry: {:?}", collector.metrics);
        assert!(collector.metrics.has_target);
        assert_eq!(collector.metrics.target_pid, pid);
        #[cfg(windows)]
        {
            assert!(collector.metrics.mem_used_mb > 0.0, "Process RAM should be > 0 MB");
        }
    }
}
