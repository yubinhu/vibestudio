import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { loadReleaseConfiguration, loadReleasePublicKey, planRelease, readGitHubJson, serverTargets } from './finalize-release.mjs';
import { legacyReleaseAssets } from './release-assets.mjs';

const encode = value => Buffer.from(value).toString('base64');
const keyId = Buffer.from('12345678');
const pubkey = encode(`untrusted comment: test public key\n${encode(Buffer.concat([Buffer.from('Ed'), keyId, Buffer.alloc(32)]))}\n`);
const signature = encode(`untrusted comment: test signature\n${encode(Buffer.concat([Buffer.from('ED'), keyId, Buffer.alloc(64)]))}\ntrusted comment: test\n${encode(Buffer.alloc(64))}\n`);
const newReleaseAssets = JSON.parse(readFileSync(new URL('../release-assets.json', import.meta.url), 'utf8'));
const encodedFile = value => ({ type: 'file', encoding: 'base64', content: encode(JSON.stringify(value)) });

function fixture(assetNames = legacyReleaseAssets) {
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
  const input = fixture(newReleaseAssets);
  const sha = 'b'.repeat(40);
  const requests = [];
  const configuration = loadReleaseConfiguration({ ...input, readJson: endpoint => {
    requests.push(endpoint);
    if (endpoint.includes('/commits/')) return { sha };
    if (endpoint.includes('tauri.conf.json')) return encodedFile({ plugins: { updater: { pubkey } } });
    return encodedFile(newReleaseAssets);
  } });
  assert.deepEqual(configuration, { pubkey, assetNames: newReleaseAssets });
  assert.deepEqual(requests, [
    `repos/${input.repo}/commits/refs/tags/${input.tag}`,
    `repos/${input.repo}/contents/client/desktop/tauri.conf.json?ref=${sha}`,
    `repos/${input.repo}/contents/release-assets.json?ref=${sha}`,
  ]);
});

test('only a genuine missing policy file falls back to legacy release filenames', () => {
  const read = policyResult => endpoint => {
    if (endpoint.includes('/commits/')) return { sha: 'b'.repeat(40) };
    if (endpoint.includes('tauri.conf.json')) return encodedFile({ plugins: { updater: { pubkey } } });
    if (policyResult instanceof Error) throw policyResult;
    return policyResult;
  };
  const missing = Object.assign(new Error('Not Found'), { status: 404 });
  assert.deepEqual(loadReleaseConfiguration({ ...fixture(), readJson: read(missing) }), {
    pubkey, assetNames: legacyReleaseAssets,
  });
  for (const error of [new Error('404 Not Found'), new Error('network unavailable'),
    ...[401, 403, 429, 500, '404'].map(status => Object.assign(new Error('API error'), { status }))]) {
    assert.throws(() => loadReleaseConfiguration({ ...fixture(), readJson: read(error) }), error);
  }
  for (const invalid of [undefined, { type: 'dir' }, encodedFile({}),
    encodedFile({ ...newReleaseAssets, schemaVersion: 2 }),
    { type: 'file', encoding: 'base64', content: encode('{malformed') }]) {
    assert.throws(() => loadReleaseConfiguration({ ...fixture(), readJson: read(invalid) }));
  }
  assert.throws(() => loadReleaseConfiguration({ ...fixture(), readJson: endpoint => {
    if (endpoint.includes('/commits/')) return { sha: 'b'.repeat(40) };
    throw missing;
  } }), missing, 'A missing signing config must not use the naming-policy fallback');
});

