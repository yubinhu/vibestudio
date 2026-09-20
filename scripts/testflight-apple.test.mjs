import assert from 'node:assert/strict';
import { generateKeyPairSync, verify } from 'node:crypto';
import test from 'node:test';
import {
  appleUrl, check, createAppleApi, createToken, DEFAULT_APP_ID, DEFAULT_GROUP_ID, deliver,
  listAll, selectBuild, validateAllocation, validateContext, validateDistribution, validateNewBuildNumber,
} from './testflight-apple.mjs';

const config = { appId: DEFAULT_APP_ID, groupId: DEFAULT_GROUP_ID };
const allocation = { tag: 'v1.2.11', commit: 'a'.repeat(40), version: '1.2.11', build: '1001', runId: '1234', runNumber: '1' };
const app = { id: config.appId, attributes: { bundleId: 'one.vibestudio.app' } };
const group = { id: config.groupId, attributes: { name: 'Alpha', isInternalGroup: true, hasAccessToAllBuilds: true }, relationships: { app: { data: { type: 'apps', id: config.appId } } } };
const currentTime = Date.parse('2026-09-20T12:00:00Z');

function buildResponse({ version = allocation.version, platform = 'IOS', build = allocation.build,
  processing = 'VALID', internal = 'IN_BETA_TESTING', external = 'NOT_APPLICABLE', audience = 'INTERNAL_ONLY',
  expired = false, expirationDate = '2026-12-01T00:00:00Z', review } = {}) {
  return {
    data: [{ type: 'builds', id: 'build-id', attributes: { version: build, processingState: processing, buildAudienceType: audience, expired, expirationDate },
      relationships: { preReleaseVersion: { data: { type: 'preReleaseVersions', id: 'version-id' } },
        buildBetaDetail: { data: { type: 'buildBetaDetails', id: 'detail-id' } },
        betaAppReviewSubmission: { data: review ? { type: 'betaAppReviewSubmissions', id: 'review-id' } : null } } }],
    included: [
      { type: 'preReleaseVersions', id: 'version-id', attributes: { version, platform } },
      { type: 'buildBetaDetails', id: 'detail-id', attributes: { internalBuildState: internal, externalBuildState: external } },
      ...(review ? [{ type: 'betaAppReviewSubmissions', id: 'review-id', attributes: { betaReviewState: review } }] : []),
    ],
  };
}

function fakeApple({ builds = [buildResponse()], history = ['10', '1.1.1'], initialMember = true,
  initialNotes = 'Test the update.', testerIds = ['tester-id'], groupValue = group } = {}) {
  const calls = [];
  let exactCalls = 0;
  let member = initialMember;
  let notes = initialNotes;
  const request = async (method, path, body) => {
    const url = new URL(path, 'https://api.appstoreconnect.apple.com');
    calls.push({ method, path: url.pathname, query: url.searchParams, body });
    if (method === 'GET') {
      if (url.pathname === `/v1/apps/${config.appId}`) return { data: app };
      if (url.pathname === `/v1/betaGroups/${config.groupId}`) return { data: groupValue };
      if (url.pathname === '/v1/builds') {
        if (url.searchParams.has('filter[version]')) return builds[Math.min(exactCalls++, builds.length - 1)];
        return { data: history.map((version, i) => ({ id: `old-${i}`, attributes: { version } })) };
      }
      if (url.pathname.endsWith('/relationships/builds')) return { data: member ? [{ type: 'builds', id: 'build-id' }] : [] };
      if (url.pathname.endsWith('/betaBuildLocalizations')) return { data: notes === null ? [] : [{ type: 'betaBuildLocalizations', id: 'notes-id', attributes: { locale: 'en-US', whatsNew: notes } }] };
      if (url.pathname.endsWith('/relationships/betaTesters')) return { data: testerIds.map(id => ({ type: 'betaTesters', id })) };
    }
    if (method === 'POST' && url.pathname.endsWith('/relationships/builds')) { member = true; return null; }
    if (['PATCH', 'POST'].includes(method) && url.pathname.startsWith('/v1/betaBuildLocalizations')) { notes = body.data.attributes.whatsNew; return { data: {} }; }
    throw new Error(`Unexpected fake request: ${method} ${url.pathname}`);
  };
  return { request, calls };
}

