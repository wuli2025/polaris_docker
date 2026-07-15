//! 外壳拼装点 (架构 Phase 0 · 分仓规划 v2): 把各引擎的具体实现注入内核桥
//! (`chat::bridges`)。桌面 `run()` setup 与 server `serve()` 都在 init 序列里调用
//! `wire_engine_bridges()` —— 这是全 crate 里唯一同时认识「内核桥 trait」与
//! 「引擎具体实现」的地方; 抽仓后本文件留在组装壳(polaris-app), 引擎换实现只改这里。

use std::sync::Arc;

use crate::chat::bridges::{
    self, ExpertBridge, FableStatusLite, KbBridge, KbHitLite, KbOverviewLite, RagHitLite,
};

/// 检索引擎桥的官方实现: 直连 kb(妈妈库/关键词) + fable(盘点/混检)。
struct ShellKbBridge;

impl KbBridge for ShellKbBridge {
    fn root(&self) -> String {
        crate::kb::kb_root()
    }

    fn overview(&self) -> KbOverviewLite {
        let ov = crate::kb::kb_overview();
        KbOverviewLite {
            root: ov.root,
            wiki: ov.wiki,
            raw_md: ov.raw_md,
            output: ov.output,
            memory: ov.memory,
        }
    }

    fn context_block_scoped(&self, scope: Option<&str>) -> String {
        crate::kb::kb_context_block_scoped(scope)
    }

    fn fable_context_block(&self, full: bool) -> String {
        crate::fable::agent::fable_context_block(full)
    }

    fn search_sync(&self, query: String, top_k: Option<usize>) -> Vec<KbHitLite> {
        crate::kb::kb_search_sync(query, top_k)
            .into_iter()
            .map(|h| KbHitLite {
                title: h.title,
                path: h.path,
                snippet: h.snippet,
            })
            .collect()
    }

    fn fable_status(&self) -> Option<FableStatusLite> {
        crate::fable::status().ok().map(|s| FableStatusLite {
            files_total: s.files_total,
            chunks_total: s.chunks_total,
            lex_files: s.lex_files,
        })
    }

    fn rag_search(
        &self,
        query: &str,
        top_k: usize,
        mode: &str,
        scope: Option<&str>,
    ) -> Option<Vec<RagHitLite>> {
        crate::fable::retrieve::search(query, top_k, mode, scope)
            .ok()
            .map(|r| {
                r.hits
                    .into_iter()
                    .map(|h| RagHitLite {
                        path: h.path,
                        snippet: h.snippet,
                    })
                    .collect()
            })
    }
}

/// 专家团桥的官方实现: 直连 expert 花名册/路由/召集。
struct ShellExpertBridge;

impl ExpertBridge for ShellExpertBridge {
    fn detect_multi_expert_task(&self, prompt: &str) -> bool {
        crate::expert::detect_multi_expert_task(prompt)
    }

    fn team_block_spawn(&self, project_id: String, prompt: String) -> Option<String> {
        let matches = crate::expert::expert_team_spawn(project_id, prompt);
        crate::expert::team_block(&matches)
    }

    fn route_block(&self, prompt: &str) -> Option<String> {
        crate::expert::route_block(prompt)
    }
}

/// 把官方引擎实现注入内核桥。幂等(重复调用被 OnceLock 忽略), 开销一次性。
pub fn wire_engine_bridges() {
    bridges::set_kb_bridge(Arc::new(ShellKbBridge));
    bridges::set_expert_bridge(Arc::new(ShellExpertBridge));
}
