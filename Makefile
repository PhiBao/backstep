.PHONY: backend web test build benchmark

backend:
	cd backend && cargo run

web:
	cd web && pnpm dev

test:
	cd backend && cargo test

build:
	cd backend && cargo build --release
	cd web && pnpm build

benchmark:
	python3 scripts/benchmark.py
