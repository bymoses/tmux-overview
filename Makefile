SHELL := /usr/bin/env bash

.PHONY: build check run test

build:
	./tmux-overview.sh --build-only

test: build
	bun test tests

check:
	bash -n tmux-overview.sh
	$(MAKE) build
	bun test tests

run:
	./tmux-overview.sh
