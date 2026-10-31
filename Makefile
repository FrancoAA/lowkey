.PHONY: run dev test test-all e2e bench clean

run:            ## start the app (same as ./run)
	./run

dev:            ## hot-reload backend (:8000) + Vite dev server (:5173)
	@trap 'kill 0' INT TERM EXIT; \
	uv run uvicorn --factory openwhisper.server:create_app --reload --host 127.0.0.1 --port 8000 & \
	(cd frontend && ([ -d node_modules ] || npm ci) && npm run dev)

test:           ## fast backend tests + frontend typecheck/build
	uv run pytest -q -m "not slow"
	cd frontend && ([ -d node_modules ] || npm ci) && npm run build

test-all: test  ## + real-model test (downloads the default model once)
	uv run pytest -q -m slow

e2e:            ## headless-browser UI check with fake microphone; writes docs/screenshots/
	uv run python scripts/e2e_ui.py

bench:          ## benchmark candidate models on this machine (see docs/benchmark.md)
	uv run --group bench python bench/make_samples.py
	uv run --group bench python bench/benchmark.py

clean:
	rm -rf frontend/dist frontend/node_modules .venv .pytest_cache
