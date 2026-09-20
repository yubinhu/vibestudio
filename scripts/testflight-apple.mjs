#!/usr/bin/env node
// App Store Connect reconciliation for the immutable allocation owned by testflight.yml.
import { createPrivateKey, sign } from 'node:crypto';
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as sleep } from 'node:timers/promises';

const ORIGIN = 'https://api.appstoreconnect.apple.com';
const BUNDLE_ID = 'one.vibestudio.app';
export const DEFAULT_APP_ID = '6789766775';
export const DEFAULT_GROUP_ID = '22cb9d8d-337b-4c4c-a5dd-5333eaa71607';
const requireValue = (condition, message) => { if (!condition) throw new Error(message); };
const query = (path, params) => `${path}?${new URLSearchParams(params)}`;
const resourceId = id => {
  requireValue(typeof id === 'string' && /^[A-Za-z0-9-]+$/.test(id), 'Invalid Apple resource ID');
  return id;
};

export function validateAllocation(value) {
  requireValue(value && /^\d+\.\d+\.\d+$/.test(value.version) && value.tag === `v${value.version}`,
    'Allocation tag and marketing version do not match');
  requireValue(typeof value.build === 'string' && /^[1-9]\d{0,3}$/.test(value.build),
    'Allocation build must be a numeric string between 1 and 9999');
  requireValue(/^[a-f0-9]{40}$/.test(value.commit), 'Allocation commit must be a full SHA');
  requireValue(/^\d+$/.test(String(value.runId)) && /^\d+$/.test(String(value.runNumber)),
    'Allocation must identify its GitHub run');
  return Object.fromEntries(['tag', 'commit', 'version', 'build', 'runId', 'runNumber'].map(key => [key, value[key]]));
}

export function appleUrl(path) {
  const url = new URL(path, ORIGIN);
  requireValue(url.origin === ORIGIN && url.pathname.startsWith('/v1/') && !url.username && !url.password && !url.hash,
    'Refusing an unexpected App Store Connect URL');
  return url.href;
}

export function createToken({ keyId, issuer, privateKey }, now = Date.now()) {
  const encode = value => Buffer.from(JSON.stringify(value)).toString('base64url');
  const iat = Math.floor(now / 1000);
  const data = `${encode({ alg: 'ES256', kid: keyId, typ: 'JWT' })}.${encode({ iss: issuer, iat, exp: iat + 300, aud: 'appstoreconnect-v1' })}`;
  return `${data}.${sign('sha256', Buffer.from(data), { key: privateKey, dsaEncoding: 'ieee-p1363' }).toString('base64url')}`;
}

// Only GET requests retry. A failed write may have succeeded remotely: reconcile on rerun.
export function createAppleApi(credentials, { fetchFn = fetch, wait = sleep, now = Date.now, tokenFn = createToken } = {}) {
  return async function request(method, path, body) {
    const url = appleUrl(path);
    const label = `${method} ${new URL(url).pathname}`;
    for (let attempt = 0; attempt < 4; attempt++) {
      let response;
      try {
        response = await fetchFn(url, {
          method, redirect: 'error', signal: AbortSignal.timeout(30_000),
          headers: { Authorization: `Bearer ${tokenFn(credentials, now())}`, 'Content-Type': 'application/json' },
          ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        });
      } catch {
        throw new Error(`${label} failed or timed out. ${method === 'GET' ? 'Rerun to reconcile.' : 'The write may have succeeded; rerun to reconcile before retrying.'}`);
      }
      if (method === 'GET' && (response.status === 429 || response.status >= 500) && attempt < 3) {
        await response.body?.cancel();
        const retryAfter = Number(response.headers.get('retry-after'));
        await wait(Math.min(30_000, Math.max(1000 * 2 ** attempt, Number.isFinite(retryAfter) ? retryAfter * 1000 : 0)));
        continue;
      }
      // Never include Apple's response body in errors: it can echo request data.
      requireValue(response.ok, `${label} returned HTTP ${response.status}. ${method === 'GET' ? 'Check API access and Apple service status.' : 'Rerun to reconcile the write; it is not retried automatically.'}`);
      const text = await response.text();
      if (!text) return null;
      try { return JSON.parse(text); } catch { throw new Error(`${label} returned invalid JSON`); }
    }
  };
}

