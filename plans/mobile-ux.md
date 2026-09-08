# Mobile UX — plan & status

Working notes for the phone experience. Not published (this dir is not the Pages
site). See [design.md](../design.md) for the authoritative architecture.

---

## 1. Linux/browser mobile dev loop — DONE

Mobile mode is **server capability detection, not device detection**: the SPA
renders the phone experience iff the server answers `GET /api/remote/profiles`
(`client/web/lib/remote.ts` → `AppShell.tsx`). No user-agent / Tauri / OS gate
exists. So the whole mobile UX is developable in a desktop browser on Linux —
no Mac, no simulator.

**How to run it:**

```bash
# terminal 1 — the switchboard backend, mobile mode on
cargo run -p skill-server --features russh-transport -- --mobile-dev

# terminal 2 — the SPA with hot reload (Vite proxies /api to :8765)
npm run dev:vite
```

Open `http://localhost:1420` and use the browser device toolbar (Chrome: `pointer:
coarse` + a phone width). You get the full connect → keygen → save → connect flow
with **sub-second HMR**, versus the ~3–6 min `scripts/ios-sim.sh` rebuild. Real
russh connects work (verified against a live sshd). `--mobile-dev` wires a
file-backed `SecureStore` (`~/.config/vibestudio/ssh_{profiles,keys}.json`, keys
`0600`); without `--features russh-transport`, connect falls back to system `ssh`
and on-device keygen is unavailable.

**What the browser can't reproduce** (keep a pre-release Mac/sim pass):
- iOS Keychain semantics (browser/dev uses the file store)
- the suspension/heal lifecycle — this is **iOS-shell-only** code
  (`client/desktop/src/lib.rs`, the `WindowEvent::Resumed` arm + `LocalServer::heal`);
  a browser tab / a normal daemon never has its sockets reclaimed, so there's
  nothing to handle and nothing to test there
- `env(safe-area-inset-*)` (0 in a browser), WKWebView soft-keyboard quirks

### Keystore abstraction (done)

`skill_core::keystore` (modelled on `skill_core::agents`): the `SecureStore`
trait + `SshProfile` + a pure `FileSecureStore` fallback + `resolve(native, dir)`.
The shell's `securestore::platform_native()` is the registry seam — Apple → the
Keychain, everything else → `None` → file fallback. Adding a platform's native
keystore = one arm there. iOS now also degrades to the sandbox file store if the
Keychain won't open (no more setup crash). Android is intentionally unwired
(keyring v3 has no Android backend); when Android lands it'll use the **native
app** flow like iOS, not the browser/tailscale path.

### Shared host and reconnect (done)

Desktop and native phone attach the same detached per-user host service. The worker
survives client/tunnel exit and owns phone serving, gateway configuration and attention
watching. Each terminal viewer has an independent attachment ID and PTY; the viewer
sending user input controls shared terminal geometry.

