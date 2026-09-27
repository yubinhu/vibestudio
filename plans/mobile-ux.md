# Mobile UX — plan & status

This document tracks mobile work and proposals. Current behavior belongs in
[design.md](../design.md); commands and validation belong in the
[development guide](../docs/development.md). Completed work below is summarized
with links to those contracts. Proposals are not shipped behavior or permission
to remove existing features.

## 1. Linux/browser mobile dev loop — DONE

The phone interface can be developed in a desktop browser using the mobile
switchboard mode. Follow [mobile UI development](../docs/development.md#mobile-ui)
for the command and its limits, and the [isolated backend procedure](../docs/development.md#isolated-backend)
when testing alongside a running host. A separate config directory alone does
not isolate machine-wide Tailscale configuration.

Implemented foundations:

- Credential profiles and platform storage: [SSH profiles and key storage](../design.md#ssh-profiles-and-key-storage).
- Persistent hosts and session recovery: [host lifecycle](../design.md#durable-host-lifecycle--open-on-your-phone)
  and [connection manager](../design.md#connection-manager-vs-code-remote---ssh).
- Native authentication and privacy: [iPhone app lock](../design.md#native-iphone-app-lock).

Browser checks do not exercise the native iOS lifecycle, biometric prompts,
Keychain or WKWebView keyboard behavior. The
[native simulator harness](../client/desktop/gen/apple/Tests/NativeUI/README.md)
documents its coverage and limitations; signing and delivery are covered by
[the TestFlight procedure](../RELEASING.md#ios--testflight). Real-device SSH,
background/resume and keyboard validation remain separate integration work.

## 2. Notifications — native local DONE; closed-app push (APNs) = TODO

Native local notifications and audio are implemented. Their current delivery
conditions, watched-session behavior and platform differences are defined in
[session attention](../design.md#session-attention).

**Not implemented:** APNs registration and delivery for a fully suspended or
terminated native app. Tailscale connectivity does not itself provide that
delivery. This remains a proposal, not a claim of background notification support.

Proposed work:

1. Register the native app for remote notifications and associate its APNs device
   token with the intended host/account through an authenticated endpoint.
2. Deliver attention events through a trusted APNs provider. Provider signing
   keys must stay with that service, not in the app or users' SSH installations.
3. Verify locked-phone delivery, token renewal and notification navigation on a
   real device before treating the feature as complete.

Open choices include the provider/relay model, host authentication to that
provider, and coexistence with the browser Web-Push path. APNs support does not
by itself decide whether to remove the browser phone experience; see the
historical proposal below.

## 3. Tailscale phone access — historical decision

A 2026-07-11 planning note proposed freezing the browser phone experience and
removing it with Web Push after native APNs support shipped. Its rationale was
that Web Push provided closed-app delivery and the browser served platforms
without a native client.

This records the earlier proposal, not the current support or deletion policy.
Current phone access is documented in [phone access](../design.md#phone-access).
Any retirement decision must also resolve the account-backed-access proposal
that retains Tailscale as a bring-your-own-network option. Neither APNs nor
browser-path retirement is implemented by this plan.

## 4. Password-based SSH + automatic key install — PLANNED

**Goal:** let a user add a connection with a password once, generate and install
an on-device SSH key, verify it, and save an ordinary key-based profile without
storing the password. The existing manual-key flow remains available. Current
credential behavior is defined in [SSH profiles and key storage](../design.md#ssh-profiles-and-key-storage).

Proposed flow:

1. Add a Password / Manual key choice to the connection form.
2. Add a mobile-switchboard bootstrap endpoint. Authenticate a transient SSH
   session with the password, with an explicit keyboard-interactive fallback
   policy, and verify/pin the host key.
3. Generate the key in the switchboard, install its public half into the target's
   `authorized_keys`, then verify a fresh key-authenticated connection before
   saving the profile. Keep the private key out of the bootstrap HTTP response.
4. Save through `SecureStore` and discard the password. Surface authentication,
   installation and verification failures without leaving a misleading saved
   profile.

Implementation areas: `sshmgr/russh_tx.rs` for authentication/remote execution,
`remote_api.rs` for orchestration, and the shared connection form/API client for
the user flow. No password field belongs in the saved-profile schema.

Open choices include keyboard-interactive prompts, rollback after partial key
installation, and the bootstrap endpoint contract. Validate the proposal against
a controlled SSH server using the [mobile development loop](../docs/development.md#mobile-ui),
then verify on a device. Earlier effort estimates and library-version feasibility
notes are not a delivery commitment.
