// GitHub-side source selection and durable retry identity. No Apple credentials.
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

export function allocationFor({ tag, commit, runId, runNumber }) {
  if (!/^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(tag)) {
    throw new Error('Select an existing stable vX.Y.Z release tag.');
  }
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new Error('Invalid release commit.');
  if (!/^[1-9][0-9]*$/.test(String(runId)) || !/^[1-9][0-9]*$/.test(String(runNumber))) {
    throw new Error('Missing GitHub run identity.');
  }
  const build = 1000 + Number(runNumber);
  if (!Number.isSafeInteger(build) || build > 9999) throw new Error('Build-number range exhausted; migrate the allocator.');
  return { tag, commit, version: tag.slice(1), build: String(build), runId: String(runId), runNumber: String(runNumber) };
}

export function validatePublishedRelease(release, tag) {
  if (release.tag_name !== tag || release.draft || release.prerelease || !release.published_at) {
    throw new Error('TestFlight requires a published, stable GitHub release.');
  }
}

export function validateCi(runs, commit, repository) {
  const run = runs.find(x => x.head_sha === commit && x.head_branch === 'master' &&
    x.event === 'push' && x.head_repository?.full_name === repository);
  if (!run || run.status !== 'completed' || run.conclusion !== 'success') {
    throw new Error(`The latest master CI for ${commit} must have completed successfully.`);
  }
  return run.id;
}

export function validateAllocation(saved, expected) {
  for (const key of Object.keys(expected)) {
    if (saved[key] !== expected[key]) throw new Error(`Retry allocation mismatch: ${key}. Start a fresh dispatch.`);
  }
}

export function retryArtifacts(artifacts, attempt, uploadAttempted = false) {
  const select = name => {
    const matches = artifacts.filter(x => x.name === name);
    if (matches.length > 1 || matches.some(x => x.expired)) {
      throw new Error(`Ambiguous or expired ${name} artifact. Reconcile Apple state and start a fresh dispatch.`);
    }
    return matches[0];
  };
  const allocation = select('testflight-allocation');
  const ipa = select('testflight-ipa');
  if ((Number(attempt) > 1 || ipa) && !allocation) {
    throw new Error('Retry allocation is missing. Reconcile Apple state and start a fresh dispatch.');
  }
  if (uploadAttempted && !ipa) {
    throw new Error('An upload was attempted but its original IPA artifact is missing. Reconcile Apple state; rebuilding this allocation is forbidden.');
  }
  return { allocation, ipa };
}

export function validatePackage(record, allocation, bytes) {
  validateAllocation(record, allocation);
  if (record.sha256 !== createHash('sha256').update(bytes).digest('hex')) {
    throw new Error('Restored IPA does not match its recorded SHA-256.');
  }
}

export function uploadWasAttempted(jobs) {
  return jobs.some(job => job.steps?.some(step => step.name === 'Upload to App Store Connect' &&
    step.started_at && step.conclusion !== 'skipped'));
}

async function github(path) {
  const response = await fetch(`https://api.github.com${path}`, {
    headers: { Authorization: `Bearer ${process.env.GH_TOKEN}`, Accept: 'application/vnd.github+json',
      'X-GitHub-Api-Version': '2022-11-28' }, signal: AbortSignal.timeout(30_000),
  });
  if (!response.ok) throw new Error(`GitHub request failed (${response.status}): ${path}`);
  return response.json();
}

async function runJobs(repository, runId) {
  const jobs = [];
  for (let page = 1; ; page++) {
    const result = await github(`/repos/${repository}/actions/runs/${runId}/jobs?filter=all&per_page=100&page=${page}`);
    jobs.push(...result.jobs);
    if (result.jobs.length < 100) return jobs;
  }
}

function output(name, value) {
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `${name}=${value}\n`);
}

