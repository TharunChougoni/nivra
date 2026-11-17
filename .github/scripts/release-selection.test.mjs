import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { selectReleaseTag } from './release-selection.mjs';

test('only a stable annotated version tag on the tested commit selects a release', () => {
  const selection = { version: '0.9.3', sha: 'a'.repeat(40), tagCommit: 'a'.repeat(40), tagType: 'tag' };
  assert.equal(selectReleaseTag(selection), 'v0.9.3');
  assert.equal(selectReleaseTag({ ...selection, tagCommit: undefined }), '');
  assert.equal(selectReleaseTag({ ...selection, tagCommit: 'b'.repeat(40) }), '');
  assert.throws(() => selectReleaseTag({ ...selection, tagType: 'commit' }), /annotated/);
  for (const version of ['0.9.3-rc1', '01.9.3', '0.9.3\ntag=bad']) {
    assert.throws(() => selectReleaseTag({ ...selection, version }), /stable workspace version/);
  }
});

test('CI selector handles absent, lightweight, matching and old tags in a real checkout', () => {
  const dir = mkdtempSync(join(tmpdir(), 'nivra-release-selection-'));
  const script = fileURLToPath(new URL('./release-selection.mjs', import.meta.url));
  const git = (...args) => execFileSync('git', ['-c', 'user.name=Fixture', '-c',
    'user.email=fixture@example.test', ...args], { cwd: dir, encoding: 'utf8' }).trim();
  const output = join(dir, 'step-output');
  const select = sha => {
    writeFileSync(output, '');
    const run = spawnSync(process.execPath, [script], { cwd: dir, encoding: 'utf8',
      env: { ...process.env, GITHUB_SHA: sha, GITHUB_OUTPUT: output } });
    return { ...run, output: readFileSync(output, 'utf8') };
  };
  try {
    git('init', '-q');
    writeFileSync(join(dir, 'Cargo.toml'), '[workspace.package]\nversion = "0.9.3"\n');
    git('add', 'Cargo.toml');
    git('commit', '-qm', 'fixture');
    const sha = git('rev-parse', 'HEAD');
    assert.equal(select(sha).output, 'tag=\n');
    git('tag', 'v0.9.3');
    assert.notEqual(select(sha).status, 0);
    git('tag', '-d', 'v0.9.3');
    git('tag', '-a', 'v0.9.3', '-m', 'fixture release');
    assert.equal(select(sha).status, 0);
    assert.equal(select(sha).output, 'tag=v0.9.3\n');
    git('commit', '--allow-empty', '-qm', 'next source change');
    assert.equal(select(git('rev-parse', 'HEAD')).output, 'tag=\n');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
