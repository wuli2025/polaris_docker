use super::*;

// ───────────────────────── 命令(后台线程 + 事件)─────────────────────────

fn emit(app: &AppHandle, payload: Value) {
    let _ = app.emit("fable:index", payload);
}

/// 开始(或继续)构建向量索引。立即返回,进度走 `fable:index` 事件。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_index_start(app: AppHandle, max_chunks: Option<usize>) -> Result<(), String> {
    let Some(index_guard) = FlagGuard::acquire(&INDEXING) else {
        return Err("索引构建已在进行中".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    let budget = max_chunks.unwrap_or(4000).clamp(100, 200_000);
    std::thread::spawn(move || {
        // 守卫 move 进线程:正常结束或 panic 栈展开都会释放 INDEXING 闸(防永久锁死)。
        let _index_guard = index_guard;
        let app2 = app.clone();
        let result = build_index(budget, &move |files, chunks, current| {
            emit(
                &app2,
                json!({ "kind": "progress", "files": files, "chunks": chunks, "current": current }),
            );
        });
        match result {
            Ok(s) => emit(
                &app,
                json!({
                    "kind": "done", "files": s.files_done, "chunks": s.chunks_added,
                    "pending": s.files_pending, "seconds": s.seconds, "stopped": s.stopped,
                }),
            ),
            Err(e) => emit(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}

/// 开始(或继续)**词法专扫**:只把全盘文本写进 FTS 倒排、不嵌入(P0②覆盖率快赢)。立即返回,
/// 进度走 `fable:lex` 事件。与索引/盘点共用 INDEXING 闸(进行中则拒绝),RAII 守卫保证 panic 也释放。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn fable_lex_build_start(app: AppHandle) -> Result<(), String> {
    let Some(index_guard) = FlagGuard::acquire(&INDEXING) else {
        return Err("索引/盘点任务进行中,稍后再做词法专扫".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let _index_guard = index_guard;
        let app2 = app.clone();
        let result = build_lexical_index(&move |files, pending| {
            let _ = app2.emit(
                "fable:lex",
                json!({ "kind": "progress", "files": files, "pending": pending }),
            );
        });
        match result {
            Ok(s) => {
                let _ = app.emit(
                    "fable:lex",
                    json!({
                        "kind": "done", "files": s.files_done, "pending": s.files_pending,
                        "seconds": s.seconds, "stopped": s.stopped,
                    }),
                );
            }
            Err(e) => {
                let _ = app.emit("fable:lex", json!({ "kind": "error", "message": e }));
            }
        }
    });
    Ok(())
}

/// 重建向量 IVF 倒排单元(20TB 级 ANN 的「优化/建索引」步)。返回汇总;
/// 与构建/盘点共用 INDEXING 闸(进行中则拒绝),用 RAII 守卫确保 panic 也释放闸。
///
/// 桌面端一律 async + spawn_blocking:optimize_vectors() 要跑二值 k-means(最多 8 轮、
/// 采样上 10 万行)再全表分批 UPDATE chunks.cell,中库(5000 万 chunk)可达 5~15s。直接当
/// 同步 Tauri 命令会在 WebView 主线程上跑 → 阻塞 >5s 被 Windows 判「无响应」强杀(AppHangB1)。
/// server flavor 无 UI 主线程、dispatch 本就在 spawn_blocking 中,保持同步直调即可。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fable_index_optimize() -> Result<OptimizeSummary, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(_guard) = FlagGuard::acquire(&INDEXING) else {
            return Err("索引任务进行中,稍后再优化".into());
        };
        CANCEL.store(false, Ordering::SeqCst);
        optimize_vectors()
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn fable_index_optimize() -> Result<OptimizeSummary, String> {
    let Some(_guard) = FlagGuard::acquire(&INDEXING) else {
        return Err("索引任务进行中,稍后再优化".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    optimize_vectors()
}

/// IVF/向量健康修复(P1⑤:清陈旧向量 + 增量重分配 cell=-1 + 回填 n 计数)。返回汇总。
/// 与索引/盘点共用 INDEXING 闸;桌面端 async + spawn_blocking 防主线程阻塞(全表分批 UPDATE)。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn fable_index_repair() -> Result<RepairSummary, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let Some(_guard) = FlagGuard::acquire(&INDEXING) else {
            return Err("索引任务进行中,稍后再修复".into());
        };
        CANCEL.store(false, Ordering::SeqCst);
        repair_vectors()
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn fable_index_repair() -> Result<RepairSummary, String> {
    let Some(_guard) = FlagGuard::acquire(&INDEXING) else {
        return Err("索引任务进行中,稍后再修复".into());
    };
    CANCEL.store(false, Ordering::SeqCst);
    repair_vectors()
}
