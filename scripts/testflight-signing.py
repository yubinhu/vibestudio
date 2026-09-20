#!/usr/bin/env python3
"""Ephemeral CI signing and independent verification of a TestFlight IPA."""

import base64
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import secrets
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile

BUNDLE = "one.vibestudio.app"
TEAM = "5J5PGFKG9H"
ALLOCATION_KEYS = ("tag", "commit", "version", "build", "runId", "runNumber")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def command(*args, data=None):
    result = subprocess.run(args, input=data, capture_output=True, check=False)
    # Commands can contain passwords, and security diagnostics can echo inputs.
    if result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} {args[1]} failed ({result.returncode}); sensitive diagnostics suppressed")
    return result.stdout


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")
    path.chmod(0o600)


def expiry(value, label):
    require(isinstance(value, dt.datetime), f"Missing {label} expiration")
    remaining = value.replace(tzinfo=dt.timezone.utc) - dt.datetime.now(dt.timezone.utc)
    require(remaining.total_seconds() > 0, f"{label} has expired")
    if remaining < dt.timedelta(days=30):
        print(f"::warning::{label} expires within 30 days ({value.date()})")


def validate_profile(profile):
    entitlements = profile.get("Entitlements", {})
    require(profile.get("TeamIdentifier") == [TEAM], "Provisioning profile team does not match")
    require(entitlements.get("application-identifier") == f"{TEAM}.{BUNDLE}", "Provisioning profile bundle does not match")
    require(entitlements.get("com.apple.developer.team-identifier") == TEAM, "Profile entitlement team does not match")
    require(entitlements.get("get-task-allow") is False, "Development provisioning profile is forbidden")
    require(entitlements.get("beta-reports-active") is True, "Profile must support TestFlight")
    require("ProvisionedDevices" not in profile and not profile.get("ProvisionsAllDevices"), "Profile must use App Store distribution")
    require(re.fullmatch(r"[A-Fa-f0-9-]{36}", profile.get("UUID", "")), "Invalid provisioning profile UUID")
    require(profile.get("DeveloperCertificates"), "Profile has no distribution certificate")
    expiry(profile.get("ExpirationDate"), "Provisioning profile")


def profile_from(path):
    profile = plistlib.loads(command("security", "cms", "-D", "-i", str(path)))
    validate_profile(profile)
    return profile


def certificate_check(der):
    result = command("openssl", "x509", "-inform", "DER", "-noout", "-enddate", "-subject", data=der).decode()
    date = re.search(r"^notAfter=(.+)$", result, re.M)
    require(date, "Certificate expiration is missing")
    expiry(dt.datetime.strptime(date[1], "%b %d %H:%M:%S %Y %Z"), "Distribution certificate")
    require("Apple Distribution" in result or "iPhone Distribution" in result, "Certificate must be an Apple distribution identity")
    return hashlib.sha1(der).hexdigest().upper()


def export_options(profile, fingerprint):
    return {"method": "app-store-connect", "destination": "export", "signingStyle": "manual",
            "teamID": TEAM, "signingCertificate": fingerprint,
            "provisioningProfiles": {BUNDLE: profile["UUID"]},
            "testFlightInternalTestingOnly": True, "manageAppVersionAndBuildNumber": False,
            "stripSwiftSymbols": True, "uploadSymbols": True}


def cleanup_path():
    return Path(os.environ["RUNNER_TEMP"]) / "vibestudio-testflight-signing.json"


