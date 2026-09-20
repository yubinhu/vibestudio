import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const script = fileURLToPath(new URL('./testflight-signing.py', import.meta.url));
const setup = `
import datetime as dt, importlib.util, json, os, tempfile
from pathlib import Path
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('signing', ${JSON.stringify(script)})
s = importlib.util.module_from_spec(spec)
spec.loader.exec_module(s)
allocation = dict(tag='v1.2.11', commit='a'*40, version='1.2.11', build='1001', runId='42', runNumber='1')
profile = dict(UUID='12345678-1234-1234-1234-123456789abc', TeamIdentifier=[s.TEAM],
    ExpirationDate=dt.datetime.now()+dt.timedelta(days=90), DeveloperCertificates=[b'certificate'],
    Entitlements={'application-identifier': s.TEAM+'.'+s.BUNDLE,
    'com.apple.developer.team-identifier': s.TEAM, 'get-task-allow': False,
    'beta-reports-active': True, 'keychain-access-groups': [s.TEAM+'.*']})
entitlements = dict(profile['Entitlements'], **{'keychain-access-groups': [s.TEAM+'.'+s.BUNDLE]})
info = dict(CFBundleIdentifier=s.BUNDLE, CFBundleShortVersionString='1.2.11',
    CFBundleVersion='1001', CFBundleSupportedPlatforms=['iPhoneOS'], ITSAppUsesNonExemptEncryption=False)
def rejects(fn, contains):
    try: fn()
    except ValueError as e: assert contains in str(e), str(e)
    else: raise AssertionError('Expected validation failure')
`;

function python(code) {
  const result = spawnSync('python3', ['-c', setup + code], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr || result.stdout || String(result.error));
}

test('distribution metadata accepts the pinned app, team, and device build', () => python(`
s.validate_profile(profile)
s.validate_bundle(info, entitlements, profile, allocation)
`));

test('development, ad hoc, enterprise, wrong-app and expired profiles are rejected', () => python(`
import copy
for field, value, expected in [('TeamIdentifier', ['OTHER'], 'team'),
    ('ProvisionedDevices', [], 'App Store'), ('ProvisionsAllDevices', True, 'App Store'),
    ('ExpirationDate', dt.datetime.now()-dt.timedelta(days=1), 'expired')]:
    changed = dict(profile, **{field: value})
    rejects(lambda: s.validate_profile(changed), expected)
for field, value, expected in [('get-task-allow', True, 'Development'),
    ('application-identifier', s.TEAM+'.other.app', 'bundle'), ('beta-reports-active', False, 'TestFlight')]:
    changed = copy.deepcopy(profile)
    changed['Entitlements'][field] = value
    rejects(lambda: s.validate_profile(changed), expected)
`));

test('version, platform, encryption, and entitlement changes fail verification', () => python(`
for field, value, expected in [('CFBundleIdentifier', 'other.app', 'bundle'),
    ('CFBundleShortVersionString', '1.1.0', 'marketing'), ('CFBundleVersion', '9', 'build'),
    ('CFBundleSupportedPlatforms', ['iPhoneSimulator'], 'device'),
    ('ITSAppUsesNonExemptEncryption', True, 'compliance')]:
    changed = dict(info, **{field:value})
    rejects(lambda: s.validate_bundle(changed, entitlements, profile, allocation), expected)
for field, value in [('get-task-allow', True), ('keychain-access-groups', ['OTHER.secret']),
    ('com.apple.security.application-groups', ['group.other'])]:
    changed = dict(entitlements, **{field:value})
    rejects(lambda: s.validate_bundle(info, changed, profile, allocation), 'entitlement')
`));

test('export options fix manual identity, internal audience, and allocated versions', () => python(`
options = s.export_options(profile, 'ABC123')
assert options == dict(method='app-store-connect', destination='export', signingStyle='manual',
    teamID=s.TEAM, signingCertificate='ABC123', provisioningProfiles={s.BUNDLE:profile['UUID']},
    testFlightInternalTestingOnly=True, manageAppVersionAndBuildNumber=False,
    stripSwiftSymbols=True, uploadSymbols=True)
`));

