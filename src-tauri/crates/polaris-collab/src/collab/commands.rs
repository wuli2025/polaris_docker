//! collab/commands.rs —— 桌面端(完整端)本地 tauri 命令。
//!
//! 完整端拓扑:看板/账号等走 HTTP 到主机(前端 collab api 已带 base+token);
//! 但 git 工作集操作发生在**成员本机**——这里把 workset 六件套暴露给前端 invoke。
//! 隧道客户端(空壳/完整端连主机)也在此启停。
#![cfg(feature = "desktop")]
// 参数名必须与前端 invoke 的 camelCase key 逐字对齐(tauri 按名字反序列化),
// 改成 snake_case 会让前端调用当场丢参,故此文件整体豁免命名 lint。
#![allow(non_snake_case)]

use super::workset;

#[tauri::command]
pub fn collab_clone_partial(
    remoteUrl: String,
    dest: String,
    sparseDirs: Vec<String>,
) -> Result<(), String> {
    let dirs: Vec<&str> = sparseDirs.iter().map(|s| s.as_str()).collect();
    workset::clone_partial(&remoteUrl, std::path::Path::new(&dest), &dirs)
}

#[tauri::command]
pub fn collab_task_setup(
    repo: String,
    branch: String,
    scope: String,
) -> Result<workset::SetupReport, String> {
    workset::task_setup(std::path::Path::new(&repo), &branch, &scope)
}

#[tauri::command]
pub fn collab_sync_main(repo: String) -> Result<String, String> {
    workset::sync_main(std::path::Path::new(&repo))
}

/// 推分支。带 scope 时先做越界照妖镜:改动落在 scope 外且未 confirm → 拒推并列出文件,
/// 前端二段确认后带 confirm=true 再推(软约束,主机检查闸另有硬闸)。
#[tauri::command]
pub fn collab_push_branch(
    repo: String,
    branch: String,
    scope: Option<String>,
    confirm: Option<bool>,
) -> Result<(), String> {
    let repo = std::path::Path::new(&repo);
    if let Some(scope) = scope.as_deref().filter(|s| !s.trim().is_empty()) {
        if !confirm.unwrap_or(false) {
            let outside = workset::out_of_scope_files(repo, scope)?;
            if !outside.is_empty() {
                return Err(format!(
                    "OUT_OF_SCOPE:改动越出任务地盘(scope):{}。确认要推吗?",
                    outside.join(", ")
                ));
            }
        }
    }
    workset::push_branch(repo, &branch)
}

/// scope 就位状态:本地稀疏集 vs 任务 scope,前端展示「已就位/缺失」。
#[tauri::command]
pub fn collab_scope_status(repo: String, scope: String) -> Result<workset::ScopeStatus, String> {
    workset::scope_status(std::path::Path::new(&repo), &scope)
}

/// 把 outbox 积压消息补传到主机(断线缓存的发送端,连上/重连后调)。
#[tauri::command]
pub fn collab_outbox_flush(
    dir: String,
    baseUrl: String,
    token: String,
) -> Result<workset::FlushReport, String> {
    workset::flush_outbox(std::path::Path::new(&dir), &baseUrl, &token)
}

/// outbox 目录:前端传空串 = 默认 ~/Polaris/data/outbox(前端无从得知 home,别让它猜)。
fn outbox_dir(dir: &str) -> Result<std::path::PathBuf, String> {
    if !dir.trim().is_empty() {
        return Ok(std::path::PathBuf::from(dir));
    }
    directories::UserDirs::new()
        .map(|u| u.home_dir().join("Polaris").join("data").join("outbox"))
        .ok_or_else(|| "无法定位用户目录".into())
}

#[tauri::command]
pub fn collab_outbox_queue(dir: String, payload: String) -> Result<String, String> {
    workset::queue_message(&outbox_dir(&dir)?, &payload)
}

#[tauri::command]
pub fn collab_outbox_pending(dir: String) -> Result<Vec<(String, String)>, String> {
    workset::pending_messages(&outbox_dir(&dir)?)
}

#[tauri::command]
pub fn collab_outbox_mark_sent(dir: String, idemKey: String) -> Result<(), String> {
    workset::mark_sent(&outbox_dir(&dir)?, &idemKey)
}

/// 成员端设备 NodeId(入伙页展示/上报,主机据此加白名单)。未编 collab-net 时给出说明。
#[tauri::command]
pub fn collab_device_node_id() -> Result<String, String> {
    #[cfg(feature = "collab-net")]
    {
        return super::tunnel::node_id_of_device_key();
    }
    #[cfg(not(feature = "collab-net"))]
    Err("本构建未启用 collab-net(iroh) 功能,直连模式请直接填主机地址".into())
}

/// 成员端隧道:本地端口 ↔ 主机 NodeId。起来后前端把 collab base 设为 http://127.0.0.1:<port>。
/// 幂等:同端口同主机重复调用 = no-op(前端启动兜底/重试可放心多次调);坏 NodeId 同步报错。
#[tauri::command]
pub fn collab_tunnel_connect(hostNodeId: String, listenPort: u16) -> Result<(), String> {
    #[cfg(feature = "collab-net")]
    {
        return super::tunnel::connect_client(&hostNodeId, listenPort);
    }
    #[cfg(not(feature = "collab-net"))]
    {
        let _ = (hostNodeId, listenPort);
        Err("本构建未启用 collab-net(iroh) 功能".into())
    }
}

