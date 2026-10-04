SHELL := /usr/bin/env bash

.PHONY: build check run test

build:
	./tmux-overview.sh --build-only

test: build
	bun test tests/e2e.test.ts

check:
	bash -n tmux-overview.sh
	$(MAKE) build
	bun test tests/e2e.test.ts

run:
	./tmux-overview.sh
