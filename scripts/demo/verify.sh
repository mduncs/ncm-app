#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
DEMO_PYTHON="${NCM_DEMO_PYTHON:-target/realuse/venv/bin/python}"
if [[ ! -x "$DEMO_PYTHON" ]]; then DEMO_PYTHON=python3; fi
exec "$DEMO_PYTHON" -B scripts/demo/verify.py
