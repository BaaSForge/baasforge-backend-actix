#!/usr/bin/env bash
# Stop local dev processes: API ports (see dev-scaled.sh / dev.sh), chat worker binary, cargo watch.
set -u

PORTS=(1101 1102 1103 1104)

echo "==> Stopping listeners on ports ${PORTS[*]}..."
for port in "${PORTS[@]}"; do
  # macOS lsof: PIDs with IPv4/IPv6 TCP listen on this port
  pids=$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true)
  if [[ -n "${pids}" ]]; then
    echo "    port $port: PIDs ${pids//$'\n'/ }"
    kill -TERM ${pids} 2>/dev/null || true
  fi
done
sleep 2
for port in "${PORTS[@]}"; do
  pids=$(lsof -nP -iTCP:"$port" -sTCP:LISTEN -t 2>/dev/null || true)
  if [[ -n "${pids}" ]]; then
    echo "    port $port (force): PIDs ${pids//$'\n'/ }"
    kill -KILL ${pids} 2>/dev/null || true
  fi
done

# Binary name from Cargo.toml: [[bin]] name = "chat_worker" (path src/bin/chat_worker.rs)
echo "==> Stopping chat_worker processes..."
pkill -x chat_worker 2>/dev/null && echo "    pkill -x chat_worker" || true
pkill -f 'run --bin chat_worker' 2>/dev/null && echo "    pkill -f 'run --bin chat_worker'" || true

echo "==> Stopping cargo watch..."
# dev.sh / dev-chat-worker.sh use `cargo watch` (installed binary is often `cargo-watch`)
pkill -f "cargo watch" 2>/dev/null && echo "    pkill -f \"cargo watch\"" || true
pkill -x cargo-watch 2>/dev/null && echo "    pkill -x cargo-watch" || true

echo "Done."
