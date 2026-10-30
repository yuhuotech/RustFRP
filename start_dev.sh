#!/usr/bin/env bash
# 本地开发面板：构建后在后台启动，数据沿用上一轮的独立预览数据库。
set -euo pipefail
umask 077

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
STATE_DIR="$ROOT/target/dev-panel"
PID_FILE="$STATE_DIR/server.pid"
LOG_FILE="$STATE_DIR/server.log"
SERVER="$ROOT/target/release/rustfrp-server"
HTTP_PORT=9674
EDGE_PORT="${DEV_EDGE_PORT:-17000}"
NODE_PORT="${DEV_NODE_PORT:-17001}"
DB="${DEV_DB:-$ROOT/target/panel-preview.db}"
CONFIG="${DEV_CONFIG:-$ROOT/target/dev-panel/server.toml}"
URL="http://localhost:$HTTP_PORT"

die() { printf '错误：%s\n' "$*" >&2; exit 1; }
usage() {
  printf '%s\n' \
    '用法：./start_dev.sh [--skip-build | status | stop | --help]' \
    '  默认：构建 Web 和 Controller，后台启动服务。' \
    '  --skip-build：使用现有构建，快速启动。' \
    '  status / stop：查看状态 / 停止此脚本启动的服务。' \
    '  可选环境变量：DEV_EDGE_PORT、DEV_NODE_PORT、DEV_DB。Web 固定使用 9674。' \
    '  管理 token：修改 target/dev-panel/server.toml 的 [admin].token 后重启。'
}

ACTION="${1:-start}"
[[ $# -le 1 ]] || die '只接受一个参数；用 --help 查看用法。'
case "$ACTION" in
  start|--skip-build|status|stop) ;;
  --help|-h) usage; exit 0 ;;
  *) usage; exit 1 ;;
esac

if [[ -n "${DEV_HTTP_PORT:-}" && "$DEV_HTTP_PORT" != 9674 ]]; then
  die 'Web 管理面板端口已固定为 9674，不支持改为其他端口。'
fi
command -v lsof >/dev/null || die '缺少工具：lsof'

# PID 可被系统复用，因此停止前必须核对完整命令。
running() {
  [[ -f "$PID_FILE" ]] || return 1
  IFS= read -r SERVER_PID < "$PID_FILE" || return 1
  [[ "$SERVER_PID" =~ ^[0-9]+$ ]] || return 1
  [[ "$(ps -p "$SERVER_PID" -o args= 2>/dev/null)" == *"rustfrp-server run"* ]]
}

stop_pid() {
  local pid="$1"
  local label="${2:-服务}"
  kill -INT "$pid" 2>/dev/null || true
  for ((i = 0; i < 50; i++)); do
    if ! kill -0 "$pid" 2>/dev/null; then
      return 0
    fi
    sleep 0.1
  done
  printf '提示：%s未响应退出信号，正在结束进程（PID %s）。\n' "$label" "$pid" >&2
  kill -TERM "$pid" 2>/dev/null || true
  for ((i = 0; i < 20; i++)); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 0.1
  done
  die "$label仍未退出（PID $pid）。"
}

http_owner_pids() {
  lsof -t -nP -iTCP:"$HTTP_PORT" -sTCP:LISTEN 2>/dev/null | sort -u || true
}

stop_http_owner() {
  local pid cmd
  while IFS= read -r pid; do
    [[ -n "$pid" ]] || continue
    cmd="$(ps -p "$pid" -o args= 2>/dev/null || true)"
    if [[ "$cmd" == *"rustfrp-server"* && "$cmd" == *" run"* ]]; then
      printf '发现旧的 RustFRP 服务（PID %s），正在回收并重启…\n' "$pid"
      stop_pid "$pid" '旧 RustFRP 服务'
    else
      die "端口 9674 被非 RustFRP 进程占用（PID $pid：$cmd），为避免误杀不会强制结束它。"
    fi
  done < <(http_owner_pids)
}

if [[ "$ACTION" == stop ]]; then
  stop_http_owner
  if running; then
    stop_pid "$SERVER_PID" '本地开发服务'
  fi
  rm -f "$PID_FILE"
  printf '此脚本启动的服务未运行。\n'
  exit 0
fi
if [[ "$ACTION" == status ]]; then
  if running; then
    printf '本地开发服务运行中（PID %s）。\n日志：%s\n' "$SERVER_PID" "$LOG_FILE"
  elif [[ -n "$(http_owner_pids)" ]]; then
    printf '9674 上已有 RustFRP 服务运行，但不是此脚本记录的 PID。\n'
  else
    printf '此脚本启动的服务未运行。\n'
  fi
  exit 0
