//! 专家头像 — 9 张卡通形象（编译期 `include_bytes!` 内嵌，打包后仍可用）。
//!
//! 旧实现按中文名查 50 个 PNG 文件、运行时 `CARGO_MANIFEST_DIR` 路径读盘 ——
//! 那条路径是「构建机」的绝对路径，装到用户机上根本不存在，所以头像只在 dev 能显示。
//! 现改为把 9 张卡通头像 `include_bytes!` 进二进制，按专家 id 稳定散列分配，
//! 任何环境（dev / 打包 desktop / server flavor）都能直接出图。

use base64::Engine;
use once_cell::sync::Lazy;

/// 9 张卡通头像（来源：专家名字.png 3×3 切片），编译期内嵌。
const AVATARS: [&[u8]; 9] = [
    include_bytes!("../templates/experts/avatars/avatar_0.png"),
    include_bytes!("../templates/experts/avatars/avatar_1.png"),
    include_bytes!("../templates/experts/avatars/avatar_2.png"),
    include_bytes!("../templates/experts/avatars/avatar_3.png"),
    include_bytes!("../templates/experts/avatars/avatar_4.png"),
    include_bytes!("../templates/experts/avatars/avatar_5.png"),
    include_bytes!("../templates/experts/avatars/avatar_6.png"),
    include_bytes!("../templates/experts/avatars/avatar_7.png"),
    include_bytes!("../templates/experts/avatars/avatar_8.png"),
];

/// 稳定散列：把任意 id 映射到 0..9 的头像槽位（与运行环境无关，可复现）。
fn slot_for(id: &str) -> usize {
    // FNV-1a 简化版，纯确定性。
    let mut h: u32 = 2166136261;
    for b in id.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h as usize) % AVATARS.len()
}

/// 9 张头像的 base64 Data URL —— 编码一次，全程复用。
/// base64 编码 ~700KB 字节并不便宜，旧实现每次调用都重编码，
/// 在「按知识库反推专家团」「专家市场」等路径上重复烧 CPU；改为 Lazy 缓存。
static SLOTS_B64: Lazy<Vec<String>> = Lazy::new(|| {
    AVATARS
        .iter()
        .map(|b| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(b)
            )
        })
        .collect()
});

/// 返回某 id 对应头像的 base64 Data URL（供前端 `<img src>` 直接用）。
pub fn avatar_data_url(id: &str) -> String {
    SLOTS_B64[slot_for(id)].clone()
}

/// 一次性返回全部 9 张头像的 Data URL（按槽位 0..9）。
/// 前端拉一次即可，配合同样的 FNV-1a 槽位散列把 id 映射到其中一张 ——
/// 避免「100+ 张卡片各发一次 IPC 取头像」造成的卡顿与重复传输。
pub fn avatar_slots() -> Vec<String> {
    SLOTS_B64.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_deterministic_and_in_range() {
        for id in ["chief-strategist", "python-pro", "team-creative-content", ""] {
            let s = slot_for(id);
            assert!(s < 9, "slot 越界: {s}");
            assert_eq!(s, slot_for(id), "同 id 应稳定散列到同槽位");
        }
    }

    #[test]
    fn avatars_non_empty() {
        for (i, a) in AVATARS.iter().enumerate() {
            assert!(a.len() > 100, "avatar_{i} 内嵌字节异常");
        }
        assert!(avatar_data_url("python-pro").starts_with("data:image/png;base64,"));
    }
}
