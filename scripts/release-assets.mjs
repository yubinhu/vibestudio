// The release commit owns its filenames. Older tags without a policy use the
// original names; the maintained finalizer must never migrate published assets.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const serverTargets = [
  'aarch64-apple-darwin', 'x86_64-apple-darwin',
  'aarch64-unknown-linux-musl', 'x86_64-unknown-linux-musl',
];

export const legacyReleaseAssets = {
  schemaVersion: 1,
  installers: {
    macos: 'VibeStudio-macOS.dmg',
    windows: 'VibeStudio-Windows-x64-setup.exe',
    linux: 'VibeStudio-Linux-x86_64.deb',
  },
  macosUpdater: 'VibeStudio_universal.app.tar.gz',
  servers: Object.fromEntries(serverTargets.map(target => [target, `skill-server-${target}`])),
};

export function parseReleaseAssets(value) {
  const record = input => input && typeof input === 'object' && !Array.isArray(input);
  const exactKeys = (input, keys) => record(input)
    && Object.keys(input).length === keys.length && keys.every(key => Object.hasOwn(input, key));
  const filename = (name, suffix) => typeof name === 'string'
    && /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(name) && name.endsWith(suffix);
  if (!exactKeys(value, ['schemaVersion', 'installers', 'macosUpdater', 'servers'])
    || value.schemaVersion !== 1
    || !exactKeys(value.installers, ['macos', 'windows', 'linux'])
    || !filename(value.installers.macos, '.dmg')
    || !filename(value.installers.windows, '.exe')
    || !filename(value.installers.linux, '.deb')
    || !filename(value.macosUpdater, '.app.tar.gz')
    || !exactKeys(value.servers, serverTargets)
    || !serverTargets.every(target => filename(value.servers[target], ''))
  ) throw new Error('Invalid release asset naming policy or unsupported schemaVersion');
  const names = [...Object.values(value.installers), value.macosUpdater, ...Object.values(value.servers)];
  const allNames = [...names, ...Object.values(value.servers).map(name => `${name}.sha256`),
    ...[value.installers.windows, value.installers.linux, value.macosUpdater].map(name => `${name}.sig`), 'latest.json'];
  if (new Set(allNames).size !== allNames.length) throw new Error('Duplicate release asset filenames');
  return value;
}

export function serverAssetName(target, policy) {
  if (!serverTargets.includes(target)) throw new Error(`Unsupported server target: ${target}`);
  return parseReleaseAssets(policy).servers[target];
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (process.argv.length !== 3) throw new Error('Usage: node scripts/release-assets.mjs <rust-target>');
  const policy = JSON.parse(readFileSync(fileURLToPath(new URL('../release-assets.json', import.meta.url)), 'utf8'));
  console.log(serverAssetName(process.argv[2], policy));
}
