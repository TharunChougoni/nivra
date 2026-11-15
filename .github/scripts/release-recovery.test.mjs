import test from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { RELEASE_ASSETS, planMissingAttachments, verifyAssetFiles } from './release-recovery.mjs';

const tag = 'v0.9.4';
const sourceCommit = 'a'.repeat(40);
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function makeFiles() {
  const main = Buffer.from('static musl sadr-rs fixture');
  const analysis = Buffer.from('static musl sadr-analysis fixture');
  const prompts = Buffer.from(JSON.stringify({ 'data.md': 'default prompt fixture' }));
  return new Map([
    [RELEASE_ASSETS[0], main],
    [RELEASE_ASSETS[1], Buffer.from(`${hash(main)}  ${RELEASE_ASSETS[0]}\n`)],
    ['release.json', Buffer.from(JSON.stringify({
      tag,
      source_commit: sourceCommit,
      target: 'x86_64-unknown-linux-musl',
      sha256: hash(main),
    }))],
    ['default-prompts.json', prompts],
    ['default-prompts.sha256', Buffer.from(`${hash(prompts)}  default-prompts.json\n`)],
    [RELEASE_ASSETS[5], analysis],
    [RELEASE_ASSETS[6], Buffer.from(`${hash(analysis)}  ${RELEASE_ASSETS[5]}\n`)],
  ]);
}

test('accepts exact immutable release assets for the annotated tag commit', () => {
  assert.equal(verifyAssetFiles({ tag, sourceCommit, files: makeFiles() }).tag, tag);
  const legacy = makeFiles();
  legacy.delete('default-prompts.json');
  legacy.delete('default-prompts.sha256');
  legacy.set('release.json', Buffer.from(JSON.stringify({
    tag, source_commit: sourceCommit, target: 'x86_64-unknown-linux-musl',
    sha256: hash(legacy.get(RELEASE_ASSETS[0])),
  })));
  assert.equal(verifyAssetFiles({ tag, sourceCommit, files: legacy }).tag, tag);
});

test('rejects wrong source identity, incomplete trees and checksum mismatch', () => {
  const files = makeFiles();
  assert.throws(() => verifyAssetFiles({ tag, sourceCommit: 'b'.repeat(40), files }), /source_commit_mismatch/);
  assert.throws(() => verifyAssetFiles({ tag, sourceCommit, files: new Map([...files].slice(1)) }), /file_set_invalid/);
  const changed = makeFiles();
  changed.set(RELEASE_ASSETS[6], Buffer.from('0'.repeat(64) + `  ${RELEASE_ASSETS[5]}\n`));
  assert.throws(() => verifyAssetFiles({ tag, sourceCommit, files: changed }), /analysis_checksum_invalid/);
  const alteredPrompts = makeFiles();
  alteredPrompts.set('default-prompts.json', Buffer.from('changed'));
  assert.throws(() => verifyAssetFiles({ tag, sourceCommit, files: alteredPrompts }), /prompts_checksum_invalid/);
});

test('release retry appends missing names and byte-checks every existing asset', () => {
  const files = makeFiles();
  const release = {
    tagName: tag,
    isDraft: false,
    isPrerelease: false,
    assets: [RELEASE_ASSETS[0], RELEASE_ASSETS[2]],
  };
  const existing = new Map(release.assets.map(name => [name, files.get(name)]));
  assert.deepEqual(
    planMissingAttachments({ tag, sourceCommit, files, release, existingFiles: existing }),
    RELEASE_ASSETS.filter(name => !release.assets.includes(name)),
  );
  const altered = new Map(existing);
  altered.set(RELEASE_ASSETS[0], Buffer.from('different published binary'));
  assert.throws(
    () => planMissingAttachments({ tag, sourceCommit, files, release, existingFiles: altered }),
    /asset_mismatch/,
  );
});

test('complete release plans no uploads and prints nothing', () => {
  const directory = mkdtempSync(join(tmpdir(), 'sadr-release-recovery-'));
  const assetsDir = join(directory, 'assets');
  const existingDir = join(directory, 'existing');
  mkdirSync(assetsDir);
  mkdirSync(existingDir);
  const files = makeFiles();
  for (const [name, bytes] of files) {
    writeFileSync(join(assetsDir, name), bytes);
    writeFileSync(join(existingDir, name), bytes);
  }
  const releaseJson = join(directory, 'release.json');
  writeFileSync(releaseJson, JSON.stringify({
    tagName: tag, isDraft: false, isPrerelease: false, assets: [...RELEASE_ASSETS],
  }));
  try {
    const result = spawnSync(process.execPath, [
      fileURLToPath(new URL('./release-recovery.mjs', import.meta.url)), 'plan',
      '--tag', tag, '--source-commit', sourceCommit,
      '--directory', assetsDir, '--release-json', releaseJson,
      '--existing-directory', existingDir,
    ], { encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, '');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test('release retry refuses unexpected assets, duplicate names and a different tag', () => {
  const files = makeFiles();
  const existingFiles = new Map();
  for (const release of [
    { tagName: 'v0.9.5', isDraft: false, isPrerelease: false, assets: [] },
    { tagName: tag, isDraft: false, isPrerelease: false, assets: ['unknown.bin'] },
    { tagName: tag, isDraft: false, isPrerelease: false, assets: [RELEASE_ASSETS[0], RELEASE_ASSETS[0]] },
    { tagName: tag, isDraft: true, isPrerelease: false, assets: [] },
  ]) {
    assert.throws(
      () => planMissingAttachments({ tag, sourceCommit, files, release, existingFiles }),
    );
  }
});