def prepare(state):
    require(sys.platform == "darwin" and os.environ.get("GITHUB_ACTIONS") == "true",
            "Signing preparation requires a GitHub-hosted macOS runner")
    require(os.environ.get("RUNNER_ENVIRONMENT") == "github-hosted", "Self-hosted signing is not supported")
    marker = cleanup_path()
    require(not marker.exists(), "Signing state already exists; run cleanup before preparing again")
    private = Path(tempfile.mkdtemp(prefix="vibestudio-signing-", dir=marker.parent))
    keychain = private / "signing.keychain-db"
    # Persist ownership before any keychain/profile mutation, including failed imports.
    metadata = {"private": str(private), "keychain": str(keychain), "profile": None,
                "originalSearch": None, "searchChanged": False}
    write_json(marker, metadata)
    metadata["originalSearch"] = shlex.split(command("security", "list-keychains", "-d", "user").decode())
    write_json(marker, metadata)
    p12, mobile = private / "identity.p12", private / "profile.mobileprovision"
    for name, path in (("IOS_CERTIFICATE", p12), ("IOS_MOBILE_PROVISION", mobile)):
        path.write_bytes(base64.b64decode("".join(os.environ[name].split()), validate=True))
        path.chmod(0o600)
    profile = profile_from(mobile)
    password = secrets.token_urlsafe(40)
    # create-keychain itself can extend the search list, even if import fails.
    metadata["searchChanged"] = True
    write_json(marker, metadata)
    command("security", "create-keychain", "-p", password, str(keychain))
    command("security", "set-keychain-settings", "-lut", "21600", str(keychain))
    command("security", "unlock-keychain", "-p", password, str(keychain))
    command("security", "import", str(p12), "-k", str(keychain), "-P", os.environ["IOS_CERTIFICATE_PASSWORD"],
            "-T", "/usr/bin/codesign", "-T", "/usr/bin/security")
    command("security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:", "-s", "-k", password, str(keychain))
    identities = command("security", "find-identity", "-v", "-p", "codesigning", str(keychain)).decode()
    matching = [cert for cert in profile["DeveloperCertificates"] if hashlib.sha1(cert).hexdigest().upper() in identities]
    require(len(matching) == 1, "Expected exactly one imported identity matching the profile")
    fingerprint = certificate_check(matching[0])
    command("security", "list-keychains", "-d", "user", "-s", str(keychain), *metadata["originalSearch"])
    destination = Path.home() / "Library/Developer/Xcode/UserData/Provisioning Profiles" / f"{profile['UUID']}.mobileprovision"
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        require(destination.read_bytes() == mobile.read_bytes(), "Existing profile differs; refusing to overwrite it")
    else:
        with destination.open("xb") as output:
            metadata["profile"] = str(destination)
            metadata["profileSha256"] = hashlib.sha256(mobile.read_bytes()).hexdigest()
            write_json(marker, metadata)
            output.write(mobile.read_bytes())
        destination.chmod(0o600)
    state.mkdir(parents=True, exist_ok=True)
    (state / "ExportOptions.plist").write_bytes(plistlib.dumps(export_options(profile, fingerprint)))
    print(f"Prepared distribution signing for {BUNDLE}, profile {profile['UUID']}")


def cleanup():
    marker = cleanup_path()
    if not marker.exists():
        return
    metadata = json.loads(marker.read_text())
    private = Path(metadata["private"])
    require(private.parent.resolve() == marker.parent.resolve() and private.name.startswith("vibestudio-signing-"), "Invalid cleanup directory")
    keychain = private / "signing.keychain-db"
    require(str(keychain) == metadata["keychain"], "Invalid cleanup keychain")
    errors = []
    if metadata["searchChanged"]:
        try:
            command("security", "list-keychains", "-d", "user", "-s", *metadata["originalSearch"])
        except RuntimeError as error:
            errors.append(str(error))
    # Default keychain was never changed. Never unlock or alter the login keychain.
    if keychain.exists():
        try:
            command("security", "delete-keychain", str(keychain))
        except RuntimeError as error:
            errors.append(str(error))
    if metadata["profile"]:
        profile = Path(metadata["profile"])
        expected = Path.home() / "Library/Developer/Xcode/UserData/Provisioning Profiles"
        require(profile.parent == expected, "Invalid cleanup profile location")
        if profile.exists():
            require(hashlib.sha256(profile.read_bytes()).hexdigest() == metadata["profileSha256"], "Installed profile changed; refusing to delete it")
            profile.unlink()
    if errors:
        raise RuntimeError("; ".join(errors))
    shutil.rmtree(private, ignore_errors=False)
    marker.unlink()


def validate_bundle(info, entitlements, profile, allocation):
    require(info.get("CFBundleIdentifier") == BUNDLE, "IPA bundle ID does not match")
    require(info.get("CFBundleShortVersionString") == allocation["version"], "IPA marketing version does not match allocation")
    require(info.get("CFBundleVersion") == str(allocation["build"]), "IPA build number does not match allocation")
    require(info.get("CFBundleSupportedPlatforms") == ["iPhoneOS"], "IPA must be built for an iOS device")
    require(info.get("ITSAppUsesNonExemptEncryption") is False, "IPA export-compliance declaration changed")
    expected = profile["Entitlements"]
    for key, value in (("application-identifier", f"{TEAM}.{BUNDLE}"),
                       ("com.apple.developer.team-identifier", TEAM), ("get-task-allow", False),
                       ("beta-reports-active", True)):
        require(entitlements.get(key) == value, f"IPA entitlement {key} does not match")
    for key, value in entitlements.items():
        allowed = expected.get(key)
        if isinstance(value, list) and isinstance(allowed, list):
            # Apple's keychain access groups may grant the team's wildcard.
            require(all(any(item == grant or (isinstance(grant, str) and grant.endswith(".*")
                        and isinstance(item, str) and item.startswith(grant[:-1])) for grant in allowed)
                        for item in value), f"IPA entitlement {key} exceeds profile")
        else:
            require(value == allowed, f"IPA entitlement {key} exceeds profile")


