# -*- coding: utf-8 -*-
"""
微信聊天 · macOS 自包含解密/导出流程
─────────────────────────────────────────────────────────────
Windows 版靠给 Weixin.exe 注入 DLL hook 在登录瞬间抓 master key；macOS 上不能注入
DLL，改走「读进程内存」的路子：微信(WCDB/SQLCipher 4) 会把每个库「已派生好的 raw
key + salt」以 x'<key><salt>' 形式缓存在内存里，扫出来对号入座即可。

与 Windows 的两点关键不同：
  1. macOS 是**每库一个 key**(不是账号级一个 master key)，抓一次缓存整份 all_keys.json。
  2. 抓 key 需要：微信 ad-hoc 重签名(去 Hardened Runtime) + 以 root(sudo) 跑扫描器。
     但**每天的解密+导出不需要 sudo**——只要缓存的 key 还对得上当前库(账号没重登)。

本模块纯自包含：AES 走 macOS 自带 CommonCrypto(ctypes)，无需 pip 装任何东西；
HMAC/PBKDF2 走标准库。被 wx_setup.py / wx_daily.py 在 Darwin 上调用。

隐私底线同技能：所有解密产物只落本机，绝不外发/上传/发布。
"""
import os, sys, json, time, glob, hashlib, hmac, struct, subprocess, ctypes, ctypes.util

HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = 4096
RESERVE = 80          # 每页尾部保留区：IV(16) + HMAC(64)
IV_LEN = 16
HMAC_LEN = 64
KDF_ITER_HMAC = 2     # MAC key 派生迭代数(SQLCipher 4)


def log(*a):
    print(*a); sys.stdout.flush()


# ═══════════════════════ 定位微信 App 与数据目录 ═══════════════════════
XWECHAT_BASE = os.path.expanduser(
    "~/Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files")


def find_data_dirs():
    """返回所有账号的 db_storage 目录(通常一个)。"""
    dirs = []
    for acct in sorted(glob.glob(os.path.join(XWECHAT_BASE, "*"))):
        st = os.path.join(acct, "db_storage")
        if os.path.isdir(st):
            dirs.append(st)
    return dirs


def find_wechat_app():
    """定位 WeChat.app。优先常见路径，兜底 mdfind。"""
    cands = [
        "/Applications/WeChat.app",
        "/Applications/微信.app",
        os.path.expanduser("~/Applications/WeChat.app"),
    ]
    for c in cands:
        if os.path.isdir(c):
            return c
    try:
        r = subprocess.run(
            ["mdfind", "kMDItemCFBundleIdentifier == 'com.tencent.xinWeChat'"],
            capture_output=True, text=True, timeout=15)
        for line in r.stdout.splitlines():
            if line.strip().endswith(".app") and os.path.isdir(line.strip()):
                return line.strip()
    except Exception:
        pass
    return None


def wechat_running():
    for name in ("WeChat", "Weixin"):
        r = subprocess.run(["pgrep", "-x", name], capture_output=True, text=True)
        if r.stdout.strip():
            return True
    return False


def quit_wechat():
    subprocess.run(["osascript", "-e", 'quit app "WeChat"'], capture_output=True)
    subprocess.run(["pkill", "-x", "WeChat"], capture_output=True)
    subprocess.run(["pkill", "-x", "Weixin"], capture_output=True)


def launch_wechat(app):
    subprocess.run(["open", app], capture_output=True)


# ═══════════════════════ ad-hoc 重签名(去 Hardened Runtime) ═══════════════════════
def is_hardened(app):
    """检测 App 是否带 Hardened Runtime(带则别的进程读不了它内存)。"""
    r = subprocess.run(["codesign", "-dv", app], capture_output=True, text=True)
    info = (r.stderr or "") + (r.stdout or "")
    for line in info.splitlines():
        if line.startswith("CodeDirectory") and "runtime" in line:
            return True
    return False


