#!/usr/bin/env bash
# Only controls a newly created virtual iPhone; never launches the Simulator UI.
set -euo pipefail
harness_dir=$(cd "$(dirname "$0")" && pwd)
simulator_app=${1:?usage: run.sh /path/to/VibeStudio.app /path/to/evidence}
evidence=${2:?usage: run.sh /path/to/VibeStudio.app /path/to/evidence}
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
simulator_app=$(cd "$simulator_app" && pwd)
test -f "$simulator_app/assets/dist/index.html"
command -v xcodegen >/dev/null

xcrun simctl list devices available --json > "$evidence/devices.json"
selection=$(python3 - "$evidence/devices.json" <<'PY'
import json, sys
with open(sys.argv[1]) as source:
    devices = json.load(source)["devices"]
choices = [(runtime, device["deviceTypeIdentifier"])
           for runtime, group in devices.items() if ".iOS-" in runtime
           for device in group
           if device.get("isAvailable") and device["name"].startswith("iPhone")
           and "SE" not in device["name"] and device.get("deviceTypeIdentifier")]
if not choices:
    raise SystemExit("No available Face ID iPhone simulator runtime")
print(*choices[-1])
PY
)
read -r runtime device_type <<< "$selection"
device_id=$(xcrun simctl create "VibeStudio native lock QA" "$device_type" "$runtime")
printf '%s\n' "$device_id" > "$evidence/device-id.txt"
collect_evidence() {
  result=$?
  trap - EXIT
  set +e
  # Diagnostics must not mask the test's result. Only this disposable device is
  # inspected, shut down and deleted; no existing simulator is changed.
  xcrun simctl io "$device_id" screenshot "$evidence/final-screen.png"
  app_pid=$(xcrun simctl spawn "$device_id" launchctl list | awk '$3 ~ /one\.vibestudio\.app/ {print $1; exit}')
  if [[ "$app_pid" =~ ^[0-9]+$ ]]; then
    if [[ "$result" -ne 0 ]]; then
      sample "$app_pid" 5 -file "$evidence/process-sample.txt"
    fi
    lsof -nP -a -p "$app_pid" -iTCP -sTCP:LISTEN -Fn > "$evidence/listeners.txt"
    while IFS= read -r port; do
      curl --max-time 5 --silent --show-error -D "$evidence/root-$port.headers" \
        "http://127.0.0.1:$port/" > "$evidence/root-$port.html"
      curl --max-time 5 --silent --show-error -D "$evidence/profiles-$port.headers" \
        "http://127.0.0.1:$port/api/remote/profiles" > "$evidence/profiles-$port.json"
    done < <(sed -n 's/^n127\.0\.0\.1://p' "$evidence/listeners.txt")
  fi
  data_dir=$(xcrun simctl get_app_container "$device_id" one.vibestudio.app data)
  if [[ -n "$data_dir" && -d "$data_dir/Library/Logs" ]]; then
    cp -R "$data_dir/Library/Logs" "$evidence/app-logs"
  fi
  xcrun simctl spawn "$device_id" log show --last 10m --style compact --info --debug \
    --predicate 'process == "VibeStudio" OR process CONTAINS "WebKit"' \
    > "$evidence/unified.log" 2>&1
  xcrun xcresulttool export attachments --path "$evidence/NativeUI.xcresult" \
    --output-path "$evidence/screenshots"
  xcrun simctl shutdown "$device_id" >/dev/null 2>&1
  xcrun simctl delete "$device_id" >/dev/null 2>&1
  exit "$result"
}
trap collect_evidence EXIT
find "$simulator_app" -maxdepth 4 -type f | sort > "$evidence/bundle-files.txt"
cp "$simulator_app/assets/dist/index.html" "$evidence/bundled-index.html"
plutil -convert xml1 -o "$evidence/Info.plist" "$simulator_app/Info.plist"
xcrun simctl boot "$device_id"
xcrun simctl bootstatus "$device_id" -b
# Simulator signing is ad hoc and needs no account or distribution private key.
codesign --force --deep --sign - "$simulator_app"
xcrun simctl install "$device_id" "$simulator_app"
xcodegen generate --spec "$harness_dir/project.yml" --project "$evidence"
xcodebuild test -project "$evidence/VibeStudioLockQA.xcodeproj" \
  -scheme LockQAHarness -destination "platform=iOS Simulator,id=$device_id" \
  -derivedDataPath "$evidence/DerivedData" -resultBundlePath "$evidence/NativeUI.xcresult" \
  -parallel-testing-enabled NO CODE_SIGNING_ALLOWED=NO 2>&1 | tee "$evidence/native-ui.log"
