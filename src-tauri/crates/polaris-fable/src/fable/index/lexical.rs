use super::*;

// ───────────────────────── 词法专扫腿(P0②:覆盖率快赢)─────────────────────────
//
// 头号问题诊断(2026-06-25 真机实测):72.9 万文本只有 ~15% 进了索引,85% 是检索盲区。
// 根因不是「没在跑」,而是**向量与词法被绑在同一个 build_index pass 里、被同一个 chunk 预算
// 闸门掐着**——嵌入吞吐(云 API 35/秒、限速后 8/秒)追不上文件增长,且任一嵌入错误 `break
// 'outer` 会把后续文件的 FTS 也一起停掉。于是「零网络、纯本地、分钟级」的 FTS5 倒排也只建到 15%。
//
// 这条专扫腿把**词法覆盖率与嵌入彻底解耦**:只扫 FTS、绝不碰嵌入、绝不因网络 abort。跑一遍就让
// 关键词搜索覆盖整个硬盘(召回地板从 15% 抬到 ~100%),实时 grep 兜底不再承重(摆脱 2 万文件上限);
// 向量可在之后的几小时里慢慢回填。这是投入最小、体感最直接的一条。

/// 词法覆盖率上限护栏:单次 build 处理的文件数上限(防一轮跑太久占着 INDEXING 闸,幂等续跑)。
const MAX_LEX_FILES_PER_BUILD: u64 = 200_000;

#[derive(Debug, Clone, Serialize)]
pub struct LexSummary {
    pub files_done: u64,
    pub files_pending: u64,
    pub seconds: f64,
    pub stopped: String,
}

