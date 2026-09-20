import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { allocationFor, validatePublishedRelease, validateCi, validateAllocation, retryArtifacts, validatePackage, uploadWasAttempted } from './testflight-release.mjs';

const input = { tag: 'v1.2.11', commit: '1'.repeat(40), runId: '5000', runNumber: '1' };
const allocation = allocationFor(input);
test('one workflow run owns a stable, bounded Apple build number', () => {
  assert.equal(allocation.build, '1001');
  assert.equal(allocation.version, '1.2.11');
  for (const tag of ['master', 'v1.2.3-rc1', 'v01.2.3', '--help', 'v1.2.3\n']) {
    assert.throws(() => allocationFor({ ...input, tag }));
  }
  assert.throws(() => allocationFor({ ...input, runNumber: '9000' }));
});
test('only the selected public stable release and successful exact-commit CI can ship', () => {
  const release = { tag_name: input.tag, published_at: '2026-09-20', draft: false, prerelease: false };
  validatePublishedRelease(release, input.tag);
  for (const delta of [{ draft: true }, { prerelease: true }, { published_at: null }, { tag_name: 'v1.2.10' }]) {
    assert.throws(() => validatePublishedRelease({ ...release, ...delta }, input.tag));
  }
  const run = { id: 42, head_sha: input.commit, head_branch: 'master', event: 'push',
    head_repository: { full_name: 'owner/repo' }, status: 'completed', conclusion: 'success' };
  assert.equal(validateCi([run], input.commit, 'owner/repo'), 42);
  for (const delta of [{ conclusion: 'failure' }, { status: 'in_progress' }, { event: 'pull_request' },
    { head_sha: '2'.repeat(40) }, { head_repository: { full_name: 'fork/repo' } }]) {
    assert.throws(() => validateCi([{ ...run, ...delta }], input.commit, 'owner/repo'));
  }
  assert.throws(() => validateCi([{ ...run, conclusion: 'failure' }, run], input.commit, 'owner/repo'));
});
test('reruns fail closed on missing, expired, conflicting or changed allocation', () => {
  assert.deepEqual(retryArtifacts([], '1'), { allocation: undefined, ipa: undefined });
  assert.throws(() => retryArtifacts([], '2'));
  const record = { name: 'testflight-allocation', expired: false };
  assert.equal(retryArtifacts([record], '2').allocation, record);
  assert.throws(() => retryArtifacts([record], '2', true), /original IPA artifact is missing/);
  const ipa = { name: 'testflight-ipa', expired: false };
  assert.equal(retryArtifacts([record, ipa], '2', true).ipa, ipa);
  assert.throws(() => retryArtifacts([{ ...record, expired: true }], '2'));
  assert.throws(() => retryArtifacts([record, record], '2'));
  for (const key of ['commit', 'tag', 'version', 'build', 'runId', 'runNumber']) {
    assert.throws(() => validateAllocation({ ...allocation, [key]: 'changed' }, allocation));
  }
});
test('an upload retry must use the recorded IPA bytes and source allocation', () => {
  const bytes = Buffer.from('signed package');
  const record = { ...allocation, sha256: createHash('sha256').update(bytes).digest('hex') };
  validatePackage(record, allocation, bytes);
  assert.throws(() => validatePackage(record, allocation, Buffer.from('replacement')));
  assert.throws(() => validatePackage({ ...record, commit: '2'.repeat(40) }, allocation, bytes));
});
test('fresh dispatch checks uploads from failed runs, including earlier attempts', () => {
  assert.equal(uploadWasAttempted([{ steps: [{ name: 'Archive the tagged iOS app', started_at: 'now', conclusion: 'failure' }] }]), false);
  const step = { name: 'Upload to App Store Connect', started_at: 'now', conclusion: 'failure' };
  assert.equal(uploadWasAttempted([{ steps: [{ ...step, conclusion: 'skipped' }] }]), false);
  assert.equal(uploadWasAttempted([{ steps: [step] }, { steps: [{ ...step, conclusion: 'skipped' }] }]), true);
  assert.equal(uploadWasAttempted([{ steps: [{ ...step, conclusion: 'success' }] }]), true);
});
