PROJECT := loopmux
VERSION ?=
REPO ?=
TAP_REPO ?=
NO_BREW ?=0

.PHONY: help release

help: ## Show available targets
	@printf "$(PROJECT) release tooling\n\n"
	@printf "Targets:\n"
	@grep -E '^[a-zA-Z_-]+:.*## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*## "}; {printf "  %-16s %s\n", $$1, $$2}'

release: ## Run full release flow (VERSION=x.y.z)
	@test -n "$(VERSION)" || (printf "error: pass VERSION=x.y.z\n" >&2; exit 1)
	@set -e; \
	args=""; \
	if [ -n "$(REPO)" ]; then args="$$args --repo $(REPO)"; fi; \
	if [ -n "$(TAP_REPO)" ]; then args="$$args --tap-repo $(TAP_REPO)"; fi; \
	if [ "$(NO_BREW)" = "1" ]; then args="$$args --no-brew"; fi; \
	./release/ship.sh "$(VERSION)" $$args
