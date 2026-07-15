//! 检索调参旋钮(RRF 权重 / 各类融合层加权 / 精排闸门 / 降权系数)—— 全为 env 可覆写的
//! 纯函数常量读取器,便于 eval 扫参后固化默认值。

use super::*;

/// 重排候选窗口 N(融合后取前 N 精排;详解第 6 节「甜点区」)。`POLARIS_RERANK_N` 可覆写
/// (clamp 到 [10,100])——报告 §5「宽召回窄重排」建议 50–75,默认保守 40 不变,留作 eval 调参。
const RERANK_N_DEFAULT: usize = 40;
pub(crate) fn rerank_n() -> usize {
    std::env::var("POLARIS_RERANK_N")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|n| n.clamp(10, 100))
        .unwrap_or(RERANK_N_DEFAULT)
}

/// RRF 融合参数(P1④ 可调化):平滑常数 k 与每腿权重。
/// **默认值已按真机 eval 固化(2026-06-26)**:本机库 103,902 chunk / 170 题扫参(eval_rrf_sweep.py)实测,
/// 旧默认 k=60、w_vec=1.0 给满权弱向量腿注入噪声 → hybrid MRR 仅 0.568(**低于纯词法 0.663**);
/// 改 k=10、w_vec=0.85 后 MRR 0.668 / nDCG 0.675(+17.6% MRR、+12% nDCG),recall 持平 0.694。
/// 故把默认改成实测最优,旋钮仍在(换库后可重扫再调):
///   - `POLARIS_RRF_K`(默认 10):小 k 锐化头部,让强腿 top 命中稳坐第一(实测 10~30 都好,60+ 明显劣化);
///   - `POLARIS_RRF_W_GREP`(默认 1.0,强腿基准)/ `POLARIS_RRF_W_VEC`(默认 0.85,给偏弱的向量腿降权):
///     直接治「弱腿注入噪声拉低 MRR」。w_vec=1.0 会把 MRR 砸回 0.568,务必 ≤0.85。
pub(crate) fn rrf_params() -> (f32, f32, f32) {
    let f = |key: &str, dft: f32, lo: f32, hi: f32| -> f32 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|x| x.is_finite())
            .map(|x| x.clamp(lo, hi))
            .unwrap_or(dft)
    };
    (
        f("POLARIS_RRF_K", 10.0, 1.0, 200.0),
        f("POLARIS_RRF_W_GREP", 1.0, 0.0, 4.0),
        f("POLARIS_RRF_W_VEC", 0.85, 0.0, 4.0),
    )
}

/// 查询是否为「纯短关键词」型:无长短语(≥4 CJK 连续字 / ≥6 拉丁词)且内容原子 ≤3。
/// 这类查询正是向量腿最弱的场景(_kbeval 分型:keywords recall@10 ~0.39,远低于 snippet_cjk ~0.87)——
/// 向量往往注入噪声。据此对向量腿动态降权(见 [`kw_vec_damp`]),自然语句/长短语查询保持满权。
pub(crate) fn is_keyword_query(query: &str) -> bool {
    let (latin, runs) = atoms(&query.trim().to_lowercase());
    let has_long_phrase =
        runs.iter().any(|r| r.chars().count() >= 4) || latin.iter().any(|w| w.chars().count() >= 6);
    let atom_count = latin.len() + runs.len();
    !has_long_phrase && (1..=3).contains(&atom_count)
}

/// 关键词查询下向量腿权重的衰减系数(w_vec ×此值)。默认 0.6:把弱向量腿的噪声贡献压下去,
/// 又不像 `POLARIS_VEC_MIN_SCORE` 那样一刀切剔除。`POLARIS_KW_VEC_DAMP=1.0` 关闭(零回归)。
pub(crate) fn kw_vec_damp() -> f32 {
    std::env::var("POLARIS_KW_VEC_DAMP")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 1.0))
        .unwrap_or(0.6)
}