test('allocation rejects version mismatches, noncanonical build numbers and missing provenance', () => {
  assert.deepEqual(validateAllocation(allocation), allocation);
  for (const overrides of [{ tag: 'v1.2.10' }, { build: '10000' }, { build: '010' }, { build: 1001 }, { commit: 'master' }, { runId: undefined }]) {
    assert.throws(() => validateAllocation({ ...allocation, ...overrides }));
  }
});

test('ES256 tokens have Apple audience, bounded lifetime and a valid P1363 signature', () => {
  const { privateKey, publicKey } = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  const token = createToken({ keyId: 'test-key', issuer: 'test-issuer', privateKey }, currentTime);
  const [header, payload, signature] = token.split('.');
  assert.deepEqual(JSON.parse(Buffer.from(header, 'base64url')), { alg: 'ES256', kid: 'test-key', typ: 'JWT' });
  const claims = JSON.parse(Buffer.from(payload, 'base64url'));
  assert.equal(claims.aud, 'appstoreconnect-v1');
  assert.equal(claims.exp - claims.iat, 300);
  assert(verify('sha256', Buffer.from(`${header}.${payload}`), { key: publicKey, dsaEncoding: 'ieee-p1363' }, Buffer.from(signature, 'base64url')));
});

test('API retries only transient read responses with bounded backoff', async () => {
  const waits = [];
  const responses = [429, 503, 200];
  const request = createAppleApi({}, { tokenFn: () => 'secret-token', wait: async ms => waits.push(ms), fetchFn: async (url, options) => {
    assert.equal(options.redirect, 'error');
    assert(options.signal instanceof AbortSignal);
    const status = responses.shift();
    return new Response(JSON.stringify({ data: [] }), { status, headers: { 'retry-after': '500' } });
  } });
  assert.deepEqual(await request('GET', '/v1/builds'), { data: [] });
  assert.deepEqual(waits, [30_000, 30_000]);
});

test('API does not retry writes or print response bodies and credentials', async () => {
  let attempts = 0;
  const request = createAppleApi({}, { tokenFn: () => 'secret-token', fetchFn: async () => {
    attempts++;
    return new Response('secret-token private-notes', { status: 503 });
  } });
  await assert.rejects(request('POST', '/v1/betaBuildLocalizations', { private: 'notes' }), error => {
    assert.match(error.message, /not retried automatically/);
    assert.doesNotMatch(error.message, /secret-token|private-notes/);
    return true;
  });
  assert.equal(attempts, 1);
});

test('API bounds failed reads and rejects cross-origin pagination before authentication', async () => {
  let attempts = 0;
  const request = createAppleApi({}, { tokenFn: () => 'token', wait: async () => {}, fetchFn: async () => { attempts++; return new Response('', { status: 500 }); } });
  await assert.rejects(request('GET', '/v1/builds'), /HTTP 500/);
  assert.equal(attempts, 4);
  for (const path of ['https://example.com/v1/builds', '//example.com/v1/builds', 'http://api.appstoreconnect.apple.com/v1/builds', '/other', 'https://user@api.appstoreconnect.apple.com/v1/builds']) {
    assert.throws(() => appleUrl(path));
  }
  await assert.rejects(listAll(async () => ({ data: [], links: { next: 'https://example.com/v1/builds' } }), '/v1/builds'), /unexpected App Store Connect URL/);
});

test('pagination includes every page and refuses cycles', async () => {
  let calls = 0;
  const response = await listAll(async () => ({ data: [{ id: String(++calls) }], links: { next: calls === 1 ? '/v1/builds?cursor=2' : null } }), '/v1/builds');
  assert.deepEqual(response.data.map(item => item.id), ['1', '2']);
  await assert.rejects(listAll(async () => ({ data: [], links: { next: '/v1/builds' } }), '/v1/builds'), /pagination/);
});

