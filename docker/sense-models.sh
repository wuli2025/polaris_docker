#!/bin/sh
# 寓言计划 · 感官包预下载(Docker full 镜像专用)
#
# 用法(polaris_docker 仓 Dockerfile,渲染层之后):
#   ARG POLARIS_SENSE_MODELS=1
#   COPY docker/sense-models.sh /tmp/sense-models.sh
#   RUN [ "$POLARIS_SENSE_MODELS" = "1" ] && sh /tmp/sense-models.sh || true
#
# 设计约定(与 src-tauri/src/sense.rs 的 SENSE_PACKS 保持同一份清单):
# - 模型落 ~/Polaris/models/<pack_id>/,应用启动后按「文件存在且非空」识别为已安装,
#   设置页直接显示「已就位」,零额外配置 —— Win/Mac 不分发模型,Docker 预置最优本地模型。
# - 下载源 hf-mirror 优先(国内构建机直连),失败回退 huggingface 官源。
# - 体积合计约 470MB;slim 镜像不要跑本脚本。
set -eu

MODELS="${POLARIS_HOME:-$HOME}/Polaris/models"

fetch() {
  # fetch <目标文件> <镜像url> <官源url>
  dst="$1"; mirror="$2"; origin="$3"
  if [ -s "$dst" ]; then
    echo "[sense-models] 已存在,跳过: $dst"
    return 0
  fi
  mkdir -p "$(dirname "$dst")"
  echo "[sense-models] 下载 $dst"
  curl -fL --retry 3 --connect-timeout 20 -o "$dst.part" "$mirror" \
    || curl -fL --retry 3 --connect-timeout 20 -o "$dst.part" "$origin"
  mv "$dst.part" "$dst"
}

# 听 · 速览:SenseVoice-Small int8(sherpa-onnx 格式,约 239MB)
SV_REPO="csukuangfj/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17"
fetch "$MODELS/sensevoice-small/model.int8.onnx" \
  "https://hf-mirror.com/$SV_REPO/resolve/main/model.int8.onnx" \
  "https://huggingface.co/$SV_REPO/resolve/main/model.int8.onnx"
fetch "$MODELS/sensevoice-small/tokens.txt" \
  "https://hf-mirror.com/$SV_REPO/resolve/main/tokens.txt" \
  "https://huggingface.co/$SV_REPO/resolve/main/tokens.txt"

# 听 · 深读:Paraformer-zh int8(字级时间戳,sherpa-onnx 格式,约 232MB)
PF_REPO="csukuangfj/sherpa-onnx-paraformer-zh-2023-09-14"
fetch "$MODELS/paraformer-zh/model.int8.onnx" \
  "https://hf-mirror.com/$PF_REPO/resolve/main/model.int8.onnx" \
  "https://huggingface.co/$PF_REPO/resolve/main/model.int8.onnx"
fetch "$MODELS/paraformer-zh/tokens.txt" \
  "https://hf-mirror.com/$PF_REPO/resolve/main/tokens.txt" \
  "https://huggingface.co/$PF_REPO/resolve/main/tokens.txt"

echo "[sense-models] 完成。$(du -sh "$MODELS" 2>/dev/null || true)"
