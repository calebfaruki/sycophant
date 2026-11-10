# scripts/

Developer utilities for the sycophant codebase. **Not for consumers** —
hobbyists installing sycophant run `sycoadm setup` (from the `cli/` crate).
These scripts are for maintainers running tests, debugging, and managing
local dev clusters.

| Script | What it does |
|---|---|
| `e2e.sh` | The end-to-end test. Reuses the k3d cluster (`--clean` rebuilds it and all images), runs `sycoadm setup`, installs the hello-world pieces through `sycoadm` from the example values files, joins the tailnet with `sycoadm connect`, drives the app path with the `syco` CLI and the operator's Dex token, and runs the Step 6 security assertions. Long-running (~20+ min); unattended apart from one `sudo` prompt for the tailnet join. |
| `preflight.sh` | Step-0 host-prereq check for `e2e.sh` (build toolchain + disk). The build and disk checks are also in `sycoadm setup`'s `check_prereqs`. |
| `quickstart.sh` | `make quickstart`. Runs the README CLI quickstart with the hello-world examples and opens `syco chat`. Shares the operator credentials and config dir with `make test-e2e`, so either reuses the other's cluster; the tailnet join asks for `sudo`. |
| `install-test-deps.sh` | Installs the `chainsaw`, `kyverno`, `shellcheck`, `shfmt` and `editorconfig-checker` binaries. Idempotent. Skips when binaries are already on PATH at the right version. |
| `kill-orphans.sh` | Kills orphan processes left behind by interrupted dev runs (rogue port-forwards, etc.). Run when ports refuse to bind. |

To run any of these you'll usually want `install-test-deps.sh` first.
