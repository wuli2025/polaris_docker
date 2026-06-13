#!/usr/bin/env python3
"""Polaris TurboDownload — cross-platform fast downloader.

aria2c multi-connection segmented download with graceful fallback. Pure stdlib
(no third-party deps), runs the same on Windows / macOS / Linux / busybox NAS.

Strategy:
  1. ensure_aria2(): locate aria2c on PATH / ~/Polaris/bin, else install per-platform.
  2. HEAD-probe the URL for Content-Length + Accept-Ranges.
  3. Decide: file < 10MB or server has no Range support -> single connection;
     otherwise aria2 -x16 -s16 -k1M (the golden multi-connection flags).
  4. Fallback chain: aria2 multi -> aria2 single -> curl -> urllib. Any layer that
     succeeds wins, so "worst case it still downloads".

Usage:
  uv run --no-project fast_download.py <url> [-o NAME] [-d DIR]
  uv run --no-project fast_download.py -i urls.txt -d DIR        # batch
  (on systems without uv, e.g. a NAS: python3 fast_download.py ...)

Env:
  POLARIS_DL_PROXY   proxy used ONLY to fetch the aria2 binary (e.g. when GitHub's
                     CDN is blocked). The actual file download stays DIRECT unless
                     you pass --proxy.
"""
import argparse
import os
import platform
import shutil
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

BIN_DIR = Path.home() / "Polaris" / "bin"
EXE = "aria2c.exe" if os.name == "nt" else "aria2c"
ARIA2_VER = "1.37.0"
SMALL = 10 * 1024 * 1024  # below this, multi-connection isn't worth it


def log(msg):
    print(f"[turbo] {msg}", flush=True)


def _runs(path):
    try:
        subprocess.run([str(path), "--version"], capture_output=True, timeout=15)
        return True
    except Exception:
        return False


def _download(url, dest, proxy=None, timeout=180):
    if proxy:
        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({"http": proxy, "https": proxy})
        )
    else:
        opener = urllib.request.build_opener()
    req = urllib.request.Request(url, headers={"User-Agent": "Polaris-TurboDownload"})
    with opener.open(req, timeout=timeout) as r, open(dest, "wb") as f:
        shutil.copyfileobj(r, f)


def _extract_aria2(zip_path, dest):
    """Pull the aria2c[.exe] member out of a release zip to `dest`."""
    want = "aria2c.exe" if os.name == "nt" else "aria2c"
    with zipfile.ZipFile(zip_path) as zf:
        member = next((n for n in zf.namelist() if n.endswith(want)), None)
        if not member:
            return False
        with zf.open(member) as src, open(dest, "wb") as out:
            shutil.copyfileobj(src, out)
    if os.name != "nt":
        os.chmod(dest, 0o755)
    return True


def ensure_aria2(proxy=None):
    """Return a path to a working aria2c, installing it if needed; else None."""
    # 1. already on PATH
    p = shutil.which("aria2c")
    if p and _runs(p):
        return p
    # 2. previously cached by us
    cached = BIN_DIR / EXE
    if cached.exists() and _runs(cached):
        return str(cached)

    BIN_DIR.mkdir(parents=True, exist_ok=True)
    system = platform.system()
    machine = platform.machine().lower()
    try:
        if system == "Windows":
            # prefer a package manager (lands on PATH, auto-updates)
            for cmd in (
                ["winget", "install", "aria2.aria2", "-e", "--silent",
                 "--accept-package-agreements", "--accept-source-agreements"],
                ["scoop", "install", "aria2"],
                ["choco", "install", "aria2", "-y"],
            ):
                try:
                    subprocess.run(cmd, capture_output=True, timeout=240)
                    p = shutil.which("aria2c")
                    if p and _runs(p):
                        return p
                except Exception:
                    pass
            # fallback: portable official build
            url = (f"https://github.com/aria2/aria2/releases/download/"
                   f"release-{ARIA2_VER}/aria2-{ARIA2_VER}-win-64bit-build1.zip")
            z = BIN_DIR / "aria2_dl.zip"
            _download(url, z, proxy)
            if _extract_aria2(z, cached):
                z.unlink(missing_ok=True)
                if _runs(cached):
                    return str(cached)

        elif system == "Darwin":
            brew = shutil.which("brew")
            if brew:
                subprocess.run([brew, "install", "aria2"], timeout=900)
                p = shutil.which("aria2c")
                if p and _runs(p):
                    return p
            # no brew + no portable static for macOS -> let caller fall back to curl
            log("macOS without Homebrew: no portable aria2; will use curl fallback. "
                "Install once with `brew install aria2` for full multi-connection speed.")

        else:  # Linux (incl. busybox NAS / containers) — abcfy2 musl full-static
            target = {
                "x86_64": "x86_64-linux-musl_static",
                "amd64": "x86_64-linux-musl_static",
                "aarch64": "aarch64-linux-musl_static",
                "arm64": "aarch64-linux-musl_static",
                "armv7l": "armv7-linux-musleabihf_static",
                "armv6l": "arm-linux-musleabi_static",
                "i686": "i686-linux-musl_static",
                "i386": "i686-linux-musl_static",
            }.get(machine, "x86_64-linux-musl_static")
            url = (f"https://github.com/abcfy2/aria2-static-build/releases/download/"
                   f"{ARIA2_VER}/aria2-{target}.zip")
            z = BIN_DIR / "aria2_dl.zip"
            _download(url, z, proxy)
            if _extract_aria2(z, cached):
                z.unlink(missing_ok=True)
                if _runs(cached):
                    return str(cached)
    except Exception as e:
        sys.stderr.write(f"[turbo] aria2 install failed: {e}\n")
    return None