def resign_adhoc(app):
    """ad-hoc 重签名去掉 Hardened Runtime；改的是 /Applications 下的包，须 sudo。"""
    log(f"[*] ad-hoc 重签名微信(去 Hardened Runtime)：{app}")
    log("    需要管理员密码(sudo)。")
    r = subprocess.run(
        ["sudo", "codesign", "--force", "--deep", "--sign", "-", app],
        capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError("重签名失败：\n" + (r.stderr or r.stdout or "")[-800:])
    log("    ✓ 重签名完成")


# ═══════════════════════ 内存扫描抓 key ═══════════════════════
def scanner_bin():
    return os.path.join(HERE, "mac", "find_all_keys_macos")


def compile_scanner():
    src = os.path.join(HERE, "mac", "find_all_keys_macos.c")
    out = scanner_bin()
    if os.path.exists(out) and os.path.getmtime(out) >= os.path.getmtime(src):
        return out
    log("[*] 编译内存扫描器 …")
    r = subprocess.run(["cc", "-O2", "-o", out, src], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError("编译扫描器失败(需 Xcode Command Line Tools：xcode-select --install)\n"
                           + (r.stderr or "")[-800:])
    return out


def scan_keys():
    """sudo 跑扫描器，返回 {rel_db_path: {enc_key, salt}}。"""
    exe = compile_scanner()
    log("[*] 扫描微信进程内存抓 key(需要管理员密码 sudo) …")
    r = subprocess.run(["sudo", exe], capture_output=True, text=True)
    # 扫描器把 JSON 打到 stdout、进度打到 stderr
    if r.stderr:
        for ln in r.stderr.splitlines():
            log("    " + ln)
    if r.returncode != 0 or not r.stdout.strip():
        raise RuntimeError("抓 key 失败(确认微信已登录进主界面、已 ad-hoc 重签名)。")
    try:
        return json.loads(r.stdout)
    except Exception as e:
        raise RuntimeError(f"扫描器输出解析失败：{e}\n{r.stdout[:400]}")


# ═══════════════════════ SQLCipher 4 解密(纯自包含) ═══════════════════════
def _aes_cbc_decrypt(key, iv, data):
    """AES-256-CBC 无填充解密。优先 macOS CommonCrypto(零依赖)，兜底 pycryptodome。"""
    # 1) CommonCrypto via ctypes(macOS 自带，最快且零 pip 依赖)
    try:
        lib = ctypes.CDLL(ctypes.util.find_library("System") or "libSystem.dylib")
        CCCrypt = lib.CCCrypt
        CCCrypt.restype = ctypes.c_int32
        outbuf = ctypes.create_string_buffer(len(data) + 16)
        moved = ctypes.c_size_t(0)
        # kCCDecrypt=1, kCCAlgorithmAES=0, options=0(CBC 无填充)
        st = CCCrypt(1, 0, 0, key, len(key), iv, data, len(data),
                     outbuf, len(outbuf), ctypes.byref(moved))
        if st == 0:
            return outbuf.raw[:moved.value]
    except Exception:
        pass
    # 2) pycryptodome 兜底
    try:
        from Crypto.Cipher import AES
        return AES.new(key, AES.MODE_CBC, iv).decrypt(data)
    except Exception:
        pass
    # 3) cryptography 兜底
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    dec = Cipher(algorithms.AES(key), modes.CBC(iv)).decryptor()
    return dec.update(data) + dec.finalize()


def _mac_key(enc_key, salt):
    mac_salt = bytes(b ^ 0x3A for b in salt)
    return hashlib.pbkdf2_hmac("sha512", enc_key, mac_salt, KDF_ITER_HMAC, dklen=32)


def decrypt_db(src, dst, enc_key_hex, salt_hex=None, verify=True):
    """把加密库 src 解成明文 sqlite 落到 dst。返回 True/False。"""
    enc_key = bytes.fromhex(enc_key_hex)
    raw = open(src, "rb").read()
    if raw[:16] == b"SQLite format 3\x00":
        # 已是明文，直接拷
        open(dst, "wb").write(raw); return True
    if len(raw) < PAGE:
        return False
    salt = bytes.fromhex(salt_hex) if salt_hex else raw[:16]
    mac_key = _mac_key(enc_key, salt)

    npages = len(raw) // PAGE
    out = bytearray()
    for i in range(npages):
        page = raw[i * PAGE:(i + 1) * PAGE]
        start = 16 if i == 0 else 0          # 第 1 页前 16 字节是 salt(明文)
        body = page[start:PAGE - RESERVE]     # 密文
        iv = page[PAGE - RESERVE:PAGE - RESERVE + IV_LEN]
        if verify and i == 0:
            # 校验第 1 页 HMAC，key 不对早失败别产出垃圾
            hm = hmac.new(mac_key, page[start:PAGE - HMAC_LEN], hashlib.sha512)
            hm.update(struct.pack("<I", i + 1))
            if hm.digest() != page[PAGE - HMAC_LEN:PAGE]:
                return False
        plain = _aes_cbc_decrypt(enc_key, iv, body)
        if i == 0:
            out += b"SQLite format 3\x00" + plain + b"\x00" * RESERVE
        else:
            out += plain + b"\x00" * RESERVE
    open(dst, "wb").write(out)
    return True


# ═══════════════════════ 从明文库导出聊天(挖待办用) ═══════════════════════
import sqlite3

# local_type → wx_daily 期望的 type 串
_TYPE_MAP = {
    1: "text", 3: "image", 34: "voice", 43: "video", 42: "contact_card",
    47: "sticker", 48: "location", 49: "link_or_file", 50: "call",
    10000: "system", 10002: "system",
}


def _open_ro(path):
    return sqlite3.connect(f"file:{path}?mode=ro", uri=True)


def _tables(conn):
    return [r[0] for r in conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table'").fetchall()]


def _decode_content(v):
    """message_content 可能是 str / bytes(可能 zlib 压缩)。尽量取出可读文本。"""
    if v is None:
        return ""
    if isinstance(v, str):
        return v
    if isinstance(v, (bytes, bytearray)):
        b = bytes(v)
        # 尝试直接 utf-8
        try:
            return b.decode("utf-8")
        except Exception:
            pass
        # 尝试 zlib 解压再 utf-8
        try:
            import zlib
            return zlib.decompress(b).decode("utf-8", "replace")
        except Exception:
            return b.decode("utf-8", "replace")
    return str(v)


def _strip_group_prefix(text):
    """群消息文本历史上会带 'wxid_xxx:\\n<正文>' 前缀，剥掉。"""
    if "\n" in text:
        head, rest = text.split("\n", 1)
        h = head.strip()
        if h and (h.startswith("wxid_") or h.endswith("@chatroom")
                  or (":" not in h and len(h) <= 40 and " " not in h)):
            return rest
    return text


def _build_contact_map(storage):
    """从 contact.db 解出的明文里取 username→显示名、以及 username 集合。"""
    names = {}          # username -> 显示名
    usernames = set()
    cpath = os.path.join(storage, "_plain", "contact", "contact.db")
    if not os.path.exists(cpath):
        # 有的版本 contact.db 直接在 db_storage 根/别处，尽力找
        for p in glob.glob(os.path.join(storage, "_plain", "**", "contact*.db"), recursive=True):
            cpath = p; break
    if not os.path.exists(cpath):
        return names, usernames
    try:
        conn = _open_ro(cpath)
    except Exception:
        return names, usernames
    try:
        for t in _tables(conn):
            if not t.lower().startswith("contact"):
                continue
            cols = [c[1] for c in conn.execute(f"PRAGMA table_info({t})").fetchall()]
            ucol = next((c for c in cols if c.lower() in ("username", "user_name", "m_username")), None)
            if not ucol:
                continue
            ncol = next((c for c in cols if c.lower() in
                         ("remark", "nick_name", "nickname", "m_nsremark", "m_nsnickname")), None)
            q = f"SELECT {ucol}" + (f", {ncol}" if ncol else "") + f" FROM {t}"
            for row in conn.execute(q).fetchall():
                u = row[0]
                if not u:
                    continue
                usernames.add(u)
                disp = (row[1] if ncol and len(row) > 1 else None) or u
                names[u] = disp
    except Exception:
        pass
    finally:
        conn.close()
    return names, usernames


def _detect_self(convos):
    """跨会话里作为 sender 出现在最多不同会话中的 wxid ≈ 本人。"""
    from collections import Counter
    seen = Counter()
    for c in convos:
        senders = {m["_sender_wxid"] for m in c["messages"] if m.get("_sender_wxid")}
        for s in senders:
            seen[s] += 1
    if not seen:
        return None
    return seen.most_common(1)[0][0]


def export_chats(storage, out_dir):
    """
    解密后从明文 message_*.db 导出成 wx_daily 期望的 exported_chats/<md5>.json。
    返回导出会话数。
    """
    plain = os.path.join(storage, "_plain")
    os.makedirs(out_dir, exist_ok=True)

    name_map, usernames = _build_contact_map(storage)
    # md5(username) -> username，用于把 Msg_<md5> 表名对回聊天对象
    md5map = {hashlib.md5(u.encode("utf-8")).hexdigest(): u for u in usernames}

    convos = []
    for db in sorted(glob.glob(os.path.join(plain, "message", "message_*.db"))):
        try:
            conn = _open_ro(db)
        except Exception:
            continue
        try:
            tabs = _tables(conn)
            # 本 db 内 sender 局部 id → wxid
            name2id = {}
            if "Name2Id" in tabs:
                try:
                    for rowid, uname in conn.execute("SELECT rowid, user_name FROM Name2Id"):
                        name2id[rowid] = uname
                except Exception:
                    pass
            for t in tabs:
                if not t.startswith("Msg_"):
                    continue
                talker = md5map.get(t[4:].lower())
                if not talker:
                    # 对不上联系人就跳过(可能是已删/陌生人；宁缺毋滥)
                    continue
                is_group = talker.endswith("@chatroom")
                cols = [c[1] for c in conn.execute(f"PRAGMA table_info({t})").fetchall()]
                def col(*names):
                    for n in names:
                        for c in cols:
                            if c.lower() == n:
                                return c
                    return None
                c_time = col("create_time", "createtime")
                c_type = col("local_type", "type", "m_ntype")
                c_send = col("real_sender_id", "talker_id", "sender_id")
                c_body = col("message_content", "m_nsmessage", "content")
                if not (c_time and c_body):
                    continue
                sel = ",".join([c for c in (c_time, c_type, c_send, c_body) if c])
                msgs = []
                try:
                    rows = conn.execute(
                        f"SELECT {sel} FROM {t} ORDER BY {c_time} ASC").fetchall()
                except Exception:
                    continue
                for r in rows:
                    d = dict(zip([c for c in (c_time, c_type, c_send, c_body) if c], r))
                    ts = int(d.get(c_time) or 0)
                    lt = int(d.get(c_type) or 1) if c_type else 1
                    typ = _TYPE_MAP.get(lt, "text")
                    sender_wxid = name2id.get(d.get(c_send)) if c_send else None
                    content = ""
                    if typ in ("text",):
                        content = _decode_content(d.get(c_body))
                        if is_group:
                            content = _strip_group_prefix(content)
                        content = content.strip()
                    msgs.append({
                        "timestamp": ts, "type": typ, "content": content,
                        "_sender_wxid": sender_wxid or "",
                    })
                if not msgs:
                    continue
                convos.append({
                    "username": talker,
                    "chat": name_map.get(talker, talker),
                    "is_group": is_group,
                    "messages": msgs,
                })
        finally:
            conn.close()

    # 判定本人 → 落 sender 字段("me" / 显示名)
    self_wxid = _detect_self(convos)
    count = 0
    for c in convos:
        for m in c["messages"]:
            sw = m.pop("_sender_wxid", "")
            if sw and sw == self_wxid:
                m["sender"] = "me"
            elif sw:
                m["sender"] = name_map.get(sw, sw)
            else:
                # 私聊里无 sender 信息时，非本人一律记作对方
                m["sender"] = "me" if False else c["chat"]
        fn = os.path.join(out_dir, hashlib.md5(c["username"].encode()).hexdigest() + ".json")
        with open(fn, "w", encoding="utf-8") as f:
            json.dump(c, f, ensure_ascii=False)
        count += 1
    return count


# ═══════════════════════ 顶层：一次性授权 & 每日流程 ═══════════════════════
def _config_path():
    return os.path.join(HERE, "wx_config.json")


def _load_cfg():
    p = _config_path()
    if os.path.exists(p):
        try:
            return json.load(open(p, encoding="utf-8"))
        except Exception:
            return {}
    return {}


def _save_cfg(cfg):
    p = _config_path()
    tmp = p + ".tmp"
    json.dump(cfg, open(tmp, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    os.replace(tmp, p)


def keys_still_valid(cfg, storage):
    """缓存的 per-db key 是否还对得上当前库(account 没重登、无新库)。"""
    keys = (cfg.get("mac_keys") or {})
    if not keys:
        return False
    # 至少 message_0.db 要在且校验通过
    for db in sorted(glob.glob(os.path.join(storage, "message", "message_*.db"))):
        rel = os.path.relpath(db, storage).replace(os.sep, "/")
        info = keys.get(rel)
        if not info:
            return False   # 出现没缓存 key 的新库 → 需重抓
        try:
            page1 = open(db, "rb").read(PAGE)
            if page1[:16] == b"SQLite format 3\x00":
                continue
            enc = bytes.fromhex(info["enc_key"])
            salt = page1[:16]
            mac_key = _mac_key(enc, salt)
            hm = hmac.new(mac_key, page1[16:PAGE - HMAC_LEN], hashlib.sha512)
            hm.update(struct.pack("<I", 1))
            if hm.digest() != page1[PAGE - HMAC_LEN:PAGE]:
                return False
        except Exception:
            return False
    return True


def setup():
    """一次性授权：重签名(如需) → 起微信登录 → sudo 抓 key → 校验缓存。"""
    if sys.platform != "darwin":
        raise RuntimeError("wx_mac.setup 只用于 macOS")
    app = find_wechat_app()
    if not app:
        raise RuntimeError("没找到 WeChat.app，请确认已安装微信 4.x。")
    storages = find_data_dirs()
    if not storages:
        raise RuntimeError(f"没找到微信数据目录：{XWECHAT_BASE}/*/db_storage")
    storage = storages[0]
    log(f"[*] 微信 App：{app}")
    log(f"[*] 数据目录：{storage}")

    if is_hardened(app):
        if wechat_running():
            log("[1] 先退出微信以便重签名 …")
            quit_wechat(); time.sleep(3)
        resign_adhoc(app)
    else:
        log("[1] 微信未开启 Hardened Runtime，跳过重签名。")

    if not wechat_running():
        log("[2] 启动微信，请扫码/解锁登录进**主界面**(key 登录后才进内存) …")
        launch_wechat(app)
    log("\n" + "=" * 52)
    log(">>> 请在微信里登录并停在主界面，然后回到终端按 Enter 继续 <<<")
    log("=" * 52)
    try:
        input()
    except EOFError:
        log("    (非交互环境，直接继续)"); time.sleep(2)

    log("[3] 抓 key …")
    keys = scan_keys()   # {rel_db: {enc_key, salt}}
    if not keys:
        raise RuntimeError("没抓到任何 key。")

    # 校验并缓存
    cfg = _load_cfg()
    cfg["platform"] = "mac"
    cfg["data_dir"] = storage
    cfg["app"] = app
    cfg["mac_keys"] = keys
    cfg.setdefault("exported_dir", os.path.join(HERE, "exported_chats"))
    cfg.setdefault("my_nicks", [])
    cfg.setdefault("window_days", 7)
    cfg.setdefault("max_tasks", 8)
    _save_cfg(cfg)
    log(f"[✓] 抓到并缓存 {len(keys)} 个库的 key → {_config_path()}")
    log("    以后每天的待办提取直接复用，不用再 sudo、不用重启微信。")
    return cfg


def run_pipeline(cfg):
    """每日流程(免 sudo)：解密全部库 → 导出聊天到 exported_dir。"""
    storage = cfg.get("data_dir") or (find_data_dirs() or [None])[0]
    if not storage:
        raise RuntimeError("找不到微信数据目录")
    keys = cfg.get("mac_keys") or {}
    if not keys:
        raise RuntimeError("没有缓存的 key，请先跑 wx_setup.py 完成授权")

    plain = os.path.join(storage, "_plain")
    ok = 0; fail = 0
    for db in sorted(glob.glob(os.path.join(storage, "**", "*.db"), recursive=True)):
        if os.sep + "_plain" + os.sep in db:
            continue
        rel = os.path.relpath(db, storage).replace(os.sep, "/")
        info = keys.get(rel)
        if not info:
            continue
        dst = os.path.join(plain, rel.replace("/", os.sep))
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        try:
            if decrypt_db(db, dst, info["enc_key"], info.get("salt")):
                ok += 1
            else:
                fail += 1
        except Exception as e:
            fail += 1
            log(f"    解密失败 {rel}: {e}")
    log(f"  ✓ 解密完成：成功 {ok}、失败 {fail}")

    out_dir = cfg.get("exported_dir") or os.path.join(HERE, "exported_chats")
    n = export_chats(storage, out_dir)
    cfg["exported_dir"] = out_dir
    _save_cfg(cfg)
    log(f"  ✓ 导出 {n} 个会话 → {out_dir}")
