//! 寓言计划 · 框架派(Schema-Guided)本体抽取 —— D 方案落地(企业 / B 端)。
//!
//! 接桌面两份报告:《聚类派 B 方案与框架派 D 方案改造报告》《工业级 Schema 模板 · GitHub
//! 真实仓库选型报告》。两派分工:
//!   - B 方案(聚类驱动 / 自下而上)= 已有 [`super::files::cluster_build`] / `cluster_llm`,
//!     零冷启动、自动发现主题,面向**个人(C 端)**;
//!   - D 方案(框架派 / 自上而下,本模块)= **先给定 schema、让 LLM 在框内做选择题抽取**,
//!     低幻觉、可审计、产出**显式三元组**,面向**企业(B 端)**。
//!
//! schema 来自 GitHub 工业级本体的**中文化精简**(实体类型 + 关系类型),来源标注在每个
//! schema 的 `source` 上(FollowTheMoney / FHIR / Schema.org / GS1 等,见选型报告)。
//!
//! 铁律(与 kb.rs / files.rs 同构):**AI 出决策(JSON 三元组)、Rust 校验落库**。LLM 只
//! 在给定类型清单内选择并输出 JSON,Rust 负责:校验类型合法、置信阈值、来源留痕、写库。

use super::open_db;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(not(feature = "desktop"))]
use crate::host::AppHandle;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};

// ───────────────────────── 内置行业 Schema(中文化精简) ─────────────────────────

/// 一个本体类型(实体或关系)。`hint` 给 LLM 一句话提示「什么算这个类型」,压缩幻觉空间。
pub struct OntoTypeDef {
    pub id: &'static str,
    pub name: &'static str,
    pub hint: &'static str,
}

/// 一个行业 schema = 实体类型表 + 关系类型表 + 来源标注。
pub struct SchemaDef {
    pub id: &'static str,
    pub name: &'static str,
    pub industry: &'static str,
    pub source: &'static str,
    pub desc: &'static str,
    pub entities: &'static [OntoTypeDef],
    pub relations: &'static [OntoTypeDef],
}

macro_rules! t {
    ($id:expr, $name:expr, $hint:expr) => {
        OntoTypeDef {
            id: $id,
            name: $name,
            hint: $hint,
        }
    };
}