/// 向量腿「条件融合」质量闸(P1④ 进阶,治 MRR < 纯词法 的更准一招):
/// 报告根因是「**弱向量腿往结果顶部注入噪声**」。单纯 `W_VEC` 降权是**全局**手段——它在向量
/// 本就有用的查询上也一并削弱了贡献。更准的做法是按**单条绝对余弦质量**门控:只让余弦 ≥ 阈值
/// 的向量命中参与融合,低于阈值的(典型的「这题向量根本不相关、只是粗筛凑数」)直接不注入。
/// 于是:向量真相关(高余弦)的查询照常受益;向量是噪声(低余弦)的查询自动退化成纯词法、
/// MRR 不被拖低。`POLARIS_VEC_MIN_SCORE` 默认 0.0 = 关闭(零回归);经 eval 扫出最优再固化
/// (BGE-M3 归一化余弦,经验起点 0.35–0.45)。
pub(crate) fn vec_min_score() -> f32 {
    std::env::var("POLARIS_VEC_MIN_SCORE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

/// 路径/文件名命中加权(检索通用强信号:查询词出现在**文件路径/名**里,几乎一定相关)。
/// 现状缺口:两腿都只算**正文**命中,文件名/路径一个字都不计 —— 搜「退款政策」时
/// `policy/退款政策.md` 不会因为名字就被顶上来,得靠正文里恰好也写了才行。这条在融合层给
/// 「路径命中查询词」的文件补一个与 RRF 同量纲的加分:整句出现在**文件名**里最强、出现在路径里
/// 次之、逐词命中累加。`POLARIS_PATH_BOOST` 控权重(默认 0.01,每个 RRF 名次≈1/60≈0.0167,
/// 故文件名整句命中 +0.03 足以把它顶到前列);设 0 关闭(零回归)。
const PATH_BOOST_DEFAULT: f32 = 0.01;
pub(crate) fn path_boost_w() -> f32 {
    std::env::var("POLARIS_PATH_BOOST")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 0.2))
        .unwrap_or(PATH_BOOST_DEFAULT)
}

/// 计算单个文件路径相对查询的加分(见 [`path_boost_w`])。`q_full`=整句小写,`terms`=内容词。
/// 文件名(末段)整句命中权重最高(×3),路径任意处整句命中 ×2,逐内容词命中各 ×1;乘以权重 w。
pub(crate) fn path_boost(path: &str, q_full: &str, terms: &[String], w: f32) -> f32 {
    if w <= 0.0 {
        return 0.0;
    }
    let p = path.to_lowercase().replace('\\', "/");
    let base = p.rsplit('/').next().unwrap_or(&p);
    let mut signal = 0f32;
    if q_full.chars().count() >= 2 {
        if base.contains(q_full) {
            signal += 3.0;
        } else if p.contains(q_full) {
            signal += 2.0;
        }
    }
    for t in terms {
        if t.chars().count() >= 2 && p.contains(t.as_str()) {
            signal += 1.0;
        }
    }
    w * signal
}

/// **全词覆盖 + 整句精确命中加权(融合层「智能重排」,零网络零额度)**。
///
/// 实测动机(2026-07-01,本机库 182,718 chunk / 265 题真机扫参,_kbeval/eval_ai_expand.py):
/// RRF 只看每腿的**名次**,丢掉了「这个候选到底命中了几个查询词、整句在不在」的强信号 ——
/// 关键词查询里「只含 1 个词的文件」会和「含全部词的文件」名次相近,真答案被淹。这条在融合层
/// 给每个候选按其**已在内存的匹配 chunk 文本**(`Fused.doc`,向量腿=chunk 全文 / grep 腿=命中
/// 行上下文)补两个与 RRF 同量纲的加分:
///   - **覆盖率**:命中的 distinct 查询内容词数 / 总词数 ∈ [0,1],×`POLARIS_COVERAGE_BOOST`;
///   - **整句命中**:匹配文本里出现**完整查询子串** → 加 `POLARIS_PHRASE_BOOST`(精确短语必相关)。
/// 实测:hybrid→+覆盖+整句,聚合 nDCG 0.433→0.484(+12%)、中文短语查询 nDCG 0.794→0.852、
/// recall@10 0.509→0.547,**每种查询类型都涨、零回归**;且因 doc 已在内存,无任何额外 IO/网络。
/// 设 `POLARIS_COVERAGE_BOOST=0` 关闭(零回归)。
const COVERAGE_BOOST_DEFAULT: f32 = 0.30;
const PHRASE_BOOST_DEFAULT: f32 = 0.6;
pub(crate) fn coverage_boost_w() -> f32 {
    std::env::var("POLARIS_COVERAGE_BOOST")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 2.0))
        .unwrap_or(COVERAGE_BOOST_DEFAULT)
}
pub(crate) fn phrase_boost_w() -> f32 {
    std::env::var("POLARIS_PHRASE_BOOST")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 4.0))
        .unwrap_or(PHRASE_BOOST_DEFAULT)
}