def probe(url, proxy=None):
    """Return (size:int|None, accept_ranges:bool)."""
    if proxy:
        opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({"http": proxy, "https": proxy})
        )
    else:
        opener = urllib.request.build_opener()
    req = urllib.request.Request(
        url, method="HEAD", headers={"User-Agent": "Polaris-TurboDownload"}
    )
    try:
        with opener.open(req, timeout=30) as r:
            size = r.headers.get("Content-Length")
            ar = (r.headers.get("Accept-Ranges", "") or "").lower()
            return (int(size) if size and size.isdigit() else None, "bytes" in ar)
    except Exception:
        return (None, False)


def _aria2_base(aria2, proxy):
    cmd = [aria2, "-c", "--max-tries=5", "--retry-wait=2", "--connect-timeout=30",
           "--timeout=60", "--console-log-level=warn", "--summary-interval=1",
           "--auto-file-renaming=true", "--file-allocation=none"]
    cmd.append(f"--all-proxy={proxy}" if proxy else "--all-proxy=")
    return cmd


def aria2_single_file(aria2, url, out, d, multi, proxy):
    cmd = _aria2_base(aria2, proxy)
    cmd += (["-x16", "-s16", "-k1M"] if multi else ["-x1", "-s1"])
    if d:
        cmd += ["-d", d]
    if out:
        cmd += ["-o", out]
    cmd.append(url)
    return subprocess.run(cmd).returncode


def aria2_batch(aria2, list_path, d, proxy):
    cmd = _aria2_base(aria2, proxy)
    cmd += ["-i", list_path, "-j5", "-x16", "-s16", "-k1M",
            "--optimize-concurrent-downloads=true"]
    if d:
        cmd += ["-d", d]
    return subprocess.run(cmd).returncode


def curl_fallback(url, out, d, proxy):
    curl = shutil.which("curl") or ("curl.exe" if os.name == "nt" else None)
    if not curl:
        return 1
    dest = Path(d or ".") / (out or Path(urllib.parse.urlparse(url).path).name or "download.bin")
    cmd = [curl, "-L", "-C", "-", "-o", str(dest)]
    cmd += (["-x", proxy] if proxy else ["--noproxy", "*"])
    cmd.append(url)
    return subprocess.run(cmd).returncode


def urllib_fallback(url, out, d, proxy):
    dest = Path(d or ".") / (out or Path(urllib.parse.urlparse(url).path).name or "download.bin")
    try:
        _download(url, dest, proxy, timeout=600)
        return 0
    except Exception as e:
        sys.stderr.write(f"[turbo] urllib fallback failed: {e}\n")
        return 1


def human(n):
    for u in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f}{u}"
        n /= 1024
    return f"{n:.1f}TB"


def main():
    ap = argparse.ArgumentParser(description="Polaris TurboDownload")
    ap.add_argument("url", nargs="?", help="file URL (single-file mode)")
    ap.add_argument("-o", "--out", help="output filename")
    ap.add_argument("-d", "--dir", help="output directory")
    ap.add_argument("-i", "--input", help="batch: file with one URL per line")
    ap.add_argument("--proxy", help="proxy for the FILE download (default: direct)")
    args = ap.parse_args()
    if not args.url and not args.input:
        ap.error("give a URL or --input list")

    t0 = time.time()
    aria2 = ensure_aria2(proxy=os.environ.get("POLARIS_DL_PROXY"))
    if aria2:
        log(f"aria2c: {aria2}")
    else:
        log("aria2c unavailable -> using curl/urllib single-connection fallback")

    if args.input:  # batch mode
        if aria2:
            rc = aria2_batch(aria2, args.input, args.dir, args.proxy)
        else:
            rc = 0
            for line in Path(args.input).read_text().splitlines():
                u = line.strip()
                if u and not u.startswith("#"):
                    rc |= urllib_fallback(u, None, args.dir, args.proxy)
        log(f"batch done in {time.time()-t0:.0f}s (rc={rc})")
        sys.exit(rc)

    url = args.url
    size, ranges = probe(url, args.proxy)
    multi = ranges and (size is None or size >= SMALL)
    if size:
        log(f"size={human(size)} accept-ranges={ranges} -> "
            f"{'multi-connection x16' if multi else 'single connection'}")

    rc = 1
    if aria2:
        rc = aria2_single_file(aria2, url, args.out, args.dir, multi, args.proxy)
        if rc != 0 and multi:
            log("multi-connection failed -> retrying single connection")
            rc = aria2_single_file(aria2, url, args.out, args.dir, False, args.proxy)
    if rc != 0:
        log("aria2 failed/absent -> curl fallback")
        rc = curl_fallback(url, args.out, args.dir, args.proxy)
    if rc != 0:
        log("curl failed -> urllib fallback")
        rc = urllib_fallback(url, args.out, args.dir, args.proxy)

    dt = time.time() - t0
    dest = Path(args.dir or ".") / (args.out or Path(urllib.parse.urlparse(url).path).name or "download.bin")
    if rc == 0 and dest.exists():
        b = dest.stat().st_size
        log(f"OK {dest}  {human(b)} in {dt:.0f}s = {human(b/max(dt,0.001))}/s")
    else:
        log(f"FAILED (rc={rc}) after {dt:.0f}s")
    sys.exit(rc)


if __name__ == "__main__":
    main()
