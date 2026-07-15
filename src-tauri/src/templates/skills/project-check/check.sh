#!/bin/sh
# 项目检测默认脚本(Unix)。退出码 0=pass,非 0=fail;工具缺失=跳过该项。
# 环境变量:POLARIS_CHECK_DIR(worktree)、POLARIS_CHECK_PROFILE、POLARIS_TASK_ID。
cd "${POLARIS_CHECK_DIR:-.}" || exit 1
failed=0

run_check() {
    name="$1"; shift
    exe="$1"
    if ! command -v "$exe" >/dev/null 2>&1; then
        echo "[$name] 工具缺失($exe),跳过"
        return 0
    fi
    echo "[$name] 开始"
    out=$("$@" 2>&1)
    rc=$?
    printf '%s\n' "$out" | tail -n 200
    if [ "$rc" -eq 0 ]; then
        echo "[$name] 通过"
    else
        echo "[$name] 失败(exit=$rc)"
        failed=1
    fi
}

[ -f Cargo.toml ] && run_check "cargo check" cargo check --quiet
if [ -f package.json ]; then
    for s in lint typecheck build; do
        if grep -q "\"$s\"[[:space:]]*:" package.json; then
            run_check "npm run $s" npm run "$s"
        fi
    done
fi
if [ -f pyproject.toml ] || [ -f ruff.toml ]; then
    run_check "ruff check" ruff check .
fi

if [ "$failed" -ne 0 ]; then exit 1; fi
echo "项目检测全部通过"
exit 0
