import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { loadReleaseConfiguration, planRelease, serverTargets } from './finalize-release.mjs';

const encode = value => Buffer.from(value).toString('base64');
const keyId = Buffer.from('12345678');
const pubkey = encode(`untrusted comment: test public key\n${encode(Buffer.concat([Buffer.from('Ed'), keyId, Buffer.alloc(32)]))}\n`);
const signature = encode(`untrusted comment: test signature\n${encode(Buffer.concat([Buffer.from('ED'), keyId, Buffer.alloc(64)]))}\ntrusted comment: test\n${encode(Buffer.alloc(64))}\n`);
const assetNames = JSON.parse(readFileSync(new URL('../release-assets.json', import.meta.url), 'utf8'));
const encodedFile = value => ({ type: 'file', encoding: 'base64', content: encode(JSON.stringify(value)) });

function fixture() {
  const names = [
    'VibeStudio_1.2.3_universal.dmg', 'VibeStudio_1.2.3_amd64.deb',
    'VibeStudio_1.2.3_x64-setup.exe', 'VibeStudio_universal.app.tar.gz',
  ];
  const signed = names.slice(1).map(name => `${name}.sig`);
  const assets = [...names, ...signed, ...serverTargets.flatMap(target => [
    assetNames.servers[target], `${assetNames.servers[target]}.sha256`,
  ])].map((name, id) => ({ name, id, size: 100, state: 'uploaded' }));
  return {
    repo: 'yubinhu/vibestudio', tag: 'v1.2.3', pubkey, assetNames,
    release: { tag_name: 'v1.2.3', draft: true, created_at: '2026-09-05T00:00:00Z', body: 'Release notes', assets },
    signatures: Object.fromEntries(signed.map(name => [name, signature])),
  };
}

function applyRename(input, rename) {
  const asset = input.release.assets.find(a => a.id === rename.id);
  if (input.signatures[asset.name]) {
    input.signatures[rename.name] = input.signatures[asset.name];
    delete input.signatures[asset.name];
  }
  asset.name = rename.name;
}

test('reads naming policy and updater key from the same immutable release commit', () => {
  const input = fixture();
  const sha = 'b'.repeat(40);
  const requests = [];
  const configuration = loadReleaseConfiguration({ ...input, readJson: endpoint => {
    requests.push(endpoint);
    if (endpoint.includes('/commits/')) return { sha };
    if (endpoint.includes('tauri.conf.json')) return encodedFile({ plugins: { updater: { pubkey } } });
    return encodedFile(assetNames);
  } });
  assert.deepEqual(configuration, { pubkey, assetNames });
  assert.deepEqual(requests, [
    `repos/${input.repo}/commits/refs/tags/${input.tag}`,
    `repos/${input.repo}/contents/client/desktop/tauri.conf.json?ref=${sha}`,
    `repos/${input.repo}/contents/release-assets.json?ref=${sha}`,
  ]);
});

test('requires a valid naming policy at the release commit, including when GitHub returns 404', () => {
  const read = policyResult => endpoint => {
    if (endpoint.includes('/commits/')) return { sha: 'b'.repeat(40) };
    if (endpoint.includes('tauri.conf.json')) return encodedFile({ plugins: { updater: { pubkey } } });
    if (policyResult instanceof Error) throw policyResult;
    return policyResult;
  };
  for (const error of [new Error('network unavailable'),
    ...[404, 401, 403, 429, 500].map(status => Object.assign(new Error('API error'), { status }))]) {
    assert.throws(() => loadReleaseConfiguration({ ...fixture(), readJson: read(error) }), error);
  }
  for (const invalid of [undefined, { type: 'dir' }, encodedFile({}),
    encodedFile({ ...assetNames, schemaVersion: 2 }),
    { type: 'file', encoding: 'base64', content: encode('{malformed') }]) {
    assert.throws(() => loadReleaseConfiguration({ ...fixture(), readJson: read(invalid) }));
  }
  assert.throws(() => planRelease({ ...fixture(), assetNames: undefined }), /naming policy/);
});

test('uses the tagged signing key even after the default branch key changes', () => {
  const input = fixture();
  const sha = 'a'.repeat(40);
  const currentKey = encode(`untrusted comment: current public key\n${encode(Buffer.concat([Buffer.from('Ed'), Buffer.from('87654321'), Buffer.alloc(32)]))}\n`);
  assert.throws(() => planRelease({ ...input, pubkey: currentKey }), /signing key/);
  const readJson = endpoint => {
    if (endpoint.includes('/commits/')) return { sha };
    if (endpoint.includes('release-assets.json')) return encodedFile(assetNames);
    const key = endpoint.endsWith(`?ref=${sha}`) ? pubkey : currentKey;
    const content = encode(JSON.stringify({ plugins: { updater: { pubkey: key } } }));
    return { type: 'file', encoding: 'base64', content: content.match(/.{1,60}/g).join('\n') + '\n' };
  };
  const configuration = loadReleaseConfiguration({ ...input, readJson });
  assert.equal(planRelease({ ...input, ...configuration }).manifest.version, '1.2.3');
});

