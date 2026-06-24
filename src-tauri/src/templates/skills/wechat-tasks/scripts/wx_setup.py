# -*- coding: utf-8 -*-
"""
微信聊天 · 一次性授权（抓取并缓存 master key）
─────────────────────────────────────────────────────────────
微信 4.0+ 只在「打开数据库设密钥那一刻」（=登录时）把 master key 交给 WCDB，
所以必须：先注入 hook，再触发一次登录，才抓得到。本脚本：
  1. 关掉微信 → 重开到登录页 → 注入 wx_key hook；
  2. 等你在微信窗口点「进入/登录」（手机确认）→ hook 抓到 master key；
  3. 校验 key 能解开 message_0.db → 写进 wx_config.json 缓存。

之后每天的 wx_daily.py 直接复用这个缓存 key，不用再重启微信
（master key 账号级稳定，除非你退出登录重登才需要再跑一次本脚本）。

配置：编辑同目录 wx_config.json，填好 weixin_exe / db_dir / wx_key_dir / venv_python / tools_dir。
"""
import os, sys, json, time, ctypes, hashlib, hmac, struct, subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = 4096


def log(*a):
    print(*a); sys.stdout.flush()


def load_config():
    p = os.path.join(HERE, "wx_config.json")
    if not os.path.exists(p):
        log("[!] 缺少 wx_config.json，请先按 wx_config.example.json 创建。")
        sys.exit(2)
    return json.load(open(p, encoding="utf-8"))


def save_config(cfg):
    p = os.path.join(HERE, "wx_config.json")
    tmp = p + ".tmp"
    json.dump(cfg, open(tmp, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    os.replace(tmp, p)


def verify_enc(enc, page1):
    salt = page1[:16]
    mac_salt = bytes(b ^ 0x3A for b in salt)
    mac_key = hashlib.pbkdf2_hmac("sha512", enc, mac_salt, 2, dklen=32)
    data = page1[16: PAGE - 80 + 16]
    stored = page1[PAGE - 64: PAGE]
    hm = hmac.new(mac_key, data, hashlib.sha512); hm.update(struct.pack("<I", 1))
    return hm.digest() == stored


def is_master(hexkey, page1):
    if len(hexkey) != 64:
        return False
    k = bytes.fromhex(hexkey); salt = page1[:16]
    enc = hashlib.pbkdf2_hmac("sha512", k, salt, 256000, dklen=32)
    return verify_enc(enc, page1)


def list_weixin():
    r = subprocess.run(["tasklist", "/FI", "IMAGENAME eq Weixin.exe", "/FO", "CSV", "/NH"],
                       capture_output=True, text=True)
    out = []
    for line in r.stdout.strip().split("\n"):
        if not line.strip():
            continue
        p = line.strip('"').split('","')
        if len(p) >= 5:
            out.append((int(p[1]), int(p[4].replace(",", "").replace(" K", "").strip() or "0")))
    return out


def main():
    cfg = load_config()
    dll_path = os.path.join(cfg["wx_key_dir"], "data", "flutter_assets", "assets", "dll", "wx_key.dll")
    weixin = cfg["weixin_exe"]
    page1 = open(os.path.join(cfg["db_dir"], "message", "message_0.db"), "rb").read(PAGE)

    log("[1] 关闭微信 …")
    subprocess.run(["taskkill", "/F", "/IM", "Weixin.exe"], capture_output=True)
    time.sleep(3)
    log("[2] 重新启动微信（停在登录页）…")
    subprocess.Popen([weixin])

    dll = ctypes.CDLL(dll_path)
    dll.InitializeHook.argtypes = [ctypes.c_uint]; dll.InitializeHook.restype = ctypes.c_bool
    dll.PollKeyData.argtypes = [ctypes.c_char_p, ctypes.c_int]; dll.PollKeyData.restype = ctypes.c_bool
    dll.CleanupHook.restype = ctypes.c_bool

    log("[3] 等登录页主进程起来并注入 hook …")
    injected = False; t0 = time.time()
    while time.time() - t0 < 40 and not injected:
        for pid, mem in sorted(list_weixin(), key=lambda x: x[1], reverse=True)[:3]:
            if mem < 60 * 1024:
                continue
            if dll.InitializeHook(pid):
                log(f"[+] hook 注入成功 PID={pid}"); injected = True; break
        if not injected:
            time.sleep(2)
    if not injected:
        log("[!] 未能注入 hook，退出"); sys.exit(2)

    log("\n" + "=" * 50)
    log(">>> 现在请在微信窗口点「进入微信 / 登录」，手机确认一下 <<<")
    log(">>> hook 会在登录打开数据库的瞬间抓到 master key <<<")
    log("=" * 50 + "\n")

    seen = set(); t0 = time.time(); buf = ctypes.create_string_buffer(128); master = None
    while time.time() - t0 < 180:
        if dll.PollKeyData(buf, 128):
            hexkey = buf.value.decode("ascii", "replace").strip()
            if hexkey and hexkey not in seen:
                seen.add(hexkey)
                if is_master(hexkey, page1):
                    master = hexkey
                    log(f"[KEY] master key 抓到并校验通过：{hexkey[:8]}…")
                    break
        time.sleep(0.1)
    dll.CleanupHook()

    if not master:
        log("[!] 没抓到有效 master key（确认这 3 分钟内完成了登录）"); sys.exit(3)

    cfg["master_key"] = master
    save_config(cfg)
    log("\n✓ 已缓存 master key 到 wx_config.json。以后每天的待办提取直接复用，不用再重启微信。")
    log("  独立重开微信还给你 …")
    subprocess.Popen([weixin])


if __name__ == "__main__":
    main()
