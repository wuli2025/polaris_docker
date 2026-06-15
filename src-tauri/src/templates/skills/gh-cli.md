# GitHub CLI 速查（gh）

用 `gh` 命令行管 GitHub：仓库 / Issue / PR / Actions / Release，不必开网页。先确认已登录：`gh auth status`，没登录走 `gh auth login`。

## 常用动作
**PR**
```bash
gh pr create --title "..." --body "..."   # 从当前分支开 PR
gh pr list                                # 列开放 PR
gh pr view <号|URL> --comments            # 看 PR + 评论
gh pr checkout <号>                        # 切到某 PR 分支
gh pr diff <号>                            # 看 diff
gh pr merge <号> --squash --delete-branch # 合并(按团队约定选 squash/merge/rebase)
```
**Issue**
```bash
gh issue create --title "..." --body "..."
gh issue list --label bug --state open
gh issue view <号>
```
**Actions / CI**
```bash
gh run list                       # 最近的工作流运行
gh run view <run-id> --log-failed # 只看失败步骤日志
gh run watch <run-id>             # 实时盯一次运行
```
**Release**
```bash
gh release create v1.2.3 --notes "..." ./dist/*   # 建 release 并传产物
gh release list
```

## 约定
- 破坏性 / 外发动作（合并 PR、发 release、关 Issue）先跟用户确认，别擅自执行。
- 脚本里用 `gh ... --json <字段> -q <jq过滤>` 拿结构化结果。
- 仓库不在当前目录时加 `-R owner/repo` 指定。
