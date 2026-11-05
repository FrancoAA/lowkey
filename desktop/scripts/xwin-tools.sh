#!/bin/sh
# Prepare a Windows x64 build from Linux (make desktop-windows), without system packages where
# possible. cargo-xwin downloads the Microsoft C runtime and Windows SDK headers and libraries
# once (~1.2 GB in ~/.cache/cargo-xwin; it accepts Microsoft's license for you).
# whisper.cpp's CMake build also needs ninja and the lld-link linker: the system ones when they
# are installed, else ninja from PyPI and Rust's own rust-lld, in desktop/.deps/xwin-tools/bin.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bin="$here/.deps/xwin-tools/bin"
mkdir -p "$bin"
target=x86_64-pc-windows-msvc

command -v cargo-xwin >/dev/null || { echo "needs cargo-xwin: cargo install --locked cargo-xwin"; exit 1; }
for tool in clang-cl llvm-lib llvm-rc; do
  command -v "$tool" >/dev/null || { echo "needs $tool (the clang and llvm packages)"; exit 1; }
done
if command -v rustup >/dev/null && ! rustup target list --installed | grep -qx "$target"; then
  rustup target add "$target"
fi

if ! command -v ninja >/dev/null && [ ! -x "$bin/ninja" ]; then
  python3 -m venv "$here/.deps/xwin-tools/venv"
  "$here/.deps/xwin-tools/venv/bin/pip" install -q ninja
  ln -sf "$here/.deps/xwin-tools/venv/bin/ninja" "$bin/ninja"
fi
if ! command -v lld-link >/dev/null && [ ! -e "$bin/lld-link" ]; then
  # rust-lld acts as the MSVC linker when it is called lld-link.
  lld="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/rust-lld"
  [ -x "$lld" ] || { echo "needs lld-link (the lld package) or Rust's rust-lld"; exit 1; }
  ln -sf "$lld" "$bin/lld-link"
fi
