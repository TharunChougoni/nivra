import { execFileSync, spawnSync } from 'node:child_process';
import { appendFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

export function selectReleaseTag({ version, sha, tagCommit, tagType }) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)) {
    throw new Error('Release selection needs a stable workspace version.');
  }
  if (!tagCommit || tagCommit !== sha) return '';
  if (tagType !== 'tag') throw new Error('Matching release tag must be annotated.');
  return `v${version}`;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const git = (...args) => execFileSync('git', args, { encoding: 'utf8' }).trim();
  const version = execFileSync('python3', ['-c',
    "import pathlib,tomllib; print(tomllib.loads(pathlib.Path('Cargo.toml').read_text())['workspace']['package']['version'])"],
    { encoding: 'utf8' }).trim();
  // Validate before forming a ref or output from package metadata.
  selectReleaseTag({ version });
  const ref = `refs/tags/v${version}`;
  const lookup = spawnSync('git', ['rev-parse', '--verify', '--quiet', ref], { encoding: 'utf8' });
  if (lookup.error || ![0, 1].includes(lookup.status)) throw new Error('Release tag lookup failed.');
  const tag = selectReleaseTag({ version, sha: process.env.GITHUB_SHA || git('rev-parse', 'HEAD'),
    tagCommit: lookup.status === 0 ? git('rev-parse', `${ref}^{}`) : undefined,
    tagType: lookup.status === 0 ? git('cat-file', '-t', ref) : undefined });
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `tag=${tag}\n`);
  else process.stdout.write(`tag=${tag}\n`);
}
