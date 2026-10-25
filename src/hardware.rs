#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::{__cpuid, __cpuid_count};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RdtCatStatus {
    pub rdt_allocation_supported: bool,
    pub rdt_monitoring_supported: bool,
    pub l3_cat_supported: bool,
    pub l2_cat_supported: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CacheLevelInfo {
    pub level: u8,
    pub cache_type: String, // "Data", "Instruction", "Unified"
    pub size_kb: u32,
    pub line_size: u16,
    pub count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CacheTopology {
    pub rdt: RdtCatStatus,
    pub levels: Vec<CacheLevelInfo>,
    pub total_l3_kb: u32,
    pub p_core_l2_kb: u32,
    pub e_cluster_l2_kb: u32,
}

pub fn detect_rdt_cat() -> RdtCatStatus {
    #[cfg(target_arch = "x86_64")]
    {
        let base = __cpuid(0);
        let max_leaf = base.eax;

        let mut status = RdtCatStatus::default();
        if max_leaf >= 7 {
            let leaf7 = __cpuid_count(7, 0);
            status.rdt_allocation_supported = (leaf7.ebx & (1 << 12)) != 0;
            status.rdt_monitoring_supported = (leaf7.ebx & (1 << 15)) != 0;
        }

        if status.rdt_allocation_supported && max_leaf >= 0x10 {
            let leaf10_0 = __cpuid_count(0x10, 0);
            status.l3_cat_supported = (leaf10_0.ebx & (1 << 1)) != 0;
            status.l2_cat_supported = (leaf10_0.ebx & (1 << 2)) != 0;
        }

        status
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        RdtCatStatus::default()
    }
}

#[cfg(windows)]
pub fn get_cache_topology() -> CacheTopology {
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationCache,
        SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    let mut topo = CacheTopology {
        rdt: detect_rdt_cat(),
        ..Default::default()
    };
    unsafe {
        let mut len = 0u32;
        GetLogicalProcessorInformationEx(RelationCache, std::ptr::null_mut(), &mut len);
        if len == 0 {
            return topo;
        }
        let mut buf = vec![0u8; len as usize];
        if GetLogicalProcessorInformationEx(
            RelationCache,
            buf.as_mut_ptr().cast(),
            &mut len,
        ) == 0 {
            return topo;
        }

        let mut offset = 0usize;
        while offset + std::mem::size_of::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>() <= len as usize {
            let ptr = buf.as_ptr().add(offset).cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>();
            let info = &*ptr;
            if info.Size == 0 || offset + info.Size as usize > len as usize {
                break;
            }
            if info.Relationship == RelationCache {
                let cache = &info.Anonymous.Cache;
                let level = cache.Level;
                let size_kb = cache.CacheSize / 1024;
                let line_size = cache.LineSize;
                let type_str = match cache.Type {
                    0 => "Unified",
                    1 => "Instruction",
                    2 => "Data",
                    _ => "Other",
                };

                if let Some(entry) = topo.levels.iter_mut().find(|l| l.level == level && l.cache_type == type_str && l.size_kb == size_kb) {
                    entry.count += 1;
                } else {
                    topo.levels.push(CacheLevelInfo {
                        level,
                        cache_type: type_str.to_string(),
                        size_kb,
                        line_size,
                        count: 1,
                    });
                }

                if level == 3 {
                    topo.total_l3_kb = size_kb;
                } else if level == 2 {
                    if size_kb == 2048 {
                        topo.p_core_l2_kb = size_kb;
                    } else if size_kb == 4096 {
                        topo.e_cluster_l2_kb = size_kb;
                    }
                }
            }
            offset += info.Size as usize;
        }
        topo.levels.sort_by(|a, b| a.level.cmp(&b.level).then_with(|| a.cache_type.cmp(&b.cache_type)));
    }
    topo
}

#[cfg(not(windows))]
pub fn get_cache_topology() -> CacheTopology {
    CacheTopology {
        rdt: detect_rdt_cat(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpuid_rdt_cat_query() {
        let rdt = detect_rdt_cat();
        println!("RDT CAT status: {:?}", rdt);
    }

    #[cfg(windows)]
    #[test]
    fn test_cache_topology_enumeration() {
        let topo = get_cache_topology();
        println!("Cache levels found: {}", topo.levels.len());
        for lvl in &topo.levels {
            println!("  L{} {} - {} KB ({}x), line {}B", lvl.level, lvl.cache_type, lvl.size_kb, lvl.count, lvl.line_size);
        }
        assert!(!topo.levels.is_empty(), "Should detect at least 1 cache level on Windows");
    }
}
