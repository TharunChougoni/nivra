// Fail closed: reuse only the latest push-main CI attempt for the tagged SHA.
export async function verifiedCiRun({
  github,
  owner,
  repo,
  sha,
  inProgressRunId,
  allowFailedAggregate = false,
}) {
  const runs = await github.paginate(github.rest.actions.listWorkflowRuns, {
    owner, repo, workflow_id: 'ci.yml', event: 'push', branch: 'main', head_sha: sha, per_page: 100,
  });
  const matching = runs.filter(run => run.head_sha === sha && run.event === 'push'
    && run.head_branch === 'main' && run.head_repository?.full_name === `${owner}/${repo}`);
  matching.sort((a, b) => b.run_number - a.run_number || b.run_attempt - a.run_attempt);
  const run = matching[0];
  // The integrated release job verifies its completed prerequisites while the
  // containing run is in progress. Manual release retries may reuse those
  // exact-source jobs after the release job itself failed, never an in-flight
  // run or an attempt whose required jobs failed.
  const sameRun = run && inProgressRunId === run.id && run.status === 'in_progress';
  const completedForRetry = allowFailedAggregate && run?.status === 'completed'
    && ['success', 'failure', 'cancelled'].includes(run.conclusion);
  if (!run || (!sameRun && run.status !== 'completed')
    || (!sameRun && run.conclusion !== 'success' && !completedForRetry)) {
    throw new Error('Exact tagged commit needs successful latest push-main CI. Wait for CI or rerun failed CI, then dispatch the existing tag.');
  }
  const jobs = await github.paginate(github.rest.actions.listJobsForWorkflowRunAttempt, {
    owner, repo, run_id: run.id, attempt_number: run.run_attempt, per_page: 100,
  });
  for (const name of ['Dashboard JS tests', 'pi agent tests', 'Rust fmt + clippy', 'Rust tests']) {
    const job = jobs.find(job => job.name === name);
    if (!job || job.status !== 'completed' || job.conclusion !== 'success') {
      throw new Error(`Required CI job did not pass: ${name}`);
    }
  }
  return run;
}