/// 计算单个候选的「覆盖 + 整句」加分。`doc`=匹配文本(已小写在外部传入以复用),`q_full`=整句小写,
/// `terms`=内容词(split_query 切好的拉丁词 + CJK 二元组)。返回 `cov_w*覆盖率 + (整句命中?phrase_w:0)`。
pub(crate) fn coverage_phrase_boost(
    doc_lower: &str,
    q_full: &str,
    terms: &[String],
    cov_w: f32,
    phrase_w: f32,
) -> f32 {
    if doc_lower.is_empty() || (cov_w <= 0.0 && phrase_w <= 0.0) {
        return 0.0;
    }
    let mut bonus = 0.0;
    if cov_w > 0.0 && !terms.is_empty() {
        let hit = terms
            .iter()
            .filter(|t| doc_lower.contains(t.as_str()))
            .count();
        bonus += cov_w * (hit as f32 / terms.len() as f32);
    }
    if phrase_w > 0.0 && q_full.chars().count() >= 4 && doc_lower.contains(q_full) {
        bonus += phrase_w;
    }
    bonus
}

/// 精排闸门松紧(P2-1 可调化):top-1 与 top-2 的相对差 `(r1-r2)/r1` 小于此值时判「咬得紧、
/// 该花一次重排」。报告 §5「宽召回窄重排」——默认 0.25 保守(只在真难分时重排,省网络);
/// 调大(→ 1.0)= 几乎总重排(MRR 优先、不惜延迟);调小(→ 0)= 几乎不重排(延迟优先)。
/// 用 eval 在 MRR 与延迟间扫出甜点再固化。默认 0.25 = 历史行为(零回归)。
const RERANK_GATE_DEFAULT: f32 = 0.25;
pub(crate) fn rerank_gate() -> f32 {
    std::env::var("POLARIS_RERANK_GATE")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 1.0))
        .unwrap_or(RERANK_GATE_DEFAULT)
}

/// 云交叉编码器重排是否启用。**默认关闭**:实测(_kbeval/eval_ai_rerank.json)云重排单次 ~3226ms,
/// 且在考卷上 recall 0.28–0.39 反不如不重排的 hybrid 0.536 —— 又慢又不涨分,是负资产。故默认直接
/// 跳过这层网络往返(p50 从 ~3.3s 降到 <100ms),把「咬得紧才精排」的闸门整个绕开。
/// `POLARIS_CLOUD_RERANK=1` 恢复旧行为(闸门 0.25 可再经 POLARIS_RERANK_GATE 调);闸门相关基础设施
/// (缓存/签名)保留给后续本地 ColBERT 重排复用。
pub(crate) fn cloud_rerank_enabled() -> bool {
    std::env::var("POLARIS_CLOUD_RERANK")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
}