export async function listAll(request, path) {
  const data = [];
  const included = new Map();
  const visited = new Set();
  while (path) {
    const url = appleUrl(path);
    requireValue(!visited.has(url) && visited.size < 1000, 'Invalid or excessive Apple pagination');
    visited.add(url);
    const page = await request('GET', url);
    requireValue(Array.isArray(page?.data), 'Apple list response is missing data');
    data.push(...page.data);
    for (const item of page.included ?? []) included.set(`${item.type}:${item.id}`, item);
    path = page.links?.next;
  }
  return { data, included: [...included.values()] };
}

export function summarizeBuilds(response) {
  const included = new Map((response.included ?? []).map(item => [`${item.type}:${item.id}`, item]));
  return response.data.map(build => {
    const related = name => {
      const link = build.relationships?.[name]?.data;
      return link && included.get(`${link.type}:${link.id}`)?.attributes;
    };
    const version = related('preReleaseVersion');
    const detail = related('buildBetaDetail');
    return {
      id: resourceId(build.id), build: build.attributes?.version, version: version?.version, platform: version?.platform,
      processing: build.attributes?.processingState, audience: build.attributes?.buildAudienceType,
      expired: build.attributes?.expired, expirationDate: build.attributes?.expirationDate,
      internalBuildState: detail?.internalBuildState, externalBuildState: detail?.externalBuildState,
      betaReviewState: related('betaAppReviewSubmission')?.betaReviewState,
    };
  });
}

export function validateBuild(build, allocation, now = Date.now()) {
  requireValue(build.build === allocation.build && build.version === allocation.version && build.platform === 'IOS',
    `Build ${allocation.build} collides with a different marketing version or platform`);
  requireValue(build.audience === 'INTERNAL_ONLY', `Build ${build.id} has an unexpected audience; INTERNAL_ONLY is required`);
  requireValue(!build.expired && (!build.expirationDate || Date.parse(build.expirationDate) > now),
    `Build ${build.id} is expired or has an invalid expiration date; use a fresh dispatch`);
  requireValue(['PROCESSING', 'VALID'].includes(build.processing),
    `Build ${build.id} has terminal or unknown processing state ${build.processing}; use a fresh dispatch`);
  const states = [build.internalBuildState, build.externalBuildState, build.betaReviewState];
  requireValue(!states.some(state => ['EXPIRED', 'REJECTED', 'PROCESSING_EXCEPTION', 'FAILED', 'INVALID'].includes(state)),
    `Build ${build.id} was rejected, expired, or failed processing; use a fresh dispatch`);
  requireValue(build.internalBuildState !== 'MISSING_EXPORT_COMPLIANCE',
    `Build ${build.id} requires export compliance information in App Store Connect`);
  return build;
}

export function selectBuild(response, allocation, now = Date.now()) {
  const builds = summarizeBuilds(response);
  requireValue(builds.length <= 1, `Multiple Apple builds use allocation ${allocation.build}; refusing ambiguous delivery`);
  return builds.length ? validateBuild(builds[0], allocation, now) : null;
}

export function validateNewBuildNumber(build, historicalBuilds) {
  const maximum = historicalBuilds.reduce((highest, item) => {
    const version = item.attributes?.version;
    // Apple accepted this app's legacy four-component builds (1.1.0.1–1.1.0.5).
    // Only historical comparisons permit that shape; new allocations stay numeric.
    requireValue(typeof version === 'string' && /^\d{1,4}(?:\.\d{1,2}){0,3}$/.test(version),
      'An existing Apple build has an unexpected number format; review allocation manually');
    return Math.max(highest, Number(version.split('.')[0]));
  }, 0);
  requireValue(Number(build) > maximum,
    `Allocated build ${build} does not exceed Apple's current maximum ${maximum}; review the numbering baseline`);
  return maximum;
}

