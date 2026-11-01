.PHONY: desktop desktop-dev desktop-bundle test test-desktop e2e-desktop install-desktop bench clean

CARGO ?= $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)
TAURI = $(CURDIR)/frontend/node_modules/.bin/tauri
DESKTOP_BIN = desktop/src-tauri/target/release/open-whisper

desktop:        ## build the desktop app -> desktop/src-tauri/target/release/open-whisper
	cd frontend && ([ -d node_modules ] || npm ci) && npm run build
	./desktop/scripts/vulkan-sdk.sh
	./desktop/scripts/sherpa-onnx.sh
	cd desktop/src-tauri && $(CARGO) build --release --features custom-protocol

desktop-dev:    ## desktop app with hot reload (Vite + tauri dev)
	cd frontend && ([ -d node_modules ] || npm ci)
	./desktop/scripts/vulkan-sdk.sh
	./desktop/scripts/sherpa-onnx.sh
	cd desktop && PATH="$(dir $(CARGO)):$$PATH" $(TAURI) dev

desktop-bundle: ## AppImage, .deb and .rpm in desktop/src-tauri/target/release/bundle/
	cd frontend && ([ -d node_modules ] || npm ci)
	./desktop/scripts/vulkan-sdk.sh
	./desktop/scripts/sherpa-onnx.sh
	cd desktop && PATH="$(dir $(CARGO)):$$PATH" $(TAURI) build

test:           ## UI typecheck + build, Rust unit tests, key script syntax
	sh -n scripts/ow-dictate
	./desktop/scripts/sherpa-onnx.sh
	cd frontend && ([ -d node_modules ] || npm ci) && npm run build
	cd desktop/src-tauri && $(CARGO) test --lib

test-desktop: test

e2e-desktop: desktop  ## desktop app in a nested Hyprland (your session is not touched); screenshots 09-18
	python3 scripts/e2e_desktop.py

install-desktop: desktop  ## the app in ~/.local/bin, plus its launcher entry and icon (the key then uses it)
	install -Dm755 $(DESKTOP_BIN) $(HOME)/.local/bin/open-whisper
	install -Dm644 desktop/src-tauri/icons/128x128.png $(HOME)/.local/share/icons/hicolor/128x128/apps/open-whisper.png
	mkdir -p $(HOME)/.local/share/applications
	sed 's|@BIN@|$(HOME)/.local/bin/open-whisper|' desktop/open-whisper.desktop > $(HOME)/.local/share/applications/open-whisper.desktop

bench:          ## research: benchmark models on this machine (docs/benchmark.md; Python via uv, not used by the app)
	HF_HUB_DISABLE_XET=1 uv run bench/make_samples.py
	HF_HUB_DISABLE_XET=1 uv run bench/benchmark.py

clean:
	rm -rf frontend/dist frontend/node_modules