async function preflight() {
  const env = process.env;
  const repository = env.GITHUB_REPOSITORY;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? '')) throw new Error('Invalid GitHub repository.');
  const toolsRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  const tag = env.RELEASE_TAG;
  // Validate before allowing the tag to reach git revision parsing or a URL.
  allocationFor({ tag, commit: '0'.repeat(40), runId: env.GITHUB_RUN_ID, runNumber: env.GITHUB_RUN_NUMBER });
  const git = args => execFileSync('git', ['-C', toolsRoot, ...args], { encoding: 'utf8' }).trim();
  const commit = git(['rev-parse', `refs/tags/${tag}^{commit}`]);
  git(['merge-base', '--is-ancestor', commit, 'origin/master']);
  const release = await github(`/repos/${repository}/releases/tags/${tag}`);
  validatePublishedRelease(release, tag);
  const ci = await github(`/repos/${repository}/actions/workflows/ci.yml/runs?head_sha=${commit}&event=push&per_page=100`);
  const ciRun = validateCi(ci.workflow_runs, commit, repository);
  const expected = allocationFor({ tag, commit, runId: env.GITHUB_RUN_ID, runNumber: env.GITHUB_RUN_NUMBER });
  const state = env.RELEASE_STATE_DIR;
  if (!state) throw new Error('RELEASE_STATE_DIR is required.');
  mkdirSync(state, { recursive: true });
  const artifacts = [];
  for (let page = 1; ; page++) {
    const result = await github(`/repos/${repository}/actions/runs/${env.GITHUB_RUN_ID}/artifacts?per_page=100&page=${page}`);
    artifacts.push(...result.artifacts);
    if (result.artifacts.length < 100) break;
  }
  const uploadAttempted = uploadWasAttempted(await runJobs(repository, env.GITHUB_RUN_ID));
  const saved = retryArtifacts(artifacts, env.GITHUB_RUN_ATTEMPT, uploadAttempted);
  writeFileSync(join(state, 'upload-state.json'), JSON.stringify({ uploadAttempted }) + '\n');
  const download = name => execFileSync('gh', ['run', 'download', env.GITHUB_RUN_ID,
    '--repo', repository, '--name', name, '--dir', state], { stdio: 'inherit' });
  if (saved.allocation) {
    download('testflight-allocation');
    validateAllocation(JSON.parse(readFileSync(join(state, 'allocation.json'))), expected);
    if (!existsSync(join(state, 'beta-notes.txt'))) throw new Error('Retry beta notes are missing.');
  } else {
    writeFileSync(join(state, 'allocation.json'), JSON.stringify(expected, null, 2) + '\n');
    const notes = `${tag}\n\n${release.body || 'VibeStudio improvements and fixes.'}\n\nSource: ${commit}`;
    writeFileSync(join(state, 'beta-notes.txt'), notes.slice(0, 3900) + '\n');
  }
  if (saved.ipa) {
    download('testflight-ipa');
    validatePackage(JSON.parse(readFileSync(join(state, 'package.json'))), expected,
      readFileSync(join(state, 'ipa', 'VibeStudio.ipa')));
  }
  // A fresh dispatch must not hide an earlier upload whose result is uncertain.
  // Apple preflight reconciles these exact allocations before permitting a new one.
  const priorUploads = [];
  if (!saved.allocation) {
    for (let page = 1; ; page++) {
      const previous = await github(`/repos/${repository}/actions/workflows/testflight.yml/runs?per_page=100&page=${page}`);
      for (const run of previous.workflow_runs) {
        if (String(run.id) === expected.runId || run.display_title !== `TestFlight ${tag}` || run.conclusion === 'success') continue;
        if (!uploadWasAttempted(await runJobs(repository, run.id))) continue;
        const list = await github(`/repos/${repository}/actions/runs/${run.id}/artifacts?per_page=100`);
        const artifact = retryArtifacts(list.artifacts, 2).allocation;
        const priorDir = join(state, `prior-upload-${run.id}`);
        execFileSync('gh', ['run', 'download', String(run.id), '--repo', repository,
          '--name', artifact.name, '--dir', priorDir], { stdio: 'inherit' });
        const prior = JSON.parse(readFileSync(join(priorDir, 'allocation.json')));
        validateAllocation(prior, allocationFor({ tag, commit, runId: run.id, runNumber: run.run_number }));
        priorUploads.push({ ...prior, workflowRunId: String(run.id) });
      }
      if (previous.workflow_runs.length < 100) break;
    }
    writeFileSync(join(state, 'prior-uploads.json'), JSON.stringify(priorUploads, null, 2) + '\n');
  }
  writeFileSync(join(state, 'release.json'), JSON.stringify({ version: expected.version,
    bundle: { iOS: { bundleVersion: expected.build } } }));
  for (const [key, value] of Object.entries(expected)) output(key, value);
  output('allocation_exists', Boolean(saved.allocation));
  output('ipa_exists', Boolean(saved.ipa));
  console.log(`Release ${tag} (${expected.build}), source ${commit}, verified CI run ${ciRun}.`);
}

function upload() {
  const env = process.env;
  if (!/^[A-Z0-9]{10}$/.test(env.APPLE_API_KEY ?? '') ||
      !/^[a-f0-9-]{36}$/i.test(env.APPLE_API_ISSUER ?? '') || !env.APPLE_API_PRIVATE_KEY) {
    throw new Error('App Store Connect API credentials are incomplete.');
  }
  const state = env.RELEASE_STATE_DIR;
  const allocation = JSON.parse(readFileSync(join(state, 'allocation.json')));
  const ipa = join(state, 'ipa', 'VibeStudio.ipa');
  validatePackage(JSON.parse(readFileSync(join(state, 'package.json'))), allocation, readFileSync(ipa));
  const privateDir = mkdtempSync(join(env.RUNNER_TEMP, 'testflight-api-'));
  try {
    writeFileSync(join(privateDir, `AuthKey_${env.APPLE_API_KEY}.p8`), env.APPLE_API_PRIVATE_KEY, { mode: 0o600 });
    const childEnv = { ...env, API_PRIVATE_KEYS_DIR: privateDir };
    delete childEnv.APPLE_API_PRIVATE_KEY;
    const result = spawnSync('xcrun', ['altool', '--upload-app', '-f', ipa,
      '--api-key', env.APPLE_API_KEY, '--api-issuer', env.APPLE_API_ISSUER, '--output-format', 'json'], {
      env: childEnv, encoding: 'utf8', timeout: 15 * 60_000, maxBuffer: 10 * 1024 * 1024,
    });
    writeFileSync(join(state, 'upload.json'), result.stdout || '');
    writeFileSync(join(state, 'upload.log'), result.stderr || '');
    if (result.status !== 0) throw new Error('Apple upload did not return success. Processing checks will reconcile the same build; see upload evidence.');
    console.log('Apple accepted the IPA. Processing and Alpha availability must still be verified.');
  } finally {
    rmSync(privateDir, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv[2] === 'preflight') await preflight();
    else if (process.argv[2] === 'upload') upload();
    else throw new Error('Usage: testflight-release.mjs preflight|upload');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