/// 全部内置 schema。新增行业只在此追加,前端 / 抽取自动跟上。
pub fn schemas() -> &'static [SchemaDef] {
    &[
        // 通用(Schema.org 跨行业骨架):没有明确行业 / 个人偏企业向时的兜底框。
        SchemaDef {
            id: "general",
            name: "通用知识图谱",
            industry: "通用 · 跨行业",
            source: "Schema.org",
            desc: "人物 / 组织 / 项目 / 地点 / 事件的通用框,适合说不清具体行业时打底。",
            entities: &[
                t!("person", "人物", "个人:同事、客户、作者、联系人等"),
                t!("org", "组织", "公司、机构、团队、部门、单位"),
                t!("project", "项目", "项目、产品、计划、工程"),
                t!("place", "地点", "城市、地址、办公地、区域"),
                t!("event", "事件", "会议、活动、节点、里程碑"),
                t!("doc", "资料", "文档、合同、报告等可引用的材料"),
            ],
            relations: &[
                t!("belongs", "隶属", "人物隶属于组织 / 项目隶属于组织"),
                t!("works_on", "参与", "人物参与项目 / 活动"),
                t!("located", "位于", "组织 / 事件位于地点"),
                t!("about", "涉及", "资料涉及某人 / 某组织 / 某项目"),
                t!("related", "关联", "两个实体存在明确关联(说不清更细时用)"),
            ],
        },
        // 金融 / 合规(FollowTheMoney + FIBO):反洗钱 / KYC / 尽调天然契合。
        SchemaDef {
            id: "finance",
            name: "金融与合规",
            industry: "金融 · 风控 · 合规",
            source: "FollowTheMoney · FIBO",
            desc: "账户 / 流水 / 持有 / 制裁,面向反洗钱、KYC、尽调与资金穿透。",
            entities: &[
                t!("person", "人物", "自然人:受益人、法定代表人、关联人"),
                t!("company", "公司", "企业、机构、空壳公司、关联方"),
                t!("account", "账户", "银行账户、资金账户、钱包"),
                t!("transaction", "交易", "一笔转账 / 支付 / 资金往来"),
                t!("asset", "资产", "不动产、股权、证券、加密货币等"),
                t!("sanction", "制裁记录", "黑名单、被制裁、风险标记"),
                t!("case", "案件", "诉讼、调查、合规事件"),
            ],
            relations: &[
                t!("owns", "持有", "人物 / 公司 持有 公司股权 / 资产 / 账户"),
                t!("controls", "控制", "实际控制、最终受益(穿透到自然人)"),
                t!("officer", "任职", "人物在公司任董事 / 高管 / 法人"),
                t!("transfer", "转账", "账户 → 账户 的资金流动"),
                t!("guarantee", "担保", "为某笔债务 / 主体提供担保"),
                t!("sanctioned", "受制裁", "主体 被列入 制裁 / 黑名单"),
                t!("involved", "涉及", "主体 涉及 某案件"),
            ],
        },
        // 医疗(HL7 FHIR,CC0):资源类型即实体,Reference 即关系。
        SchemaDef {
            id: "medical",
            name: "医疗健康",
            industry: "医疗 · 临床",
            source: "HL7 FHIR",
            desc: "患者 / 诊断 / 用药 / 就诊,贴 FHIR 资源模型,面向病历与临床资料。",
            entities: &[
                t!("patient", "患者", "就诊的个人"),
                t!("condition", "诊断", "疾病、症状、临床诊断"),
                t!("medication", "用药", "药品、处方、给药"),
                t!("procedure", "检查处置", "检查、手术、操作"),
                t!("encounter", "就诊", "一次门诊 / 住院 / 接诊"),
                t!("practitioner", "医生", "医师、护士、医务人员"),
                t!("org", "医疗机构", "医院、科室、诊所"),
            ],
            relations: &[
                t!("diagnosed", "诊断为", "患者 被诊断为 某疾病"),
                t!("prescribed", "开具", "医生 / 就诊 开具 某用药"),
                t!("performed", "执行", "对患者 执行 某检查处置"),
                t!("visited", "就诊于", "患者 就诊于 机构 / 医生"),
                t!("affiliated", "隶属", "医生 隶属于 医疗机构"),
            ],
        },
        // 电商(Schema.org Product/Offer):LLM 极熟,属性扁平。
        SchemaDef {
            id: "ecommerce",
            name: "电商零售",
            industry: "电商 · 零售",
            source: "Schema.org · Google Product Taxonomy",
            desc: "商品 / 报价 / 订单 / 评价,面向商品库、店铺与交易资料。",
            entities: &[
                t!("product", "商品", "在售商品、SKU"),
                t!("brand", "品牌", "品牌、厂商"),
                t!("offer", "报价", "价格、促销、库存口径"),
                t!("order", "订单", "一笔下单 / 交易"),
                t!("customer", "客户", "买家、会员"),
                t!("review", "评价", "评论、评分、反馈"),
            ],
            relations: &[
                t!("of_brand", "属于品牌", "商品 属于 某品牌"),
                t!("priced", "定价", "商品 由 报价 标价"),
                t!("ordered", "下单", "客户 下单 某商品 / 订单"),
                t!("contains", "包含", "订单 包含 某商品"),
                t!("reviewed", "评价", "客户 评价 某商品"),
            ],
        },
        // 法律 / 合同:合同要素 + 当事方 + 义务权利。
        SchemaDef {
            id: "legal",
            name: "法律与合同",
            industry: "法律 · 合规",
            source: "Schema.org Legislation · LKIF",
            desc: "合同 / 当事方 / 条款 / 义务,面向合同、法务与合规文档。",
            entities: &[
                t!("contract", "合同", "合同、协议、契约"),
                t!("party", "当事方", "签约主体:甲方 / 乙方 / 第三方"),
                t!("clause", "条款", "合同中的具体条款"),
                t!("obligation", "义务", "应履行的责任、付款、交付"),
                t!("right", "权利", "享有的权利、许可、保障"),
                t!("case", "案件", "纠纷、诉讼、仲裁"),
            ],
            relations: &[
                t!("signed", "签署", "当事方 签署 合同"),
                t!("stipulates", "约定", "合同 / 条款 约定 某义务 / 权利"),
                t!("bears", "承担", "当事方 承担 某义务"),
                t!("entitled", "享有", "当事方 享有 某权利"),
                t!("disputes", "涉诉", "合同 / 当事方 涉及 某案件"),
            ],
        },
        // 制造 / 供应链(GS1 EPCIS + AAS):货品 / 库位 / 运单 / 设备。
        SchemaDef {
            id: "supplychain",
            name: "制造与供应链",
            industry: "制造 · 供应链 · 物流",
            source: "GS1 EPCIS · Asset Administration Shell",
            desc: "货品 / 库位 / 运单 / 设备,面向生产、仓储、物流可视化资料。",
            entities: &[
                t!("item", "货品", "物料、成品、批次"),
                t!("location", "库位", "仓库、工位、产线、货架"),
                t!("shipment", "运单", "发货、运输、配送单"),
                t!("equipment", "设备", "机床、机器人、产线设备"),
                t!("process", "工序", "生产工序、加工步骤"),
                t!("supplier", "供应商", "上游供应商、合作方"),
            ],
            relations: &[
                t!("stored_at", "存放于", "货品 存放于 某库位"),
                t!("shipped", "运输", "货品 经 某运单 运输"),
                t!("produced_by", "由生产", "货品 由 某设备 / 工序 生产"),
                t!("supplied_by", "供应", "货品 由 某供应商 供应"),
                t!("step_of", "工序属于", "工序 属于 某产线 / 设备"),
            ],
        },
    ]
}

