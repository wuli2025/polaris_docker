---
id: wechat-tasks
name: 微信聊天 · 每日待办
description: 每天本地解密微信聊天，从「你回过话的私聊 + 活跃群里最近几天别人发来、你还没回」的消息里挖出待办，写进 Polaris 晨报卡片。全本地、不发布、不上传。
source: official
author: Polaris Team
---

# 微信聊天 · 每日待办提取

把微信变成一份「今天该回谁」的待办清单。全程在本机完成：本地解密 → 读消息 → 挑出
**你参与过、且最近几天别人发来还没回**的消息 → 落进 Polaris 晨报。**不联网、不上传、不发布。**

## 原理（一次授权，之后每天自动）

微信 4.0+ 的数据库用 SQLCipher(WCDB) 加密。Windows 与 macOS 抓 key 的手法不同，但抓到后
的解密/挖待办/写晨报完全一样：

**Windows** —— master key 只在**登录那一刻**交给微信内核，只能靠注入 hook 抓：
- **首次** `wx_setup.py`：重启微信、注入 hook、等你扫码登录的瞬间抓到 master key 并缓存。
  基于开源项目 `ycccccccy/wx_key` 的 hook 法，版本无关。
- **之后每天** `wx_daily.py` 复用缓存 key（账号级稳定），解密+导出+挖待办，不用再重启微信。

**macOS** —— 不能注入 DLL，改走**读进程内存**：微信把每个库「已派生好的 raw key + salt」
以 `x'<key><salt>'` 形式缓存在内存里，扫出来对号入座即可（每库一个 key，不是一个 master key）：
- **首次** `wx_setup.py`：如需先给微信 ad-hoc 重签名（去 Hardened Runtime，须管理员密码），
  你登录进主界面后，用一个自带的内存扫描器（`sudo` 跑一次）抓齐所有库的 key 并缓存。
- **之后每天** `wx_daily.py`：**免 sudo**，直接用缓存 key 解密+导出+挖待办。只有账号重登
  或出现新库导致 key 对不上时，才在晨报里放「请重新授权」提醒。
- 全自包含：解密的 AES 走 macOS 自带 CommonCrypto，**无需 pip 安装任何东西**；只需一次
  `xcode-select --install` 提供 `cc` 编译器。数据目录/App 路径自动探测，一般无需填配置。

## 怎么用

### 一次性授权（仅首次 / 重新登录后）
```
python <技能目录>/scripts/wx_setup.py
```
- **Windows**：按提示在微信窗口点登录、手机确认，抓到 master key 写进 `scripts/wx_config.json`。
- **macOS**：脚本会（如需）重签名微信 → 让你登录进主界面 → 回终端按 Enter → `sudo` 扫内存
  抓齐每库 key 写进 `scripts/wx_config.json`。全程会提示要输管理员密码的地方。

### 每天提取待办（自动化流程会自动跑）
```
python <技能目录>/scripts/wx_daily.py
```
完成后，待办会出现在**对话框顶部 / 自动化页**的晨报卡片里。点「让我去做」会起一轮对话，
我会帮你判断要不要回、并拟回复草稿；点「先放一放」忽略（当天不再提示）。

调试：`python <技能目录>/scripts/wx_daily.py --no-export` 跳过解密导出，只对已有导出重算待办。

## 待办的口径（已按用户偏好固化）

- **只看你回过话的会话**：私聊全要；群聊只算「你近 30 天内发过言」的活跃群——纯广播、
  公众号(gh_*)、你从不参与的群一律不挖，避免噪音。
- **优先最近几天别人发来的**：默认取最近 **7 天**、对方发来、且在**你最后一次发言之后**
  （=还没回）的消息，每个这样的会话出一条待办。
- **排序**：私聊 > 群里 @你 > 其它活跃群；同档按最新来信时间。默认最多 8 条。
- 窗口天数 / 条数 / 群昵称（用于识别 @你）都在 `wx_config.json` 可调。

## 配置 `scripts/wx_config.json`

**Windows**：依赖现成的 `wechat-decrypt` 工具链（解密/导出）与 `wx_key` 工具（抓 key）。
字段见 `wx_config.example.json`：`weixin_exe / db_dir / wx_key_dir / tools_dir / venv_python /
exported_dir / my_nicks / window_days / max_tasks`。`master_key` 由 `wx_setup.py` 自动写入。

**macOS**：数据目录、App 路径都自动探测，**通常不用手填**。`wx_setup.py` 会自动写好
`platform=mac / data_dir / app / mac_keys`（每库 key）等字段。只有想识别群里「@你」时，可选填
`my_nicks`（你的群昵称/微信名）；`window_days / max_tasks` 同 Windows 可调。

## 隐私底线

解密产物（含明文私聊）只落本机，**绝不**外发、上传或对外发布。晨报里只放标题摘要与
你本人可见的待办。
