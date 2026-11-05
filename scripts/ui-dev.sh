#!/usr/bin/env bash
# 作者: kejiqing
set -euo pipefail
cd "$(dirname "$0")/../ui"
exec npm run dev