def validate_manifest(manifest, allocation, digest):
    require(all(manifest.get(key) == allocation.get(key) for key in ALLOCATION_KEYS), "Stored IPA allocation does not match this run")
    require(manifest.get("sha256") == digest, "Stored IPA checksum does not match")


def verify(state):
    allocation = json.loads((state / "allocation.json").read_text())
    require(all(key in allocation for key in ALLOCATION_KEYS), "Incomplete allocation")
    ipa = state / "ipa/VibeStudio.ipa"
    digest = hashlib.sha256(ipa.read_bytes()).hexdigest()
    manifest_path = state / "package.json"
    previous = json.loads(manifest_path.read_text()) if manifest_path.exists() else None
    if previous:
        validate_manifest(previous, allocation, digest)
    with tempfile.TemporaryDirectory(prefix="vibestudio-verify-", dir=os.environ["RUNNER_TEMP"]) as directory:
        unpack = Path(directory)
        with zipfile.ZipFile(ipa) as archive:
            for member in archive.infolist():
                path = Path(member.filename)
                require(not path.is_absolute() and ".." not in path.parts and not stat.S_ISLNK(member.external_attr >> 16), "Unsafe IPA archive member")
        command("ditto", "-x", "-k", str(ipa), str(unpack))
        apps = list((unpack / "Payload").glob("*.app"))
        require(len(apps) == 1, "Expected exactly one application in IPA")
        app = apps[0]
        require(not list(app.glob("**/*.appex")), "App extensions require additional signing validation")
        command("codesign", "--verify", "--deep", "--strict", str(app))
        info = plistlib.loads((app / "Info.plist").read_bytes())
        entitlements = plistlib.loads(command("codesign", "--display", "--entitlements", "-", "--xml", str(app)))
        profile = profile_from(app / "embedded.mobileprovision")
        validate_bundle(info, entitlements, profile, allocation)
        executable = info.get("CFBundleExecutable", "")
        require(executable and Path(executable).name == executable, "Invalid IPA executable name")
        binary = app / executable
        require(command("lipo", "-archs", str(binary)).decode().strip() == "arm64", "IPA executable must contain device arm64 only")
        platform = command("xcrun", "vtool", "-show-build", str(binary)).decode()
        require(re.search(r"platform\s+IOS(?:\s|$)", platform) and "IOSSIMULATOR" not in platform, "IPA executable platform must be iOS")
        certificate_prefix = str(unpack / "signing-cert-")
        command("codesign", "--display", f"--extract-certificates={certificate_prefix}", str(app))
        der = Path(certificate_prefix + "0").read_bytes()
        require(der in profile["DeveloperCertificates"], "IPA signing certificate is absent from profile")
        fingerprint = certificate_check(der)
        export = state / "ExportOptions.plist"
        if export.exists():
            require(plistlib.loads(export.read_bytes()) == export_options(profile, fingerprint), "IPA signing identity/profile differs from prepared export")
        manifest = {**{key: allocation[key] for key in ALLOCATION_KEYS}, "sha256": digest,
                    "bundleId": BUNDLE, "teamId": TEAM, "profileUuid": profile["UUID"],
                    "certificateSha1": fingerprint, "architecture": "arm64", "audience": "INTERNAL_ONLY"}
        if previous:
            require(previous == manifest, "Stored package metadata differs from verified IPA")
        write_json(manifest_path, manifest)
    print(f"Verified {BUNDLE} {allocation['version']} ({allocation['build']}), SHA-256 {digest}")


if __name__ == "__main__":
    try:
        action = sys.argv[1] if len(sys.argv) == 2 else ""
        require(action in ("prepare", "verify", "cleanup"), "Usage: testflight-signing.py prepare|verify|cleanup")
        os.umask(0o077)
        if action == "cleanup":
            cleanup()
        else:
            {"prepare": prepare, "verify": verify}[action](Path(os.environ["RELEASE_STATE_DIR"]))
    except (ValueError, RuntimeError, OSError, KeyError, plistlib.InvalidFileException, zipfile.BadZipFile) as error:
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)
