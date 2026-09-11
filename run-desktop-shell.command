#!/bin/bash
set -e
cd "$(dirname "$0")"
echo "== checking toolchain =="
echo -n "rustc: "; command -v rustc && rustc --version || echo "MISSING"
echo -n "cargo: "; command -v cargo && cargo --version || echo "MISSING"
echo -n "node: "; command -v node && node --version || echo "MISSING"
echo -n "npm: "; command -v npm && npm --version || echo "MISSING"
echo -n "xcode CLT: "; xcode-select -p 2>/dev/null || echo "MISSING (run: xcode-select --install)"
echo ""
if ! command -v rustc >/dev/null || ! command -v npm >/dev/null; then
  echo "Missing a required tool above - install it, then double-click this file again."
  echo "Press Enter to close..."
  read
  exit 1
fi
echo "== installing root + frontend npm deps (first run only, may take a minute) =="
npm install
npm install --prefix frontend
echo ""
echo "== launching the desktop shell: npx tauri dev =="
echo "(this window will show live logs; the app window should open shortly)"
npx tauri dev
echo "tauri dev exited. Press Enter to close..."
read
