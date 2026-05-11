# Sycophant Flutter Client

A Flutter chat client for sycophant. Phase 2 ships Android only; iOS support is scaffolded but not built.

The client is a single-screen app:

1. **First launch (no JWT):** enrollment screen — paste server `host:port` + an enrollment code minted by the operator → app calls `EnrollDevice` → JWT is persisted via `shared_preferences` → transitions to chat.
2. **Subsequent launches:** chat screen — text input, send button, scrollable message list. Each send opens a server-streaming `Turn` call; the assistant bubble fills in as `ContentDelta` events arrive.

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
   helm upgrade --install e2e-test charts/sycophant/ \
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
3. Mint an enrollment code for the phone:
   ```sh
   kubectl exec deploy/tightbeam-controller -n e2e-test -- \
     tightbeam-controller mint-enrollment hello-world calebs-iphone
   # → prints a long JWT-shaped string to stdout
   ```
4. Send the code to your phone (Signal, AirDrop, paste into a note, etc.).
5. Open the sideloaded sycophant app, paste the code into the enrollment screen, server stays at `tightbeam:9090` (the tsnet bridge's MagicDNS hostname). Tap **Enroll**.
6. App receives a 90-day device JWT, persists it, and lands on the chat screen.

## Chat

Type a message, tap send. The app opens a server-streaming `Turn` RPC; deltas render into the assistant bubble as they arrive. The first message takes ~10–30 s (LLM Job cold start); subsequent messages stream within a few hundred ms.

## Re-enrolling (Phase 2 has no refresh)

The 90-day JWT expires. When that happens, sends start failing with `[auth rejected - JWT expired or revoked. Sign out and re-enroll.]`. Tap the logout icon (top-right of the chat screen), confirm, and re-do the enrollment flow with a fresh code.

The same flow applies if the operator deletes the controller's signing key (`/var/log/tightbeam/.signing_key`) — that invalidates every JWT issued so far. There's no per-device revocation in Phase 2; key deletion is the nuclear option.

## iOS (kept-in-mind, not shipped)

The Flutter project is scaffolded for both Android and iOS (`flutter create --platforms=android,ios`). `ios/Runner/Info.plist` already has the `NSAppTransportSecurity` exception for cleartext to `*.ts.net` and `*.ts.local`. Building iOS requires Xcode + an Apple Developer account; that's Phase 3+ work.

## Codegen

When `crates/tightbeam-proto/proto/tightbeam/v1/tightbeam.proto` changes, regenerate the Dart stubs:

```sh
cd client && ./scripts/codegen.sh
```

The generated files in `client/lib/src/generated/` are committed (so a fresh clone is buildable without `protoc`); rerun the script and commit the diff when the proto evolves.

## Known limitations (Phase 2)

- **No refresh / no per-device revoke** — entire deployment-wide signing-key rotation is the only way to invalidate a stolen device JWT before its 90-day expiry.
- **No multi-conversation support** — single chat thread per device.
- **No offline queue, no push notifications, no background sync** — when the app isn't foregrounded, the gRPC stream dies.
- **`tools` field is unused** — the chat sends only text content; the LLM has access to the workspace's tools server-side (configured in the chart), but the Flutter app doesn't render tool-call confirmation flows.
- **No image content** — `ContentBlock` supports inline images at the proto layer; the Flutter UI doesn't surface them yet.
- **Connection lifecycle is naive** — the channel stays open for the lifetime of the chat screen; backgrounding/foregrounding may need handling for production.
