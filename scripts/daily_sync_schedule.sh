#!/bin/sh
set -eu
PROJECT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
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
exec cargo run --release --locked -- --data-dir data-core sync-daily-latest "$@"
