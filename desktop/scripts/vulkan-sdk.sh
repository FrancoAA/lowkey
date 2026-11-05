#!/bin/sh
# Prepare what whisper.cpp's Vulkan build needs, without installing system packages.
# Result: desktop/.deps/vulkan-sdk/{include,lib,bin,share} — used via VULKAN_SDK in
# src-tauri/.cargo/config.toml. If your system has vulkan-headers and spirv-headers
# installed, the system headers are used instead.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
deps="$here/.deps"
sdk="$deps/vulkan-sdk"
mkdir -p "$deps" "$sdk/lib" "$sdk/bin"

for tool in glslc; do
  command -v "$tool" >/dev/null || { echo "needs $tool (shaderc)"; exit 1; }
done
lib=$(ldconfig -p 2>/dev/null | awk '/libvulkan\.so(\.1)? /{print $NF; exit}')
[ -n "$lib" ] || lib=/usr/lib/libvulkan.so.1
[ -e "$lib" ] || { echo "needs the Vulkan loader (libvulkan.so.1)"; exit 1; }
ln -sf "$lib" "$sdk/lib/libvulkan.so"
ln -sf "$(command -v glslc)" "$sdk/bin/glslc"

if [ -f /usr/include/vulkan/vulkan.h ] && [ -f /usr/include/spirv/unified1/spirv.hpp ]; then
  ln -sfn /usr/include "$sdk/include"
else
  [ -d "$deps/Vulkan-Headers" ] || git clone -q --depth 1 https://github.com/KhronosGroup/Vulkan-Headers.git "$deps/Vulkan-Headers"
  [ -d "$deps/SPIRV-Headers" ] || git clone -q --depth 1 https://github.com/KhronosGroup/SPIRV-Headers.git "$deps/SPIRV-Headers"
  cmake -S "$deps/SPIRV-Headers" -B "$deps/SPIRV-Headers/build" -DCMAKE_INSTALL_PREFIX="$sdk" -DSPIRV_HEADERS_ENABLE_TESTS=OFF >/dev/null
  cmake --install "$deps/SPIRV-Headers/build" >/dev/null
  mkdir -p "$sdk/include"
  cp -r "$deps/Vulkan-Headers/include/vulkan" "$deps/Vulkan-Headers/include/vk_video" "$sdk/include/" 2>/dev/null || true
fi
echo "Vulkan build SDK ready: $sdk"