test('requires a release commit and its signing key', () => {
  for (const sha of [undefined, 'master', 'v1.2.3', 'a'.repeat(39)]) {
    assert.throws(() => loadReleaseConfiguration({ ...fixture(), readJson: () => ({ sha }) }), /release commit/);
  }
  const sha = 'a'.repeat(40);
  for (const file of [
    { type: 'dir' },
    { type: 'file', encoding: 'base64', content: encode('{}') },
  ]) {
    assert.throws(() => loadReleaseConfiguration({
      ...fixture(), readJson: endpoint => endpoint.includes('/commits/') ? { sha } : file,
    }), /release commit/);
  }
});

test('uses human installers and the dedicated autoupdate package on all eight platforms', () => {
  const { manifest, renames } = planRelease(fixture());
  assert.equal(manifest.version, '1.2.3');
  assert.deepEqual(renames.map(rename => rename.name).sort(), [
    'VibeStudio-macos.dmg', 'VibeStudio-linux.deb', 'VibeStudio-linux.deb.sig',
    'VibeStudio-windows.exe', 'VibeStudio-windows.exe.sig',
    'autoupdate-macos-universal.app.tar.gz', 'autoupdate-macos-universal.app.tar.gz.sig',
  ].sort());
  const base = 'https://github.com/yubinhu/vibestudio/releases/download/v1.2.3/';
  assert.deepEqual(Object.keys(manifest.platforms).sort(), [
    'darwin-aarch64', 'darwin-x86_64', 'darwin-aarch64-app', 'darwin-x86_64-app',
    'linux-x86_64', 'linux-x86_64-deb', 'windows-x86_64', 'windows-x86_64-nsis',
  ].sort());
  for (const [platform, value] of Object.entries(manifest.platforms)) {
    const filename = platform.startsWith('darwin') ? 'autoupdate-macos-universal.app.tar.gz'
      : platform.startsWith('linux') ? 'VibeStudio-linux.deb' : 'VibeStudio-windows.exe';
    assert.deepEqual(value, { signature, url: base + filename });
  }
});

test('reruns tolerate either payload or signature being renamed first', () => {
  const first = planRelease(fixture());
  for (const rename of first.renames) {
    const partial = fixture();
    applyRename(partial, rename);
    assert.deepEqual(planRelease(partial).manifest, first.manifest);
  }
  for (const renames of [first.renames, [...first.renames].reverse()]) {
    const input = fixture();
    for (const rename of renames) {
      applyRename(input, rename);
      assert.deepEqual(planRelease(input).manifest, first.manifest);
    }
    input.previous = first.manifest;
    assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
  }
});

test('requires every signature sidecar even with a complete prior manifest', () => {
  const finalized = fixture();
  const first = planRelease(finalized);
  for (const rename of first.renames) applyRename(finalized, rename);
  finalized.previous = first.manifest;
  for (const name of Object.keys(finalized.signatures)) {
    const input = structuredClone(finalized);
    input.release.assets = input.release.assets.filter(a => a.name !== name);
    assert.throws(() => planRelease(input), /exactly one fully uploaded .*\.sig/);
  }
});

test('published releases remain unchanged and never rename payloads or sidecars', () => {
  const input = fixture();
  const first = planRelease(input);
  input.release.draft = false;
  assert.throws(() => planRelease(input), /published release/);
  for (const rename of first.renames.filter(rename => !rename.name.endsWith('.sig'))) applyRename(input, rename);
  assert.throws(() => planRelease(input), /published release/, 'Sidecar-only renames are forbidden too');
  for (const rename of first.renames.filter(rename => rename.name.endsWith('.sig'))) applyRename(input, rename);
  input.previous = first.manifest;
  assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
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
  for (const bad of [undefined, '', 'not-base64', encode('bad signature'), signature.replace(/^./, '!')]) {
    const input = fixture();
    input.signatures['VibeStudio_1.2.3_amd64.deb.sig'] = bad;
    assert.throws(() => planRelease(input));
  }
  const input = fixture();
  input.pubkey = encode(`untrusted comment: wrong key\n${encode(Buffer.concat([Buffer.from('Ed'), Buffer.from('87654321'), Buffer.alloc(32)]))}\n`);
  assert.throws(() => planRelease(input), /signing key/);
});

test('rejects branch names, mismatched tags, invalid versions and repository inputs', () => {
  for (const tag of ['master', 'v0.1', 'v01.2.3', 'v1.2.3-rc.1', 'v1.2.3\nextra']) {
    assert.throws(() => planRelease({ ...fixture(), tag }));
  }
  assert.throws(() => planRelease({ ...fixture(), repo: 'owner/repo/extra' }));
  assert.throws(() => planRelease({ ...fixture(), tag: 'v1.2.4' }), /tag mismatch/);
});
