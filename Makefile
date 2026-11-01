.PHONY: run dev test test-all e2e e2e-hotkey bench clean

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

e2e-hotkey:     ## global dictation key in a nested Hyprland; writes docs/screenshots/06-07
	uv run python scripts/e2e_hotkey.py

bench:          ## benchmark candidate models on this machine (see docs/benchmark.md)
	uv run --group bench python bench/make_samples.py
	uv run --group bench python bench/benchmark.py

clean:
	rm -rf frontend/dist frontend/node_modules .venv .pytest_cache