test('exact allocation selection rejects duplicate and wrong-version or platform builds', () => {
  assert.equal(selectBuild(buildResponse(), allocation, currentTime).id, 'build-id');
  assert.equal(selectBuild({ data: [] }, allocation, currentTime), null);
  const duplicate = buildResponse();
  duplicate.data.push({ ...duplicate.data[0], id: 'second-id' });
  assert.throws(() => selectBuild(duplicate, allocation, currentTime), /Multiple Apple builds/);
  for (const overrides of [{ version: '1.2.10' }, { platform: 'MAC_OS' }, { build: '1002' }]) {
    assert.throws(() => selectBuild(buildResponse(overrides), allocation, currentTime), /collides/);
  }
});

test('expired, rejected, invalid and audience-mismatched builds cannot be resumed', () => {
  for (const overrides of [{ expired: true }, { expirationDate: '2026-01-01' }, { expirationDate: 'bad-date' },
    { processing: 'INVALID' }, { processing: 'FAILED' }, { internal: 'PROCESSING_EXCEPTION' }, { internal: 'EXPIRED' },
    { external: 'REJECTED' }, { review: 'REJECTED' }, { audience: 'APP_STORE_ELIGIBLE' }, { internal: 'MISSING_EXPORT_COMPLIANCE' }]) {
    assert.throws(() => selectBuild(buildResponse(overrides), allocation, currentTime));
  }
});

test('number allocation exceeds all numeric and legacy dotted leading components', () => {
  const builds = versions => versions.map(version => ({ attributes: { version } }));
  assert.equal(validateNewBuildNumber('1001', builds(['10', '999.12.2', '1.2'])), 999);
  assert.throws(() => validateNewBuildNumber('1001', builds(['1001.1'])), /does not exceed/);
  assert.throws(() => validateNewBuildNumber('1001', builds(['1002'])), /does not exceed/);
  for (const value of ['1.2.3.4.5', '1a', '10000', '2.123', undefined]) {
    assert.throws(() => validateNewBuildNumber('1001', builds([value])), /unexpected number format/);
  }
});

test('historical four-component Apple builds permit migration without weakening fresh allocation format', () => {
  const historical = ['1.1.0.1', '1.1.0.2', '1.1.0.3', '1.1.0.4', '1.1.0.5', '6', '7', '8', '9', '10'];
  assert.equal(validateNewBuildNumber('1001', historical.map(version => ({ attributes: { version } }))), 10);
  assert.throws(() => validateNewBuildNumber('1001', [{ attributes: { version: '1001.1.0.5' } }]), /does not exceed/);
  assert.throws(() => validateAllocation({ ...allocation, build: '1.1.0.6' }), /numeric string/);
});

test('group must be internal Alpha owned by the verified app', () => {
  assert.equal(validateContext(app, group, config), group);
  for (const groupValue of [{ ...group, attributes: { ...group.attributes, isInternalGroup: false } },
    { ...group, attributes: { ...group.attributes, name: 'External' } },
    { ...group, relationships: { app: { data: { id: 'other-app' } } } }]) {
    assert.throws(() => validateContext(app, groupValue, config));
  }
  assert.throws(() => validateContext({ ...app, attributes: { bundleId: 'another.app' } }, group, config));
});

test('check permits only an unused, increasing build and performs no mutations', async () => {
  const api = fakeApple({ builds: [{ data: [] }] });
  const states = [];
  const state = await check({ ...api, config, allocation, now: () => currentTime, onState: value => states.push(value) });
  assert.equal(state.existing, false);
  assert.equal(state.previousMaximum, 10);
  assert.equal(states.length, 1);
  assert(api.calls.every(call => call.method === 'GET'));
  const exact = api.calls.find(call => call.query.has('filter[version]'));
  assert.equal(exact.query.get('filter[version]'), allocation.build);
  assert.equal(exact.query.get('filter[preReleaseVersion.version]'), null, 'must also see conflicting marketing versions');
  await assert.rejects(check({ ...fakeApple({ builds: [{ data: [] }], history: ['1002'] }), config, allocation, now: () => currentTime }), /does not exceed/);
});

