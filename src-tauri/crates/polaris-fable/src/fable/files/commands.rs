use super::*;

// ───────────────────────── 命令(薄包装;三壳共用) ─────────────────────────

// 文件中心几个「读大库 / 读盘上文件(可能是慢 SMB 的 NAS 盘)」的命令:桌面端一律 async +
// spawn_blocking,把重活挪离 Tauri 主线程,绝不冻 WebView 消息泵(否则大库 GROUP BY 或 NAS
// 一抖,主线程阻塞 >5s 就被 Windows 判「无响应」强杀)。server flavor 无 UI 主线程可冻、且
// dispatch_sync 本就在 spawn_blocking 中,保持同步直调内层即可。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_overview(root: Option<String>) -> Result<FileOverview, String> {
    tauri::async_runtime::spawn_blocking(move || overview(root))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_overview(root: Option<String>) -> Result<FileOverview, String> {
    overview(root)
}

#[cfg(feature = "desktop")]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn file_grid(
    root: Option<String>,
    cluster_id: Option<i64>,
    kind: Option<String>,
    lang: Option<String>,
    sort: Option<String>,
    query: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<FileGridPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        grid(
            root,
            cluster_id,
            kind,
            lang,
            sort,
            query,
            page.unwrap_or(0),
            page_size.unwrap_or(60),
        )
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
#[allow(clippy::too_many_arguments)]
pub fn file_grid(
    root: Option<String>,
    cluster_id: Option<i64>,
    kind: Option<String>,
    lang: Option<String>,
    sort: Option<String>,
    query: Option<String>,
    page: Option<usize>,
    page_size: Option<usize>,
) -> Result<FileGridPage, String> {
    grid(
        root,
        cluster_id,
        kind,
        lang,
        sort,
        query,
        page.unwrap_or(0),
        page_size.unwrap_or(60),
    )
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_thumb(abspath: String, max: Option<u32>) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || thumb(abspath, max.unwrap_or(360)))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_thumb(abspath: String, max: Option<u32>) -> Result<Option<String>, String> {
    thumb(abspath, max.unwrap_or(360))
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_gist(abspath: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || gist(abspath))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_gist(abspath: String) -> Result<String, String> {
    gist(abspath)
}

/// 归类(纯数学聚类)进行中闸 —— 防双发,panic 栈展开也释放(见 [`FlagGuard`])。
pub(crate) static CLUSTERING: AtomicBool = AtomicBool::new(false);

pub(crate) fn emit_cluster(app: &AppHandle, payload: Value) {
    let _ = app.emit("file:cluster", payload);
}

/// 文件中心 v3 渐进式智能归类进行中闸(独立于 CLUSTERING/LLM_CLUSTERING,防双发)。
pub(crate) static SMART_CLUSTERING: AtomicBool = AtomicBool::new(false);

/// 文件中心 v3 渐进式智能归类:后台一个线程顺序推进三档,全程发 `file:cluster` 事件。
///  - **T0 骨架**:`cluster_build(Lexical)` 秒级、零嵌入,先把全库分簇 → `tier=skeleton`;
///  - **T1 初级**:`cluster_rename_llm` 让大模型读簇画像起亲切名 + 关系 → `tier=ai-primary`;
///  - **T2 精修**:配了嵌入服务商时,`build_index_full` 全量向量化 → `cluster_build(Semantic)`
///    语义重聚 → `cluster_rename_llm` 再命名 → `tier=semantic`(用户要的「向量化完后再归一次」)。
///
/// 每档完成 emit `{kind:"tier", tier, note}` 让前端原地刷新星图;全部结束 emit `kind:"done"`。
/// LLM 档失败可降级:保留 `cluster_build` 的启发式名、继续后续档,绝不卡住。
/// `deep=false`(快速档)只跑 T0 词法全覆盖 + T1 AI 命名就收尾——几秒归全库 + 一次 AI 调用,
/// 远低于 2 分钟,**给新用户向导用**(向导自己在收尾另起后台建索引,这里再触发 T2 全量向量化
/// 会与之冲突、且大库要几十分钟爆掉「2 分钟」预期)。`deep=true`(文件中心按钮)才追加 T2。
pub(crate) fn smart_cluster_progressive(
    app: &AppHandle,
    root: Option<String>,
    deep: bool,
) -> Result<(), String> {
    // ── T0:结构骨架(秒级,零嵌入)──
    emit_cluster(
        app,
        json!({ "kind": "phase", "tier": "skeleton", "text": "正在快速归类(按结构)…几秒就好" }),
    );
    let s0 = cluster_build_mode(root.clone(), ClusterMode::Lexical)?;
    emit_cluster(
        app,
        json!({
            "kind": "tier", "tier": "skeleton", "clusters": s0.clusters, "files": s0.files,
            "note": format!("已把 {} 个文件快速归成 {} 簇,正在请 AI 起名…", s0.files, s0.clusters),
        }),
    );

    // ── T1:AI 初级命名 + 关系(读簇画像,不读文件,成本与文件数无关)──
    let mut report = String::new();
    match cluster_rename_llm(app, root.clone(), "ai-primary") {
        Ok((renamed, edges, rep)) => {
            report = rep.clone();
            emit_cluster(
                app,
                json!({
                    "kind": "tier", "tier": "ai-primary", "renamed": renamed, "edges": edges, "report": rep,
                    "note": format!("AI 已读懂并命名 {renamed} 个主题、理出 {edges} 条关系"),
                }),
            );
        }
        Err(e) => {
            // 起名失败不致命:骨架名仍在,提示后继续。
            emit_cluster(
                app,
                json!({
                    "kind": "tier", "tier": "ai-primary",
                    "note": format!("AI 命名暂不可用({e}),已先按结构归好;稍后可重试"),
                }),
            );
        }
    }

    // ── T2:全量向量化 → 语义重聚 → 再命名(配了嵌入能力时;全程后台)──
    // 「嵌入能力」= 云 API 服务商 **或** 本地开源嵌入(local-embed,离线就能产向量);
    // 后者此前不被计入 → 纯本地用户永远停在结构归类、走不到「按内容语义」这一档。见 embed_capable。
    if deep && crate::fable::index::embed_capable() {
        emit_cluster(
            app,
            json!({ "kind": "phase", "tier": "semantic", "text": "后台精修:正在把全部资料向量化(可关页面去忙别的)…" }),
        );
        let app_idx = app.clone();
        let idx = crate::fable::index::build_index_full(&move |files, _chunks, pending| {
            emit_cluster(
                &app_idx,
                json!({
                    "kind": "phase", "tier": "semantic",
                    "text": format!("后台精修:已向量化 {files} 个文件{}",
                        if pending > 0 { format!(",还剩约 {pending} 个") } else { String::new() }),
                }),
            );
        });
        match idx {
            Ok(_) => {
                emit_cluster(
                    app,
                    json!({ "kind": "phase", "tier": "semantic", "text": "向量化完成,正在按内容语义重新归类…" }),
                );
                let s2 = cluster_build_mode(root.clone(), ClusterMode::Semantic)?;
                let rep2 = match cluster_rename_llm(app, root.clone(), "semantic") {
                    Ok((_, _, rep)) => rep,
                    Err(_) => report.clone(),
                };
                emit_cluster(
                    app,
                    json!({
                        "kind": "tier", "tier": "semantic", "clusters": s2.clusters, "files": s2.files, "report": rep2,
                        "note": format!("已按内容语义把 {} 个文件精修归成 {} 簇", s2.files, s2.clusters),
                    }),
                );
                report = rep2;
            }
            Err(e) => {
                emit_cluster(
                    app,
                    json!({ "kind": "phase", "tier": "semantic", "text": format!("后台向量化未完成:{e}(已保留 AI 初级归类)") }),
                );
            }
        }
    }

    emit_cluster(
        app,
        json!({ "kind": "done", "report": report, "note": "智能归类完成" }),
    );
    Ok(())
}

/// 重建语义/结构聚类(复用已存向量,纯数学,不调嵌入 API)。**后台线程跑**,进度走
/// `file:cluster` 事件(phase/done/error)—— 切走文件中心也不中断,回来仍见结果。
///
/// 改自旧同步命令:上千文件时均值池化(逐 chunk 反序列化)+ 球面 k-means(16 轮 Lloyd)
/// 是 0.1–0.5s 的纯 CPU 阻塞,放在 Tauri 同步命令里会冻结 WebView 主线程。挪到后台线程后
/// 界面全程可点,与 [`file_cluster_llm`] / [`fable_inventory_start`] 同构。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_build(app: AppHandle, root: Option<String>) -> Result<(), String> {
    let Some(guard) = FlagGuard::acquire(&CLUSTERING) else {
        return Err("归类正在进行中".into());
    };
    emit_cluster(
        &app,
        json!({ "kind": "phase", "text": "正在把相似文件归类…" }),
    );
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        match cluster_build(root) {
            Ok(s) => emit_cluster(
                &app,
                json!({
                    "kind": "done", "clusters": s.clusters, "files": s.files,
                    "seconds": s.seconds, "note": s.note,
                }),
            ),
            Err(e) => emit_cluster(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}

/// 文件中心 v3 渐进式智能归类(秒级骨架 → AI 初级命名+关系 → 全量向量化后语义重聚再命名)。
/// 后台线程跑,进度/各档完成走 `file:cluster` 事件(phase / tick / tier / done / error);
/// 切走文件中心也不中断,与 [`file_cluster_build`] / [`file_cluster_llm`] 同构。
///
/// `quick=Some(true)`:只跑 T0+T1(全覆盖词法 + AI 命名)就收尾,不追加耗时的 T2 全量向量化
/// —— 新用户向导用(几秒 + 一次 AI 调用,远低于 2 分钟);其余(含文件中心按钮)默认深档跑全程。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_smart_cluster(
    app: AppHandle,
    root: Option<String>,
    quick: Option<bool>,
) -> Result<(), String> {
    let Some(guard) = FlagGuard::acquire(&SMART_CLUSTERING) else {
        return Err("智能归类正在进行中".into());
    };
    let deep = !quick.unwrap_or(false);
    emit_cluster(
        &app,
        json!({ "kind": "phase", "tier": "skeleton", "text": "正在启动智能归类…" }),
    );
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        if let Err(e) = smart_cluster_progressive(&app, root, deep) {
            emit_cluster(&app, json!({ "kind": "error", "message": e }));
        }
    });
    Ok(())
}

