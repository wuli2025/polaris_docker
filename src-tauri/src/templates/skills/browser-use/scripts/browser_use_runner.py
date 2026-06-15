#!/usr/bin/env python3
"""Polaris · browser-use runner —— 让 browser-use 智能体驱动 CloakBrowser（隐身 Chromium）。

铁律:浏览器底层必须是 CloakBrowser,绝不用 browser-use 自带的 Patchright/Playwright 浏览器。
做法:用 cloakbrowser 启动一个带远程调试端口(CDP)的隐身 Chromium,再把 browser-use 通过
cdp_url 接到这个端口上 —— 「智能体决策 + 隐身浏览器执行」两全。

关键工程点:CloakBrowser 的 launch() 是同步 Playwright,若在 asyncio 事件循环里调它会触发
「Sync API inside asyncio loop」崩溃。所以本脚本把「启浏览器 / 等 CDP / 关浏览器」全放在同步
的 main() 里,异步循环(asyncio.run)只跑 browser-use,且只通过 CDP 连接,不碰任何 cloakbrowser 句柄。

用法:
  uv run --no-project browser_use_runner.py "高层任务描述" \
      [--start-url URL] [--out DIR] [--max-steps N] [--headful] [--port N] [--model NAME]

LLM:从环境变量取(Polaris 切供应商时已注入到进程 env);也可用 --model / BROWSER_USE_MODEL 覆盖:
  - Anthropic:  ANTHROPIC_AUTH_TOKEN 或 ANTHROPIC_API_KEY (+ 可选 ANTHROPIC_BASE_URL)
  - OpenAI 兼容: OPENAI_API_KEY (+ 可选 OPENAI_BASE_URL)
"""
import argparse
import asyncio
import os
import socket
import subprocess
import sys
import time
import urllib.request


def log(msg):
    print(f"[browser-use] {msg}", flush=True)


def free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def wait_cdp(port, timeout=30):
    url = f"http://127.0.0.1:{port}/json/version"
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2) as r:
                if r.status == 200:
                    return True
        except Exception:
            time.sleep(0.4)
    return False


def ensure_browser_use():
    try:
        import browser_use  # noqa: F401
        return
    except Exception:
        log("未装 browser-use,正在安装(pip install browser-use)…")
        subprocess.run(
            [sys.executable, "-m", "pip", "install", "-q", "browser-use"], check=True
        )


def launch_cloak(port, headful):
    """用 CloakBrowser 启动带 CDP 端口的隐身 Chromium(同步,务必在 asyncio 之外调用)。"""
    from cloakbrowser import launch

    return launch(
        headless=not headful,
        humanize=True,
        args=[f"--remote-debugging-port={port}"],
    )


def build_llm(model_override):
    """按环境变量选 LLM。优先 browser-use 内置 Chat* 封装,回退 langchain。"""
    anth_key = os.environ.get("ANTHROPIC_AUTH_TOKEN") or os.environ.get("ANTHROPIC_API_KEY")
    oai_key = os.environ.get("OPENAI_API_KEY")
    pick = model_override or os.environ.get("BROWSER_USE_MODEL")

    if anth_key:
        model = pick or os.environ.get("ANTHROPIC_MODEL") or "claude-sonnet-4-5"
        base = os.environ.get("ANTHROPIC_BASE_URL")
        kw = {"model": model, "api_key": anth_key}
        if base:
            kw["base_url"] = base
        try:
            from browser_use import ChatAnthropic
            return ChatAnthropic(**kw)
        except Exception:
            from langchain_anthropic import ChatAnthropic as LCAnthropic
            return LCAnthropic(**kw)

    if oai_key:
        model = pick or os.environ.get("OPENAI_MODEL") or "gpt-4o"
        base = os.environ.get("OPENAI_BASE_URL")
        kw = {"model": model, "api_key": oai_key}
        if base:
            kw["base_url"] = base
        try:
            from browser_use import ChatOpenAI
            return ChatOpenAI(**kw)
        except Exception:
            from langchain_openai import ChatOpenAI as LCOpenAI
            return LCOpenAI(**kw)

    raise SystemExit(
        "没有可用的 LLM 凭证:在 Polaris 配好供应商(会注入 ANTHROPIC_*/OPENAI_* 到环境),"
        "或自行设 OPENAI_API_KEY / ANTHROPIC_API_KEY。"
    )


