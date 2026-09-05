#!/usr/bin/env node
// One writer for release filenames and latest.json, after every build completes.
// GH_REPO=owner/repo TAG=v1.2.3 node scripts/finalize-release.mjs [--check]
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export const serverTargets = [
  'aarch64-apple-darwin', 'x86_64-apple-darwin',
  'aarch64-unknown-linux-musl', 'x86_64-unknown-linux-musl',
];

function validateInputs(repo, tag) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo ?? '')) throw new Error('Expected GH_REPO=owner/repo');
  if (!/^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(tag ?? '')) {
    throw new Error('Expected TAG=vX.Y.Z (a stable version tag)');
  }
}

function decode64(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9+/]+={0,2}$/.test(value)) {
    throw new Error('Invalid base64 signing data');
  }
  const bytes = Buffer.from(value, 'base64');
  if (bytes.toString('base64') !== value) throw new Error('Invalid base64 signing data');
  return bytes;
}

// Catch missing/malformed signatures and a mistakenly restored DIFFERENT key.
// The updater performs cryptographic verification against the actual payload.
function validateSignature(signature, pubkey) {
  const lines = decode64(signature).toString('utf8').trim().split(/\r?\n/);
  const key = decode64(decode64(pubkey).toString('utf8').trim().split(/\r?\n/)[1]);
  const record = decode64(lines[1]);
  if (lines.length !== 4 || !lines[0].startsWith('untrusted comment: ')
    || !lines[2].startsWith('trusted comment: ') || record.length !== 74
    || !['Ed', 'ED'].includes(record.subarray(0, 2).toString())
    || decode64(lines[3]).length !== 64 || key.length !== 42
    || !record.subarray(2, 10).equals(key.subarray(2, 10))) {
    throw new Error('Invalid updater signature or signing key does not match tauri.conf.json');
  }
  return signature;
}

export function planRelease({ repo, tag, release, signatures, previous, pubkey }) {
  validateInputs(repo, tag);
  if (release.tag_name !== tag) throw new Error('Release tag mismatch');
  const assets = release.assets;
  const one = (predicate, label) => {
    const matches = assets.filter(predicate);
    if (matches.length !== 1 || matches[0].size <= 0 || matches[0].state !== 'uploaded') {
      throw new Error(`Expected exactly one fully uploaded ${label}`);
    }
    return matches[0];
  };
  for (const target of serverTargets) {
    for (const suffix of ['', '.sha256']) {
      const name = `skill-server-${target}${suffix}`;
      one(a => a.name === name, name);
    }
  }
  const version = tag.slice(1);
  const bundles = [
    { names: [`VibeStudio_${version}_universal.dmg`, 'VibeStudio-macOS.dmg'], stable: 'VibeStudio-macOS.dmg', platforms: [] },
    { names: [`VibeStudio_${version}_amd64.deb`, 'VibeStudio-Linux-x86_64.deb'], stable: 'VibeStudio-Linux-x86_64.deb', platforms: ['linux-x86_64', 'linux-x86_64-deb'] },
    { names: [`VibeStudio_${version}_x64-setup.exe`, 'VibeStudio-Windows-x64-setup.exe'], stable: 'VibeStudio-Windows-x64-setup.exe', platforms: ['windows-x86_64', 'windows-x86_64-nsis'] },
    { names: ['VibeStudio_universal.app.tar.gz'], stable: 'VibeStudio_universal.app.tar.gz', platforms: ['darwin-aarch64', 'darwin-x86_64', 'darwin-aarch64-app', 'darwin-x86_64-app'] },
  ];
  const base = `https://github.com/${repo}/releases/download/${tag}/`;
  const renames = [];
  const platforms = {};
  for (const bundle of bundles) {
    const asset = one(a => bundle.names.includes(a.name), bundle.stable);
    if (asset.name !== bundle.stable) renames.push({ id: asset.id, name: bundle.stable });
    if (!bundle.platforms.length) continue;
    const sidecars = assets.filter(a => bundle.names.some(name => a.name === `${name}.sig`));
    let signature;
    if (sidecars.length) {
      const sidecar = one(a => sidecars.includes(a), `${bundle.stable}.sig`);
      signature = signatures[sidecar.name]?.trim();
      if (sidecar.name !== `${bundle.stable}.sig`) renames.push({ id: sidecar.id, name: `${bundle.stable}.sig` });
    } else {
      // Old releases deleted their .sig assets. Reuse only signatures attached to
      // this exact repository/tag/payload in a complete matching prior manifest.
      if (previous?.version !== version) throw new Error(`Missing signature for ${asset.name}`);
      const entries = bundle.platforms.map(platform => previous.platforms?.[platform]);
      signature = entries[0]?.signature;
      if (!entries.every(entry => entry?.signature === signature && entry?.url === base + asset.name)) {
        throw new Error(`Missing or inconsistent prior manifest entries for ${asset.name}`);
      }
    }
    validateSignature(signature, pubkey);
    for (const platform of bundle.platforms) platforms[platform] = { signature, url: base + bundle.stable };
  }
  const pubDate = previous?.version === version ? previous.pub_date : release.created_at;
  if (!pubDate || !Number.isFinite(Date.parse(pubDate))) throw new Error('Missing valid release date');
  return { renames, manifest: { version, notes: release.body ?? '', pub_date: pubDate, platforms } };
}

