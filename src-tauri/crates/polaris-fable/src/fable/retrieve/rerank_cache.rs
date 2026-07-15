//! 重排结果缓存(P2-1 ③):查询 + 候选签名 → 重排序结果,LRU 上限 128。

use super::*;

/// 缓存签名 = 查询 + 候选集(path#location 有序拼接)。候选集变了就重算,故签名编入全部候选键。
pub(crate) fn rerank_sig(query: &str, cands: &[(&String, &String)]) -> String {
    let mut s = String::with_capacity(query.len() + cands.len() * 24);
    s.push_str(query);
    for (p, loc) in cands {
        s.push('\u{0}');
        s.push_str(p);
        s.push('#');
        s.push_str(loc);
    }
    s
}

struct RerankCache {
    cap: usize,
    map: HashMap<String, Vec<(usize, f32)>>,
    order: VecDeque<String>,
}
static RERANK_CACHE: Lazy<Mutex<RerankCache>> = Lazy::new(|| {
    Mutex::new(RerankCache {
        cap: 128,
        map: HashMap::new(),
        order: VecDeque::new(),
    })
});

pub(crate) fn rerank_cache_get(sig: &str) -> Option<Vec<(usize, f32)>> {
    let mut c = RERANK_CACHE.lock().unwrap();
    let v = c.map.get(sig)?.clone();
    c.order.retain(|x| x != sig);
    c.order.push_back(sig.to_string());
    Some(v)
}

pub(crate) fn rerank_cache_put(sig: String, val: Vec<(usize, f32)>) {
    let mut c = RERANK_CACHE.lock().unwrap();
    if c.map.insert(sig.clone(), val).is_none() {
        c.order.push_back(sig);
        while c.order.len() > c.cap {
            if let Some(old) = c.order.pop_front() {
                c.map.remove(&old);
            }
        }
    } else {
        c.order.retain(|x| x != &sig);
        c.order.push_back(sig);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_sig_changes_with_candidates() {
        let p1 = "a/b.md".to_string();
        let l1 = "L3".to_string();
        let l2 = "C5".to_string();
        let s1 = rerank_sig("q", &[(&p1, &l1)]);
        let s2 = rerank_sig("q", &[(&p1, &l2)]); // 候选位置变了 → 签名必须变
        let s3 = rerank_sig("q2", &[(&p1, &l1)]); // 查询变了 → 签名必须变
        assert_ne!(s1, s2);
        assert_ne!(s1, s3);
        assert_eq!(s1, rerank_sig("q", &[(&p1, &l1)])); // 同输入同签名(确定性)
    }

    #[test]
    fn rerank_cache_roundtrip() {
        let sig = "unit-test-sig-xyz".to_string();
        assert!(rerank_cache_get(&sig).is_none());
        rerank_cache_put(sig.clone(), vec![(2, 0.9), (0, 0.5)]);
        assert_eq!(rerank_cache_get(&sig), Some(vec![(2, 0.9), (0, 0.5)]));
    }
}
