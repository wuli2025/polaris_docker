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
        if batch.is_empty() {
            break;
        }
        // 整批单事务:几万小文件逐条提交会被 fsync 拖死,批量提交把吞吐拉满。
        conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        for (file_id, root, rel) in &batch {
            if cancelled() {
                break;
            }
            // 显示路径 → 真实字节路径(GBK 名文件在 Unix 上直接 read 恒失败,会被
            // 当「已消失」空文本标记完成,变成永久检索盲区)。
            let abs = super::reencode_fs_path(
                &std::path::Path::new(root).join(rel).to_string_lossy(),
            );
            let text = match std::fs::read(&abs) {
                Ok(bytes) => {
                    if bytes.iter().take(4096).any(|&b| b == 0) {
                        String::new() // 伪文本(二进制改名),跳过正文但仍标记完成
                    } else {
                        String::from_utf8_lossy(&bytes).into_owned()
                    }
                }
                Err(_) => String::new(), // 文件已消失/不可读:标记完成,下轮重扫会清
            };
            conn.execute("DELETE FROM lex WHERE rowid=?1", [file_id])
                .map_err(|e| e.to_string())?;
            if !text.is_empty() {
                conn.execute(
                    "INSERT INTO lex(rowid, body) VALUES(?1, ?2)",
                    rusqlite::params![file_id, text],
                )
                .map_err(|e| e.to_string())?;
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