/// 「新压旧」降权系数:被同目录同名新版本压制的命中,其融合分 ×此值(默认 0.4,把旧版从 top3
/// 挤到 top10 边缘但仍可达)。`POLARIS_SUPERSEDE_DECAY=1.0` → 不降权、且跳过整段查库(一键关闭)。
pub(crate) fn supersede_decay() -> f32 {
    std::env::var("POLARIS_SUPERSEDE_DECAY")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 1.0))
        .unwrap_or(0.4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_query_detection_targets_short_terms() {
        // 短关键词组 → 判为 keyword(向量腿降权)
        assert!(is_keyword_query("退款 政策"));
        assert!(is_keyword_query("api rate limit"));
        assert!(is_keyword_query("索引"));
        // 长短语 / 自然语句 → 不降权
        assert!(!is_keyword_query("如何处理退款政策的具体流程"));
        assert!(!is_keyword_query("configuration"));
        // 空查询不判为 keyword(避免对空输入误降权)
        assert!(!is_keyword_query("   "));
    }

    #[test]
    fn rrf_params_default_is_eval_tuned_optimum() {
        // 默认值已按真机 eval 固化:k=10、w_grep=1.0、w_vec=0.85(实测 MRR 0.668 > 旧默认 0.568)。
        // (本测试不写 env 以免污染并行测试;只校验默认值与权重应用语义。)
        let (k, wg, wv) = rrf_params();
        assert_eq!(k, 10.0);
        assert_eq!(wg, 1.0);
        assert_eq!(wv, 0.85);
        // 强腿(词法)未降权:grep rank0 贡献 = 1/(k+0) = 0.1。
        let rank = 0usize;
        assert!((wg / (k + rank as f32) - 1.0 / 10.0).abs() < 1e-9);
        // 弱腿(向量)已降权:同名次下贡献严格小于强腿(治噪声注入)。
        assert!(wv / (k + rank as f32) < wg / (k + rank as f32));
        // rerank_n 默认 40,且在合理范围。
        let n = rerank_n();
        assert!((10..=100).contains(&n));
    }

    #[test]
    fn vec_min_score_default_off_and_gate_semantics() {
        // 默认(不设 env)阈值 0.0 → 条件融合关闭:任何余弦的向量命中都过门(零回归)。
        let vmin = vec_min_score();
        assert_eq!(vmin, 0.0);
        // 门控语义:`h.score >= vmin`。默认下连分数为 0 的命中也保留(恒真)。
        for s in [0.0_f32, 0.1, 0.35, 0.7, 1.0] {
            assert!(s >= vmin, "默认阈值下余弦 {s} 应过门");
        }
        // 若阈值设为 0.4(模拟固化值),则 0.35 的噪声命中被门掉、0.7 的真命中保留。
        let strict = 0.4_f32;
        assert!(0.35_f32 < strict, "0.35 应被 0.4 阈值门掉(噪声不注入)");
        assert!(0.7_f32 >= strict, "0.7 应过 0.4 阈值(真相关保留)");
    }

    #[test]
    fn path_boost_rewards_filename_and_path_hits() {
        let w = 0.01_f32;
        let terms = vec!["退款".to_string(), "政策".to_string()];
        // 文件名整句命中 → ×3 + 两个词各 ×1 = 5w
        let a = path_boost("policy/退款政策.md", "退款政策", &terms, w);
        assert!((a - 5.0 * w).abs() < 1e-6, "文件名整句命中 a={a}");
        // 路径里整句命中(非末段)→ ×2 + 两词 ×1 = 4w
        let b = path_boost("退款政策/notes/readme.md", "退款政策", &terms, w);
        assert!((b - 4.0 * w).abs() < 1e-6, "路径整句命中 b={b}");
        // 完全不沾边 → 0
        assert_eq!(path_boost("misc/todo.txt", "退款政策", &terms, w), 0.0);
        // 文件名命中应当 > 路径命中 > 无命中(排序意义)
        assert!(a > b && b > 0.0);
        // 权重 0 = 关闭(零回归)
        assert_eq!(
            path_boost("policy/退款政策.md", "退款政策", &terms, 0.0),
            0.0
        );
        // 默认权重为正(通用增益默认开)
        assert!(path_boost_w() > 0.0);
    }

    #[test]
    fn coverage_phrase_boost_rewards_full_term_coverage_and_exact_phrase() {
        let terms = vec!["知识".to_string(), "识库".to_string(), "检索".to_string()];
        let q_full = "知识库检索精度";
        // 含全部内容词 → 覆盖率 1.0 × cov_w
        let full = coverage_phrase_boost("如何提升知识库检索效果", q_full, &terms, 0.30, 0.6);
        // 只含一个词 → 覆盖率 1/3 × cov_w
        let partial = coverage_phrase_boost("检索系统设计", q_full, &terms, 0.30, 0.6);
        // 完全不沾边 → 0
        let none = coverage_phrase_boost("今天天气不错", q_full, &terms, 0.30, 0.6);
        assert!(
            full > partial && partial > none && none == 0.0,
            "覆盖越多分越高"
        );
        assert!(
            (full - 0.30).abs() < 1e-6,
            "全覆盖=cov_w(无整句命中)full={full}"
        );
        assert!(
            (partial - 0.10).abs() < 1e-6,
            "1/3 覆盖=cov_w/3 partial={partial}"
        );
        // 整句精确命中 → 额外 +phrase_w(且整句必含全部内容词 → 再 +cov_w)
        let phrase = coverage_phrase_boost("文档讲知识库检索精度的做法", q_full, &terms, 0.30, 0.6);
        assert!(
            (phrase - (0.30 + 0.6)).abs() < 1e-6,
            "整句命中=cov_w+phrase_w phrase={phrase}"
        );
        // 权重置 0 → 关闭(零回归)
        assert_eq!(
            coverage_phrase_boost("知识库检索", q_full, &terms, 0.0, 0.0),
            0.0
        );
        // 默认权重为正(默认开)
        assert!(coverage_boost_w() > 0.0 && phrase_boost_w() > 0.0);
    }

    #[test]
    fn rerank_gate_default_preserves_behavior() {
        // 默认(不设 env)= 0.25 = 历史硬编码,零回归。
        assert_eq!(rerank_gate(), 0.25);
        // 门控语义自洽:top1/top2 相对差 0.1(咬得紧)< 默认阈值 → 触发重排;0.5(一骑绝尘)→ 不触发。
        let (r1, close, far) = (1.0_f32, 0.9_f32, 0.5_f32);
        assert!(
            (r1 - close) / r1 < rerank_gate(),
            "差 0.1 应判咬得紧、该重排"
        );
        assert!(
            (r1 - far) / r1 >= rerank_gate(),
            "差 0.5 应判一骑绝尘、不必重排"
        );
    }
}
