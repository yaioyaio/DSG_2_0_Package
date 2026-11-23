#!/usr/bin/env sh
set -eu
cd "$(dirname "$0")/../poc"
python3 -m unittest discover -s tests -v
python3 -m dsg doctor --root .
python3 -m dsg scan --root .
