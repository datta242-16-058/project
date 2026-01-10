use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GpuStats {
    /// Best-effort overall GPU utilization (0-100). Prefer 3D engine max if available.
    pub utilization_percent: Option<f64>,

    /// Dedicated GPU memory (VRAM) usage/limit.
    pub dedicated_used_mib: Option<f64>,
    pub dedicated_total_mib: Option<f64>,

    /// Shared GPU memory usage/limit.
    pub shared_used_mib: Option<f64>,
    pub shared_total_mib: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuAdapterInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub vendor: u32,
    pub device: u32,
    pub driver: String,
    pub driver_info: String,
}

/// Best-effort GPU adapter enumeration.
///
/// Note: This reports adapters ("GPU(s)") but not utilization.
pub fn enumerate_adapters() -> Vec<GpuAdapterInfo> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });

    // wgpu enumerates adapters per backend (DX12/Vulkan/Metal/GL/...).
    // On Windows this often results in duplicates (same physical GPU shown twice).
    // We deduplicate by (vendor, device, name) and keep the “best” backend per platform.
    fn backend_rank(b: wgpu::Backend) -> u8 {
        #[cfg(windows)]
        {
            match b {
                wgpu::Backend::Dx12 => 0,
                wgpu::Backend::Vulkan => 1,
                wgpu::Backend::Gl => 2,
                wgpu::Backend::Metal => 3,
                wgpu::Backend::BrowserWebGpu => 4,
                _ => 10,
            }
        }

        #[cfg(target_os = "macos")]
        {
            match b {
                wgpu::Backend::Metal => 0,
                wgpu::Backend::Vulkan => 1,
                wgpu::Backend::Gl => 2,
                wgpu::Backend::BrowserWebGpu => 3,
                _ => 10,
            }
        }

        #[cfg(all(not(windows), not(target_os = "macos")))]
        {
            match b {
                wgpu::Backend::Vulkan => 0,
                wgpu::Backend::Gl => 1,
                wgpu::Backend::BrowserWebGpu => 2,
                wgpu::Backend::Metal => 3,
                wgpu::Backend::Dx12 => 4,
                _ => 10,
            }
        }
    }

    let mut best: HashMap<(u32, u32, String), (u8, GpuAdapterInfo)> = HashMap::new();

    for a in instance.enumerate_adapters(wgpu::Backends::all()) {
        let info = a.get_info();
        let rank = backend_rank(info.backend);
        let key = (info.vendor, info.device, info.name.clone());

        let candidate = GpuAdapterInfo {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            vendor: info.vendor,
            device: info.device,
            driver: info.driver,
            driver_info: info.driver_info,
        };

        match best.get_mut(&key) {
            None => {
                best.insert(key, (rank, candidate));
            }
            Some((best_rank, best_info)) => {
                if rank < *best_rank {
                    *best_rank = rank;
                    *best_info = candidate;
                }
            }
        }
    }

    let mut out: Vec<GpuAdapterInfo> = best.into_values().map(|(_, v)| v).collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Best-effort global GPU stats.
///
/// On Windows this uses WMI perf classes (GPUPerformanceCounters).
/// On other platforms it returns `None`.
pub fn read_gpu_stats() -> Option<GpuStats> {
    #[cfg(windows)]
    {
        let wmi = windows_gpu_stats::read_gpu_stats();

        #[cfg(feature = "nvidia-nvml")]
        {
            wmi.or_else(nvml_gpu_stats::read_gpu_stats)
        }

        #[cfg(not(feature = "nvidia-nvml"))]
        {
            wmi
        }
    }

    #[cfg(all(not(windows), feature = "nvidia-nvml"))]
    {
        nvml_gpu_stats::read_gpu_stats()
    }

    #[cfg(all(not(windows), not(feature = "nvidia-nvml")))]
    {
        None
    }
}

/// Best-effort per-process GPU usage (0-100) by PID.
///
/// On Windows this uses WMI perf class `Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine`
/// and aggregates engine rows per PID.
pub fn read_gpu_process_usage() -> HashMap<u32, f64> {
    #[cfg(windows)]
    {
        windows_gpu_stats::read_gpu_process_usage()
    }

    #[cfg(not(windows))]
    {
        HashMap::new()
    }
}

