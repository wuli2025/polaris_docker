#!/bin/sh
# 容器内 Xvfb 启动器 —— 给需要 X server 的子命令(Chromium / CloakBrowser / Chromium 截图)
# 提供虚拟显示。polaris-server 本体是 headless 服务,直接走 ENTRYPOINT,不需要 Xvfb。
#
# 用法(在容器内手动跑):
#   xvfb-wrap python wechat_yiban.py --mode publish --body-file body.html --title "x" --theme 墨韵
#   xvfb-wrap chromium --no-sandbox --headless --dump-dom https://example.com
#   xvfb-wrap python -c "from cloakbrowser import launch; b = launch(headless=False); ..."
#
# 设计:
#   · 后台起 Xvfb :99 + 屏幕 1280x800x24
#   · 写 cookie 到 /tmp/.X99-auth,任何子进程都能接
#   · 父进程退出/被信号时,顺手 kill 掉 Xvfb,不留孤儿
#   · 端口冲突就尝试 :100/:101;实在起不来就直接报错不静默吞
#   · 任何命令的退出码原样透传
set -e

if [ $# -eq 0 ]; then
  echo "usage: xvfb-wrap <command> [args...]" >&2
  exit 2
fi

# 找一个空闲的 DISPLAY
pick_display() {
  for d in 99 100 101 102 103; do
    if [ ! -e "/tmp/.X11-unix/X$d" ]; then
      echo "$d"
      return 0
    fi
  done
  echo "no free X display" >&2
  return 1
}

DISP=$(pick_display)
XVFB_AUTH=/tmp/.X${DISP}-auth
rm -f "$XVFB_AUTH" 2>/dev/null || true

# 后台启 Xvfb
Xvfb ":$DISP" -screen 0 1280x800x24 -nolisten tcp -auth "$XVFB_AUTH" >/tmp/xvfb-$DISP.log 2>&1 &
XVFB_PID=$!

# 等 Xvfb 起来(socket 文件出现)
i=0
while [ $i -lt 30 ]; do
  if [ -e "/tmp/.X11-unix/X$DISP" ]; then break; fi
  sleep 0.1
  i=$((i + 1))
done

if [ ! -e "/tmp/.X11-unix/X$DISP" ]; then
  echo "xvfb-wrap: Xvfb 启动失败（log: /tmp/xvfb-$DISP.log）" >&2
  kill "$XVFB_PID" 2>/dev/null || true
  exit 1
fi

# 任何路径走人 —— 务必清掉 Xvfb,不留孤儿占内存
cleanup() {
  if kill -0 "$XVFB_PID" 2>/dev/null; then
    kill "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

# 把 DISPLAY/cookie 喂给子命令
export DISPLAY=":$DISP"
export XAUTHORITY="$XVFB_AUTH"

exec "$@"
