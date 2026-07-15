use super::*;

#[test]
fn b64_roundtrip_basic() {
    assert_eq!(b64(b"Man"), "TWFu");
    assert_eq!(b64(b"Ma"), "TWE=");
    assert_eq!(b64(b"M"), "TQ==");
}

// file_overview 并发合并: TTL 内的重复概览应命中缓存(只算一次),TTL=0 关闭缓存。
// 用一个空库(overview 在空 fable.db 上仍返回结构),验证第二次调用走缓存(远快)且结果相等。
#[test]
fn overview_ttl_cache_coalesces() {
    // 隔离到临时 fable.db,避免碰真实库。
    let tmp = std::env::temp_dir().join(format!("fable-ovcache-{}.db", std::process::id()));
    std::env::set_var("POLARIS_FABLE_DB", &tmp);
    std::env::set_var("POLARIS_FABLE_OVERVIEW_TTL_MS", "5000");
    let a = overview(None).expect("overview 1");
    let b = overview(None).expect("overview 2 (应命中缓存)");
    // 缓存返回同一份聚合(结构与计数一致)。
    assert_eq!(a.total_files, b.total_files);
    // TTL=0 关闭缓存后仍能直算(不 panic)。
    std::env::set_var("POLARIS_FABLE_OVERVIEW_TTL_MS", "0");
    let c = overview(None).expect("overview 3 (关缓存直算)");
    assert_eq!(a.total_files, c.total_files);
    std::env::remove_var("POLARIS_FABLE_OVERVIEW_TTL_MS");
    let _ = std::fs::remove_file(&tmp);
}

// ── 文件中心 v3 渐进式归类 ──

#[test]
fn loose_i64_accepts_num_and_str() {
    assert_eq!(loose_i64(&Value::from(5i64)), Some(5));
    assert_eq!(loose_i64(&Value::from("7")), Some(7));
    assert_eq!(loose_i64(&Value::from(" 9 ")), Some(9));
    assert_eq!(loose_i64(&Value::from("x")), None);
    assert_eq!(loose_i64(&Value::Null), None);
}

#[test]
fn name_rel_json_parses_mixed_id_types() {
    // 模型可能把 id 写成数字或字符串,两种都要吃下。
    let raw = r#"{"names":[{"id":1,"name":"我的报税","summary":"2023 报税材料"},
                            {"id":"2","name":"装修"}],
                  "relations":[{"from":1,"to":2,"label":"配套"}]}"#;
    let p: LlmNameRel = serde_json::from_str(raw).unwrap();
    assert_eq!(p.names.len(), 2);
    assert_eq!(p.relations.len(), 1);
    assert_eq!(loose_i64(&p.names[1].id), Some(2));
}

#[test]
fn rename_apply_validates_and_writes() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
            size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
            summary TEXT NOT NULL DEFAULT '');
         CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
         INSERT INTO clusters(id,label) VALUES(1,'簇A'),(2,'簇B');",
    )
    .unwrap();
    // id=99 越界、from=3 越界、2→2 自环 —— 都必须被挡掉。
    let raw = r#"{"names":[{"id":1,"name":"我的报税","summary":"报税材料都在这"},
                            {"id":99,"name":"越界忽略"}],
                  "relations":[{"from":1,"to":2,"label":"同源"},
                               {"from":2,"to":2,"label":"自环丢"},
                               {"from":3,"to":1,"label":"越界丢"}]}"#;
    let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
    let valid: std::collections::HashSet<i64> = [1i64, 2].into_iter().collect();
    let mut croot = HashMap::new();
    croot.insert(1i64, 0i64);
    croot.insert(2i64, 0i64);
    let (renamed, edges, _merged) =
        apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 123).unwrap();
    assert_eq!(renamed, 1, "只有 id=1 在范围内且有名字");
    assert_eq!(edges, 1, "只有 1→2 合法(自环 / 越界被丢)");
    let label: String = conn
        .query_row("SELECT label FROM clusters WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(label, "我的报税");
    let summary: String = conn
        .query_row("SELECT summary FROM clusters WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(summary, "报税材料都在这");
    // id=2 未被命名 → 保留原名。
    let label2: String = conn
        .query_row("SELECT label FROM clusters WHERE id=2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(label2, "簇B");
    let (s, d, l): (i64, i64, String) = conn
        .query_row("SELECT src,dst,label FROM cluster_edges", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((s, d, l), (1, 2, "同源".to_string()));
}

#[test]
fn rename_apply_is_idempotent_rebuild() {
    // 二次 apply 应清掉旧关系边再重建,不累积。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
            size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
            summary TEXT NOT NULL DEFAULT '');
         CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
         INSERT INTO clusters(id,label) VALUES(1,'A'),(2,'B');",
    )
    .unwrap();
    let raw = r#"{"names":[],"relations":[{"from":1,"to":2,"label":"同源"}]}"#;
    let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
    let valid: std::collections::HashSet<i64> = [1i64, 2].into_iter().collect();
    let croot: HashMap<i64, i64> = [(1i64, 0i64), (2, 0)].into_iter().collect();
    apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 1).unwrap();
    apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 2).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM cluster_edges", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "重跑不累积关系边");
}