def connect_browser(cdp_url):
    """把 browser-use 接到 CloakBrowser 的 CDP(兼容新旧版入参)。"""
    from browser_use import Browser

    try:
        return Browser(cdp_url=cdp_url)
    except TypeError:
        from browser_use import BrowserConfig  # 老版
        return Browser(config=BrowserConfig(cdp_url=cdp_url))


def extract_final(history):
    for attr in ("final_result", "final_answer"):
        f = getattr(history, attr, None)
        if callable(f):
            try:
                v = f()
            except Exception:
                v = None
        else:
            v = f
        if v:
            return v
    return None


async def run_agent(task, start_url, out_dir, max_steps, cdp_url, model_override):
    llm = build_llm(model_override)
    browser = connect_browser(cdp_url)

    from browser_use import Agent

    full_task = task if not start_url else f"先打开 {start_url}。然后:{task}"
    kw = {"task": full_task, "llm": llm, "browser": browser}
    try:
        agent = Agent(**kw)
    except TypeError:
        kw.pop("browser")
        kw["browser_session"] = browser  # 老版入参名
        agent = Agent(**kw)

    log("智能体开始执行…")
    try:
        history = await agent.run(max_steps=max_steps)
    except TypeError:
        history = await agent.run()

    os.makedirs(out_dir, exist_ok=True)
    result_path = os.path.abspath(os.path.join(out_dir, "browser_use_result.txt"))
    final = extract_final(history)
    with open(result_path, "w", encoding="utf-8") as fh:
        fh.write(str(final) if final is not None else str(history))
    log(f"完成。结果文件:{result_path}")
    print(final if final is not None else "(无最终结论文本,详见结果文件)")

    # browser-use 自己的连接收尾(best-effort);CloakBrowser 句柄在 main() 同步关。
    closer = getattr(browser, "close", None)
    if closer:
        try:
            r = closer()
            if asyncio.iscoroutine(r):
                await r
        except Exception:
            pass


def main():
    ap = argparse.ArgumentParser(description="browser-use 智能体(驱动 CloakBrowser)")
    ap.add_argument("task", help="高层任务描述(大白话)")
    ap.add_argument("--start-url", default="", help="起始页 URL(可选)")
    ap.add_argument("--out", default=".", help="结果输出目录")
    ap.add_argument("--max-steps", type=int, default=25, help="步数上限")
    ap.add_argument("--port", type=int, default=0, help="CDP 端口(默认随机空闲端口)")
    ap.add_argument("--headful", action="store_true", help="显示浏览器窗口(默认无头)")
    ap.add_argument("--model", default="", help="覆盖 LLM 模型名")
    a = ap.parse_args()

    ensure_browser_use()
    port = a.port or free_port()

    cloak = None
    try:
        try:
            cloak = launch_cloak(port, a.headful)
        except Exception as e:
            raise SystemExit(
                f"CloakBrowser 启动失败(本技能铁律必须走它):{e}\n"
                "先装:pip install cloakbrowser  或  pip install ~/Polaris/plugins/cloakbrowser"
            )
        if not wait_cdp(port):
            raise SystemExit(f"CloakBrowser 的 CDP 端口 {port} 在超时内未就绪")
        cdp_url = f"http://127.0.0.1:{port}"
        log(f"CloakBrowser CDP 就绪:{cdp_url}")
        asyncio.run(
            run_agent(a.task, a.start_url, a.out, a.max_steps, cdp_url, a.model)
        )
    finally:
        if cloak is not None:
            try:
                c = getattr(cloak, "close", None)
                if c:
                    c()
            except Exception:
                pass


if __name__ == "__main__":
    main()
