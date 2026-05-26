# scripts/

Developer utilities for the sycophant codebase. **Not for consumers** —
hobbyists installing sycophant run `syco install` (from the `cli/` crate).
These scripts are for maintainers running tests, debugging, and managing
local dev clusters.

| Script | What it does |
|---|---|
| `e2e.sh` | Full end-to-end against an Android emulator. Bootstraps a clean k3d cluster, builds + loads all images, deploys, launches the Flutter client, runs the Step 6 security assertions. Long-running (~20+ min) with an interactive pause for the operator to enroll the phone. |
| `quickstart-test.sh` | Chainsaw e2e against a fresh k3d cluster. Builds nothing — exercises `syco install` (so the dev test hits the same code path consumers do), then runs the 27 chainsaw integration tests + helm uninstall hygiene. ~10 min. |
| `mutation-removal.sh <mutation>` | Smoke-test that the chainsaw harness is non-performative: deletes a sycophant policy, runs the affected chainsaw tests (expects RED), restores. Names: `workspace-vap`, `tenant-naming`, `protect-security`, `tenant-perimeter`. |
| `install-test-deps.sh` | Installs `chainsaw` + `kyverno` CLI binaries. Idempotent. Skips when binaries are already on PATH at the right version. |
| `kill-orphans.sh` | Kills orphan processes left behind by interrupted dev runs (rogue port-forwards, etc.). Run when ports refuse to bind. |
| `phone-up.sh` | Helper to install the Flutter client APK onto a running Android emulator. Used standalone or by `e2e.sh`. |

Required env vars for `e2e.sh`: `ANTHROPIC_API_KEY`, `MISTRAL_API_KEY`.

To run any of these you'll usually want `install-test-deps.sh` first.