function main() {
  const { GH_REPO: repo, TAG: tag } = process.env;
  validateInputs(repo, tag);
  const gh = (...args) => execFileSync('gh', args, { encoding: 'utf8', maxBuffer: 10 * 1024 * 1024 });
  // JSON lines work with older gh versions too (before `api --slurp`).
  const list = endpoint => gh('api', '--paginate', '--jq', '.[] | @json', endpoint)
    .trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
  const release = list(`repos/${repo}/releases?per_page=100`).find(r => r.tag_name === tag);
  if (!release) throw new Error(`Release ${tag} does not exist`);
  release.assets = list(`repos/${repo}/releases/${release.id}/assets?per_page=100`);
  const readAsset = asset => gh('api', '-H', 'Accept: application/octet-stream', `repos/${repo}/releases/assets/${asset.id}`);
  const signatures = Object.fromEntries(release.assets.filter(a => a.name.endsWith('.sig')).map(a => [a.name, readAsset(a)]));
  const previousAsset = release.assets.find(a => a.name === 'latest.json');
  const previous = previousAsset ? JSON.parse(readAsset(previousAsset)) : undefined;
  const config = JSON.parse(readFileSync(new URL('../client/desktop/tauri.conf.json', import.meta.url), 'utf8'));
  const plan = planRelease({ repo, tag, release, signatures, previous, pubkey: config.plugins.updater.pubkey });
  console.log(`Validated ${tag}: 3 installers, universal macOS updater, 4 servers, 8 updater platforms.`);
  if (process.argv.includes('--check')) {
    console.log(JSON.stringify(plan, null, 2));
    return;
  }
  // No mutations until the full release has passed validation. Keep sidecars so
  // retries can always reconstruct the manifest, even after a partial rename.
  for (const { id, name } of plan.renames) {
    gh('api', '-X', 'PATCH', `repos/${repo}/releases/assets/${id}`, '-f', `name=${name}`);
    console.log(`Renamed asset ${id} to ${name}`);
  }
  if (JSON.stringify(previous) === JSON.stringify(plan.manifest)) {
    console.log('Updater manifest is already current.');
    return;
  }
  const temp = mkdtempSync(join(tmpdir(), 'vibestudio-release-'));
  try {
    const path = join(temp, 'latest.json');
    writeFileSync(path, `${JSON.stringify(plan.manifest, null, 2)}\n`);
    gh('release', 'upload', tag, path, '--repo', repo, '--clobber');
  } finally {
    rmSync(temp, { recursive: true, force: true });
  }
  console.log('Uploaded complete latest.json; publication state unchanged.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) main();