Same-host recovery preserves the mounted workspace and session selection. A reconnect
overlay pauses input while the tunnel recovers with capped backoff; trust/auth errors
await Retry. Streams reattach with fresh IDs and discard queued input/clipboard results.
Lists refresh and attention rebaselines silently. The iOS shell retains its foreground
listener-heal path, preserving the route if a new origin is unavoidable. See
[design.md](../design.md#connection-manager-vs-code-remote---ssh) for the lifecycle contract.
Native iOS suspension and keyboard behavior still require a device validation pass.

---

## 2. Notifications — native local DONE; closed-app push (APNs) = TODO

**Done:** the iOS app now has its own native notification channel. `ShellNotifier`
+ `tauri-plugin-notification` are wired into the iOS shell; `setup_mobile` sets
`ServerConfig.notifier`. Turn-finish events arrive from the remote hub over SSE,
the SPA posts the pinned-local `/api/notify*` routes, and the phone shows a
native local notification. `/api/notify/status` returns `native: true` and
`notifyWhileVisible: true`, so iOS also shows banners on Home or while watching
another session. The watched session stays banner-free. The mobile workspace
requests permission on connection, including when joining agents created on a
desktop. Native `AVAudioPlayer` plays the bundled request/done sounds through
`/api/notify/sound`, without Web Audio's gesture requirement; it respects the
phone's Silent mode and the app's sound toggle.
The app owns iOS notification presentation/tap callbacks; it reads OS content
directly so tapping a notification after relaunch cannot dereference the plugin's
discarded in-memory metadata. Permission and scheduling still use the plugin.

**Scope limit — this delivers notifications while the app is running or briefly
backgrounded, NOT when fully suspended/killed.** True closed-app delivery needs
**APNs**: an app-side device-token registration, and the remote hub (or a small
relay) sending push payloads to Apple. That's the real "notifications moved to the
native app" endpoint and the next step here.

Tailscale provides connectivity, but cannot keep the app executing after iOS
suspends it. No APNs capability, device registration, or provider is configured.
An APNs signing key must stay on a trusted provider; never bundle it in the app
or distribute it to users' SSH hosts. Verify locked-phone delivery on a real
device before claiming background notifications work.

**APNs sketch (future):**
- iOS: register for remote notifications, capture the APNs device token, hand it
  to the connected hub over an `/api/push/*`-style route (replacing the Web-Push
  subscription for the native app).
- Hub: on a turn-finish bell (`events.rs`), send an APNs push (HTTP/2 + JWT auth
  with an APNs key, à la the ASC key we already use) to the registered token.
- This lets us retire the Web-Push/PWA stack for the app (`push.rs`, `sw.js`,
  `lib/push.ts`) — see §3.

---

## 3. Tailscale "web app on your phone" path — FROZEN

Decision (2026-07-11): **keep it, frozen** — no new features, demoted in docs to
an "also works" path. Not deleted yet because today it is:
- the only closed-app notification channel for *every* platform (until APNs, §2);
- (was) Android's only story — Android is now deprioritised and will use the
  native flow, so this reason is lapsing.

~940 LOC are exclusive to it (`phone.rs`, `tailscale.rs`, `PhoneModal.tsx`,
`/api/phone/*`, the tray item + `#/?phone=1` deep-link) plus ~950 in the
Web-Push/PWA stack. **Delete criteria:** once APNs (§2) ships, the notification
argument is gone → remove the tailscale phone path + the Web-Push stack together.
Keep `from_this_machine()` pinning (residual hardening) and rename `PHONE_PORT`
(8765 is also the dev proxy + standalone default), don't delete it.

---

## 4. Password-based SSH + automatic key install — PLANNED (next)

**Today:** publickey only. The connect journey has a painful cross-device step —
the user must relay the generated public key from phone to computer by hand
(Universal Clipboard on Mac; Slack/email on Linux) before connecting. 2–3 context
switches; the single biggest UX cliff.

**Goal:** a one-shot **password bootstrap** (the Termius pattern) — password used
once at add-connection time, an on-device key generated and installed over that
password session, profile saved as a normal key profile, **password never stored**.
Zero device switches.

**Feasibility (verified):** russh 0.62.2 already ships `authenticate_password`
and the `authenticate_keyboard_interactive_*` pair (needed as a fallback — many
sshds deliver passwords via keyboard-interactive; `AuthResult::Failure.remaining_methods`
drives the fallback). The `exec_with_stdin` machinery that `provision.rs` uses to
pipe a binary over SSH is directly reusable to append to `authorized_keys`.

**Flow:**
1. UI: auth-mode toggle (Password | Manual key); password field.
2. New route `POST /api/remote/profiles/bootstrap {host,user,port,password}`
   (gated on `secure_store` + `russh-transport`):
   a. open a transient `RusshSession` with `authenticate_password` (kbd-interactive
      fallback); TOFU-pins the host key.
   b. `keygen::generate_ed25519` **server-side** (private key never crosses HTTP —
      better than today's flow).
   c. `exec_with_stdin("umask 077; mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys
      && grep -qxF <key> ~/.ssh/authorized_keys || printf '%s\n' <key> >> ...",
      pubkey)`.
   d. verify with a fresh **key-auth** connect before persisting (recommended).
   e. `store.put_profile(profile, private_key)`; discard the password.
3. Manual key flow stays as a fallback (hardened sshd with `PasswordAuthentication
   no`).

**Touch list (≈1–2 focused days incl. tests):**
| File | Effort | Change |
|---|---|---|
| `server/skill-server/src/sshmgr/russh_tx.rs` | M | `Auth::{Key,Password}` on creds; `connect` match adds `authenticate_password` + kbd-interactive fallback; a `connect_with_password` entry point |
| `server/skill-server/src/remote_api.rs` | L | new `POST /api/remote/profiles/bootstrap` (validate parts, keygen, install key, verify, save) |
| `client/web/lib/api.ts` | S | `sshProfileBootstrap({host,user,port,password})` |
| `client/web/components/connections.tsx` | M | auth-mode toggle + password field; "Connect & install key" button; wrong-password error surface |
| `client/desktop/src/lib.rs` | S/none | none expected |
| `SecureStore` / `KeychainStore` / `SshProfile` | none | password not stored → no schema change |

**Open choices:** kbd-interactive fallback policy; whether to verify key-auth with
a second connect before persisting (recommended, above). Build & test this on the
Linux dev loop (§1) against a local sshd.