fi

for port in "$HTTP_PORT" "$EDGE_PORT" "$NODE_PORT"; do
  [[ "$port" =~ ^[0-9]+$ && ${#port} -le 5 ]] || die "无效端口：$port"
  ((10#$port >= 1 && 10#$port <= 65535)) || die "端口超出范围：$port"
done
[[ "$HTTP_PORT" != "$EDGE_PORT" ]] || die 'Web 和 Edge 不能使用相同端口。'
[[ "$HTTP_PORT" != "$NODE_PORT" ]] || die 'Web 和节点接入不能使用相同端口。'
[[ "$EDGE_PORT" != "$NODE_PORT" ]] || die 'Edge 和节点接入不能使用相同端口。'
command -v curl >/dev/null || die '缺少工具：curl'

if [[ "$ACTION" != --skip-build ]]; then
  command -v node >/dev/null || die '找不到 Node；请安装开发工具链。'
  command -v npm >/dev/null || die '找不到 npm；请安装开发工具链。'
  export PATH="$HOME/.cargo/bin:$PATH"
  command -v cargo >/dev/null || die '找不到 cargo；请安装 Rust 工具链。'
  printf '[1/3] 构建 Web 面板…\n'
  (cd rustfrp-ui && if [[ ! -d node_modules ]]; then npm ci; fi && npm run check && npm run build)
  printf '[2/3] 构建 Controller…\n'
  cargo build --release --target-dir "$ROOT/target" -p rustfrp-server --features embedded-panel
fi
[[ -x "$SERVER" && -f "$ROOT/rustfrp-ui/dist/index.html" ]] || \
  die '缺少构建产物；请先运行 ./start_dev.sh（不带 --skip-build）。'
mkdir -p "$STATE_DIR" "$(dirname -- "$DB")"
if [[ ! -f "$CONFIG" ]]; then
  mkdir -p "$(dirname -- "$CONFIG")"
  cp rustfrp-server.example.toml "$CONFIG"
  die "已创建 ${CONFIG}，请修改 [admin].token（不要保留 change-me）后重新启动。"
fi

# 只传入本地开发配置，避免继承终端中的生产环境 RUSTFRP_* 设置。
RUNNER=(env -i "PATH=$PATH" "HOME=$HOME" "TMPDIR=${TMPDIR:-/tmp}" \
  "RUSTFRP_CONFIG=$CONFIG" \
  "RUSTFRP_DB=$DB" "RUSTFRP_HTTP=127.0.0.1:$HTTP_PORT" \
  "RUSTFRP_EDGE=127.0.0.1:$EDGE_PORT" "RUSTFRP_NODE_ADDR=127.0.0.1:$NODE_PORT" \
  'RUSTFRP_PROXY_BIND=127.0.0.1' \
  'RUSTFRP_PUBLIC_HOST=127.0.0.1' "RUSTFRP_PANEL_DIR=$ROOT/rustfrp-ui/dist")

# 构建和配置文件检查完成后再回收旧服务。
stop_http_owner
if lsof -nP -iTCP:"$EDGE_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  die "Edge 端口 $EDGE_PORT 已被其他服务占用；可用 DEV_EDGE_PORT 指定其他端口。"
fi
if lsof -nP -iTCP:"$NODE_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  die "节点接入端口 $NODE_PORT 已被其他服务占用；可用 DEV_NODE_PORT 指定其他端口。"
fi

printf '[3/3] 启动本地服务…\n'
nohup "${RUNNER[@]}" "$SERVER" run > "$LOG_FILE" 2>&1 < /dev/null &
SERVER_PID=$!
printf '%s\n' "$SERVER_PID" > "$PID_FILE"
for ((i = 0; i < 100; i++)); do
  if ! running; then
    rm -f "$PID_FILE"
    die "服务启动失败，请查看 $LOG_FILE"
  fi
  if curl --fail --silent --max-time 1 "$URL/" > /dev/null; then
    printf '\n已启动：%s\n管理 token 配置：%s（[admin].token）\n独立节点接入端口：127.0.0.1:%s\n数据库：%s\n日志：%s\n停止：./start_dev.sh stop\n' \
      "$URL" "$CONFIG" "$NODE_PORT" "$DB" "$LOG_FILE"
    exit 0
  fi
  sleep 0.1
done
die "服务就绪超时；用 ./start_dev.sh status 查看状态，日志：$LOG_FILE"