export function validateContext(app, group, config) {
  requireValue(app?.id === config.appId && app.attributes?.bundleId === BUNDLE_ID, 'Apple app does not match one.vibestudio.app');
  requireValue(group?.id === config.groupId && group.attributes?.name === 'Alpha' && group.attributes?.isInternalGroup === true,
    'The configured TestFlight group is not the existing internal Alpha group');
  requireValue(group.relationships?.app?.data?.id === config.appId, 'The Alpha group belongs to a different app');
  return group;
}

async function readContext(request, config) {
  const [app, group] = await Promise.all([
    request('GET', `/v1/apps/${resourceId(config.appId)}`),
    request('GET', `/v1/betaGroups/${resourceId(config.groupId)}?include=app`),
  ]);
  return validateContext(app?.data, group?.data, config);
}

async function readBuilds(request, config, allocation) {
  return listAll(request, query('/v1/builds', {
    'filter[app]': config.appId, 'filter[version]': allocation.build,
    include: 'preReleaseVersion,buildBetaDetail,betaAppReviewSubmission', limit: '200',
  }));
}

async function findBuild(request, config, allocation, now) {
  return selectBuild(await readBuilds(request, config, allocation), allocation, now());
}

export async function reconcilePriorUploads(request, config, allocation, priorUploads) {
  requireValue(Array.isArray(priorUploads), 'Prior uploads must be an array of immutable allocations');
  for (const prior of priorUploads) {
    validateAllocation(prior);
    requireValue(typeof prior.workflowRunId === 'string' && /^[1-9]\d*$/.test(prior.workflowRunId), 'Prior upload is missing its GitHub workflow run ID');
    const resume = `Rerun earlier GitHub run ${prior.workflowRunId} to reconcile its upload before starting a new build.`;
    requireValue(prior.tag === allocation.tag && prior.version === allocation.version && prior.commit === allocation.commit,
      `Prior upload source does not match the selected release. ${resume}`);
    const builds = summarizeBuilds(await readBuilds(request, config, prior));
    requireValue(builds.length <= 1, `Prior allocation ${prior.build} has multiple Apple builds. ${resume}`);
    const build = builds[0];
    requireValue(build, `Prior upload ${prior.version} (${prior.build}) is not visible in Apple. ${resume}`);
    requireValue(build.build === prior.build && build.version === prior.version && build.platform === 'IOS',
      `Prior upload ${prior.build} has a different marketing version or platform. ${resume}`);
    requireValue(build.audience === 'INTERNAL_ONLY', `Prior upload ${prior.build} has an unexpected audience. ${resume}`);
    // Terminal outcomes reconcile an earlier attempt even though that build must
    // never be delivered. A processed VALID upload is also no longer ambiguous.
    const states = [build.processing, build.internalBuildState, build.externalBuildState, build.betaReviewState];
    const terminal = build.expired === true || states.some(state => ['FAILED', 'INVALID', 'EXPIRED', 'REJECTED'].includes(state));
    requireValue(terminal || build.processing === 'VALID',
      `Prior upload ${prior.version} (${prior.build}) is still processing or has an unknown state. ${resume}`);
  }
}

export async function check({ request, config, allocation, priorUploads = [], uploadAttempted = false, now = Date.now, onState = () => {} }) {
  validateAllocation(allocation);
  await readContext(request, config);
  const build = await findBuild(request, config, allocation, now);
  requireValue(!build || uploadAttempted === true,
    `Apple build ${allocation.version} (${allocation.build}) exists without an upload attempt owned by this GitHub run; refusing a build-number collision`);
  if (!build) await reconcilePriorUploads(request, config, allocation, priorUploads);
  const maximum = build ? undefined : validateNewBuildNumber(allocation.build,
    (await listAll(request, query('/v1/builds', { 'filter[app]': config.appId, 'fields[builds]': 'version', limit: '200' }))).data);
  const state = { ...allocation, appId: config.appId, groupId: config.groupId, existing: Boolean(build), buildState: build, previousMaximum: maximum, checkedAt: new Date(now()).toISOString() };
  onState(state);
  return state;
}

