#!/usr/bin/env bash
# Download (once) and start a local open-weights model server for the analyst.
#
#   scripts/llm.sh install    download llama.cpp only and print the path to llama-server
#                             (the Rust binary downloads and swaps the models itself)
#   scripts/llm.sh            install if needed, start one model on port 8080, wait until ready
#   scripts/llm.sh stop       stop that server
#
# The model is Qwen3-4B-Instruct-2507 (Apache-2.0) quantized to 4 bits, about 2.5 GB.
# The runtime is llama.cpp. Nothing here needs an account or an API key.
# Override with LLM_DIR, LLM_PORT, LLAMA_CPP_BUILD, MODEL_URL, MODEL_FILE.
set -euo pipefail

DIR="${LLM_DIR:-$HOME/.cache/paper-trader-llm}"
PORT="${LLM_PORT:-8080}"
BUILD="${LLAMA_CPP_BUILD:-b12211}"
MODEL_FILE="${MODEL_FILE:-qwen3-4b-instruct-2507-q4_k_m.gguf}"
MODEL_URL="${MODEL_URL:-https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf}"
PID_FILE="$DIR/server.pid"

if [ "${1:-}" = "install" ]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) ASSET="ubuntu-x64" ;;
    Linux-aarch64) ASSET="ubuntu-arm64" ;;
    Darwin-arm64) ASSET="macos-arm64" ;;
    Darwin-x86_64) ASSET="macos-x64" ;;
    *) echo "unsupported platform" >&2; exit 1 ;;
  esac
  mkdir -p "$DIR"
  if [ ! -x "$DIR/llama-$BUILD/llama-server" ]; then
    curl -fsSL --retry 5 "https://github.com/ggml-org/llama.cpp/releases/download/$BUILD/llama-$BUILD-bin-$ASSET.tar.gz" | tar -xz -C "$DIR"
  fi
  echo "$DIR/llama-$BUILD/llama-server"
  exit 0
fi

if [ "${1:-}" = "stop" ]; then
  [ -f "$PID_FILE" ] && kill "$(cat "$PID_FILE")" 2>/dev/null || true
  rm -f "$PID_FILE"
  exit 0
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ASSET="ubuntu-x64" ;;
  Linux-aarch64) ASSET="ubuntu-arm64" ;;
  Darwin-arm64) ASSET="macos-arm64" ;;
  Darwin-x86_64) ASSET="macos-x64" ;;
  *) echo "unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

mkdir -p "$DIR"
SERVER="$DIR/llama-$BUILD/llama-server"
if [ ! -x "$SERVER" ]; then
  echo "downloading llama.cpp $BUILD ($ASSET)"
  curl -fsSL "https://github.com/ggml-org/llama.cpp/releases/download/$BUILD/llama-$BUILD-bin-$ASSET.tar.gz" | tar -xz -C "$DIR"
fi
if [ ! -s "$DIR/$MODEL_FILE" ]; then
  echo "downloading model"
  curl -fL --retry 5 --retry-delay 5 -o "$DIR/$MODEL_FILE.part" "$MODEL_URL"
  mv "$DIR/$MODEL_FILE.part" "$DIR/$MODEL_FILE"
fi

if curl -fs "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
  echo "model server already running on port $PORT"
  exit 0
fi

THREADS="$( (nproc 2>/dev/null || sysctl -n hw.ncpu) )"
nohup "$SERVER" -m "$DIR/$MODEL_FILE" -c 16384 -np 1 --host 127.0.0.1 --port "$PORT" -t "$THREADS" >"$DIR/server.log" 2>&1 &
echo $! >"$PID_FILE"

for _ in $(seq 1 120); do
  if curl -fs "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    echo "model server ready on port $PORT"
    exit 0
  fi
  sleep 2
done
echo "model server did not become ready; log follows" >&2
tail -n 40 "$DIR/server.log" >&2
exit 1
