# scripts/

Maintainer tools for tests, debugging and local dev clusters. To install
sycophant, use `sycoadm setup` from the `cli/` crate.

| Script | What it does |
|---|---|
| `e2e.sh` | The end-to-end test. Reuses the k3d cluster (`--clean` rebuilds it and all images), calls `sycoadm setup`, installs the hello-world pieces from the example values files, and joins the tailnet with `sycoadm connect`. It drives the app path with `syco` and the operator's Dex token, then makes the Step 6 security assertions. Takes 20+ minutes, unattended apart from one `sudo` prompt for the tailnet join. |
| `preflight.sh` | Host check for `e2e.sh` (build toolchain and disk). `sycoadm setup`'s `check_prereqs` repeats the build and disk checks. |
| `quickstart.sh` | `make quickstart`. Executes the README CLI quickstart with the hello-world examples and opens `syco chat`. Shares the operator credentials and config dir with `make test-e2e`, so either reuses the other's cluster. The tailnet join asks for `sudo`. |
| `install-test-deps.sh` | Installs the `chainsaw`, `kyverno`, `shellcheck`, `shfmt` and `editorconfig-checker` binaries. Skips when binaries are already on PATH at the right version. |
| `kill-orphans.sh` | Kills orphan processes left behind by interrupted dev sessions (rogue port-forwards, etc.). Use it when ports refuse to bind. |

Most of these need `install-test-deps.sh` first.
