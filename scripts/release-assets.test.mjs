import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { parse } from 'yaml';
import { legacyReleaseAssets, parseReleaseAssets, serverAssetName, serverTargets } from './release-assets.mjs';

const root = fileURLToPath(new URL('..', import.meta.url));
const policy = JSON.parse(readFileSync(join(root, 'release-assets.json'), 'utf8'));
const release = parse(readFileSync(join(root, '.github/workflows/release.yml'), 'utf8'));
const smoke = parse(readFileSync(join(root, '.github/workflows/provision-smoke.yml'), 'utf8'));

test('the policy reserves the approved human, update, and server filenames', () => {
  assert.deepEqual(parseReleaseAssets(policy), {
    schemaVersion: 1,
    installers: { macos: 'VibeStudio-macos.dmg', windows: 'VibeStudio-windows.exe', linux: 'VibeStudio-linux.deb' },
    macosUpdater: 'autoupdate-macos-universal.app.tar.gz',
    servers: {
      'aarch64-apple-darwin': 'server-macos-arm64',
      'x86_64-apple-darwin': 'server-macos-x64',
      'aarch64-unknown-linux-musl': 'server-linux-arm64-musl',
      'x86_64-unknown-linux-musl': 'server-linux-x64-musl',
    },
  });
  assert.deepEqual(release.jobs['server-binaries'].strategy.matrix.include.map(item => item.target).sort(), [...serverTargets].sort());
  assert.deepEqual(smoke.jobs.smoke.strategy.matrix.include.map(item => item.target).sort(), [...serverTargets].sort());
});

test('invalid policies cannot supply filenames to release tooling', () => {
  for (const mutate of [
    value => { value.schemaVersion = 2; },
    value => { delete value.servers[serverTargets[0]]; },
    value => { value.servers[serverTargets[0]] = '../server'; },
    value => { value.servers[serverTargets[0]] = 'server\ninjected=value'; },
    value => { value.servers[serverTargets[0]] = value.installers.macos; },
    value => { value.servers[serverTargets[0]] = 'latest.json'; },
  ]) {
    const bad = structuredClone(policy);
    mutate(bad);
    assert.throws(() => parseReleaseAssets(bad));
  }
  assert.throws(() => serverAssetName('unsupported-target', policy), /Unsupported server target/);
});

test('actual packaging and smoke scripts agree on new and historical filenames and checksums', () => {
  const packageScript = release.jobs['server-binaries'].steps.find(step => step.name === 'Package + checksum').run;
  const resolveScript = smoke.jobs.smoke.steps.find(step => step.name === 'Resolve server asset name').run;
  const verifyScript = smoke.jobs.smoke.steps.find(step => step.name === 'Verify checksum').run;
  const temp = mkdtempSync(join(tmpdir(), 'vibestudio-asset-names-'));
  try {
    for (const historical of [false, true]) {
      for (const target of serverTargets) {
        const cwd = join(temp, `${historical ? 'legacy' : 'current'}-${target}`);
        const source = join(cwd, 'target', target, 'release', 'skill-server');
        mkdirSync(dirname(source), { recursive: true });
        writeFileSync(source, `test server payload: ${target}\n`);
        if (!historical) {
          mkdirSync(join(cwd, 'scripts'));
          copyFileSync(join(root, 'release-assets.json'), join(cwd, 'release-assets.json'));
          copyFileSync(join(root, 'scripts/release-assets.mjs'), join(cwd, 'scripts/release-assets.mjs'));
        }
        const outputEnv = join(cwd, 'release.env');
        const env = { ...process.env, TARGET: target, GITHUB_ENV: outputEnv };
        execFileSync('bash', ['-e', '-o', 'pipefail', '-c', packageScript], { cwd, env });
        const expected = historical ? legacyReleaseAssets.servers[target] : policy.servers[target];
        const packaged = readFileSync(outputEnv, 'utf8');
        assert.equal(packaged, `SERVER_ASSET=${expected}\n`);
        assert.ok(existsSync(join(cwd, expected)));
        assert.match(readFileSync(join(cwd, `${expected}.sha256`), 'utf8'), new RegExp(` ${expected}\\n$`));

        writeFileSync(outputEnv, '');
        execFileSync('bash', ['-e', '-o', 'pipefail', '-c', resolveScript], { cwd, env });
        assert.equal(readFileSync(outputEnv, 'utf8'), packaged);
        execFileSync('bash', ['-e', '-o', 'pipefail', '-c', verifyScript], {
          cwd, env: { ...env, SERVER_ASSET: expected },
        });
      }
    }
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
});

test('upload, download, and boot steps consume the resolved filename', () => {
  const upload = release.jobs['server-binaries'].steps.find(step => step.name === 'Upload to the release').run;
  const download = smoke.jobs.smoke.steps.find(step => step.name === 'Download skill-server asset').run;
  const boot = smoke.jobs.smoke.steps.find(step => step.name === 'Smoke-test the binary').run;
  assert.ok(upload.includes('"$SERVER_ASSET" "$SERVER_ASSET.sha256"'));
  assert.ok(download.includes('--pattern "$SERVER_ASSET"'));
  assert.ok(download.includes('--pattern "$SERVER_ASSET.sha256"'));
  assert.ok(boot.includes('bin="./$SERVER_ASSET"'));
  assert.ok(smoke.jobs.smoke.steps.some(step => step.with?.ref === 'refs/tags/${{ inputs.tag }}'));
});
