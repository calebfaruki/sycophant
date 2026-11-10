# Single source of truth for the release version.
# Edit VERSION, run `make version`, commit Cargo.toml + charts together.
VERSION := 0.1.0

CHARTS := $(wildcard charts/sycophant-*/Chart.yaml)

.DEFAULT_GOAL := help

.PHONY: help
help: ## List every target
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z0-9_-]+:.*## / {printf "  \033[1m%-22s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

.PHONY: lint
lint: lint-rust lint-go lint-k8s lint-shell ## All static checks

.PHONY: lint-rust
lint-rust: ## rustfmt + clippy, warnings are errors
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

.PHONY: lint-go
lint-go: ## go vet on the syco CLI
	cd cmd/syco && go vet ./...

# The default idp keeps the always-on apiserver-trust render valid, so lint the
# same shape. A component chart is linted with the name and minimal command its
# installer always sets. The Kyverno policies always render; helm lint has no cluster
# capabilities, so it logs the CRD-absent guard but still passes.
.PHONY: lint-k8s
lint-k8s: ## helm lint every chart
	helm lint charts/sycophant-tenant
	helm lint charts/sycophant-agent --set name=lint
	helm lint charts/sycophant-inference --set name=lint --set command.baseUrl=https://lint.example
	helm lint charts/sycophant-toolset --set name=lint
	helm lint charts/sycophant-user
	helm lint charts/sycophant-channel --set name=lint --set command.loginServer=https://lint.example
	helm lint charts/sycophant-cluster
	helm lint charts/sycophant-commands
	helm lint charts/sycophant-ingress --set domain=lint.example.com --set nodePort=30444 \
		--set vpn.namespace=lint --set vpn.service=lint --set vpn.port=8080 --set vpn.component=lint

.PHONY: lint-shell
lint-shell: ## shellcheck + shfmt + editorconfig-checker on all tracked .sh files
	git ls-files '*.sh' | xargs shellcheck -x -S warning
	git ls-files '*.sh' | xargs shfmt -d
	git ls-files '*.sh' | xargs editorconfig-checker

.PHONY: test-unit
test-unit: test-unit-rust test-unit-go test-unit-k8s ## All unit tests (hermetic, seconds)

.PHONY: test-unit-rust
test-unit-rust: ## Inline #[cfg(test)] units across the workspace
	cargo test --workspace --lib --bins --no-fail-fast

.PHONY: test-unit-go
test-unit-go: ## Go unit tests for the syco CLI
	cd cmd/syco && go test ./...

# The two policy fixtures are rendered from the chart on every run and are not
# tracked. `--api-versions` stands in for the Kyverno CRDs. The acceptance
# suites render the charts themselves. No cluster is contacted.
.PHONY: test-unit-k8s
test-unit-k8s: ## Render the two policy fixtures, run them, then the chart acceptance suites
	@helm template c charts/sycophant-cluster -n kyverno \
		--api-versions policies.kyverno.io/v1/ValidatingPolicy \
		--show-only templates/capability-job-gate-kpol.yaml \
		> tests/unit/kyverno-policies/capability-job-gate-as-harness/policy.yaml
	@helm template c charts/sycophant-cluster -n kyverno \
		--api-versions policies.kyverno.io/v1/ValidatingPolicy \
		--show-only templates/protect-security-kpol.yaml \
		> tests/unit/kyverno-policies/protect-security-idp-delete/policy.yaml
	@rc=0; \
	kyverno test tests/unit/kyverno-policies || rc=1; \
	for s in tests/unit/*/acceptance.sh; do bash "$$s" || rc=1; done; \
	exit $$rc

.PHONY: test-integration
test-integration: test-integration-rust test-integration-k8s ## All integration tests (k8s leg needs a cluster)

.PHONY: test-integration-rust
test-integration-rust: ## crates/*/tests/ composition tests (hermetic, in-process servers)
	cargo test --workspace --tests --no-fail-fast

.PHONY: test-integration-k8s
test-integration-k8s: ## Both chainsaw suites against the local cluster (turns self-service on)
	chainsaw test tests/integration --config tests/integration/.chainsaw.yaml
	helm upgrade sycophant charts/sycophant-cluster -n sycophant-system \
		--reuse-values -f scripts/fixtures/cluster-values.yaml \
		--set-string selfService.operator="$$(helm get values sycophant \
		-n sycophant-system --all -o json | jq -r .idp.issuer)#operator" --wait
	chainsaw test tests/self-service --config tests/integration/.chainsaw.yaml

.PHONY: syco
syco: ## Build the Go syco CLI binary
	cd cmd/syco && go build -o ../../target/syco .

.PHONY: sycoadm
sycoadm: ## Build the Rust sycoadm operator CLI binary
	CARGO_BUILD_JOBS=3 cargo build -p sycoadm --release
	@cp target/release/sycoadm target/sycoadm

.PHONY: quickstart
quickstart: ## Run the README quickstart end to end with the hello-world examples, then open syco chat
	bash scripts/quickstart.sh

.PHONY: test-e2e
test-e2e: ## Full install, real turn, security audit, client in the loop (CLEAN=1 rebuilds the cluster)
	bash scripts/e2e.sh $(if $(CLEAN),--clean)

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
