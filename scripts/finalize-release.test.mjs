import assert from 'node:assert/strict';
import test from 'node:test';
import { planRelease, serverTargets } from './finalize-release.mjs';

const encode = value => Buffer.from(value).toString('base64');
const keyId = Buffer.from('12345678');
const pubkey = encode(`untrusted comment: test public key\n${encode(Buffer.concat([Buffer.from('Ed'), keyId, Buffer.alloc(32)]))}\n`);
const signature = encode(`untrusted comment: test signature\n${encode(Buffer.concat([Buffer.from('ED'), keyId, Buffer.alloc(64)]))}\ntrusted comment: test\n${encode(Buffer.alloc(64))}\n`);

function fixture() {
  const names = [
    'VibeStudio_1.2.3_universal.dmg', 'VibeStudio_1.2.3_amd64.deb',
    'VibeStudio_1.2.3_x64-setup.exe', 'VibeStudio_universal.app.tar.gz',
  ];
  const signed = names.slice(1).map(name => `${name}.sig`);
  const assets = [...names, ...signed, ...serverTargets.flatMap(target => [
    `skill-server-${target}`, `skill-server-${target}.sha256`,
  ])].map((name, id) => ({ name, id, size: 100, state: 'uploaded' }));
  return {
    repo: 'yubinhu/vibestudio', tag: 'v1.2.3', pubkey,
    release: { tag_name: 'v1.2.3', created_at: '2026-09-05T00:00:00Z', body: 'Release notes', assets },
    signatures: Object.fromEntries(signed.map(name => [name, signature])),
  };
}

test('assembles all eight updater platforms with stable installer URLs and original signatures', () => {
  const { manifest, renames } = planRelease(fixture());
  assert.equal(manifest.version, '1.2.3');
  assert.equal(Object.keys(manifest.platforms).length, 8);
  assert.equal(renames.length, 5); // Three installers and the two renamed signature sidecars.
  const base = 'https://github.com/yubinhu/vibestudio/releases/download/v1.2.3/';
  for (const [key, value] of Object.entries(manifest.platforms)) {
    assert.equal(value.signature, signature);
    const filename = key.startsWith('darwin') ? 'VibeStudio_universal.app.tar.gz'
      : key.startsWith('linux') ? 'VibeStudio-Linux-x86_64.deb' : 'VibeStudio-Windows-x64-setup.exe';
    assert.equal(value.url, base + filename);
  }
});

test('a rerun after complete or partial renaming produces the same manifest', () => {
  const input = fixture();
  const first = planRelease(input);
  for (const rename of first.renames) {
    const asset = input.release.assets.find(a => a.id === rename.id);
    if (input.signatures[asset.name]) input.signatures[rename.name] = input.signatures[asset.name];
    asset.name = rename.name;
    assert.deepEqual(planRelease(input).manifest, first.manifest);
  }
  input.previous = first.manifest;
  assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
});

test('releases without sidecars retain a complete canonical manifest', () => {
  const input = fixture();
  const first = planRelease(input);
  for (const rename of first.renames) input.release.assets.find(a => a.id === rename.id).name = rename.name;
  input.release.assets = input.release.assets.filter(a => !a.name.endsWith('.sig'));
  input.previous = first.manifest;
  assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
  delete input.previous.platforms['darwin-x86_64-app'];
  assert.throws(() => planRelease(input), /inconsistent prior manifest/);
});

test('missing, duplicate, empty, or incomplete artifacts fail before a plan is returned', () => {
  for (const name of fixture().release.assets.map(a => a.name)) {
    for (const defect of ['missing', 'duplicate', 'empty', 'uploading']) {
      const input = fixture();
      const asset = input.release.assets.find(a => a.name === name);
      if (defect === 'missing') input.release.assets = input.release.assets.filter(a => a !== asset);
      if (defect === 'duplicate') input.release.assets.push({ ...asset, id: 1000 });
      if (defect === 'empty') asset.size = 0;
      if (defect === 'uploading') asset.state = 'starter';
      assert.throws(() => planRelease(input), undefined, `${name}: ${defect}`);
    }
  }
});

test('rejects malformed signatures and signatures from a different updater key', () => {
  for (const bad of ['', 'not-base64', encode('bad signature'), signature.replace(/^./, '!')]) {
    const input = fixture();
    input.signatures['VibeStudio_1.2.3_amd64.deb.sig'] = bad;
    assert.throws(() => planRelease(input));
  }
  const input = fixture();
  input.pubkey = encode(`untrusted comment: wrong key\n${encode(Buffer.concat([Buffer.from('Ed'), Buffer.from('87654321'), Buffer.alloc(32)]))}\n`);
  assert.throws(() => planRelease(input), /signing key/);
});

test('rejects another repository, version, or payload in an existing manifest', () => {
  for (const change of ['repo', 'version', 'payload']) {
    const input = fixture();
    input.previous = planRelease(input).manifest;
    // Use versioned filenames to test manifests before installer renaming.
    for (const entry of Object.values(input.previous.platforms)) {
      entry.url = entry.url.replace('VibeStudio-Linux-x86_64.deb', 'VibeStudio_1.2.3_amd64.deb')
        .replace('VibeStudio-Windows-x64-setup.exe', 'VibeStudio_1.2.3_x64-setup.exe');
    }
    input.release.assets = input.release.assets.filter(a => !a.name.endsWith('.sig'));
    if (change === 'version') input.previous.version = '1.2.2';
    else input.previous.platforms['linux-x86_64'].url += change === 'payload' ? '.old' : '?other-repo';
    assert.throws(() => planRelease(input));
  }
});

test('rejects branch names, mismatched tags, invalid versions and repository inputs', () => {
  for (const tag of ['master', 'v0.1', 'v01.2.3', 'v1.2.3-rc.1', 'v1.2.3\nextra']) {
    assert.throws(() => planRelease({ ...fixture(), tag }));
  }
  assert.throws(() => planRelease({ ...fixture(), repo: 'owner/repo/extra' }));
  assert.throws(() => planRelease({ ...fixture(), tag: 'v1.2.4' }), /tag mismatch/);
});