export function validateNotes(value) {
  const notes = value.trim();
  requireValue(notes.length > 0 && [...notes].length <= 4000, 'Beta notes must contain 1–4000 characters');
  return notes;
}

async function saveNotes(request, buildId, notes) {
  const path = `/v1/builds/${buildId}/betaBuildLocalizations`;
  const items = (await listAll(request, path)).data.filter(item => item.attributes?.locale === 'en-US');
  requireValue(items.length <= 1, 'Multiple en-US beta note localizations exist');
  const existing = items[0];
  if (existing?.attributes.whatsNew === notes) return;
  if (existing) {
    const id = resourceId(existing.id);
    await request('PATCH', `/v1/betaBuildLocalizations/${id}`, { data: { type: 'betaBuildLocalizations', id, attributes: { whatsNew: notes } } });
  } else {
    await request('POST', '/v1/betaBuildLocalizations', { data: {
      type: 'betaBuildLocalizations', attributes: { locale: 'en-US', whatsNew: notes },
      relationships: { build: { data: { type: 'builds', id: buildId } } },
    } });
  }
}

export function validateDistribution(build, { groupMembershipVerified, betaNotesVerified, testerCount }) {
  requireValue(build.processing === 'VALID' && build.audience === 'INTERNAL_ONLY' && build.internalBuildState === 'IN_BETA_TESTING' && build.externalBuildState === 'NOT_APPLICABLE',
    `Build ${build.id} is not available for internal-only beta testing`);
  requireValue(groupMembershipVerified && betaNotesVerified, 'Alpha membership or beta notes are not verified');
  requireValue(Number.isInteger(testerCount) && testerCount > 0, 'Alpha has no existing testers; delivery is not complete');
}

export async function deliver({ request, config, allocation, notes, now = Date.now, wait = sleep,
  timeoutMs = 20 * 60_000, intervalMs = 30_000, onState = () => {}, log = console.log }) {
  validateAllocation(allocation);
  notes = validateNotes(notes);
  const deadline = now() + timeoutMs;
  let group = await readContext(request, config);
  const groupPath = `/v1/betaGroups/${resourceId(config.groupId)}`;
  const memberPath = `${groupPath}/relationships/builds?limit=200`;
  let build;
  const pause = async stage => {
    requireValue(now() < deadline, `Timed out waiting for ${stage} for ${allocation.version} (${allocation.build})${build ? `, Apple build ${build.id}` : ''}. Rerun this workflow to resume the same build.`);
    log(`Waiting for ${stage}: ${allocation.version} (${allocation.build})${build ? `, ${build.id}` : ''}`);
    await wait(Math.min(intervalMs, deadline - now()));
  };
  const snapshot = () => onState({ ...allocation, appId: config.appId, groupId: config.groupId, existing: Boolean(build), buildState: build, checkedAt: new Date(now()).toISOString() });
  while (true) {
    build = await findBuild(request, config, allocation, now);
    snapshot();
    if (build?.processing === 'VALID') break;
    await pause('Apple processing');
  }
  // Notes are saved before assigning a newly available build to the existing audience.
  await saveNotes(request, build.id, notes);
  const isMember = async () => (await listAll(request, memberPath)).data.some(item => item.id === build.id);
  if (!await isMember()) {
    if (!group.attributes.hasAccessToAllBuilds) log('Alpha no longer has automatic access; assigning this build explicitly.');
    await request('POST', `${groupPath}/relationships/builds`, { data: [{ type: 'builds', id: build.id }] });
  }
  while (true) {
    build = await findBuild(request, config, allocation, now);
    snapshot();
    requireValue(build, 'Apple build disappeared during delivery; rerun to reconcile');
    const [groupMembershipVerified, localizations] = await Promise.all([
      isMember(), listAll(request, `/v1/builds/${build.id}/betaBuildLocalizations`),
    ]);
    const betaNotesVerified = localizations.data.some(item => item.attributes?.locale === 'en-US' && item.attributes.whatsNew === notes);
    if (build.internalBuildState === 'IN_BETA_TESTING' && groupMembershipVerified && betaNotesVerified) {
      group = await readContext(request, config);
      // Linkage IDs are sufficient to count testers; personal details are never requested.
      const testers = await listAll(request, `${groupPath}/relationships/betaTesters?limit=200`);
      const testerCount = new Set(testers.data.map(item => item.id)).size;
      const verification = { groupMembershipVerified, betaNotesVerified, testerCount };
      validateDistribution(build, verification);
      return { ...allocation, appId: config.appId, buildId: build.id, processing: build.processing, audience: build.audience,
        internalBuildState: build.internalBuildState, externalBuildState: build.externalBuildState,
        groupId: config.groupId, groupName: 'Alpha', hasAccessToAllBuilds: group.attributes.hasAccessToAllBuilds,
        ...verification, verifiedAt: new Date(now()).toISOString() };
    }
    await pause('Alpha TestFlight availability');
  }
}