test('GitHub JSON reads distinguish actual HTTP errors from process and transport errors', () => {
  assert.deepEqual(readGitHubJson(() => 'HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n{"ok":true}', '/file'), { ok: true });
  for (const status of [404, 403, 500]) {
    const failure = Object.assign(new Error('gh failed'), {
      status: 1, stdout: `HTTP/2.0 ${status} Error\nContent-Type: application/json\n\n{"message":"Error"}`,
    });
    assert.throws(() => readGitHubJson(() => { throw failure; }, '/file'), error => error.status === status);
  }
  const failure = Object.assign(new Error('404 Not Found'), { status: 1, stderr: 'gh: Not Found (HTTP 404)' });
  assert.throws(() => readGitHubJson(() => { throw failure; }, '/file'), error => error.status === 1);
  failure.stdout = 'transport failure\nHTTP/2.0 404 Not Found';
  assert.throws(() => readGitHubJson(() => { throw failure; }, '/file'), error => error.status === 1);
  for (const response of ['{}', 'HTTP/2.0 200 OK\n\ninvalid json', 'HTTP/2.0 404 Not Found\n\n{}']) {
    assert.throws(() => readGitHubJson(() => response, '/file'));
  }
});

test('finalizes an older tag with its own key after the default branch key changes', () => {
  const input = fixture();
  const sha = 'a'.repeat(40);
  const currentKey = encode(`untrusted comment: current public key\n${encode(Buffer.concat([Buffer.from('Ed'), Buffer.from('87654321'), Buffer.alloc(32)]))}\n`);
  assert.throws(() => planRelease({ ...input, pubkey: currentKey }), /signing key/);
  const requests = [];
  const readJson = endpoint => {
    requests.push(endpoint);
    if (endpoint === `repos/${input.repo}/commits/refs/tags/${input.tag}`) return { sha };
    const key = endpoint.endsWith(`?ref=${sha}`) ? pubkey : currentKey;
    const content = encode(JSON.stringify({ plugins: { updater: { pubkey: key } } }));
    return { type: 'file', encoding: 'base64', content: content.match(/.{1,60}/g).join('\n') + '\n' };
  };
  input.pubkey = loadReleasePublicKey({ ...input, readJson });
  assert.equal(planRelease(input).manifest.version, '1.2.3');
  assert.deepEqual(requests, [
    `repos/${input.repo}/commits/refs/tags/${input.tag}`,
    `repos/${input.repo}/contents/client/desktop/tauri.conf.json?ref=${sha}`,
  ]);
});

test('does not fall back to the checkout when a release commit or its key is missing', () => {
  for (const sha of [undefined, 'master', 'v1.2.3', 'a'.repeat(39)]) {
    assert.throws(() => loadReleasePublicKey({ ...fixture(), readJson: () => ({ sha }) }), /release commit/);
  }
  const sha = 'a'.repeat(40);
  for (const file of [
    { type: 'dir' },
    { type: 'file', encoding: 'base64', content: encode('{}') },
  ]) {
    assert.throws(() => loadReleasePublicKey({
      ...fixture(), readJson: endpoint => endpoint.includes('/commits/') ? { sha } : file,
    }), /release commit/);
  }
});

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