test('restored package checksum and every allocation field must match', () => python(`
manifest = dict(allocation, sha256='abc123')
s.validate_manifest(manifest, allocation, 'abc123')
rejects(lambda: s.validate_manifest(manifest, allocation, 'tampered'), 'checksum')
for key in s.ALLOCATION_KEYS:
    changed = dict(manifest, **{key:'tampered'})
    rejects(lambda: s.validate_manifest(changed, allocation, 'abc123'), 'allocation')
`));

test('cleanup restores only the captured search list and owned temporary files', () => python(`
with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, RUNNER_TEMP=root):
    private = Path(root)/'vibestudio-signing-test'
    private.mkdir()
    keychain = private/'signing.keychain-db'
    keychain.touch()
    marker = s.cleanup_path()
    s.write_json(marker, dict(private=str(private), keychain=str(keychain), profile=None,
        originalSearch=['/original/login.keychain-db'], searchChanged=True))
    calls=[]
    with patch.object(s, 'command', side_effect=lambda *args: calls.append(args)):
        s.cleanup()
    assert calls == [('security','list-keychains','-d','user','-s','/original/login.keychain-db'),
        ('security','delete-keychain',str(keychain))], calls
    assert not marker.exists() and not private.exists()
    s.cleanup()  # No-op on a clean runner, no credentials required.
`));

test('cleanup refuses unrelated files and never deletes a changed profile', () => python(`
with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, RUNNER_TEMP=root), patch.object(s.Path, 'home', return_value=Path(root)):
    private = Path(root)/'vibestudio-signing-test'
    private.mkdir()
    installed = Path(root)/'Library/Developer/Xcode/UserData/Provisioning Profiles/test.mobileprovision'
    installed.parent.mkdir(parents=True)
    installed.write_text('changed profile')
    meta = dict(private=str(private), keychain=str(private/'signing.keychain-db'), profile=str(installed),
        profileSha256='not-the-hash', originalSearch=[], searchChanged=False)
    s.write_json(s.cleanup_path(), meta)
    with patch.object(s, 'command', side_effect=AssertionError('Unexpected keychain access')):
        rejects(s.cleanup, 'changed')
    assert installed.read_text() == 'changed profile'
    meta['private'] = str(Path(root)/'unrelated')
    s.write_json(s.cleanup_path(), meta)
    rejects(s.cleanup, 'cleanup directory')
`));

test('an import failure still restores changes made by keychain creation', () => python(`
with tempfile.TemporaryDirectory() as root, patch.dict(os.environ, RUNNER_TEMP=root,
    GITHUB_ACTIONS='true', RUNNER_ENVIRONMENT='github-hosted', IOS_CERTIFICATE='Y2VydA==',
    IOS_MOBILE_PROVISION='cHJvZmlsZQ==', IOS_CERTIFICATE_PASSWORD='test-only'), \\
    patch.object(s.sys, 'platform', 'darwin'), patch.object(s, 'profile_from', return_value=profile):
    calls=[]
    def fake_command(*args):
        calls.append(args)
        if args == ('security','list-keychains','-d','user'):
            return b'"/original/login.keychain-db"'
        if args[1] == 'create-keychain':
            assert json.loads(s.cleanup_path().read_text())['searchChanged'] is True
            Path(args[-1]).touch()
        if args[1] == 'import': raise RuntimeError('simulated import failure')
        return b''
    with patch.object(s, 'command', side_effect=fake_command):
        try: s.prepare(Path(root)/'release')
        except RuntimeError as error: assert 'import failure' in str(error)
        else: raise AssertionError('Expected failed import')
        s.cleanup()
    assert ('security','list-keychains','-d','user','-s','/original/login.keychain-db') in calls
    assert any(args[1] == 'delete-keychain' for args in calls)
    assert not list(Path(root).iterdir())
`));

test('preparation cannot touch a personal or self-hosted keychain', () => python(`
for env in [{}, {'GITHUB_ACTIONS':'true','RUNNER_ENVIRONMENT':'self-hosted'}]:
    with patch.dict(os.environ, env, clear=True), patch.object(s.sys, 'platform', 'darwin'), \\
         patch.object(s, 'command', side_effect=AssertionError('Unexpected keychain access')):
        rejects(lambda: s.prepare(Path('/unused')), 'runner' if not env else 'Self-hosted')
`));
