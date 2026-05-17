# Sycophant Flutter Client

A Flutter chat client for sycophant. Phase 2 ships Android only; iOS support is scaffolded but not built.

The client is a single-screen app:

1. **First launch (no keypair):** enrollment screen — paste server `host:port` + workspace + an enrollment code from the operator → app generates a P-256 keypair, calls `RedeemEnrollment` with the public half → keypair + workspace + clientName persist via `flutter_secure_storage` (Keychain on iOS / EncryptedSharedPreferences on Android) → transitions to chat.
2. **Subsequent launches:** chat screen — text input, send button, scrollable message list. Each send opens a server-streaming `Turn` call with a signed `x-sig-*` metadata envelope; the assistant bubble fills in as `ContentDelta` events arrive.

## Prereqs

- macOS (for the Android build chain via Flutter)
- Flutter 3.41+ — `brew install --cask flutter`
- Android command-line tools — `brew install --cask android-commandlinetools`, then symlink/install build-tools 36.0.0 + 28.0.3 + platforms;android-36 into `~/Library/Android/sdk` (see Stage 4 setup notes if `flutter doctor` complains)
- `protoc` — `brew install protobuf`
- `dart pub global activate protoc_plugin`
- Android phone with USB debugging enabled (Settings → System → Developer Options → USB debugging)
- `adb` — comes with Android SDK platform-tools

## Build

```sh
cd client
flutter pub get
flutter analyze    # should report only cosmetic info from generated code
flutter test       # smoke test only
flutter build apk --debug
```

The APK lands at `client/build/app/outputs/flutter-apk/app-debug.apk` (~150 MB; debug builds are large because of the JIT runtime).

## Sideload to phone

```sh
# Connect phone via USB, accept the "trust this computer" prompt on the
# phone, then verify it's reachable:
adb devices
# expect: <serial>  device

adb install client/build/app/outputs/flutter-apk/app-debug.apk
```

If `adb install` fails with `INSTALL_FAILED_USER_RESTRICTED`, enable "Install via USB" in the phone's Developer Options.

## First-time pairing

Phase 2 trust flow:

1. Operator (you) deploys headscale + tsnetBridge with ACME enabled (Layer 2 of the e2e doc):
   ```sh
   helm upgrade --install e2e-test charts/sycophant-tenant/ \
     -n e2e-test \
     -f docs/e2e/values.yaml \
     --set headscale.enabled=true \
     --set headscale.serverUrl=https://hs.yourdomain.com \
     --set headscale.acme.enabled=true \
     --set headscale.acme.email=you@yourdomain.com \
     --set tsnetBridge.enabled=true \
     --set tsnetBridge.loginServer=https://hs.yourdomain.com \
     --wait
   ```
2. On the phone, install the official Tailscale Android client (Play Store or `sideload-via-adb` an F-Droid build); set "Use an alternate server" to `https://hs.yourdomain.com`; log in via auth-key minted from headscale.
3. Authorize the device by adding a Client CR to your values file (see the `clients:` block in `charts/sycophant-tenant/values.yaml`), then `helm upgrade --install ...`. Example:
   ```yaml
   clients:
     calebs-iphone:
       workspaces:
         - hello-world
   ```
4. Read the one-time enrollment code the controller minted onto the Client CR's status and send it to the phone (Signal, AirDrop, paste into a note, etc.):
   ```sh
   kubectl get tbcl calebs-iphone -n e2e-test \
     -o jsonpath='{.status.enrollmentCode}'
   ```
5. Open the sideloaded sycophant app. Fill in server `tightbeam:9090` (the tsnet bridge's MagicDNS hostname), workspace `hello-world`, paste the enrollment code. Tap **Enroll**.
6. App generates a P-256 keypair, calls `RedeemEnrollment` with the public half, persists the keypair + workspace via `flutter_secure_storage`, and lands on the chat screen. Each subsequent `Turn` RPC carries a signed metadata envelope verified by the controller's external listener.

## Chat

Type a message, tap send. The app opens a server-streaming `Turn` RPC; deltas render into the assistant bubble as they arrive. The first message takes ~10–30 s (LLM Job cold start); subsequent messages stream within a few hundred ms.

## Re-enrolling

Two scenarios:

- **Operator rotates a single device's key.** Clear the registered public key on the Client CR so the controller mints a fresh enrollment code on the next reconcile:
  ```sh
  kubectl patch client calebs-iphone -n e2e-test \
    --subresource=status --type=merge \
    -p '{"status":{"publicKey":null}}'
  ```
  The user's existing signed requests start failing with `[signature rejected — key may be rotated. Sign out and re-enroll.]`. Tap the logout icon, confirm, and re-do the enrollment flow with the fresh code (read it the same way as Step 4 above).

- **Operator rotates the per-tenant signing key.** Delete the `tightbeam-signing-key` Secret and re-run `helm upgrade`; the chart's pre-install hook mints a fresh signing key. Already-enrolled Clients keep working — `Turn` calls verify against per-Client public keys, not the signing key. Only outstanding (unredeemed) enrollment codes become invalid.

## iOS (kept-in-mind, not shipped)

The Flutter project is scaffolded for both Android and iOS (`flutter create --platforms=android,ios`). `ios/Runner/Info.plist` already has the `NSAppTransportSecurity` exception for cleartext to `*.ts.net` and `*.ts.local`. Building iOS requires Xcode + an Apple Developer account; that's Phase 3+ work.

## Codegen

When `crates/tightbeam-proto/proto/tightbeam/v1/tightbeam.proto` changes, regenerate the Dart stubs:

```sh
cd client && ./scripts/codegen.sh
```

The generated files in `client/lib/src/generated/` are committed (so a fresh clone is buildable without `protoc`); rerun the script and commit the diff when the proto evolves.

## Known limitations (Phase 2)

- **Per-device revoke is operator-driven** — `kubectl patch client <name> --subresource=status -p '{"status":{"publicKey":null}}'`. No in-app refresh or rotate UX.
- **No multi-conversation support** — single chat thread per device.
- **No offline queue, no push notifications, no background sync** — when the app isn't foregrounded, the gRPC stream dies.
- **`tools` field is unused** — the chat sends only text content; the LLM has access to the workspace's tools server-side (configured in the chart), but the Flutter app doesn't render tool-call confirmation flows.
- **No image content** — `ContentBlock` supports inline images at the proto layer; the Flutter UI doesn't surface them yet.
- **Connection lifecycle is naive** — the channel stays open for the lifetime of the chat screen; backgrounding/foregrounding may need handling for production.
