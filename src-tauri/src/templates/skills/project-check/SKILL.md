---
id: project-check-default
name: 项目检测(默认检查技能)
description: 协作检查闸的默认项目检测:按仓库文件探测工具链(cargo check / npm lint·typecheck·build / ruff),退出码 0=通过、非 0=不通过。确定性脚本,AI 不参与判定;可复制此技能改造出团队自己的检查项
source: official
author: polaris
check_entry_windows: scripts/check.ps1
check_entry_unix: scripts/check.sh
check_timeout_secs: 600
---

# 项目检测(默认检查技能)

这是 Polaris 多人协作「检查工作流」的默认项目检测技能。任务卡提交后,主机会在临时
worktree 里执行本技能的入口脚本,以**退出码**判定:0=pass,非 0=fail(stdout/stderr
尾部会记入检查详情)。AI 永远不参与 pass/fail 判定——脚本说了算。

## 检查协议(想自定义检查的团队照此写自己的技能)

frontmatter 声明三个扁平键:

- `check_entry_windows`: Windows 入口脚本(相对技能目录,PowerShell)
- `check_entry_unix`: Unix 入口脚本(相对技能目录,sh)
- `check_timeout_secs`: 超时秒数(超时判 fail,防死循环绕过)

脚本运行时收到环境变量:

- `POLARIS_CHECK_DIR`: 待检代码的临时 worktree 路径(工作目录也在此)
- `POLARIS_CHECK_PROFILE`: 项目检查档位(code / creative)
- `POLARIS_TASK_ID`: 任务卡 id

## 默认脚本做什么

按文件探测,探测到什么跑什么(工具未安装则跳过该项,不误伤):

| 探测 | 检查 |
|---|---|
| Cargo.toml | `cargo check` |
| package.json(有对应 script 才跑) | `npm run lint` / `typecheck` / `build` |
| pyproject.toml 或 ruff.toml | `ruff check .` |

任一项失败即整体退出码 1。密钥扫描与大文件闸不在本技能内——它们是检查闸的
不可关前置硬闸,永远由主机内置执行。