pub fn find_schema(id: &str) -> Option<&'static SchemaDef> {
    schemas().iter().find(|s| s.id == id)
}

// ───────────────────────── 前端视图 ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OntoTypeView {
    pub id: String,
    pub name: String,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaView {
    pub id: String,
    pub name: String,
    pub industry: String,
    pub source: String,
    pub desc: String,
    pub entities: Vec<OntoTypeView>,
    pub relations: Vec<OntoTypeView>,
    /// 已抽取并落库的三元组数(让企业一眼看到「框里已经装了多少」)。
    pub triples: u64,
}

fn type_views(defs: &[OntoTypeDef]) -> Vec<OntoTypeView> {
    defs.iter()
        .map(|d| OntoTypeView {
            id: d.id.into(),
            name: d.name.into(),
            hint: d.hint.into(),
        })
        .collect()
}

/// 列出全部内置行业 schema(企业路径选「框」用),附各自已落库三元组数。
fn schemas_inner() -> Result<Vec<SchemaView>, String> {
    let conn = open_db()?;
    let mut out = Vec::new();
    for s in schemas() {
        let triples = conn
            .query_row(
                "SELECT COUNT(*) FROM triples WHERE schema_id=?1",
                [s.id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;
        out.push(SchemaView {
            id: s.id.into(),
            name: s.name.into(),
            industry: s.industry.into(),
            source: s.source.into(),
            desc: s.desc.into(),
            entities: type_views(s.entities),
            relations: type_views(s.relations),
            triples,
        });
    }
    Ok(out)
}

fn overview_inner() -> Result<OntologyOverview, String> {
    let schemas = schemas_inner()?;
    let total = schemas.iter().map(|s| s.triples).sum();
    Ok(OntologyOverview {
        total_triples: total,
        schemas,
    })
}

// 桌面端 async + spawn_blocking:每个 schema 一条 `COUNT(*) FROM triples`,在后台索引满负荷
// 时同步直调会阻塞 UI 主线程。server flavor 保持同步直调内层。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn ontology_schemas() -> Result<Vec<SchemaView>, String> {
    tauri::async_runtime::spawn_blocking(schemas_inner)
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn ontology_schemas() -> Result<Vec<SchemaView>, String> {
    schemas_inner()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OntologyOverview {
    pub total_triples: u64,
    pub schemas: Vec<SchemaView>,
}

/// 本体总览:各 schema 的三元组数(给「核心层」/ 企业首页用)。桌面端 async + spawn_blocking。
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn ontology_overview() -> Result<OntologyOverview, String> {
    tauri::async_runtime::spawn_blocking(overview_inner)
        .await
        .map_err(|e| format!("任务调度失败: {e}"))?
}
#[cfg(not(feature = "desktop"))]
pub fn ontology_overview() -> Result<OntologyOverview, String> {
    overview_inner()
}

/// 把某 schema 的类型清单写进 onto_types(幂等:先清同 schema 再写)。让本体类型可被
/// 检索 / 前端枚举,也作为「这个框定了哪些类型」的单一事实源。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn ontology_seed(schema_id: String) -> Result<usize, String> {
    let s = find_schema(&schema_id).ok_or_else(|| format!("未知 schema: {schema_id}"))?;
    let conn = open_db()?;
    conn.execute("DELETE FROM onto_types WHERE schema_id=?1", [s.id])
        .ok();
    let mut n = 0usize;
    conn.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    {
        let mut stmt = conn
            .prepare_cached(
                "INSERT OR IGNORE INTO onto_types(schema_id,type_id,name,kind,hint) VALUES(?1,?2,?3,?4,?5)",
            )
            .map_err(|e| e.to_string())?;
        for (kind, defs) in [("entity", s.entities), ("relation", s.relations)] {
            for d in defs {
                stmt.execute(rusqlite::params![s.id, d.id, d.name, kind, d.hint])
                    .map_err(|e| e.to_string())?;
                n += 1;
            }
        }
    }
    conn.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    Ok(n)
}

// ───────────────────────── Schema-Guided 抽取(AI 决策 / Rust 落库) ─────────────────────────

/// 抽取进行中(防双发)。
static EXTRACTING: AtomicBool = AtomicBool::new(false);
static EXTRACT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn emit_onto(app: &AppHandle, payload: Value) {
    let _ = app.emit("fable:ontology", payload);
}

/// LLM 回的一条三元组(字段宽松,兼容多种写法)。
#[derive(Debug, Deserialize)]
struct TripleIn {
    #[serde(default)]
    subject: String,
    #[serde(default, alias = "subjectType")]
    subject_type: String,
    #[serde(default, alias = "predicate", alias = "relation")]
    predicate: String,
    #[serde(default)]
    object: String,
    #[serde(default, alias = "objectType")]
    object_type: String,
    #[serde(default)]
    confidence: f64,
    #[serde(default, alias = "source", alias = "sourceFile")]
    source_file: String,
}

/// Schema-Guided 抽取指令:给定类型清单,让 LLM「做选择题」抽三元组,只输出 JSON。
fn extract_directive(s: &SchemaDef, root_disp: &str) -> String {
    let ents: String = s
        .entities
        .iter()
        .map(|e| format!("  - {} ({}):{}", e.name, e.id, e.hint))
        .collect::<Vec<_>>()
        .join("\n");
    let rels: String = s
        .relations
        .iter()
        .map(|r| format!("  - {} ({}):{}", r.name, r.id, r.hint))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"你是「{name}」领域的**结构化抽取员**(框架派 / Schema-Guided)。工作目录就是资料库:`{root}`。

你的任务:读资料,抽出**实体之间的关系三元组**。这是**做选择题,不是自由发挥** ——
实体类型和关系类型都**只能从下面给定的清单里选**,清单里没有的类型一律不要造。

## 实体类型(主语 / 宾语只能属于这些)
{ents}

## 关系类型(谓语只能用这些的中文名)
{rels}

## 怎么做
1. 用 Glob / Grep / Read 浏览资料(靠文件名和抽样了解,**不要逐篇全文读**,控制成本);
2. 找出明确出现的实体,以及它们之间**资料里真实写明**的关系;
3. 每条关系输出一条三元组:主语、主语类型、谓语(关系)、宾语、宾语类型、置信度、来源文件;
4. **拿不准就不抽**;资料没写明的关系**绝不臆造**(这是合规底线);
5. 置信度 confidence ∈ 0~1:资料里白纸黑字=0.9 以上,需要推断=0.6 左右,不足 0.5 的别输出。

## 只输出一个 JSON 数组,不要任何额外文字、不要 markdown 代码围栏:
[
  {{"subject":"实体名","subjectType":"实体类型中文名","predicate":"关系中文名","object":"实体名","objectType":"实体类型中文名","confidence":0.9,"source":"raw/某文件"}},
  ...
]

现在开始。先浏览资料,再输出 JSON。"#,
        name = s.name,
        root = root_disp,
        ents = ents,
        rels = rels,
    )
}

/// 校验:谓语必须是 schema 里的关系名(或 id);类型名归一到中文名。返回归一后的 (谓语, 主类, 宾类)。
fn validate_triple(s: &SchemaDef, t: &TripleIn) -> Option<(String, String, String)> {
    if t.subject.trim().is_empty() || t.object.trim().is_empty() || t.predicate.trim().is_empty() {
        return None;
    }
    let p = t.predicate.trim();
    let rel = s.relations.iter().find(|r| r.name == p || r.id == p)?;
    let norm_ent = |raw: &str| -> String {
        let raw = raw.trim();
        s.entities
            .iter()
            .find(|e| e.name == raw || e.id == raw)
            .map(|e| e.name.to_string())
            .unwrap_or_else(|| raw.to_string())
    };
    Some((
        rel.name.to_string(),
        norm_ent(&t.subject_type),
        norm_ent(&t.object_type),
    ))
}

/// 启动一次 Schema-Guided 抽取:后台 headless claude 在框内抽三元组 → Rust 校验落库。
/// 立即返回 run_id;进度走 `fable:ontology` 事件(phase / tick / done / error)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn ontology_extract(app: AppHandle, schema_id: String) -> Result<String, String> {
    let s = find_schema(&schema_id).ok_or_else(|| format!("未知 schema: {schema_id}"))?;
    if EXTRACTING.swap(true, Ordering::SeqCst) {
        return Err("已有一个抽取任务在跑,请等它完成".into());
    }
    // 进库前先把类型清单 seed 好(忽略错误,不阻断抽取)。
    let _ = ontology_seed(schema_id.clone());

    let c = EXTRACT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_id = format!("onto-{c:x}");
    let root = PathBuf::from(crate::kb::kb_root());
    let cwd = if root.exists() {
        root
    } else {
        std::env::temp_dir()
    };
    let root_disp = cwd.to_string_lossy().replace('\\', "/");
    let prompt = extract_directive(s, &root_disp);
    let schema_owned = s.id.to_string();

    std::thread::spawn(move || {
        // 守卫:线程无论正常 / panic 结束都释放抽取闸。
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                EXTRACTING.store(false, Ordering::SeqCst);
            }
        }
        let _g = Guard;

        emit_onto(
            &app,
            json!({ "kind": "phase", "text": "在框内读资料、抽关系三元组…" }),
        );
        let collected = match crate::kb::run_claude_readonly(&cwd, &prompt, |kind, _t| {
            if kind == "delta" {
                emit_onto(&app, json!({ "kind": "tick" }));
            } else if kind == "tool" {
                emit_onto(&app, json!({ "kind": "phase", "text": "正在浏览资料…" }));
            }
        }) {
            Ok(t) => t,
            Err(e) => {
                emit_onto(&app, json!({ "kind": "error", "message": e }));
                return;
            }
        };

        let raw = match crate::kb::extract_balanced_json(&collected) {
            Some(r) => r,
            None => {
                emit_onto(
                    &app,
                    json!({ "kind": "error", "message": "模型没有返回可解析的 JSON,可换更强的模型重试" }),
                );
                return;
            }
        };
        let parsed: Vec<TripleIn> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                emit_onto(
                    &app,
                    json!({ "kind": "error", "message": format!("三元组 JSON 解析失败: {e}") }),
                );
                return;
            }
        };

        emit_onto(&app, json!({ "kind": "phase", "text": "校验类型 + 落库…" }));
        let Some(sdef) = find_schema(&schema_owned) else {
            emit_onto(&app, json!({ "kind": "error", "message": "schema 丢失" }));
            return;
        };
        let conn = match open_db() {
            Ok(c) => c,
            Err(e) => {
                emit_onto(&app, json!({ "kind": "error", "message": e }));
                return;
            }
        };
        // 本轮重抽:先清掉同 schema 旧三元组,避免重复累积。
        conn.execute("DELETE FROM triples WHERE schema_id=?1", [&schema_owned])
            .ok();
        let made_at = chrono::Local::now().timestamp_millis();
        let mut kept = 0usize;
        let mut dropped = 0usize;
        let _ = conn.execute_batch("BEGIN");
        if let Ok(mut stmt) = conn.prepare_cached(
            "INSERT INTO triples(schema_id,subject,subject_type,predicate,object,object_type,confidence,source_file,made_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        ) {
            for t in &parsed {
                // 置信阈值 0.5(报告里的可靠性闸之一)。
                if t.confidence > 0.0 && t.confidence < 0.5 {
                    dropped += 1;
                    continue;
                }
                let Some((pred, st, ot)) = validate_triple(sdef, t) else {
                    dropped += 1;
                    continue;
                };
                let conf = if t.confidence <= 0.0 { 0.6 } else { t.confidence.min(1.0) };
                if stmt
                    .execute(rusqlite::params![
                        schema_owned,
                        t.subject.trim(),
                        st,
                        pred,
                        t.object.trim(),
                        ot,
                        conf,
                        t.source_file.trim(),
                        made_at,
                    ])
                    .is_ok()
                {
                    kept += 1;
                }
            }
        }
        let _ = conn.execute_batch("COMMIT");

        emit_onto(
            &app,
            json!({
                "kind": "done",
                "kept": kept,
                "dropped": dropped,
                "schemaId": schema_owned,
                "note": format!("已在「{}」框内抽出 {} 条可靠关系(过滤掉 {} 条越框 / 低置信)", sdef.name, kept, dropped),
            }),
        );
    });

    Ok(run_id)
}

