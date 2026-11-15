import test from 'node:test';
import assert from 'node:assert/strict';
import { verifiedCiRun } from './release-ci.mjs';

const sha = 'a'.repeat(40);
const passed = { id: 100, run_number: 10, run_attempt: 2, head_sha: sha,
  event: 'push', head_branch: 'main', head_repository: { full_name: 'owner/repo' },
  status: 'completed', conclusion: 'success', html_url: 'https://example.test/run/100' };
const jobs = ['Dashboard JS tests', 'Rust fmt + clippy', 'Rust tests'].map(name =>
  ({ name, status: 'completed', conclusion: 'success' }));
function verify(runs, jobRows = jobs, inProgressRunId, allowFailedAggregate = false) {
  const calls = [];
  const listRuns = Symbol('runs'), listJobs = Symbol('jobs');
  const github = { rest: { actions: { listWorkflowRuns: listRuns,
    listJobsForWorkflowRunAttempt: listJobs } },
  paginate: async (method, args) => {
    calls.push(args);
    if (method === listRuns) {
      assert.equal(args.workflow_id, 'ci.yml');
      assert.equal(args.head_sha, sha);
      assert.equal(args.event, 'push');
      assert.equal(args.branch, 'main');
      return runs;
    }
    assert.equal(method, listJobs);
    assert.equal(args.run_id, passed.id);
    assert.equal(args.attempt_number, passed.run_attempt);
    return jobRows;
  } };
  return { result: verifiedCiRun({
    github, owner: 'owner', repo: 'repo', sha, inProgressRunId, allowFailedAggregate,
  }), calls };
}

test('uses exact commit and latest successful attempt with all required jobs', async () => {
  const { result, calls } = verify([{ ...passed, id: 1, run_number: 1 }, passed]);
  assert.equal((await result).id, 100);
  assert.equal(calls.length, 2);
});
test('rejects unrelated commits, pull requests, branches and foreign repositories', async () => {
  for (const patch of [{ head_sha: 'b'.repeat(40) }, { event: 'pull_request' },
    { head_branch: 'feature' }, { head_repository: { full_name: 'fork/repo' } }]) {
    await assert.rejects(verify([{ ...passed, ...patch }]).result, /Exact tagged commit/);
  }
  await assert.rejects(verify([]).result, /Exact tagged commit/);
});
test('older success cannot hide latest failure, cancellation or unfinished CI', async () => {
  for (const patch of [{ conclusion: 'failure' }, { conclusion: 'cancelled' },
    { status: 'in_progress', conclusion: null }]) {
    await assert.rejects(verify([passed, { ...passed, id: 101, run_number: 11, ...patch }]).result,
      /Exact tagged commit/);
  }
});
test('missing, skipped and failed required jobs refuse release', async () => {
  for (const name of jobs.map(job => job.name)) {
    await assert.rejects(verify([passed], jobs.filter(job => job.name !== name)).result, /Required CI job/);
    for (const conclusion of ['failure', 'skipped', 'cancelled']) {
      await assert.rejects(verify([passed], jobs.map(job => job.name === name ? { ...job, conclusion } : job)).result,
        /Required CI job/);
    }
  }
});

test('integrated release accepts only its own running CI after every prerequisite passes', async () => {
  const running = { ...passed, status: 'in_progress', conclusion: null };
  assert.equal((await verify([running], jobs, passed.id).result).id, passed.id);
  await assert.rejects(verify([running], jobs, passed.id + 1).result, /Exact tagged commit/);
  for (const conclusion of ['failure', 'skipped', null]) {
    await assert.rejects(verify([running], jobs.map((job, index) => index === 0
      ? { ...job, conclusion } : job), passed.id).result, /Required CI job/);
  }
  await assert.rejects(verify([{ ...running, status: 'queued' }], jobs, passed.id).result,
    /Exact tagged commit/);
});

test('manual release retry accepts aggregate release failure only after the required jobs pass', async () => {
  const releaseFailed = { ...passed, conclusion: 'failure' };
  assert.equal((await verify([releaseFailed], jobs, undefined, true).result).id, passed.id);
  assert.equal((await verify([{ ...releaseFailed, conclusion: 'cancelled' }], jobs, undefined, true).result).id,
    passed.id);
  await assert.rejects(
    verify([releaseFailed], jobs.map((job, index) => index === 1
      ? { ...job, conclusion: 'failure' } : job), undefined, true).result,
    /Required CI job/,
  );
  await assert.rejects(
    verify([{ ...releaseFailed, status: 'in_progress' }], jobs, undefined, true).result,
    /Exact tagged commit/,
  );
  await assert.rejects(
    verify([releaseFailed, { ...releaseFailed, id: 101, run_number: 11, status: 'in_progress', conclusion: null }],
      jobs, undefined, true).result,
    /Exact tagged commit/,
  );
  await assert.rejects(
    verify([{ ...releaseFailed, conclusion: 'timed_out' }], jobs, undefined, true).result,
    /Exact tagged commit/,
  );
});
