# Sycophant Flutter Client

A Flutter chat client for sycophant. Phase 2 ships Android only; iOS support is scaffolded but not built.

The client is a single-screen app:

1. **First launch (no keypair):** the app's sign-in screen — paste server `host:port` + workspace + a grant code from the operator → app generates a P-256 keypair, calls `RedeemCode` with the public half → keypair + workspace + clientName persist via `flutter_secure_storage` (Keychain on iOS / EncryptedSharedPreferences on Android) → transitions to chat.
2. **Subsequent launches:** chat screen — text input, send button, scrollable message list. On entry the app opens a persistent server-streaming `ChannelReceive` stream; each send is a unary `ChannelIngest` carrying a signed `x-sig-*` metadata envelope, and the assistant bubble fills in as `ChannelOutbound` reply events arrive on the receive stream.

## Prereqs

- macOS (for the Android build chain via Flutter)
- Flutter 3.41+ — `brew install --cask flutter`
- Android command-line tools — `brew install --cask android-commandlinetools`, then symlink/install build-tools 36.0.0 + 28.0.3 + platforms;android-36 into `~/Library/Android/sdk` (run `flutter doctor --android-licenses` if `flutter doctor` complains)
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

1. Operator (you) deploys headscale plus the app channel with ACME enabled. Channel config lives in the values file, not `--set`:
   ```yaml
   # docs/e2e/values.yaml (or your own)
   headscale:
     enabled: true
     serverUrl: https://hs.yourdomain.com
     acme:
       enabled: true
       email: you@yourdomain.com
   channels:
     app:
       kind: app
       loginServer: https://hs.yourdomain.com
   ```
   ```sh
   helm upgrade --install e2e-test charts/sycophant-tenant/ \
     -n e2e-test -f docs/e2e/values.yaml --wait
   ```
2. On the phone, install the official Tailscale Android client (Play Store or `sideload-via-adb` an F-Droid build); set "Use an alternate server" to `https://hs.yourdomain.com`; log in via auth-key minted from headscale.
3. Invent an unguessable code and write the device's grant row. The code IS the row's identity; the relay mints nothing:
   ```sh
   kubectl patch configmap relay-grants -n e2e-test --type=merge -p '{"data":{
     "calebs-iphone": "channel: app\nidentity: kJ8f2QwXnR4tYv6b\nworkspace: hello-world\n"
   }}'
   ```
4. Send the code to the phone out of band (Signal, AirDrop, paste into a note).
5. Open the sideloaded sycophant app. Fill in server `relay:9090` (the app adapter's MagicDNS hostname), workspace `hello-world`, paste the code. Tap **Enroll**.
6. App generates a P-256 keypair, calls `RedeemCode` with the public half, persists the keypair + workspace via `flutter_secure_storage`, and lands on the chat screen. The row is now spent: a second presentation of the same code is refused.

## Chat

The chat path uses the **channel-adapter pattern**: the Flutter app behaves as an external channel for a single end-user. The workspace harness is the sole authority over what gets dispatched to the LLM for that workspace (it reads AGENTS.md and pulls the workspace's tool catalog on every turn); the Flutter app never calls `Turn` directly.

Per chat-screen entry:

1. The app opens a persistent server-streaming `ChannelReceive` RPC with `adapter_hint: "flutter-app:<clientName>"`. The `adapter_hint` is free-form, untrusted, and log-only — operators use it to grep ChannelReceive registrations in controller logs; it's never used for routing.
2. **The first `ChannelOutbound` event on the response stream is a `ChannelAck` carrying the server-minted `channel_id` (a UUID).** The app stores this id for the rest of the session and echoes it on every subsequent `ChannelIngest`. The id is opaque to the client and valid only within the lifetime of this `ChannelReceive` stream.
3. On user send, the app issues a unary `ChannelIngest` carrying `channel_id` + the user message. The controller validates the channel_id is bound to the caller's verified workspace (PermissionDenied otherwise — preventing cross-workspace routing-key hijack), stamps the user message's `reply_channel = channel_id`, and routes through the workspace's `Subscribe` stream to the harness. The harness runs the agent loop and emits replies via the workspace outbound channel sink, which the controller forwards to the open `ChannelReceive` stream.
4. Subsequent `ChannelOutbound` events are `send_message` variants carrying the agent's reply content.

The `ChannelIngestAck` returns the same `channel_id` plus a `conversation_id`. The conversation_id is the handle the app would use to call `GetConversationHistory(conversation_id, since: last_seen_seq)` on reconnect to fetch any assistant replies missed while the receive stream was down (the conversation log on the harness side is the durable source of truth; the `ChannelReceive` stream is a push-notification optimization on top). This replay path is not yet wired in the app — the field is captured but unused — but the controller-side primitives are in place.

Both RPCs carry the same `x-sig-*` signed-metadata envelope verified by the controller's external listener middleware. First message takes ~10–30 s (LLM Job cold start); subsequent messages typically arrive within a few hundred ms.

## Re-inviting a device

Two scenarios:

- **Operator re-invites a single device.** A row is spent once a key is registered against it, so re-invite is delete-and-rewrite: remove the row, then write a new row with a fresh code.
  ```sh
  kubectl patch configmap relay-grants -n e2e-test --type=json \
    -p '[{"op":"remove","path":"/data/calebs-iphone"}]'
  kubectl patch configmap relay-grants -n e2e-test --type=merge -p '{"data":{
    "calebs-iphone": "channel: app\nidentity: <a fresh unguessable string>\nworkspace: hello-world\n"
  }}'
  ```
  The device's existing signed requests start failing within seconds, with no pod restart. Tap the logout icon, confirm, and redeem the fresh code.

- **Operator revokes a device.** Remove the grant row from the `relay-grants` ConfigMap; the relay reloads the table within seconds and the device's next request is refused. To re-invite, write a new row with a fresh code — the device generates a new keypair and registers it.

## iOS (kept-in-mind, not shipped)

The Flutter project is scaffolded for both Android and iOS (`flutter create --platforms=android,ios`). `ios/Runner/Info.plist` already has the `NSAppTransportSecurity` exception for cleartext to `*.ts.net` and `*.ts.local`. Building iOS requires Xcode + an Apple Developer account; that's Phase 3+ work.

## Codegen

When `crates/relay-proto/proto/relay/v1/relay.proto` changes, regenerate the Dart stubs:

```sh
cd client && ./scripts/codegen.sh
```

The generated files in `client/lib/src/generated/` are committed (so a fresh clone is buildable without `protoc`); rerun the script and commit the diff when the proto evolves.

## Known limitations (Phase 2)

- **Per-device revoke is operator-driven** — remove the device's row from the `relay-grants` ConfigMap. No in-app refresh or rotate UX.
- **No multi-conversation support** — single chat thread per device.
- **No offline queue, no push notifications, no background sync** — when the app isn't foregrounded, the gRPC stream dies.
- **`tools` field is unused** — the chat sends only text content; the LLM has access to the workspace's tools server-side (configured in the chart), but the Flutter app doesn't render tool-call confirmation flows.
- **No image content** — `ContentBlock` supports inline images at the proto layer; the Flutter UI doesn't surface them yet.
- **Connection lifecycle is naive** — the channel stays open for the lifetime of the chat screen; backgrounding/foregrounding may need handling for production.