// ───────────────────────── 三元组查询(核心层 / 实体卡用) ─────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TripleView {
    pub subject: String,
    pub subject_type: String,
    pub predicate: String,
    pub object: String,
    pub object_type: String,
    pub confidence: f64,
    pub source_file: String,
}

/// 取某 schema 已抽出的三元组(按置信度倒序,封顶 limit)。
#[cfg_attr(feature = "desktop", tauri::command)]
pub fn ontology_triples(schema_id: String, limit: Option<u32>) -> Result<Vec<TripleView>, String> {
    let conn = open_db()?;
    let lim = limit.unwrap_or(500).min(5000);
    let mut stmt = conn
        .prepare(
            "SELECT subject,subject_type,predicate,object,object_type,confidence,source_file
             FROM triples WHERE schema_id=?1 ORDER BY confidence DESC, id DESC LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![schema_id, lim], |r| {
            Ok(TripleView {
                subject: r.get(0)?,
                subject_type: r.get(1)?,
                predicate: r.get(2)?,
                object: r.get(3)?,
                object_type: r.get(4)?,
                confidence: r.get(5)?,
                source_file: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_have_chinese_names_and_types() {
        for s in schemas() {
            assert!(!s.name.is_empty(), "schema 缺中文名");
            assert!(!s.entities.is_empty(), "{} 缺实体类型", s.id);
            assert!(!s.relations.is_empty(), "{} 缺关系类型", s.id);
            // 全中文显示名(让企业看到的是中文,不是一堆英文)。
            for d in s.entities.iter().chain(s.relations.iter()) {
                assert!(
                    d.name
                        .chars()
                        .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                    "{}/{} 显示名应含中文",
                    s.id,
                    d.id
                );
            }
        }
    }

    #[test]
    fn validate_rejects_out_of_frame() {
        let s = find_schema("finance").unwrap();
        // 越框的谓语 → 拒。
        let bad = TripleIn {
            subject: "张三".into(),
            subject_type: "人物".into(),
            predicate: "喜欢".into(),
            object: "李四".into(),
            object_type: "人物".into(),
            confidence: 0.9,
            source_file: "raw/x".into(),
        };
        assert!(super::validate_triple(s, &bad).is_none(), "越框关系应被拒");
        // 框内谓语(中文名)→ 收,且类型归一。
        let good = TripleIn {
            subject: "甲公司".into(),
            subject_type: "company".into(),
            predicate: "持有".into(),
            object: "乙公司".into(),
            object_type: "公司".into(),
            confidence: 0.95,
            source_file: "raw/y".into(),
        };
        let (p, st, ot) = super::validate_triple(s, &good).expect("框内关系应收");
        assert_eq!(p, "持有");
        assert_eq!(st, "公司", "subjectType 应从 id 归一到中文名");
        assert_eq!(ot, "公司");
    }

    #[test]
    fn empty_fields_rejected() {
        let s = find_schema("general").unwrap();
        let empty = TripleIn {
            subject: "".into(),
            subject_type: "人物".into(),
            predicate: "隶属".into(),
            object: "X".into(),
            object_type: "组织".into(),
            confidence: 0.9,
            source_file: "".into(),
        };
        assert!(super::validate_triple(s, &empty).is_none(), "空主语应被拒");
    }
}