#[test]
fn merge_consolidates_same_parent_leaves_and_remaps() {
    // 按意思合并:同父叶簇 {1,2,3} 并成最大簇(1),文件改挂、余簇删除、size 重算;
    // 跨父组 [1,4] 被拒;指向被并簇的命名/关系自动重映射到 survivor。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
            size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
            summary TEXT NOT NULL DEFAULT '');
         CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE files(id INTEGER PRIMARY KEY, cluster_id INTEGER NOT NULL DEFAULT 0);
         INSERT INTO clusters(id,parent,size,label) VALUES
            (10,0,6,'大主题'),(1,10,3,'发票'),(2,10,2,'invoices'),(3,10,1,'报销单'),(4,99,5,'别的');
         INSERT INTO files(id,cluster_id) VALUES
            (101,1),(102,1),(103,1),(201,2),(202,2),(301,3);",
    )
    .unwrap();
    // names 指向被并簇 id=2 → 应改到 survivor 1;relation 2→4 → 应映射成 1→4。
    let raw = r#"{"names":[{"id":2,"name":"发票报销","summary":"发票和报销单都在这"},
                            {"id":4,"name":"其它东西"}],
                  "merges":[[1,2,3],[1,4]],
                  "relations":[{"from":2,"to":4,"label":"配套"}]}"#;
    let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
    let valid: std::collections::HashSet<i64> = [10i64, 1, 2, 3, 4].into_iter().collect();
    let croot: HashMap<i64, i64> = [(10i64, 0), (1, 0), (2, 0), (3, 0), (4, 0)]
        .into_iter()
        .collect();
    let (renamed, edges, merged) =
        apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 1).unwrap();
    assert_eq!(merged, 2, "1/2/3 同父并入 survivor=1,合并掉 2 个");
    // survivor=1(最大簇)留下,2/3 删除。
    let gone: i64 = conn
        .query_row("SELECT COUNT(*) FROM clusters WHERE id IN (2,3)", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(gone, 0, "被并簇 2/3 已删");
    // survivor 吸收全部 6 个文件,size 重算为 6。
    let nf: i64 = conn
        .query_row("SELECT COUNT(*) FROM files WHERE cluster_id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(nf, 6, "原 1/2/3 的文件全改挂 survivor=1");
    let sz: i64 = conn
        .query_row("SELECT size FROM clusters WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sz, 6, "survivor size 重算 = 实际文件数");
    // 跨父组 [1,4] 被拒 → 4 仍在。
    let kept: i64 = conn
        .query_row("SELECT COUNT(*) FROM clusters WHERE id=4", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 1, "跨父合并被拒,簇 4 保留");
    // 命名重映射:id=2 → survivor 1,故 1 被命名「发票报销」。
    let label1: String = conn
        .query_row("SELECT label FROM clusters WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(label1, "发票报销");
    assert_eq!(renamed, 2, "id=2(→1) 与 id=4 各命名一次");
    // 关系重映射:2→4 变 1→4。
    assert_eq!(edges, 1);
    let (s, d): (i64, i64) = conn
        .query_row("SELECT src,dst FROM cluster_edges", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((s, d), (1, 4), "关系端点跟随合并重映射");
}

#[test]
fn merge_skips_parent_clusters() {
    // 父簇(有子簇者)绝不可被并 —— 否则孤立其子簇。merges 里含父簇 id 的组应被安全跳过。
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
            size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
            summary TEXT NOT NULL DEFAULT '');
         CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE files(id INTEGER PRIMARY KEY, cluster_id INTEGER NOT NULL DEFAULT 0);
         INSERT INTO clusters(id,parent,size) VALUES (10,0,1),(20,0,1),(1,10,1),(2,20,1);",
    )
    .unwrap();
    // 两个顶层父簇 10、20 都各有子簇 → 都在 parent_set,合并 [10,20] 必须被拒。
    let raw = r#"{"merges":[[10,20]],"names":[],"relations":[]}"#;
    let parsed: LlmNameRel = serde_json::from_str(raw).unwrap();
    let valid: std::collections::HashSet<i64> = [10i64, 20, 1, 2].into_iter().collect();
    let croot: HashMap<i64, i64> = HashMap::new();
    let (_r, _e, merged) =
        apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 1).unwrap();
    assert_eq!(merged, 0, "父簇不参与合并");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM clusters", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4, "无簇被删");
}

#[test]
fn digest_directive_includes_ids_and_samples() {
    let d = ClusterDigest {
        id: 7,
        parent: 0,
        label: "财务".into(),
        keywords: "发票 报税".into(),
        size: 12,
        folders: vec!["财务/2023".into()],
        samples: vec!["增值税申报表".into()],
        kinds: vec![("doc".into(), 10)],
    };
    let s = digest_directive(&[d]);
    assert!(s.contains("id=7"));
    assert!(s.contains("增值税申报表"));
    assert!(s.contains("发票 报税"));
    assert!(s.contains("文档×10"));
}

// ───────────────────────── 聚类准确度评测台(真大模型介入) ─────────────────────────
//
// 目标:量化「几分钟路径」(T0 词法骨架 + T1 大模型命名)在**贴近真人杂乱硬盘**的语料上的
//   ① 覆盖率(是否真把全部文件都归了,不再 240/6000 截断)
//   ② 聚类纯度(同主题文件是否落进同一簇)
//   ③ 命名准确度(AI 簇名是否命中该簇主导主题 + 是否亲切中文)
//   ④ 关系边数量
// 隔离:用临时 db,绝不碰用户真实 ~/Polaris/data/fable.db。
// 触发:仅当置 POLARIS_CLUSTER_EVAL=1 才跑(普通 cargo test 跳过);真大模型走 run_claude_readonly
//   (置 EVAL_NO_LLM=1 则只测 T0,不调模型)。结果按 EVAL_OUT 追加一行 JSON。

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(d)
}
// 可复现 LCG(避免 rand 依赖,种子可控)。
fn lcg(s: &mut u64) -> u64 {
    *s = s
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *s >> 33
}