/// Best-effort per-process GPU memory (VRAM/shared) usage by PID.
///
/// On Windows this uses WMI perf class `Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory`.
pub fn read_gpu_process_memory() -> HashMap<u32, GpuProcessMemory> {
    #[cfg(windows)]
    {
        let wmi = windows_gpu_stats::read_gpu_process_memory();

        #[cfg(feature = "nvidia-nvml")]
        {
            if wmi.is_empty() {
                nvml_gpu_stats::read_gpu_process_memory()
            } else {
                wmi
            }
        }

        #[cfg(not(feature = "nvidia-nvml"))]
        {
            wmi
        }
    }

    #[cfg(all(not(windows), feature = "nvidia-nvml"))]
    {
        nvml_gpu_stats::read_gpu_process_memory()
    }

    #[cfg(all(not(windows), not(feature = "nvidia-nvml")))]
    {
        HashMap::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GpuProcessMemory {
    pub dedicated_used_mib: Option<f64>,
    pub shared_used_mib: Option<f64>,
    pub total_committed_mib: Option<f64>,
}

#[cfg(windows)]
mod windows_gpu_stats {
    use super::{GpuProcessMemory, GpuStats};
    use log::debug;
    use serde::Deserialize;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use wmi::{COMLibrary, WMIConnection};

    thread_local! {
        static WMI: RefCell<Option<WMIConnection>> = const { RefCell::new(None) };
    }

    fn with_wmi<T>(f: impl FnOnce(&WMIConnection) -> T) -> Option<T> {
        WMI.with(|cell| {
            if cell.borrow().is_none() {
                let com_con = COMLibrary::new().ok()?;
                let wmi_con = WMIConnection::new(com_con).ok()?;
                *cell.borrow_mut() = Some(wmi_con);
            }

            let borrow = cell.borrow();
            let con = borrow.as_ref()?;
            Some(f(con))
        })
    }

    #[allow(non_snake_case)]
    #[derive(Debug, Deserialize)]
    struct Win32GpuEngine {
        Name: String,
        UtilizationPercentage: u64,
    }

    #[allow(non_snake_case)]
    #[derive(Debug, Deserialize)]
    struct Win32GpuAdapterMemory {
        Name: String,
        DedicatedUsage: u64,
        DedicatedLimit: u64,
        SharedUsage: u64,
        SharedLimit: u64,
    }

    #[allow(non_snake_case)]
    #[derive(Debug, Deserialize)]
    struct Win32GpuProcessMemory {
        Name: String,
        DedicatedUsage: u64,
        SharedUsage: u64,
        TotalCommitted: u64,
    }

    fn normalize_to_mib(v: u64) -> f64 {
        // Some systems report bytes, others report MiB for these perf classes.
        // Heuristic: if value is "large", treat it as bytes.
        if v > 1_048_576 {
            v as f64 / 1_048_576.0
        } else {
            v as f64
        }
    }

    pub(super) fn read_gpu_stats() -> Option<GpuStats> {
        let (engines, mems) = with_wmi(|wmi_con| {
            // Utilization: take max of 3D engines if present; else max of all engines.
            let engines: Vec<Win32GpuEngine> = match wmi_con.raw_query(
                "SELECT Name, UtilizationPercentage FROM Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine",
            ) {
                Ok(v) => v,
                Err(e) => {
                    debug!("WMI GPUEngine query failed: {e}");
                    Vec::new()
                }
            };

            // Memory: prefer _Total if available.
            let mems: Vec<Win32GpuAdapterMemory> = match wmi_con.raw_query(
                "SELECT Name, DedicatedUsage, DedicatedLimit, SharedUsage, SharedLimit FROM Win32_PerfFormattedData_GPUPerformanceCounters_GPUAdapterMemory",
            ) {
                Ok(v) => v,
                Err(e) => {
                    debug!("WMI GPUAdapterMemory query failed: {e}");
                    Vec::new()
                }
            };

            (engines, mems)
        })?;

        let mut max_all = 0u64;
        let mut max_3d = 0u64;
        for e in engines {
            max_all = max_all.max(e.UtilizationPercentage);
            if e.Name.contains("engtype_3D") {
                max_3d = max_3d.max(e.UtilizationPercentage);
            }
        }
        let utilization_percent = if max_3d > 0 || max_all > 0 {
            let util = if max_3d > 0 { max_3d } else { max_all };
            Some((util as f64).clamp(0.0, 100.0))
        } else {
            None
        };

        let chosen = mems
            .iter()
            .find(|m| m.Name == "_Total")
            .or_else(|| mems.first());

        // If both utilization and memory are missing, treat as not available.
        // This enables optional fallbacks (e.g. NVML) at the call site.
        if utilization_percent.is_none() && chosen.is_none() {
            return None;
        }

        let (ded_used, ded_total, sh_used, sh_total) = if let Some(m) = chosen {
            (
                Some(normalize_to_mib(m.DedicatedUsage)),
                Some(normalize_to_mib(m.DedicatedLimit)),
                Some(normalize_to_mib(m.SharedUsage)),
                Some(normalize_to_mib(m.SharedLimit)),
            )
        } else {
            (None, None, None, None)
        };

        Some(GpuStats {
            utilization_percent,
            dedicated_used_mib: ded_used,
            dedicated_total_mib: ded_total,
            shared_used_mib: sh_used,
            shared_total_mib: sh_total,
        })
    }

    fn extract_pid(name: &str) -> Option<u32> {
        // Example: "pid_1128_luid_0x..._eng_0_engtype_3D"
        let idx = name.find("pid_")?;
        let digits = &name[(idx + 4)..];
        let end = digits.find('_').unwrap_or(digits.len());
        digits[..end].parse::<u32>().ok()
    }

    pub(super) fn read_gpu_process_usage() -> HashMap<u32, f64> {
        let mut result = HashMap::new();

        let Some(engines) = with_wmi(|wmi_con| {
            let engines: Vec<Win32GpuEngine> = match wmi_con.raw_query(
                "SELECT Name, UtilizationPercentage FROM Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine",
            ) {
                Ok(v) => v,
                Err(e) => {
                    debug!("WMI GPUEngine query failed: {e}");
                    Vec::new()
                }
            };
            engines
        }) else {
            return result;
        };

        let mut max_all: HashMap<u32, u64> = HashMap::new();
        let mut max_3d: HashMap<u32, u64> = HashMap::new();

        for e in engines {
            let Some(pid) = extract_pid(&e.Name) else {
                continue;
            };
            if pid == 0 {
                continue;
            }

            let v = e.UtilizationPercentage.min(100);
            max_all
                .entry(pid)
                .and_modify(|m| *m = (*m).max(v))
                .or_insert(v);

            if e.Name.contains("engtype_3D") {
                max_3d
                    .entry(pid)
                    .and_modify(|m| *m = (*m).max(v))
                    .or_insert(v);
            }
        }

        for (pid, all) in max_all {
            let use_v = max_3d.get(&pid).copied().unwrap_or(all);
            result.insert(pid, (use_v as f64).clamp(0.0, 100.0));
        }

        result
    }

    pub(super) fn read_gpu_process_memory() -> HashMap<u32, GpuProcessMemory> {
        let mut result: HashMap<u32, GpuProcessMemory> = HashMap::new();

        let Some(rows) = with_wmi(|wmi_con| {
            let rows: Vec<Win32GpuProcessMemory> = match wmi_con.raw_query(
                "SELECT Name, DedicatedUsage, SharedUsage, TotalCommitted FROM Win32_PerfFormattedData_GPUPerformanceCounters_GPUProcessMemory",
            ) {
                Ok(v) => v,
                Err(e) => {
                    debug!("WMI GPUProcessMemory query failed: {e}");
                    Vec::new()
                }
            };
            rows
        }) else {
            return result;
        };

        for r in rows {
            let Some(pid) = extract_pid(&r.Name) else {
                continue;
            };
            if pid == 0 {
                continue;
            }

            // Multiple rows can exist per PID; take max values.
            let entry = result.entry(pid).or_default();

            let d = normalize_to_mib(r.DedicatedUsage);
            let s = normalize_to_mib(r.SharedUsage);
            let t = normalize_to_mib(r.TotalCommitted);

            entry.dedicated_used_mib = Some(entry.dedicated_used_mib.unwrap_or(0.0).max(d));
            entry.shared_used_mib = Some(entry.shared_used_mib.unwrap_or(0.0).max(s));
            entry.total_committed_mib = Some(entry.total_committed_mib.unwrap_or(0.0).max(t));
        }

        result
    }
}

#[cfg(feature = "nvidia-nvml")]
mod nvml_gpu_stats {
    use super::{GpuProcessMemory, GpuStats};
    use log::debug;
    use nvml_wrapper::Nvml;
    use std::collections::HashMap;

    pub(super) fn read_gpu_stats() -> Option<GpuStats> {
        let nvml = match Nvml::init() {
            Ok(n) => n,
            Err(e) => {
                debug!("NVML init failed: {e}");
                return None;
            }
        };

        let count = nvml.device_count().ok()?;
        if count == 0 {
            return None;
        }

        let mut max_util: u32 = 0;
        let mut total_used: u64 = 0;
        let mut total_total: u64 = 0;

        for i in 0..count {
            let dev = match nvml.device_by_index(i) {
                Ok(d) => d,
                Err(e) => {
                    debug!("NVML device_by_index({i}) failed: {e}");
                    continue;
                }
            };

            if let Ok(u) = dev.utilization_rates() {
                max_util = max_util.max(u.gpu);
            }

            if let Ok(m) = dev.memory_info() {
                total_used = total_used.saturating_add(m.used);
                total_total = total_total.saturating_add(m.total);
            }
        }

        Some(GpuStats {
            utilization_percent: Some((max_util as f64).clamp(0.0, 100.0)),
            dedicated_used_mib: if total_used > 0 {
                Some(total_used as f64 / 1_048_576.0)
            } else {
                None
            },
            dedicated_total_mib: if total_total > 0 {
                Some(total_total as f64 / 1_048_576.0)
            } else {
                None
            },
            // NVML doesn't expose “shared” memory in the same way as WMI.
            shared_used_mib: None,
            shared_total_mib: None,
        })
    }

    pub(super) fn read_gpu_process_memory() -> HashMap<u32, GpuProcessMemory> {
        let mut out: HashMap<u32, GpuProcessMemory> = HashMap::new();

        let nvml = match Nvml::init() {
            Ok(n) => n,
            Err(e) => {
                debug!("NVML init failed: {e}");
                return out;
            }
        };

        let count = match nvml.device_count() {
            Ok(c) => c,
            Err(e) => {
                debug!("NVML device_count failed: {e}");
                return out;
            }
        };

        for i in 0..count {
            let dev = match nvml.device_by_index(i) {
                Ok(d) => d,
                Err(e) => {
                    debug!("NVML device_by_index({i}) failed: {e}");
                    continue;
                }
            };

            // Graphics/compute processes; sum per PID across devices.
            for proc_list in [
                dev.running_graphics_processes(),
                dev.running_compute_processes(),
            ] {
                let Ok(procs) = proc_list else {
                    continue;
                };

                for p in procs {
                    let pid = p.pid;
                    let used_bytes: u64 = match p.used_gpu_memory {
                        nvml_wrapper::enums::device::UsedGpuMemory::Used(v) => v,
                        nvml_wrapper::enums::device::UsedGpuMemory::Unavailable => 0,
                    };
                    if used_bytes == 0 {
                        continue;
                    }
                    let used_mib = used_bytes as f64 / 1_048_576.0;
                    let entry = out.entry(pid).or_default();
                    entry.dedicated_used_mib =
                        Some(entry.dedicated_used_mib.unwrap_or(0.0) + used_mib);
                }
            }
        }

        out
    }
}