test('new releases use human installers and dedicated autoupdate package URLs on all eight platforms', () => {
  const { manifest, renames } = planRelease(fixture(newReleaseAssets));
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

test('new drafts recover desktop assets already finalized under the legacy naming scheme', () => {
  const input = fixture(newReleaseAssets);
  const expected = planRelease(input).manifest;
  for (const rename of planRelease(fixture()).renames) applyRename(input, rename);
  const plan = planRelease(input);
  assert.deepEqual(plan.manifest, expected);
  assert.equal(plan.renames.length, 7);
  assert.ok(plan.renames.every(rename => !rename.name.startsWith('server-')));
});

for (const [label, policy] of [['legacy', legacyReleaseAssets], ['new', newReleaseAssets]]) {
  test(`${label}: reruns tolerate either payload or signature being renamed first`, () => {
    const first = planRelease(fixture(policy));
    for (const rename of first.renames) {
      const partial = fixture(policy);
      applyRename(partial, rename);
      assert.deepEqual(planRelease(partial).manifest, first.manifest);
    }
    for (const renames of [first.renames, [...first.renames].reverse()]) {
      const input = fixture(policy);
      for (const rename of renames) {
        applyRename(input, rename);
        assert.deepEqual(planRelease(input).manifest, first.manifest);
      }
      input.previous = first.manifest;
      assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
    }
  });

  test(`${label}: releases without sidecars retain a complete canonical manifest`, () => {
    const input = fixture(policy);
    const first = planRelease(input);
    for (const rename of first.renames) applyRename(input, rename);
    input.release.assets = input.release.assets.filter(a => !a.name.endsWith('.sig'));
    input.previous = first.manifest;
    assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
    delete input.previous.platforms['darwin-x86_64-app'];
    assert.throws(() => planRelease(input), /inconsistent prior manifest/);
  });

  test(`${label}: signatureless retry repairs the manifest after any completed payload rename`, () => {
    const input = fixture(policy);
    const first = planRelease(input);
    input.previous = structuredClone(first.manifest);
    for (const [platform, entry] of Object.entries(input.previous.platforms)) {
      const raw = platform.startsWith('darwin') ? 'VibeStudio_universal.app.tar.gz'
        : platform.startsWith('linux') ? 'VibeStudio_1.2.3_amd64.deb' : 'VibeStudio_1.2.3_x64-setup.exe';
      entry.url = `https://github.com/${input.repo}/releases/download/${input.tag}/${raw}`;
    }
    input.release.assets = input.release.assets.filter(a => !a.name.endsWith('.sig'));
    assert.deepEqual(planRelease(input).manifest, first.manifest);
    for (const rename of first.renames.filter(rename => !rename.name.endsWith('.sig'))) {
      applyRename(input, rename);
      assert.deepEqual(planRelease(input).manifest, first.manifest);
    }
    assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
  });

  test(`${label}: published releases remain unchanged and never rename payloads or sidecars`, () => {
    const input = fixture(policy);
    const first = planRelease(input);
    input.release.draft = false;
    assert.throws(() => planRelease(input), /published release/);
    for (const rename of first.renames.filter(rename => !rename.name.endsWith('.sig'))) applyRename(input, rename);
    assert.throws(() => planRelease(input), /published release/, 'Sidecar-only renames are forbidden too');
    for (const rename of first.renames.filter(rename => rename.name.endsWith('.sig'))) applyRename(input, rename);
    input.previous = first.manifest;
    assert.deepEqual(planRelease(input), { renames: [], manifest: first.manifest });
  });
}

test('maintained finalizer leaves a published historical release unchanged despite checkout naming changes', () => {
  const input = fixture();
  const first = planRelease(input);
  for (const rename of first.renames) applyRename(input, rename);
  input.previous = first.manifest;
  input.release.draft = false;
  const configuration = loadReleaseConfiguration({ ...input, readJson: endpoint => {
    if (endpoint.includes('/commits/')) return { sha: 'a'.repeat(40) };
    if (endpoint.includes('tauri.conf.json')) return encodedFile({ plugins: { updater: { pubkey } } });
    throw Object.assign(new Error('Not Found'), { status: 404 });
  } });
  assert.deepEqual(planRelease({ ...input, ...configuration }), { renames: [], manifest: first.manifest });
});

test('missing, duplicate, empty, or incomplete artifacts fail before a plan is returned', () => {
  for (const policy of [legacyReleaseAssets, newReleaseAssets]) {
    for (const name of fixture(policy).release.assets.map(a => a.name)) {
      for (const defect of ['missing', 'duplicate', 'empty', 'uploading']) {
        const input = fixture(policy);
        const asset = input.release.assets.find(a => a.name === name);
        if (defect === 'missing') input.release.assets = input.release.assets.filter(a => a !== asset);
        if (defect === 'duplicate') input.release.assets.push({ ...asset, id: 1000 });
        if (defect === 'empty') asset.size = 0;
        if (defect === 'uploading') asset.state = 'starter';
        assert.throws(() => planRelease(input), undefined, `${name}: ${defect}`);
      }
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
