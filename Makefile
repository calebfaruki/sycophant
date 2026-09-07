# Single source of truth for the release version.
# Edit VERSION, run `make version`, commit Cargo.toml + charts together.
VERSION := 0.1.0

CHARTS := $(wildcard charts/sycophant-*/Chart.yaml)
SWEEP_GATES := $(wildcard scripts/*-sweep-gate.sh)

.DEFAULT_GOAL := help

.PHONY: help
help: ## List every target
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  \033[1m%-22s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

.PHONY: lint
lint: lint-rust lint-k8s ## All static checks

.PHONY: lint-rust
lint-rust: ## rustfmt + clippy, warnings are errors
	cargo fmt --all --check
	cargo clippy --workspace -- -D warnings

# The cluster chart's values schema requires policyEngine; kyverno is the
# rendering the policy tests exercise, so lint the same shape.
.PHONY: lint-k8s
lint-k8s: ## helm lint + zero-match sweep gates
	helm lint charts/sycophant-tenant
	helm lint charts/sycophant-cluster --set policyEngine=kyverno
	@for g in $(SWEEP_GATES); do echo "== $$g =="; bash "$$g" || exit 1; done

.PHONY: test-unit
test-unit: test-unit-rust test-unit-k8s ## All unit tests (hermetic, seconds)

.PHONY: test-unit-rust
test-unit-rust: ## Inline #[cfg(test)] units across the workspace
	cargo test --workspace --lib --bins

# The policy fixture is re-rendered from the chart on every run, so an edit to
# the rules file cannot leave the suite passing against a stale copy.
# `--api-versions` stands in for the Kyverno CRDs the chart's capability check
# looks for; no cluster is contacted.
.PHONY: test-unit-k8s
test-unit-k8s: ## Re-render offline policy fixtures from the chart, then run them
	@helm template c charts/sycophant-cluster -n kyverno \
		--set policyEngine=kyverno \
		--api-versions kyverno.io/v1/ClusterPolicy \
		--show-only templates/capability-job-gate-cpol.yaml \
		> tests/unit/kyverno-policies/capability-job-gate-as-harness/policy.yaml
	@kyverno test tests/unit/kyverno-policies

.PHONY: test-integration
test-integration: test-integration-rust test-integration-k8s ## All integration tests (k8s leg needs a cluster)

.PHONY: test-integration-rust
test-integration-rust: ## crates/*/tests/ composition tests (hermetic, in-process servers)
	cargo test --workspace --tests

.PHONY: test-integration-k8s
test-integration-k8s: ## Chainsaw suite against the local cluster
	chainsaw test tests/integration --config tests/integration/.chainsaw.yaml

.PHONY: test-e2e
test-e2e: ## Full install, real turn, security audit, client in the loop
	bash scripts/e2e.sh

.PHONY: test-client
test-client: ## Flutter client tests (e2e scaffolding; not a release gate)
	cd client && flutter test

.PHONY: mutants
mutants: ## Mutation testing; advisory, read the report
	cargo mutants

.PHONY: version
version: ## Stamp VERSION into Cargo.toml and every chart appVersion
	@perl -0pi -e 's/(\[workspace\.package\][^\[]*?\nversion = ")[^"]*(")/$${1}$(VERSION)$${2}/' Cargo.toml
	@for c in $(CHARTS); do \
		perl -i -pe 's/^appVersion:.*/appVersion: "$(VERSION)"/' $$c; \
	done
	@echo "stamped $(VERSION) -> Cargo.toml + $(words $(CHARTS)) charts"