struct EvalTopic {
    id: usize,
    folder: &'static str,
    ext: &'static str,
    kind: &'static str,
    stems: &'static [&'static str],
    aliases: &'static [&'static str],
}
fn eval_topics() -> Vec<EvalTopic> {
    vec![
        EvalTopic {
            id: 0,
            folder: "财务/报税",
            ext: "xlsx",
            kind: "doc",
            stems: &[
                "增值税申报表",
                "个税专项扣除",
                "年度利润表",
                "记账凭证",
                "京东发票",
                "报销单",
                "工资表",
                "对账单",
            ],
            aliases: &[
                "报税", "财务", "税", "发票", "报销", "账", "工资", "对账", "凭证", "利润", "扣除",
                "报表", "单据",
            ],
        },
        EvalTopic {
            id: 1,
            folder: "装修/新房",
            ext: "jpg",
            kind: "image",
            stems: &[
                "客厅效果图",
                "水电改造预算",
                "家具清单",
                "施工合同",
                "瓷砖选样",
                "全屋定制报价",
                "卫生间布局",
            ],
            aliases: &[
                "装修",
                "房",
                "家具",
                "施工",
                "效果图",
                "户型",
                "布局",
                "选材",
                "报价",
                "预算",
            ],
        },
        EvalTopic {
            id: 2,
            folder: "考研/复习",
            ext: "pdf",
            kind: "doc",
            stems: &[
                "数学强化讲义",
                "英语真题2022",
                "政治大纲笔记",
                "专业课总结",
                "错题本",
                "肖四肖八",
                "高数公式",
            ],
            aliases: &[
                "考研", "复习", "真题", "笔记", "讲义", "学习", "数学", "英语", "政治", "错题",
                "公式",
            ],
        },
        EvalTopic {
            id: 3,
            folder: "照片/宝宝",
            ext: "jpg",
            kind: "image",
            stems: &[
                "周岁照",
                "幼儿园运动会",
                "全家福",
                "第一次走路",
                "生日蛋糕",
                "公园游玩",
            ],
            aliases: &["照片", "宝宝", "孩子", "娃", "家庭", "全家福"],
        },
        EvalTopic {
            id: 4,
            folder: "工作/汇报",
            ext: "pptx",
            kind: "doc",
            stems: &[
                "季度汇报",
                "周报",
                "项目方案v3",
                "OKR复盘",
                "需求评审纪要",
                "述职报告",
            ],
            aliases: &[
                "工作", "汇报", "项目", "报告", "周报", "方案", "复盘", "述职", "纪要", "评审",
            ],
        },
        EvalTopic {
            id: 5,
            folder: "副业/接单",
            ext: "psd",
            kind: "image",
            stems: &[
                "logo设计稿",
                "客户需求",
                "报价单",
                "海报终稿",
                "名片排版",
                "公众号配图",
            ],
            aliases: &[
                "副业", "接单", "客户", "设计", "海报", "logo", "名片", "排版", "配图",
            ],
        },
        EvalTopic {
            id: 6,
            folder: "旅行/日本",
            ext: "pdf",
            kind: "doc",
            stems: &[
                "行程单",
                "机票确认",
                "东京攻略",
                "酒店预订",
                "签证材料",
                "美食清单",
            ],
            aliases: &[
                "旅行", "旅游", "行程", "攻略", "机票", "日本", "酒店", "住宿", "签证", "美食",
                "东京",
            ],
        },
        EvalTopic {
            id: 7,
            folder: "code/polaris",
            ext: "rs",
            kind: "text",
            stems: &[
                "main",
                "lib",
                "server",
                "README",
                "cluster_build",
                "retrieve",
            ],
            aliases: &["代码", "项目", "开发", "程序", "code", "源码"],
        },
        EvalTopic {
            id: 8,
            folder: "movies",
            ext: "mkv",
            kind: "video",
            stems: &["复仇者联盟", "星际穿越", "盗梦空间", "教父", "肖申克的救赎"],
            aliases: &[
                "电影", "影视", "视频", "剧", "movie", "大片", "片", "科幻", "经典", "动作",
            ],
        },
        EvalTopic {
            id: 9,
            folder: "合同",
            ext: "docx",
            kind: "doc",
            stems: &["租房合同", "劳动合同", "保密协议", "采购合同", "服务协议"],
            aliases: &["合同", "协议", "法律", "租房", "劳动"],
        },
    ]
}

