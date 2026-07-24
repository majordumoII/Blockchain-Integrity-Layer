#!/usr/bin/env bash
# Tears down what start.sh started: proof-service and anvil (by PID file).
# Leaves bil-test-postgres running by default, since it's slow to
# reinitialize (schema/publication/slot setup) and holds no secrets — pass
# --all to stop it too.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

DEMO_DIR=.demo
STOP_POSTGRES=false
[[ "${1:-}" == "--all" ]] && STOP_POSTGRES=true

stop_pid_file() {
  local name="$1" pid_file="$2"
  if [[ -f "$pid_file" ]]; then
    local pid
    pid=$(cat "$pid_file")
    if kill -0 "$pid" 2>/dev/null; then
      echo "==> stopping $name (pid $pid)"
      kill "$pid"
      for _ in $(seq 1 20); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.5
      done
      kill -0 "$pid" 2>/dev/null && kill -9 "$pid" 2>/dev/null
    else
      echo "==> $name not running (stale pid file)"
    fi
    rm -f "$pid_file"
  else
    echo "==> $name not running (no pid file)"
  fi
}

stop_pid_file "proof-service" "$DEMO_DIR/proof-service.pid"
stop_pid_file "anvil" "$DEMO_DIR/anvil.pid"

if $STOP_POSTGRES; then
  echo "==> stopping bil-test-postgres"
  docker stop bil-test-postgres >/dev/null 2>&1 || true
else
  echo "==> leaving bil-test-postgres running (pass --all to stop it too)"
fi

echo "Done."
