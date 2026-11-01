.PHONY: run dev test test-all e2e e2e-hotkey bench clean desktop desktop-dev desktop-bundle test-desktop e2e-desktop install-desktop

run:            ## start the app (same as ./run)
	./run

dev:            ## hot-reload backend (:8000) + Vite dev server (:5173)
	@uv run uvicorn --factory openwhisper.server:create_app --reload --host 127.0.0.1 --port 8000 & api=$$!; \
	trap 'kill $$api 2>/dev/null' EXIT; \
	cd frontend && ([ -d node_modules ] || npm ci) && npm run dev

test:           ## frontend typecheck/build + fast backend tests (build first: one test checks static serving)
	cd frontend && ([ -d node_modules ] || npm ci) && npm run build
	uv run pytest -q -m "not slow"

test-all: test  ## + real-model test (downloads the default model once)
	uv run pytest -q -m slow

e2e:            ## headless-browser UI check with fake microphone; writes docs/screenshots/
	uv run python scripts/e2e_ui.py

e2e-hotkey:     ## global dictation key in a nested Hyprland; writes docs/screenshots/06-08
	uv run python scripts/e2e_hotkey.py

bench:          ## benchmark candidate models on this machine (see docs/benchmark.md)
	HF_HUB_DISABLE_XET=1 uv run --group bench python bench/make_samples.py
	HF_HUB_DISABLE_XET=1 uv run --group bench python bench/benchmark.py

# ---------------------------------------------------------------- desktop app (Tauri)
CARGO ?= $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)
TAURI = $(CURDIR)/frontend/node_modules/.bin/tauri
DESKTOP_BIN = desktop/src-tauri/target/release/open-whisper

desktop:        ## build the desktop app -> desktop/src-tauri/target/release/open-whisper
	cd frontend && ([ -d node_modules ] || npm ci) && npm run build
	./desktop/scripts/vulkan-sdk.sh
	cd desktop/src-tauri && $(CARGO) build --release --features custom-protocol

desktop-dev:    ## desktop app with hot reload (Vite + tauri dev)
	cd frontend && ([ -d node_modules ] || npm ci)
	./desktop/scripts/vulkan-sdk.sh
	cd desktop && PATH="$(dir $(CARGO)):$$PATH" $(TAURI) dev

desktop-bundle: ## AppImage, .deb and .rpm in desktop/src-tauri/target/release/bundle/
	cd frontend && ([ -d node_modules ] || npm ci)
	./desktop/scripts/vulkan-sdk.sh
	cd desktop && PATH="$(dir $(CARGO)):$$PATH" $(TAURI) build

test-desktop:   ## Rust unit tests
	cd desktop/src-tauri && $(CARGO) test --lib

e2e-desktop: desktop  ## desktop app in a nested Hyprland (your session is not touched); screenshots 09-16
	uv run python scripts/e2e_desktop.py

install-desktop: desktop  ## copy the app to ~/.local/bin/open-whisper (the key then uses the desktop app)
	install -Dm755 $(DESKTOP_BIN) $(HOME)/.local/bin/open-whisper

clean:
	rm -rf frontend/dist frontend/node_modules .venv .pytest_cache