/// 「让 AI 更懂你」:据盘点统计确定性生成知识画像 HTML → 桌面,返回文件路径(同步;不调大模型)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_profile_html(root: Option<String>) -> Result<String, String> {
    profile_html(root)
}

/// 智能向导收尾「建议工作流」:大模型据**真实知识库**智能匹配,而非固定阈值套话。
/// 桌面端为 async + spawn_blocking,避免数秒的大模型调用冻结主线程 WebView;
/// server flavor 由 dispatch_sync 直接调内层 [`suggest_workflows`](已在 spawn_blocking 中)。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_suggest_workflows(root: Option<String>) -> Result<Vec<SuggestedFlow>, String> {
    tauri::async_runtime::spawn_blocking(move || suggest_workflows(root))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}

/// 文件中心「星图」:语义簇 + 抽样文件 → 与知识图谱同构的 KbGraph(供 KnowledgeGraph.vue 星河渲染)。
/// 桌面端 async + spawn_blocking,大库建图不冻 UI 主线程(理由同 [`file_overview`])。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_graph(root: Option<String>) -> Result<crate::kb::KbGraph, String> {
    tauri::async_runtime::spawn_blocking(move || build_file_graph(root))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn file_graph(root: Option<String>) -> Result<crate::kb::KbGraph, String> {
    build_file_graph(root)
}