/// 拆除一条成员端隧道(移除远程盘时调,别让孤儿隧道永远在后台重连)。幂等。
#[tauri::command]
pub fn collab_tunnel_disconnect(listenPort: u16) -> Result<(), String> {
    #[cfg(feature = "collab-net")]
    {
        super::tunnel::disconnect_client(listenPort);
        return Ok(());
    }
    #[cfg(not(feature = "collab-net"))]
    {
        let _ = listenPort;
        Err("本构建未启用 collab-net(iroh) 功能".into())
    }
}

#[tauri::command]
pub fn collab_tunnel_status() -> serde_json::Value {
    #[cfg(feature = "collab-net")]
    {
        return super::tunnel::status();
    }
    #[cfg(not(feature = "collab-net"))]
    serde_json::json!({"running": false, "unavailable": "本构建未启用 collab-net"})
}

// ── 云机中继网关:桌面主机挂牌(真·中继完整形态) ──

#[cfg(feature = "collab-net")]
fn gateway_cfg_path() -> Option<std::path::PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().join("Polaris/data/gateway.json"))
}

/// 挂牌到云机网关:本机主机 NodeId → 云机注册 → 云机 NodeId 加白名单 → 确保隧道在跑 → 存自启。
/// 成功后手机把「主机地址」填成返回的 shareUrl(https://cloud/h/<hostId>),任何网络、零安装可达。
#[tauri::command]
#[allow(non_snake_case)]
pub fn collab_gateway_attach(
    cloudBase: String,
    token: String,
    hostName: Option<String>,
) -> Result<serde_json::Value, String> {
    #[cfg(feature = "collab-net")]
    {
        let base = cloudBase.trim().trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err("云机地址为空".into());
        }
        let host_nid = super::tunnel::host_node_id()?;
        // 网关经 iroh 连进来的落点 = 本机主机隧道,确保它在跑。
        if !super::tunnel::is_running() {
            super::tunnel::start_host_blocking_thread();
        }
        let name = hostName
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "桌面主机".into());
        let resp = ureq::post(&format!("{base}/api/gw/register"))
            .set("Authorization", &format!("Bearer {}", token.trim()))
            .timeout(std::time::Duration::from_secs(30))
            .send_json(serde_json::json!({"hostNodeId": host_nid, "hostName": name}))
            .map_err(|e| format!("注册到云机失败: {e}"))?;
        let v: serde_json::Value = resp
            .into_json()
            .map_err(|e| format!("解析注册响应失败: {e}"))?;
        let gw_nid = v
            .get("gatewayNodeId")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if gw_nid.is_empty() {
            return Err("云机未返回 gatewayNodeId".into());
        }
        // 云机 NodeId 加设备白名单(host_listen 校验连入方 NodeId);幂等,已在则跳过。
        if !super::identity::is_node_allowed(&gw_nid) {
            super::identity::add_device(0, "云机网关", &gw_nid)?;
        }
        // 存配置供开机自启重挂(云机重启后注册表清空,需桌面重挂)。
        if let Some(p) = gateway_cfg_path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(
                &p,
                serde_json::json!({"cloudBase": base, "token": token.trim(), "hostName": name})
                    .to_string(),
            );
        }
        Ok(serde_json::json!({
            "ok": true,
            "hostId": host_nid,
            "gatewayNodeId": gw_nid,
            "shareUrl": format!("{base}/h/{host_nid}"),
        }))
    }
    #[cfg(not(feature = "collab-net"))]
    {
        let _ = (cloudBase, token, hostName);
        Err("本构建未启用 collab-net(iroh) 功能".into())
    }
}

/// 断开挂牌:删本地自启配置(云机侧注册随其重启自然清)。
#[tauri::command]
pub fn collab_gateway_detach() -> Result<(), String> {
    #[cfg(feature = "collab-net")]
    {
        if let Some(p) = gateway_cfg_path() {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }
    #[cfg(not(feature = "collab-net"))]
    Err("本构建未启用 collab-net 功能".into())
}

/// 开机自启:上次挂过牌就重新向云机注册(云机重启后注册表清空需桌面重挂)。desktop setup 调。
#[cfg(feature = "collab-net")]
pub fn gateway_auto_reattach() {
    let Some(p) = gateway_cfg_path() else { return };
    let Ok(txt) = std::fs::read_to_string(&p) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return;
    };
    let base = v
        .get("cloudBase")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let token = v
        .get("token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let name = v
        .get("hostName")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    if base.is_empty() || token.is_empty() {
        return;
    }
    std::thread::spawn(move || match collab_gateway_attach(base, token, name) {
        Ok(_) => println!("[gateway] 开机自动重挂云机成功"),
        Err(e) => eprintln!("[gateway] 开机自动重挂失败: {e}"),
    });
}
