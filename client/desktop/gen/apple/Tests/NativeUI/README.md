# Native iPhone lock checks

This XCTest harness drives the installed VibeStudio app on a disposable iPhone
Simulator. It is separate from the product target. Biometric test notifications
are sent from inside that virtual phone; no product authentication bypass,
Computer Use, physical phone, or desktop UI automation is involved.

After building the arm64 simulator app, run from the repository root:

```bash
bash client/desktop/gen/apple/Tests/NativeUI/run.sh \
  client/desktop/gen/apple/build/arm64-sim/VibeStudio.app \
  target/ios-native-check
```

Use an empty evidence directory for each run. The harness requires Xcode with an
available Face ID iPhone runtime and `xcodegen` (`brew install xcodegen`). It
creates, boots, and later deletes only its own virtual phone. It enrolls simulated
Face ID and handles the first app permission prompt itself.

The checks cover cold-launch protection, a real connection screen after a
simulated match, return after five seconds, relock after 61 seconds, failed match,
cancel/retry, and an app-switcher card whose image contains the privacy cover
instead of the connection screen. Screenshots, UI hierarchies, logs, and the
XCTest result bundle are saved in the evidence directory. CI also runs the
portable policy tests for exact timeout boundaries and stale authentication.

Simulated biometrics do not validate physical Face ID, Touch ID, passcode fallback,
or an SSH-connected computer. Those remain hardware/integration checks.
