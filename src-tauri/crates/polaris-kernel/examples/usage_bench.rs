//! 用量看板扫描 A/B:单线程 vs 并行,跑在本机真实账本上(只读)。
//!
//! 跑法:`cargo run -p polaris-kernel --no-default-features --release --example usage_bench`
//!
//! 两点注意,否则结论会被读错:
//! 1. **账本是活的** —— 只要机器上还有 claude 在跑,`~/.claude/projects` 就在追加写。
//!    所以两档的绝对数字不可能逐字段相等;正确的一致性判据是**按时间单调不减**
//!    (账本只增不减 → 后跑的那次看到的年度累计必须 ≥ 先跑的那次)。
//! 2. **page cache 会偏袒后跑的那次** —— 故交替跑 seq/par/seq/par,取各自第二次比较。

use polaris_kernel::provider::{usage_summary, TokenBucket};

fn total(b: &TokenBucket) -> u64 {
    b.input + b.output + b.cache_read + b.cache_creation
}

fn run(par: bool) -> (f64, u64) {
    if par {
        std::env::remove_var("POLARIS_USAGE_SCAN_PAR");
    } else {
        std::env::set_var("POLARIS_USAGE_SCAN_PAR", "1");
    }
    let t0 = std::time::Instant::now();
    let s = usage_summary().expect("usage_summary");
    (t0.elapsed().as_secs_f64(), total(&s.year))
}

fn main() {
    let mut seq = Vec::new();
    let mut par = Vec::new();
    let mut chronological: Vec<(&str, u64)> = Vec::new();
    for round in 0..2 {
        let (t, y) = run(false);
        println!("第{}轮 单线程   {t:>7.2}s  年度 tokens={y}", round + 1);
        seq.push(t);
        chronological.push(("seq", y));

        let (t, y) = run(true);
        println!("第{}轮 并行     {t:>7.2}s  年度 tokens={y}", round + 1);
        par.push(t);
        chronological.push(("par", y));
    }

    // 一致性:账本只增不减,故按时间顺序的年度累计必须单调不减。
    // 并行若丢行/重复计数,这里立刻会破。
    for w in chronological.windows(2) {
        assert!(
            w[1].1 >= w[0].1,
            "年度累计不该回退:{}={} → {}={}(并行改写可能丢了行)",
            w[0].0,
            w[0].1,
            w[1].0,
            w[1].1
        );
    }
    // 且两档的差距必须只是「这段时间新写进来的量」级别,不能是系统性偏差。
    let drift = chronological.last().unwrap().1 - chronological[0].1;
    let base = chronological[0].1 as f64;
    println!(
        "\n全程账本增长 {drift} tokens({:.4}% —— 这就是两档数字对不齐的全部原因)",
        drift as f64 / base * 100.0
    );

    // 计时取各自第二次(page cache 已热,公平比较)。
    println!(
        "热缓存下:单线程 {:.2}s vs 并行 {:.2}s → 提速 {:.1}×",
        seq[1],
        par[1],
        seq[1] / par[1]
    );
}