struct EvalFile {
    relpath: String,
    name: String,
    ext: String,
    kind: String,
    size: i64,
    mtime: i64,
    topic: usize,
}

// 按场景生成贴近真人硬盘的语料 + 真值主题标签。
//  organized = 按主题文件夹整齐摆放(文件夹信号强);flat = 全堆根目录(只靠文件名);
//  messy = 混合 + ~15% 乱名噪声(IMG_/微信图片/副本);multiling = 含英文命名主题。
fn gen_corpus(scenario: &str, seed: u64, size: usize) -> (Vec<EvalFile>, Vec<EvalTopic>) {
    let topics = eval_topics();
    let mut rng = seed.wrapping_add(0x9e3779b9);
    let mut out: Vec<EvalFile> = Vec::with_capacity(size);
    let noise = scenario == "messy";
    let flat = scenario == "flat";
    for i in 0..size {
        let t = &topics[(lcg(&mut rng) as usize) % topics.len()];
        let stem = t.stems[(lcg(&mut rng) as usize) % t.stems.len()];
        let variant = lcg(&mut rng) % 9000 + 1000; // 后缀,造出大量不同文件名
        let garbled = noise && (lcg(&mut rng) % 100) < 15;
        let name = if garbled {
            // 乱名(无主题信号)→ 仍标真值主题,考验「靠文件夹/同簇邻居兜底」
            let kinds = ["IMG_", "微信图片_", "DSC", "副本_未命名"];
            format!(
                "{}{}.{}",
                kinds[(lcg(&mut rng) as usize) % kinds.len()],
                variant,
                t.ext
            )
        } else {
            format!("{stem}_{variant}.{}", t.ext)
        };
        let folder_flat = flat || (noise && (lcg(&mut rng) % 100) < 30);
        let relpath = if folder_flat {
            name.clone()
        } else {
            format!("{}/{}", t.folder, name)
        };
        out.push(EvalFile {
            relpath,
            name: name.clone(),
            ext: t.ext.to_string(),
            kind: t.kind.to_string(),
            size: 1024 + (variant as i64) * 7,
            mtime: (size - i) as i64, // 越靠前 mtime 越大(新)
            topic: t.id,
        });
    }
    (out, topics)
}

