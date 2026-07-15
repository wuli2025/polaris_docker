//! src/sysstat.rs —— 本机资源真实采样(设备联盟遥测 Phase 1)。
//!
//! 只采「本机」实况:CPU 占用%、内存 used/total、主盘 used/total、逻辑核数。远端设备的
//! 同款数据由各自 Polaris 上报(Phase 2 遥测协议),口径与此一致。不插值、不造假。
use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct SysStats {
    /// 全局 CPU 占用,0–100。
    pub cpu_pct: f32,
    /// 已用 / 总内存(字节)。
    pub mem_used: u64,
    pub mem_total: u64,
    /// 主盘(容量最大的一块)已用 / 总容量(字节)。
    pub disk_used: u64,
    pub disk_total: u64,
    /// 逻辑核数。
    pub cores: usize,
}

/// 采一帧本机资源。CPU 需两次采样间隔 ≥ MINIMUM_CPU_UPDATE_INTERVAL 才有意义。
pub fn sample() -> SysStats {
    use sysinfo::{Disks, System};
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    sys.refresh_memory();

    let cpu_pct = sys.global_cpu_usage();
    let mem_total = sys.total_memory();
    let mem_used = sys.used_memory();
    let cores = sys.cpus().len();

    // 磁盘:取容量最大的一块当「主盘」,避免多盘叠加后语义混乱。
    let disks = Disks::new_with_refreshed_list();
    let (mut disk_total, mut disk_avail) = (0u64, 0u64);
    if let Some(d) = disks.list().iter().max_by_key(|d| d.total_space()) {
        disk_total = d.total_space();
        disk_avail = d.available_space();
    }
    let disk_used = disk_total.saturating_sub(disk_avail);

    SysStats {
        cpu_pct,
        mem_used,
        mem_total,
        disk_used,
        disk_total,
        cores,
    }
}

/// 桌面端:前端 invoke("sys_stats") 取本机实况(设备卡「本机」那张的三条仪表)。
#[cfg(feature = "desktop")]
#[tauri::command]
pub fn sys_stats() -> SysStats {
    sample()
}
