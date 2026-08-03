//! 词法索引真机 A/B:同一份真实语料,串行逐文件读 vs 批量并行读。
//!
//! 跑法:`cargo run -p polaris-fable --no-default-features --release --example lex_bench`
//!
//! 只碰 fable.db 的**临时副本**(整库 copy 到 temp 后再改),绝不动用户的真库。
//! 每档跑前把 `ftsed` 全部清零、`lex` 表清空,两档面对完全相同的待办集。

use std::time::Instant;

fn reset(db: &std::path::Path) -> (u64, u64) {
    let conn = rusqlite::Connection::open(db).expect("open db copy");
    conn.execute_batch("UPDATE files SET ftsed=0; DELETE FROM lex;")
        .expect("reset ftsed/lex");
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE kind='text' AND ftsed=0 AND size<=4000000",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let bytes: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(size),0) FROM files WHERE kind='text' AND ftsed=0 AND size<=4000000",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    (pending as u64, bytes as u64)
}

fn main() {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .expect("home");
    let src = std::path::PathBuf::from(&home).join("Polaris/data/fable.db");
    if !src.exists() {
        eprintln!("找不到真实 fable.db:{}", src.display());
        return;
    }
    let dst = std::env::temp_dir().join("fable_lexbench.db");
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", dst.display()));
        let from = std::path::PathBuf::from(format!("{}{s}", src.display()));
        if from.exists() {
            std::fs::copy(&from, format!("{}{s}", dst.display())).expect("copy db");
        }
    }
    std::env::set_var("POLARIS_FABLE_DB", &dst);

    // 先量「纯读」占整条管子的多少 —— 判断并行读到底该不该指望它提速。
    {
        let conn = rusqlite::Connection::open(&dst).expect("open");
        let mut stmt = conn
            .prepare(
                "SELECT r.path, f.relpath FROM files f JOIN roots r ON r.id=f.root_id
                 WHERE f.kind='text' AND f.size<=4000000",
            )
            .unwrap();
        let paths: Vec<std::path::PathBuf> = stmt
            .query_map([], |r| {
                Ok(std::path::Path::new(&r.get::<_, String>(0)?).join(r.get::<_, String>(1)?))
            })
            .unwrap()
            .flatten()
            .collect();
        for (label, par) in [("串行", "1"), ("并行", "")] {
            if par.is_empty() {
                std::env::remove_var("POLARIS_INDEX_READ_PAR");
            } else {
                std::env::set_var("POLARIS_INDEX_READ_PAR", par);
            }
            let t = Instant::now();
            let mut n = 0usize;
            for chunk in paths.chunks(256) {
                n += polaris_fable::fable::sched::read_batch_bg(chunk.to_vec(), 20)
                    .iter()
                    .filter(|r| matches!(r, Some(Ok(_))))
                    .count();
            }
            println!("  [纯读 {label}] {:.1}s({n} 个文件)", t.elapsed().as_secs_f64());
        }
    }

    let mut out = Vec::new();
    for (label, par) in [("串行(每文件一条线程,旧实现形状)", "1"), ("批量并行(新实现默认宽度)", "")] {
        if par.is_empty() {
            std::env::remove_var("POLARIS_INDEX_READ_PAR");
        } else {
            std::env::set_var("POLARIS_INDEX_READ_PAR", par);
        }
        let (pending, bytes) = reset(&dst);
        let t0 = Instant::now();
        let s = polaris_fable::fable::index::build_lexical_index(&|_f, _p| {})
            .expect("build_lexical_index");
        let dt = t0.elapsed().as_secs_f64();
        println!(
            "{label:34} {dt:>7.1}s  索引 {} 个文件({:.0} MB) → {:.0} 文件/s  [{}]",
            s.files_done,
            bytes as f64 / 1024.0 / 1024.0,
            s.files_done as f64 / dt,
            s.stopped
        );
        assert!(pending > 0, "待办集不该是空的");
        out.push((dt, s.files_done));
    }
    assert_eq!(
        out[0].1, out[1].1,
        "两档索引到的文件数必须一致(否则并行改写漏了文件)"
    );
    println!("\n提速 {:.1}×(同一份语料、同一个待办集)", out[0].0 / out[1].0);
    for s in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{s}", dst.display()));
    }
}
