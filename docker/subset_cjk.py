#!/usr/bin/env python3
"""Polaris CJK 字体子集化脚本
- 输入:/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc
  (full 镜像装 fonts-noto-cjk 后,Debian 包安装位置)
- 输出:/out/NotoSansCJK-{Regular,Bold,Medium}.woff2
- 字符集:docker/font-subset-chars.txt(ASCII + 6763 高频中文 + 实用 emoji)
- 体积:从 102MB 全语种 → 3 weight × ~25MB = ~75MB(共享字符),实际单 weight ~12MB
- 失败:exit 1,build 阶段不 fail(Dockerfile 软降级到全语种)
"""
import os
import sys
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.subset import Subsetter, Options

SRC = Path("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")
if not SRC.exists():
    SRC = Path("/usr/share/fonts/truetype/noto-cjk/NotoSansCJK-Regular.ttc")
OUT = Path("/out")
OUT.mkdir(parents=True, exist_ok=True)
CHARS = Path("/docker/font-subset-chars.txt")
if not CHARS.exists():
    CHARS = Path("/chars.txt")
if not CHARS.exists():
    print("FATAL: chars.txt 缺失", file=sys.stderr)
    sys.exit(1)

# 选 Regular / Bold / Medium 三个 weight(PPTX 实际用法)
WEIGHTS = [
    ("NotoSansCJK-Regular", "Regular"),
    ("NotoSansCJK-Bold",    "Bold"),
    ("NotoSansCJK-Medium",  "Medium"),
]
# TTC 集合索引:Regular=0, Bold=1, Medium=2(NotoSansCJK 排布)
TTC_IDX = {"Regular": 0, "Bold": 1, "Medium": 2}

text = CHARS.read_text(encoding="utf-8")
chars = set(c for c in text if c.strip() and c != "#" and not text.split("#", 1)[0].endswith(c))
# 简化:直接读所有非 # 开头行
chars = set()
for line in text.splitlines():
    s = line.strip()
    if not s or s.startswith("#"):
        continue
    chars.update(s)

print(f"[subset] 字符数: {len(chars)}")

opts = Options()
opts.flavor = "woff2"
opts.desubroutinize = True
opts.hinting = False  # 砍 hinting 减 10-15%
opts.layout_features = []  # 不带 OT layout,体积再小
opts.name_IDs = []  # 不写 name 表,体积再小
opts.notdef_outline = True
opts.recalc_bounds = True
opts.recalc_timestamp = False

ok = 0
for stem, weight in WEIGHTS:
    if not SRC.exists():
        print(f"WARN: {SRC} 缺失,跳 {weight}", file=sys.stderr)
        continue
    try:
        font = TTFont(str(SRC), fontNumber=TTC_IDX.get(weight, 0))
        subsetter = Subsetter(options=opts)
        subsetter.populate(text="".join(chars))
        subsetter.subset(font)
        out_path = OUT / f"{stem}.woff2"
        font.flavor = "woff2"
        font.save(str(out_path))
        sz = out_path.stat().st_size
        print(f"[subset] {weight}: {sz/1024:.1f} KB → {out_path}")
        ok += 1
    except Exception as e:
        print(f"WARN: {weight} 失败: {e}", file=sys.stderr)

if ok == 0:
    print("FATAL: 三个 weight 全失败", file=sys.stderr)
    sys.exit(1)
print(f"[subset] done, ok={ok}")