export async function main(env = process.env, command = process.argv[2]) {
  requireValue(['check', 'deliver'].includes(command), 'Usage: node scripts/testflight-apple.mjs check|deliver');
  for (const name of ['APPLE_API_KEY', 'APPLE_API_ISSUER', 'APPLE_API_PRIVATE_KEY', 'RELEASE_STATE_DIR']) {
    requireValue(env[name], `Missing required environment variable ${name}`);
  }
  const allocation = validateAllocation(JSON.parse(readFileSync(resolve(env.RELEASE_STATE_DIR, 'allocation.json'), 'utf8')));
  let privateKey;
  try { privateKey = createPrivateKey(env.APPLE_API_PRIVATE_KEY); } catch { throw new Error('APPLE_API_PRIVATE_KEY is not a valid private key'); }
  requireValue(privateKey.asymmetricKeyType === 'ec' && privateKey.asymmetricKeyDetails?.namedCurve === 'prime256v1', 'Apple API key must be an ES256 private key');
  const request = createAppleApi({ keyId: env.APPLE_API_KEY, issuer: env.APPLE_API_ISSUER, privateKey });
  const config = { appId: env.TESTFLIGHT_APP_ID || DEFAULT_APP_ID, groupId: env.TESTFLIGHT_GROUP_ID || DEFAULT_GROUP_ID };
  const write = (name, value) => writeFileSync(resolve(env.RELEASE_STATE_DIR, name), `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
  const args = { request, config, allocation, onState: state => write('apple-state.json', state) };
  if (command === 'check') {
    const priorPath = resolve(env.RELEASE_STATE_DIR, 'prior-uploads.json');
    const priorUploads = existsSync(priorPath) ? JSON.parse(readFileSync(priorPath, 'utf8')) : [];
    const uploadPath = resolve(env.RELEASE_STATE_DIR, 'upload-state.json');
    const uploadState = existsSync(uploadPath) ? JSON.parse(readFileSync(uploadPath, 'utf8')) : { uploadAttempted: false };
    requireValue(typeof uploadState?.uploadAttempted === 'boolean', 'Upload state is missing its uploadAttempted boolean');
    const state = await check({ ...args, priorUploads, uploadAttempted: uploadState.uploadAttempted });
    if (env.GITHUB_OUTPUT) appendFileSync(env.GITHUB_OUTPUT, `existing=${state.existing}\n`);
    console.log(`Apple preflight: ${allocation.version} (${allocation.build}) ${state.existing ? 'already exists; resume delivery' : 'is clear for upload'}.`);
  } else {
    const receipt = await deliver({ ...args, notes: readFileSync(resolve(env.RELEASE_STATE_DIR, 'beta-notes.txt'), 'utf8') });
    write('receipt.json', receipt);
    const message = `TestFlight ${receipt.version} (${receipt.build}) verified in internal Alpha (${receipt.testerCount} existing tester(s)).`;
    console.log(message);
    if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY, `${message}\n\nSource: \`${receipt.commit}\`. Apple build: \`${receipt.buildId}\`. Beta notes and group membership verified.\n`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main().catch(error => { console.error(`TestFlight: ${error.message}`); process.exitCode = 1; });
}
