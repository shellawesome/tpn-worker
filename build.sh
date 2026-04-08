#!/usr/bin/env bash
set -euo pipefail

cargo build --release

echo "Binary: target/release/tpn-worker"
ls -lh target/release/tpn-worker

cp -f -v target/release/tpn-worker ./