test('check resumes a valid allocation even after newer builds exist', async () => {
  const api = fakeApple({ history: ['1002'] });
  const state = await check({ ...api, config, allocation, uploadAttempted: true, now: () => currentTime });
  assert.equal(state.existing, true);
  assert.equal(state.buildState.id, 'build-id');
  assert(!api.calls.some(call => call.query.has('fields[builds]')));
});

test('existing exact build without this run’s upload evidence is a collision, not a retry', async () => {
  for (const uploadAttempted of [undefined, false, 'true']) {
    const api = fakeApple();
    await assert.rejects(check({ ...api, config, allocation, uploadAttempted, now: () => currentTime }), /exists without an upload attempt owned by this GitHub run/);
    assert(api.calls.every(call => call.method === 'GET'));
  }
});

test('fresh dispatch blocks earlier invisible, processing, or unknown uploads and identifies the run to resume', async () => {
  const next = { ...allocation, build: '1002', runId: '1235', runNumber: '2' };
  const priorUploads = [{ ...allocation, workflowRunId: allocation.runId }];
  for (const prior of [{ data: [] }, buildResponse({ processing: 'PROCESSING', internal: 'PROCESSING' }), buildResponse({ processing: 'UNKNOWN' })]) {
    const api = fakeApple({ builds: [{ data: [] }, prior], history: ['1001'] });
    await assert.rejects(check({ ...api, config, allocation: next, priorUploads, now: () => currentTime }), /Rerun earlier GitHub run 1234/);
    assert(api.calls.every(call => call.method === 'GET'));
    assert(!api.calls.some(call => call.query.has('fields[builds]')), 'unresolved upload must stop new-number clearance');
  }
});

test('fresh dispatch accepts processed or terminal earlier uploads without changing their delivery state', async () => {
  const next = { ...allocation, build: '1002', runId: '1235', runNumber: '2' };
  const priorUploads = [{ ...allocation, workflowRunId: allocation.runId }];
  for (const overrides of [{ internal: 'READY_FOR_BETA_TESTING' }, { processing: 'FAILED' }, { processing: 'INVALID' },
    { internal: 'EXPIRED' }, { external: 'REJECTED' }, { review: 'REJECTED' }, { expired: true }]) {
    const api = fakeApple({ builds: [{ data: [] }, buildResponse(overrides)], history: ['1001'] });
    const state = await check({ ...api, config, allocation: next, priorUploads, now: () => currentTime });
    assert.equal(state.existing, false);
    assert.equal(state.previousMaximum, 1001);
    assert.deepEqual(api.calls.filter(call => call.query.has('filter[version]')).map(call => call.query.get('filter[version]')), ['1002', '1001']);
    assert(api.calls.every(call => call.method === 'GET'));
  }
});

test('earlier upload identity and audience mismatches block fresh dispatch even after terminal processing', async () => {
  const next = { ...allocation, build: '1002', runId: '1235', runNumber: '2' };
  const priorUploads = [{ ...allocation, workflowRunId: allocation.runId }];
  for (const overrides of [{ version: '1.2.10' }, { platform: 'MAC_OS' }, { build: '999' }, { audience: 'APP_STORE_ELIGIBLE', processing: 'INVALID' }]) {
    const api = fakeApple({ builds: [{ data: [] }, buildResponse(overrides)] });
    await assert.rejects(check({ ...api, config, allocation: next, priorUploads, now: () => currentTime }), /Prior upload.*Rerun earlier GitHub run 1234/);
  }
  await assert.rejects(check({ ...fakeApple({ builds: [{ data: [] }] }), config, allocation: next,
    priorUploads: [{ ...priorUploads[0], commit: 'b'.repeat(40) }], now: () => currentTime }), /source does not match/);
});

test('resuming a current exact build does not block on unrelated older upload reconciliation', async () => {
  const api = fakeApple();
  const state = await check({ ...api, config, allocation, uploadAttempted: true, priorUploads: [{ ...allocation, workflowRunId: '1111', build: '999' }], now: () => currentTime });
  assert.equal(state.existing, true);
  assert.equal(api.calls.filter(call => call.query.has('filter[version]')).length, 1);
});

