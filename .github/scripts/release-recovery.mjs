import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const RELEASE_ASSETS = [
  'sadr-rs-x86_64-unknown-linux-musl',
  'sadr-rs-x86_64-unknown-linux-musl.sha256',
  'release.json',
  'default-prompts.json',
  'default-prompts.sha256',
  'sadr-analysis-x86_64-unknown-linux-musl',
  'sadr-analysis-x86_64-unknown-linux-musl.sha256',
];
const LEGACY_RELEASE_ASSETS = RELEASE_ASSETS.filter(name => !name.startsWith('default-prompts.'));

const MAIN_BINARY = RELEASE_ASSETS[0];
const ANALYSIS_BINARY = RELEASE_ASSETS[5];

function sha256(bytes) {
  return crypto.createHash('sha256').update(bytes).digest('hex');
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function checksum(bytes, name) {
  return `${sha256(bytes)}  ${name}\n`;
}

function assertExactNames(actual, expected, message) {
  assert(Array.isArray(actual), message);
  assert(actual.length === expected.length, message);
  assert(new Set(actual).size === actual.length, message);
  assert([...actual].sort().every((name, i) => name === [...expected].sort()[i]), message);
}

export function verifyAssetFiles({ tag, sourceCommit, files }) {
  assert(/^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(tag), 'release_tag_invalid');
  assert(/^[0-9a-f]{40}$/.test(sourceCommit), 'release_source_commit_invalid');
  assert(files instanceof Map, 'release_asset_files_invalid');
  const manifestBytes = files.get('release.json');
  let manifest;
  try {
    manifest = JSON.parse(manifestBytes.toString('utf8'));
  } catch {
    throw new Error('release_manifest_invalid');
  }
  assert(manifest && typeof manifest === 'object' && !Array.isArray(manifest), 'release_manifest_invalid');
  const hasPrompts = files.has('default-prompts.json') || files.has('default-prompts.sha256');
  const expectedAssets = hasPrompts ? RELEASE_ASSETS : LEGACY_RELEASE_ASSETS;
  assertExactNames([...files.keys()], expectedAssets, 'release_asset_file_set_invalid');
  assertExactNames(Object.keys(manifest), ['tag', 'source_commit', 'target', 'sha256'], 'release_manifest_fields_invalid');
  assert(manifest.tag === tag, 'release_tag_mismatch');
  assert(manifest.source_commit === sourceCommit, 'release_source_commit_mismatch');
  assert(manifest.target === 'x86_64-unknown-linux-musl', 'release_target_mismatch');
  assert(/^[0-9a-f]{64}$/.test(manifest.sha256), 'release_binary_sha_invalid');
  if (hasPrompts) {
    const promptHash = sha256(files.get('default-prompts.json'));
    assert(files.get('default-prompts.sha256').toString('utf8') === checksum(files.get('default-prompts.json'), 'default-prompts.json'), 'release_prompts_checksum_invalid');
    assert(/^[0-9a-f]{64}$/.test(promptHash), 'release_prompts_sha_invalid');
  }

  const mainBinary = files.get(MAIN_BINARY);
  const analysisBinary = files.get(ANALYSIS_BINARY);
  assert(Buffer.isBuffer(mainBinary) && mainBinary.length > 0, 'release_binary_missing');
  assert(Buffer.isBuffer(analysisBinary) && analysisBinary.length > 0, 'release_analysis_missing');
  assert(sha256(mainBinary) === manifest.sha256, 'release_binary_hash_mismatch');
  assert(files.get(`${MAIN_BINARY}.sha256`).toString('utf8') === checksum(mainBinary, MAIN_BINARY), 'release_binary_checksum_invalid');
  assert(files.get(`${ANALYSIS_BINARY}.sha256`).toString('utf8') === checksum(analysisBinary, ANALYSIS_BINARY), 'release_analysis_checksum_invalid');
  return manifest;
}

export function planMissingAttachments({ tag, sourceCommit, files, release, existingFiles }) {
  verifyAssetFiles({ tag, sourceCommit, files });
  assert(release && release.tagName === tag, 'github_release_tag_mismatch');
  assert(release.isDraft === false && release.isPrerelease === false, 'github_release_state_invalid');
  assert(Array.isArray(release.assets), 'github_release_assets_invalid');
  assertExactNames(release.assets, [...new Set(release.assets)], 'github_release_asset_duplicate');
  const assetNames = [...files.keys()];
  assert(release.assets.every(name => assetNames.includes(name)), 'github_release_unexpected_asset');
  assert(existingFiles instanceof Map, 'github_release_downloads_invalid');

  for (const name of release.assets) {
    const expected = files.get(name);
    const actual = existingFiles.get(name);
    assert(Buffer.isBuffer(actual), 'github_release_asset_download_missing');
    assert(actual.equals(expected), 'github_release_asset_mismatch');
  }
  assertExactNames([...existingFiles.keys()], release.assets, 'github_release_download_set_invalid');
  return assetNames.filter(name => !release.assets.includes(name));
}

function readDirectory(directory) {
  const names = fs.readdirSync(directory);
  assert(names.includes('release.json'), 'release_asset_file_set_invalid');
  const files = new Map();
  for (const name of names) {
    const filePath = path.join(directory, name);
    assert(fs.lstatSync(filePath).isFile(), 'release_asset_not_regular_file');
    files.set(name, fs.readFileSync(filePath));
  }
  return files;
}

function parseArgs(argv) {
  const values = new Map();
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i];
    const value = argv[i + 1];
    assert(key?.startsWith('--') && value !== undefined, 'release_recovery_arguments_invalid');
    values.set(key.slice(2), value);
  }
  return values;
}

function runCli() {
  const [command, ...argv] = process.argv.slice(2);
  const args = parseArgs(argv);
  const tag = args.get('tag');
  const sourceCommit = args.get('source-commit');
  const directory = args.get('directory');
  const files = readDirectory(directory);
  verifyAssetFiles({ tag, sourceCommit, files });

  if (command === 'verify') {
    console.log('immutable_release_assets_verified');
    return;
  }
  if (command !== 'plan') throw new Error('release_recovery_command_invalid');
  const release = JSON.parse(fs.readFileSync(args.get('release-json'), 'utf8'));
  const existingDirectory = args.get('existing-directory');
  const existingFiles = new Map();
  if (release.assets.length > 0) {
    for (const name of release.assets) {
      assert(RELEASE_ASSETS.includes(name), 'github_release_unexpected_asset');
      const filePath = path.join(existingDirectory, name);
      assert(fs.lstatSync(filePath).isFile(), 'github_release_asset_download_missing');
      existingFiles.set(name, fs.readFileSync(filePath));
    }
  }
  const missing = planMissingAttachments({ tag, sourceCommit, files, release, existingFiles });
  // Keep an empty plan empty: a blank line would become a bogus upload entry.
  if (missing.length > 0) console.log(missing.join('\n'));
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  try {
    runCli();
  } catch (error) {
    console.error(error instanceof Error ? error.message : 'release_recovery_failed');
    process.exitCode = 1;
  }
}