/// 词法专扫:把所有还没进 FTS 倒排(ftsed=0)的文本文件**只写倒排、不嵌入**,直到 pending 清零 /
/// 取消 / 文件预算耗尽。与向量构建解耦 —— 零网络、不因嵌入失败中断。`progress(files_done, pending)`。
pub fn build_lexical_index(progress: &dyn Fn(u64, u64)) -> Result<LexSummary, String> {
    let started = std::time::Instant::now();
    let conn = open_db()?;
    if !lex_available(&conn) {
        return Err("FTS5 全文倒排未就绪(数据库未启用 fts5):无法做词法专扫,请重建数据库。".into());
    }
    let mut files_done = 0u64;
    let mut stopped = "全部完成".to_string();
    // 读超时被跳过的文件(ftsed 保持 0 下轮重试)与已判僵死的根(零 IO 跳过)。
    let mut skipped_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut dead_read_roots: std::collections::HashSet<String> = std::collections::HashSet::new();

    // recency 优先(报告 P0③):最近动过的文件先进倒排,大库下用户最可能搜的先可搜。
    // mtime 列自盘点起即有;按它 DESC 排序走 idx_files_lex_pending 仍是范围扫,代价可接受。
    const PENDING_SQL: &str = "SELECT f.id, r.path, f.relpath
         FROM files f JOIN roots r ON r.id=f.root_id
         WHERE f.kind='text' AND f.size<=?1 AND f.ftsed=0
         ORDER BY f.mtime DESC LIMIT 256";

    loop {
        if cancelled() {
            stopped = "已取消".into();
            break;
        }
        if files_done >= MAX_LEX_FILES_PER_BUILD {
            stopped = format!("本轮文件预算({MAX_LEX_FILES_PER_BUILD} 文件)耗尽,可再点继续");
            break;
        }
        let batch: Vec<(i64, String, String)> = {
            let mut stmt = conn.prepare(PENDING_SQL).map_err(|e| e.to_string())?;
            let rows: Vec<(i64, String, String)> = stmt
                .query_map([MAX_LEX_FILE_BYTES], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?
                .flatten()
                .collect();
            rows
        };
        // 被跳过的文件 ftsed 仍为 0,会被 PENDING_SQL 反复选中 → 本轮过滤掉,防死循环。
        let batch: Vec<_> = batch
            .into_iter()
            .filter(|r| !skipped_ids.contains(&r.0))
            .collect();
        if batch.is_empty() {
            break;
        }
        // ── 先读后写:整批正文在**事务外**读完,再开短事务批量写 ──
        // 旧实现 BEGIN 之后才逐个 std::fs::read:写锁横跨整批慢 IO(NAS 上分钟级),
        // 盘点 writer/索引/归类全被钉在 busy_timeout 后面,一个僵死读更是无限期。
        // 读走有界旁路 + 死根拉黑(同 build_index);且**只有真消失(NotFound)才标记完成**:
        // 瞬断/权限/超时一律跳过不标记 —— 否则空正文 + ftsed=1 且 mtime 不变,重扫不会重置,
        // 一次 NAS 抖动就把该文件永久踢出关键词搜索。
        enum Body {
            Text(String),
            /// 真消失 / 伪二进制:照旧清倒排并标记完成(下轮重扫会清行)。
            MarkDone,
            /// 瞬断 / 权限 / 读超时:不写不标,下轮重试。
            Skip,
        }
        let mut bodies: Vec<(i64, Body)> = Vec::with_capacity(batch.len());
        for (file_id, root, rel) in &batch {
            if cancelled() {
                break;
            }
            if dead_read_roots.contains(root) {
                bodies.push((*file_id, Body::Skip));
                continue;
            }
            // 显示路径 → 真实字节路径(GBK 名文件在 Unix 上直接 read 恒失败,会被
            // 当「已消失」空文本标记完成,变成永久检索盲区)。
            let abs = super::reencode_fs_path(
                &std::path::Path::new(root).join(rel).to_string_lossy(),
            );
            let read = {
                let p = abs.clone();
                // _bg:读在旁路线程上发生,优先级不继承 → 旁路自己降后台档,IO 让路前台。
                crate::fable::sched::with_deadline_bg(20, move || {
                    std::fs::read(&p).map_err(|e| e.kind())
                })
            };
            let body = match read {
                None => {
                    dead_read_roots.insert(root.clone());
                    Body::Skip // 读超时:挂载僵死,整根拉黑
                }
                Some(Ok(bytes)) => {
                    if bytes.iter().take(4096).any(|&b| b == 0) {
                        Body::MarkDone // 伪文本(二进制改名),跳过正文但仍标记完成
                    } else {
                        Body::Text(String::from_utf8_lossy(&bytes).into_owned())
                    }
                }
                Some(Err(std::io::ErrorKind::NotFound)) => Body::MarkDone, // 真消失
                Some(Err(_)) => Body::Skip, // 权限/瞬断:下轮重试
            };
            bodies.push((*file_id, body));
        }
        // 整批单事务:几万小文件逐条提交会被 fsync 拖死,批量提交把吞吐拉满。
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        for (file_id, body) in &bodies {
            match body {
                Body::Skip => {
                    skipped_ids.insert(*file_id);
                    continue;
                }
                Body::MarkDone => {
                    conn.execute("DELETE FROM lex WHERE rowid=?1", [file_id])
                        .map_err(|e| e.to_string())?;
                }
                Body::Text(text) => {
                    conn.execute("DELETE FROM lex WHERE rowid=?1", [file_id])
                        .map_err(|e| e.to_string())?;
                    conn.execute(
                        "INSERT INTO lex(rowid, body) VALUES(?1, ?2)",
                        rusqlite::params![file_id, text],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            conn.execute("UPDATE files SET ftsed=1 WHERE id=?1", [file_id])
                .map_err(|e| e.to_string())?;
            files_done += 1;
        }
        conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND ftsed=0",
                [MAX_LEX_FILE_BYTES],
                |r| r.get(0),
            )
            .unwrap_or(0);
        progress(files_done, pending as u64);
        if cancelled() {
            stopped = "已取消".into();
            break;
        }
    }

    if !dead_read_roots.is_empty() && stopped == "全部完成" {
        stopped = "部分存储无响应(NAS 掉线?),其上文件已跳过 —— 恢复连接后再点继续".into();
    }
    let files_pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE kind='text' AND size<=?1 AND ftsed=0",
            [MAX_LEX_FILE_BYTES],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(LexSummary {
        files_done,
        files_pending: files_pending as u64,
        seconds: started.elapsed().as_secs_f64(),
        stopped,
    })
}