test('delivery waits for upload visibility and processing, then saves notes and assigns Alpha once', async () => {
  const api = fakeApple({ builds: [{ data: [] }, buildResponse({ processing: 'PROCESSING', internal: 'PROCESSING' }), buildResponse()], initialMember: false, initialNotes: null });
  let time = currentTime;
  const waits = [];
  const receipt = await deliver({ ...api, config, allocation, notes: 'Test the update.\n', now: () => time,
    wait: async ms => { waits.push(ms); time += ms; }, log: () => {} });
  assert.deepEqual(waits, [30_000, 30_000]);
  assert.equal(receipt.testerCount, 1);
  assert.equal(receipt.groupMembershipVerified, true);
  assert.equal(receipt.betaNotesVerified, true);
  assert.equal(receipt.commit, allocation.commit);
  assert(!JSON.stringify(receipt).includes('tester-id'));
  const writes = api.calls.filter(call => call.method !== 'GET');
  assert.deepEqual(writes.map(call => call.path), ['/v1/betaBuildLocalizations', `/v1/betaGroups/${config.groupId}/relationships/builds`]);
});

test('delivery reruns preserve existing membership and notes without mutations', async () => {
  const api = fakeApple();
  const receipt = await deliver({ ...api, config, allocation, notes: 'Test the update.', now: () => currentTime, log: () => {} });
  assert.equal(receipt.audience, 'INTERNAL_ONLY');
  assert(api.calls.every(call => call.method === 'GET'));
});

test('delivery patches existing notes and polls internal availability after processing', async () => {
  const ready = buildResponse({ internal: 'READY_FOR_BETA_TESTING' });
  const api = fakeApple({ builds: [ready, ready, buildResponse()], initialNotes: 'Old notes' });
  let time = currentTime;
  let waits = 0;
  await deliver({ ...api, config, allocation, notes: 'Updated notes', now: () => time,
    wait: async ms => { waits++; time += ms; }, log: () => {} });
  assert.equal(waits, 1);
  assert.equal(api.calls.filter(call => call.method === 'PATCH').length, 1);
});

test('pending delivery times out with the allocation and Apple build ID for reconciliation', async () => {
  const api = fakeApple({ builds: [buildResponse({ processing: 'PROCESSING', internal: 'PROCESSING' })] });
  let time = currentTime;
  await assert.rejects(deliver({ ...api, config, allocation, notes: 'Notes', now: () => time, timeoutMs: 60_000,
    wait: async ms => { time += ms; }, log: () => {} }), /Timed out.*1\.2\.11 \(1001\).*build-id.*resume/);
  assert(api.calls.every(call => call.method === 'GET'));
});

test('rejected or misdirected delivery fails before mutations', async () => {
  for (const options of [{ builds: [buildResponse({ processing: 'INVALID' })] },
    { builds: [buildResponse({ audience: 'APP_STORE_ELIGIBLE' })] },
    { groupValue: { ...group, relationships: { app: { data: { id: 'other-app' } } } } }]) {
    const api = fakeApple(options);
    await assert.rejects(deliver({ ...api, config, allocation, notes: 'Notes', now: () => currentTime, log: () => {} }));
    assert(api.calls.every(call => call.method === 'GET'));
  }
});

test('receipt requires both distribution proofs, internal-only availability and existing testers', async () => {
  const build = selectBuild(buildResponse(), allocation, currentTime);
  const proof = { groupMembershipVerified: true, betaNotesVerified: true, testerCount: 1 };
  validateDistribution(build, proof);
  for (const overrides of [{ groupMembershipVerified: false }, { betaNotesVerified: false }, { testerCount: 0 }]) {
    assert.throws(() => validateDistribution(build, { ...proof, ...overrides }));
  }
  assert.throws(() => validateDistribution({ ...build, externalBuildState: 'IN_BETA_TESTING' }, proof));
  await assert.rejects(deliver({ ...fakeApple({ testerIds: [] }), config, allocation, notes: 'Test the update.', now: () => currentTime, log: () => {} }), /no existing testers/);
});
