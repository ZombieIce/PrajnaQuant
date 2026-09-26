#!/bin/sh
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ -n "${CARGO_BIN:-}" ]; then
  CARGO_CMD=$CARGO_BIN
elif [ -x "${HOME:?}/.cargo/bin/cargo" ]; then
  CARGO_CMD="$HOME/.cargo/bin/cargo"
else
  CARGO_CMD=$(command -v cargo || true)
fi
if [ -z "$CARGO_CMD" ] || [ ! -x "$CARGO_CMD" ]; then
  echo "cargo executable not found; set CARGO_BIN or install Rust under ~/.cargo/bin" >&2
  exit 127
fi
SYMBOL_FILE=${DAILY_SYNC_SYMBOL_FILE:-"$PROJECT_DIR/configs/daily-sync.symbols"}
if [ ! -r "$SYMBOL_FILE" ]; then
  echo "missing symbol configuration: $SYMBOL_FILE (copy configs/daily-sync.symbols.example and edit it)" >&2
  exit 2
fi
set --
while IFS= read -r symbol || [ -n "$symbol" ]; do
  case "$symbol" in ''|'#'*) continue ;; esac
  case "$symbol" in sh[0-9][0-9][0-9][0-9][0-9][0-9]|sz[0-9][0-9][0-9][0-9][0-9][0-9]) ;; *) echo "invalid symbol: $symbol" >&2; exit 2 ;; esac
  set -- "$@" --symbol "$symbol"
done < "$SYMBOL_FILE"
if [ "$#" -eq 0 ]; then
  echo "symbol configuration is empty" >&2
  exit 2
fi
cd "$PROJECT_DIR"
exec "$CARGO_CMD" run --release --locked -- --data-dir data-core sync-daily-latest "$@"