fn eval_schema(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "CREATE TABLE roots(id INTEGER PRIMARY KEY, path TEXT);
         CREATE TABLE files(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 1,
            relpath TEXT NOT NULL, name TEXT NOT NULL, ext TEXT NOT NULL DEFAULT '',
            kind TEXT NOT NULL DEFAULT 'other', size INTEGER NOT NULL DEFAULT 0,
            mtime INTEGER NOT NULL DEFAULT 0, cluster_id INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE clusters(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            label TEXT NOT NULL DEFAULT '', color TEXT NOT NULL DEFAULT '', keywords TEXT NOT NULL DEFAULT '',
            size INTEGER NOT NULL DEFAULT 0, built_at INTEGER NOT NULL DEFAULT 0, parent INTEGER NOT NULL DEFAULT 0,
            summary TEXT NOT NULL DEFAULT '');
         CREATE TABLE cluster_edges(id INTEGER PRIMARY KEY, root_id INTEGER NOT NULL DEFAULT 0,
            src INTEGER NOT NULL, dst INTEGER NOT NULL, label TEXT NOT NULL DEFAULT '', built_at INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE titles(file_id INTEGER PRIMARY KEY, title TEXT NOT NULL DEFAULT '', source TEXT NOT NULL DEFAULT '', made_at INTEGER NOT NULL DEFAULT 0);",
    )
    .unwrap();
}

#[test]
fn cluster_eval_run() {
    if std::env::var("POLARIS_CLUSTER_EVAL").is_err() {
        return; // 普通 cargo test 跳过
    }
    let seed = env_u64("EVAL_SEED", 1);
    let size = env_u64("EVAL_SIZE", 800) as usize;
    let scenario = std::env::var("EVAL_SCENARIO").unwrap_or_else(|_| "organized".into());
    let use_llm = std::env::var("EVAL_NO_LLM").is_err();

    let (corpus, topics) = gen_corpus(&scenario, seed, size);
    let dbp = std::env::temp_dir().join(format!("polaris_eval_{seed}_{size}_{scenario}.db"));
    let _ = std::fs::remove_file(&dbp);
    let conn = rusqlite::Connection::open(&dbp).unwrap();
    eval_schema(&conn);
    conn.execute("INSERT INTO roots(id,path) VALUES(1,'/eval')", [])
        .unwrap();
    {
        let mut ins = conn
            .prepare("INSERT INTO files(id,root_id,relpath,name,ext,kind,size,mtime,cluster_id) VALUES(?1,1,?2,?3,?4,?5,?6,?7,0)")
            .unwrap();
        for (i, f) in corpus.iter().enumerate() {
            ins.execute(rusqlite::params![
                (i + 1) as i64,
                f.relpath,
                f.name,
                f.ext,
                f.kind,
                f.size,
                f.mtime
            ])
            .unwrap();
        }
    }
    let topic_of: HashMap<i64, usize> = corpus
        .iter()
        .enumerate()
        .map(|(i, f)| ((i + 1) as i64, f.topic))
        .collect();

    // ── T0:词法骨架(真生产函数)──
    let t0 = std::time::Instant::now();
    let summ = cluster_build_on(&conn, &[], ClusterMode::Lexical, std::time::Instant::now())
        .expect("cluster_build_on");
    let t0_ms = t0.elapsed().as_millis();

    // 读回归簇
    let assign: HashMap<i64, i64> = {
        let mut stmt = conn.prepare("SELECT id, cluster_id FROM files").unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .unwrap();
        rows.flatten().collect()
    };
    let total = corpus.len();
    let covered = assign.values().filter(|&&c| c > 0).count();
    let coverage = covered as f64 / total as f64;

    // 叶簇(有文件挂着的簇)→ 各簇成员 + 主导主题。
    let mut members: HashMap<i64, Vec<i64>> = HashMap::new();
    for (&fid, &cid) in &assign {
        if cid > 0 {
            members.entry(cid).or_default().push(fid);
        }
    }
    // 纯度:每簇主导主题占比之和 / 已归类文件数。
    let mut pure_hits = 0usize;
    let mut cluster_dom: HashMap<i64, usize> = HashMap::new();
    for (&cid, mem) in &members {
        let mut tf: HashMap<usize, usize> = HashMap::new();
        for &fid in mem {
            *tf.entry(topic_of[&fid]).or_insert(0) += 1;
        }
        let (dom, cnt) = tf.into_iter().max_by_key(|&(_, c)| c).unwrap();
        pure_hits += cnt;
        cluster_dom.insert(cid, dom);
    }
    let purity = if covered > 0 {
        pure_hits as f64 / covered as f64
    } else {
        0.0
    };
    let leaf_n = members.len();

    // ── T1:真大模型命名(读簇画像)──
    let mut name_acc = -1.0f64; // -1 = 未跑 LLM
    let mut name_acc_w = -1.0f64;
    let mut named_leaf = 0usize;
    let mut edges_n = 0usize;
    let mut samples: Vec<(String, String, bool)> = Vec::new(); // (主导主题文件夹, AI名, 命中)
    let mut llm_err = String::new();
    if use_llm {
        let digests = collect_cluster_digests(&conn, &[]).unwrap();
        let prompt = digest_directive(&digests);
        let cwd = std::env::temp_dir();
        match crate::kb::run_claude_readonly(&cwd, &prompt, |_, _| {}) {
            Ok(text) => match crate::kb::extract_balanced_json(&text) {
                Some(raw) => match serde_json::from_str::<LlmNameRel>(&raw) {
                    Ok(parsed) => {
                        let valid: std::collections::HashSet<i64> =
                            digests.iter().map(|d| d.id).collect();
                        let croot: HashMap<i64, i64> =
                            digests.iter().map(|d| (d.id, 1i64)).collect();
                        let (_r, e, _m) =
                            apply_names_and_relations(&conn, &[], &parsed, &valid, &croot, 0)
                                .unwrap();
                        edges_n = e;
                        // 读回每个叶簇的 AI 名,核对是否命中其主导主题别名。
                        let labels: HashMap<i64, String> = {
                            let mut stmt = conn.prepare("SELECT id, label FROM clusters").unwrap();
                            let rows = stmt
                                .query_map([], |r| {
                                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                                })
                                .unwrap();
                            rows.flatten().collect()
                        };
                        let mut hit = 0usize;
                        let mut hit_w = 0usize;
                        for (&cid, mem) in &members {
                            let dom = cluster_dom[&cid];
                            let label = labels.get(&cid).cloned().unwrap_or_default();
                            if !label.trim().is_empty() {
                                named_leaf += 1;
                            }
                            let ok = topics[dom].aliases.iter().any(|a| label.contains(a));
                            if ok {
                                hit += 1;
                                hit_w += mem.len();
                            }
                            if samples.len() < 40 {
                                samples.push((topics[dom].folder.to_string(), label, ok));
                            }
                        }
                        name_acc = if leaf_n > 0 {
                            hit as f64 / leaf_n as f64
                        } else {
                            0.0
                        };
                        name_acc_w = if covered > 0 {
                            hit_w as f64 / covered as f64
                        } else {
                            0.0
                        };
                    }
                    Err(e) => llm_err = format!("json parse: {e}"),
                },
                None => llm_err = "no json in model output".into(),
            },
            Err(e) => llm_err = format!("llm call: {e}"),
        }
    }

    let result = json!({
        "scenario": scenario, "seed": seed, "size": total,
        "t0_ms": t0_ms, "clusters": summ.clusters, "leaf_clusters": leaf_n,
        "coverage": (coverage * 1000.0).round() / 1000.0,
        "purity": (purity * 1000.0).round() / 1000.0,
        "name_acc": (name_acc * 1000.0).round() / 1000.0,
        "name_acc_weighted": (name_acc_w * 1000.0).round() / 1000.0,
        "named_leaf": named_leaf, "edges": edges_n,
        "llm_err": llm_err, "samples": samples.iter().map(|(f,n,ok)| json!({"topic_folder":f,"ai_name":n,"hit":ok})).collect::<Vec<_>>(),
    });
    let line = serde_json::to_string(&result).unwrap();
    println!("EVAL_RESULT {line}");
    if let Ok(out) = std::env::var("EVAL_OUT") {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&out)
        {
            let _ = writeln!(f, "{line}");
        }
    }
    let _ = std::fs::remove_file(&dbp);
    let _ = std::fs::remove_file(dbp.with_extension("db-wal"));
    let _ = std::fs::remove_file(dbp.with_extension("db-shm"));
}

