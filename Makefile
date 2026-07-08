# Single source of truth for the release version.
# Edit VERSION, run `make version`, commit Cargo.toml + charts together.
VERSION := 0.1.0

CHARTS := $(wildcard charts/sycophant-*/Chart.yaml)

.PHONY: version
version: ## Stamp VERSION into Cargo.toml and every chart appVersion
	@perl -0pi -e 's/(\[workspace\.package\][^\[]*?\nversion = ")[^"]*(")/$${1}$(VERSION)$${2}/' Cargo.toml
	@for c in $(CHARTS); do \
		perl -i -pe 's/^appVersion:.*/appVersion: "$(VERSION)"/' $$c; \
	done
	@echo "stamped $(VERSION) -> Cargo.toml + $(words $(CHARTS)) charts"
