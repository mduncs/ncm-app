#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
HEAVY="${NCM_HEAVY:-env}"
[[ -x "$HEAVY" ]] || HEAVY=env
PYTHON=target/realuse/venv/bin/python
mkdir -p target/realuse
if [[ ! -x "$PYTHON" ]]; then python3 -m venv target/realuse/venv; fi
if ! "$PYTHON" -c 'import pyte' 2>/dev/null; then
  "$HEAVY" "$PYTHON" -m pip install --disable-pip-version-check --no-cache-dir -r scripts/realuse/requirements.txt
fi
"$HEAVY" cargo build -p ncm --quiet
"$HEAVY" "$PYTHON" scripts/realuse/check_terminal.py
exec "$HEAVY" "$PYTHON" scripts/realuse/harness.py "$@"