#[test]
fn maximal_roots_drops_nested() {
    // 还原线上那台机的真实根集合:D:\ 与 C:\ 之下各挂了一堆子根。
    let all = vec![
        (1, r"D:\polaris\polaris-app\src".to_string()),
        (2, r"D:\polaris\专家团队".to_string()),
        (3, r"C:\".to_string()),
        (4, r"D:\polaris\polaris-app\src-tauri".to_string()),
        (5, r"C:\Windows\System32".to_string()),
        (6, r"D:\".to_string()),
        (8, r"D:\polaris\polaris-app".to_string()),
    ];
    let mut keep = maximal_root_ids(&all);
    keep.sort();
    // 只剩两个极大根:C:\(3) 与 D:\(6);其余全是它们的子根。
    assert_eq!(keep, vec![3, 6]);
}

#[test]
fn maximal_roots_keeps_siblings_and_prefix_lookalikes() {
    // 同级、以及「前缀像但不是子目录」的根都要保留(D:\foo 不是 D:\foobar 的祖先)。
    let all = vec![
        (1, r"D:\foo".to_string()),
        (2, r"D:\foobar".to_string()),
        (3, r"E:\data".to_string()),
        (4, r"D:\foo\child".to_string()),
    ];
    let mut keep = maximal_root_ids(&all);
    keep.sort();
    assert_eq!(keep, vec![1, 2, 3]); // 仅 D:\foo\child(4) 被剔除
}

#[test]
fn tokenize_splits_cjk_and_ascii() {
    let t = tokenize("全澳房产_dataset_v2.csv");
    assert!(t.iter().any(|x| x == "全澳房产"));
    assert!(t.iter().any(|x| x == "dataset"));
    // 纯数字 / 版本号被过滤
    assert!(!t.iter().any(|x| x == "2"));
}

#[test]
fn extract_gist_prefers_title_and_first_para() {
    let g = extract_gist("# 标题行\n\n这是第一段正文内容。");
    assert!(g.contains("标题行"));
    assert!(g.contains("第一段"));
}

#[test]
fn human_size_scales() {
    assert_eq!(human_size(512), "512 B");
    assert_eq!(human_size(2048), "2.0 KB");
}

#[test]
fn source_tag_recognizes_download_dirs() {
    assert_eq!(source_tag(r"C:\Users\me\Downloads", "a.pdf"), "下载");
    assert_eq!(
        source_tag(r"C:\Users\me\Documents\WeChat Files", "x/y.jpg"),
        "微信"
    );
    assert_eq!(
        source_tag(r"C:\Users\me\Documents\xwechat_files", "f.docx"),
        "微信"
    );
    assert_eq!(
        source_tag(r"C:\Users\me\Documents\WXWork", "f.zip"),
        "企业微信"
    );
    // Tencent Files:按根末段,或 relpath 里的 FileRecv 命中
    assert_eq!(
        source_tag(r"C:\Users\me\Documents\Tencent Files", "123/FileRecv/a.7z"),
        "QQ"
    );
    assert_eq!(source_tag("/data/nas/share", "2024/FileRecv/b.rar"), "QQ");
    // 普通目录 → 空(不显示徽标)
    assert_eq!(source_tag(r"D:\datasets", "housing/a.csv"), "");
}

#[test]
fn clean_title_strips_noise_keeps_meaning() {
    // 时间戳/计数器/分隔符清掉,保留有意义的词
    assert_eq!(
        clean_title("全澳房产_dataset_v2 (3).csv"),
        "全澳房产 dataset v2"
    );
    // 纯噪声图片名:无可保留 → 退回去扩展名的原名(总比空好)
    assert_eq!(
        clean_title("IMG_20230101_123456.jpg"),
        "IMG_20230101_123456"
    );
    // 长 hex 哈希被丢
    assert_eq!(clean_title("a1b2c3d4e5f6 报告.pdf"), "报告");
    // 正常中文名原样
    assert_eq!(clean_title("会议纪要.docx"), "会议纪要");
}
