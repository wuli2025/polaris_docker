# 智能提交（Conventional Commits）

把工作区改动整理成清晰、规范的 git commit。遵循 Conventional Commits，让历史可读、可自动生成 changelog。

## 流程
1. **看清改了什么**：`git status` + `git diff`（已暂存看 `--staged`）。理解这批改动的意图，别只看文件名。
2. **合理暂存**：把不相关的改动拆成多个 commit，别一锅烩。一个 commit 只讲一件事。
3. **生成消息**：
   ```
   <type>(<scope>): <简短祈使句，不超过 ~50 字>

   <可选正文：为什么这么改、影响面、坑>
   ```
   - `type`：feat / fix / docs / style / refactor / perf / test / build / ci / chore。
   - `scope`：受影响的模块（可选），如 `fix(auth):`。
   - 标题用现在时祈使句、不加句号；正文解释「为什么」而非「做了啥」（diff 已说明做了啥）。

## 约定
- **优先新建 commit，不随意 amend** 已有提交。
- 不跳过 hooks（`--no-verify`）、不绕过签名，除非用户明确要求；hook 失败先查根因。
- 提交前确认没把密钥 / 大文件 / 临时日志带进去。
- 破坏性变更在正文加 `BREAKING CHANGE:` 段。

## 示例
```
feat(filecenter): 盘点默认纳入微信/QQ/浏览器下载目录

新增 app_data_roots() 作为默认勾选根，经显式根绕过 appdata 黑名单。
```