/// 缩略图预取:批量解码盘上图片(可能是慢 NAS 盘),桌面端 async + spawn_blocking 不冻 UI。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn file_warm_thumbs(paths: Vec<String>, max: Option<u32>) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || warm_thumbs(paths, max.unwrap_or(360)))
        .await
        .map_err(|e| format!("任务调度失败: {e}"))
}
#[cfg(not(feature = "desktop"))]
pub fn file_warm_thumbs(paths: Vec<String>, max: Option<u32>) -> Result<usize, String> {
    Ok(warm_thumbs(paths, max.unwrap_or(360)))
}

/// 用已连接的大模型按语义归类(免嵌入 key)。
/// 后台线程跑,进度走 `file:cluster_llm` 事件(phase/tick/done/error)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_llm(app: AppHandle, root: Option<String>) -> Result<(), String> {
    let Some(guard) = FlagGuard::acquire(&LLM_CLUSTERING) else {
        return Err("AI 归类正在进行中".into());
    };
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        let res = cluster_llm_run(&app, root);
        match res {
            Ok((clusters, assigned, report)) => emit_llm(
                &app,
                json!({ "kind": "done", "clusters": clusters, "assigned": assigned, "report": report }),
            ),
            Err(e) => emit_llm(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}

/// AI 智能命名:给杂乱/乱码文件名起可读中文标题,写进 titles 表(只覆盖显示,不改磁盘)。
/// 后台线程跑,进度走 `file:title_llm` 事件(phase/tick/done/error)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_titles_llm(app: AppHandle, root: Option<String>) -> Result<(), String> {
    let Some(guard) = FlagGuard::acquire(&LLM_TITLING) else {
        return Err("AI 命名正在进行中".into());
    };
    std::thread::spawn(move || {
        let _guard = guard; // panic 栈展开也释放闸,防永久锁死
        let res = titles_llm_run(&app, root);
        match res {
            Ok(n) => emit_title(&app, json!({ "kind": "done", "count": n })),
            Err(e) => emit_title(&app, json!({ "kind": "error", "message": e })),
        }
    });
    Ok(())
}

/// 清空 AI 标题 → 卡片标题回落到本地清洗名(撤销 AI 命名)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_titles_clear() -> Result<usize, String> {
    let conn = open_db()?;
    conn.execute("DELETE FROM titles", [])
        .map_err(|e| e.to_string())
}

/// 读「归类专用模型」配置(key 只回 key_set,不回明文)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_model_get() -> ClusterModelView {
    cluster_model_view(&load_cluster_model())
}

/// 存「归类专用模型」配置。api_key 传空字符串=保留旧 key(方便只改模型不重填 key)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn file_cluster_model_set(
    enabled: Option<bool>,
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
) -> Result<ClusterModelView, String> {
    let mut c = load_cluster_model();
    if let Some(e) = enabled {
        c.enabled = e;
    }
    if let Some(b) = base_url {
        if !b.trim().is_empty() {
            c.base_url = b.trim().to_string();
        }
    }
    if let Some(m) = model {
        if !m.trim().is_empty() {
            c.model = m.trim().to_string();
        }
    }
    if let Some(k) = api_key {
        if !k.trim().is_empty() {
            c.api_key = k.trim().to_string(); // 空=保留旧 key
        }
    }
    if c.base_url.trim().is_empty() {
        c.base_url = "https://api.siliconflow.cn".into();
    }
    save_cluster_model(&c)?;
    Ok(cluster_model_view(&c))
}
