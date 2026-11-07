#!/usr/bin/env bash
set -euo pipefail

# Preserve the original amd64 entry point.
exec "$(dirname "${BASH_SOURCE[0]}")/build-rust-windows.sh" amd64 "$@"
