#!/usr/bin/env sh
set -eu
export PYTHONDONTWRITEBYTECODE=1
cd "$(dirname "$0")/../poc"
python3 -m unittest discover -s tests -v
python3 -m dsg doctor --root .
python3 -m dsg scan --root .
