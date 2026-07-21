//! Polaris Forge · Python 出片桥的 Rust 侧驱动(路线 B「无限版式」上层梯队)。
//!
//! 契约:同一份 spec JSON。spec 顶层带 `"engine":"python"`(强制)或 `"auto"`(优先 Python、
//! 失败回退原生)时,`forge::spec_to_pptx_sync` 调这里。本模块只负责**找 python + 找 bridge
//! 脚本 + 跑 + 解析结果**;真正的排版在 py/pptx_bridge.py,用户可自由扩展版式(见其文件头)。
//!
//! 找不到 python 或 python-pptx 时:`build_via_python` 返 Err,`auto` 会被上层回退到原生引擎,
//! `python` 则如实报错——绝不静默出一份缺版式的片让用户以为成功了。

use serde_json::Value;
use std::process::Command;

/// 依次探测可用的 python 解释器(可用 `POLARIS_PYTHON` 覆盖)。返回其路径/名。
fn find_python() -> Option<String> {
    if let Ok(p) = std::env::var("POLARIS_PYTHON") {
        if !p.trim().is_empty() {
            return Some(p);
        }
    }
    for cand in ["python3", "python"] {
        let ok = Command::new(cand)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

/// 定位 pptx_bridge.py:`POLARIS_PPTX_BRIDGE` 覆盖 → exe 同级/bin/vendor/Resources(打包用)
/// → 编译期 crate 内 py/(开发+测试)。命中第一个存在的。
fn find_bridge() -> Option<String> {
    if let Ok(p) = std::env::var("POLARIS_PPTX_BRIDGE") {
        if std::path::Path::new(&p).is_file() {
            return Some(p);
        }
    }
    let mut cands: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for sub in ["", "bin", "vendor", "py", "Resources", "Resources/py"] {
                let base = if sub.is_empty() { dir.to_path_buf() } else { dir.join(sub) };
                cands.push(base.join("pptx_bridge.py"));
            }
            if let Some(contents) = dir.parent() {
                cands.push(contents.join("Resources").join("pptx_bridge.py"));
                cands.push(contents.join("Resources").join("py").join("pptx_bridge.py"));
            }
        }
    }
    // 开发/测试:crate 源码内的 py/pptx_bridge.py(编译期已知路径)。
    cands.push(std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/py/pptx_bridge.py"
    )));
    cands.into_iter().find(|p| p.is_file()).map(|p| p.to_string_lossy().to_string())
}

/// python + python-pptx 是否就绪(preflight 用;不产文件)。
pub fn available() -> bool {
    let py = match find_python() {
        Some(p) => p,
        None => return false,
    };
    if find_bridge().is_none() {
        return false;
    }
    Command::new(py)
        .args(["-c", "import pptx"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// spec JSON → .pptx,经 py/pptx_bridge.py。成功返回 bridge 打印的结果 JSON(含 slides/images/
/// warnings),并补 `"engine":"python"`。任一环节缺失/失败一律 Err,交上层决定回退还是报错。
pub fn build_via_python(spec_json: &str, out_path: &str) -> Result<Value, String> {
    let py = find_python().ok_or("未找到 python 解释器(装 python3 或设 POLARIS_PYTHON)")?;
    let bridge = find_bridge().ok_or("未找到 pptx_bridge.py(设 POLARIS_PPTX_BRIDGE 指向它)")?;

    // spec 落临时文件传给子进程(避开超长 argv / 编码坑)。pid 命名避免并发互踩。
    let tmp = std::env::temp_dir().join(format!("polaris_spec_{}.json", std::process::id()));
    std::fs::write(&tmp, spec_json).map_err(|e| format!("写临时 spec 失败: {e}"))?;
    struct TmpClean(std::path::PathBuf);
    impl Drop for TmpClean {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _clean = TmpClean(tmp.clone());

    // 目标目录先建好(与原生引擎一致,子进程只管写)。
    if let Some(parent) = std::path::Path::new(out_path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    let output = Command::new(&py)
        .arg(&bridge)
        .arg(&tmp)
        .arg(out_path)
        .output()
        .map_err(|e| format!("启动 python 桥失败: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "python 桥退出码 {}: {}",
            output.status.code().unwrap_or(-1),
            err.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // bridge 约定最后一行是结果 JSON;宽容取最后一个非空行。
    let last = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| format!("python 桥无 JSON 输出: {}", stdout.trim()))?;
    let mut v: Value =
        serde_json::from_str(last).map_err(|e| format!("解析 python 桥输出失败: {e}: {last}"))?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("engine".into(), Value::String("python".into()));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 本机有 python-pptx 才跑真桥;没有则只验证「优雅报错、不 panic」。
    #[test]
    fn python_bridge_builds_or_reports_cleanly() {
        let dir = std::env::temp_dir().join("polaris_py_bridge_test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("py.pptx");
        let spec = r#"{"engine":"python","theme":"ink-gold","slides":[
            {"layout":"title","title":"桥测试","subtitle":"same spec","notes":"n"},
            {"layout":"freeform","boxes":[
                {"type":"rect","x":0,"y":0,"w":1280,"h":10,"color":"accent"},
                {"type":"text","x":100,"y":200,"w":900,"h":120,"text":"自由版式","size":36,"color":"ink","bold":true}
            ]}
        ]}"#;
        let r = build_via_python(spec, &out.to_string_lossy());
        if available() {
            let v = r.expect("python-pptx 可用时应成功");
            assert_eq!(v["slides"], 2);
            assert_eq!(v["engine"], "python");
            assert!(out.is_file(), "应产出 pptx 文件");
            // 真是个 zip(pptx)包。
            let f = std::fs::File::open(&out).unwrap();
            assert!(zip::ZipArchive::new(f).is_ok(), "产物应是合法 zip/pptx");
        } else {
            assert!(r.is_err(), "缺 python-pptx 时应 Err 而非 panic 或假成功");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
