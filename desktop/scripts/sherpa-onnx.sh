#!/bin/sh
# Fetch the sherpa-onnx static libraries (with onnxruntime) for the Parakeet engine, checked with
# SHA-256 (the value GitHub publishes for the release asset). The sherpa-onnx-sys crate takes the
# archive from desktop/.deps/sherpa-onnx (SHERPA_ONNX_ARCHIVE_DIR in src-tauri/.cargo/config.toml)
# instead of downloading it unchecked during the build.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
dir="$here/.deps/sherpa-onnx"
version=2.0.1
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    name="sherpa-onnx-v$version-linux-x64-static-lib.tar.bz2"
    sha=55cfadf65ba50d0afc1a802b7a337942819e51c94a5b5a384def1978f7c70871 ;;
  *)
    echo "sherpa-onnx: no pinned archive for $(uname -s)-$(uname -m) yet; add one to $0"; exit 1 ;;
esac
mkdir -p "$dir"
sum() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1 || shasum -a 256 "$1" | cut -d' ' -f1; }
if [ ! -f "$dir/$name" ] || [ "$(sum "$dir/$name")" != "$sha" ]; then
  curl -fsSL -o "$dir/$name.part" "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$version/$name"
  got=$(sum "$dir/$name.part")
  if [ "$got" != "$sha" ]; then
    rm -f "$dir/$name.part"
    echo "sherpa-onnx: $name has an unexpected checksum ($got)"; exit 1
  fi
  mv "$dir/$name.part" "$dir/$name"
fi
echo "sherpa-onnx libraries ready: $dir/$name"
