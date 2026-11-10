import { spawnSync } from 'node:child_process';
import fs from 'node:fs/promises';
import path from 'node:path';

const repository = process.env.GITHUB_REPOSITORY;
const releaseCommit = process.env.GITHUB_SHA;
if (repository !== 'superFRANK666/Gemma-Harness' || !/^[0-9a-f]{40}$/.test(releaseCommit || '')) throw new Error('Expected the release repository and commit.');
const directory = path.resolve(process.env.HARNESS_RELEASE_ASSET_DIR || 'release-assets');
const bytes = await fs.readFile(path.join(directory, 'latest.json'));
const manifest = JSON.parse(bytes);
if (!/^\d+\.\d+\.\d+$/.test(manifest.version || '')) throw new Error('Invalid update version.');
const branch = 'update-channel';
function api(method, endpoint, body) {
  const args = ['api', '-X', method, `repos/${repository}/${endpoint}`];
  if (body) args.push('--input', '-');
  const result = spawnSync('gh', args, { input: body && JSON.stringify(body), encoding: 'utf8' });
  if (result.status !== 0) {
    if (result.stderr.includes('HTTP 404')) return null;
    throw new Error(`GitHub API ${method} ${endpoint} failed: ${result.stderr}`);
  }
  return JSON.parse(result.stdout);
}
function compare(a, b) {
  const left = a.split('.').map(Number); const right = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) if (left[i] !== right[i]) return Math.sign(left[i] - right[i]);
  return 0;
}
let ref = api('GET', `git/ref/heads/${branch}`);
if (!ref) {
  ref = api('POST', 'git/refs', { ref: `refs/heads/${branch}`, sha: releaseCommit });
  if (!ref) throw new Error('Unable to create the update channel branch.');
}
const existing = api('GET', `contents/latest.json?ref=${branch}`);
if (existing) {
  const oldBytes = Buffer.from(existing.content.replace(/\s/g, ''), 'base64');
  const oldVersion = JSON.parse(oldBytes).version;
  if (!/^\d+\.\d+\.\d+$/.test(oldVersion || '')) throw new Error('Existing update channel version is invalid.');
  if (compare(oldVersion, manifest.version) > 0) throw new Error(`Refusing to replace newer ${oldVersion} with ${manifest.version}.`);
  if (compare(oldVersion, manifest.version) === 0) {
    if (!oldBytes.equals(bytes)) throw new Error('The published version has a different update manifest.');
    process.stdout.write(`Update channel already points to ${manifest.version}.\n`);
    process.exit(0);
  }
}
const body = { message: `Publish updater manifest v${manifest.version}`, content: bytes.toString('base64'), branch };
if (existing) body.sha = existing.sha;
const result = api('PUT', 'contents/latest.json', body);
if (!result) throw new Error('Unable to update the channel manifest.');
const verified = api('GET', `contents/latest.json?ref=${branch}`);
if (!verified || !Buffer.from(verified.content.replace(/\s/g, ''), 'base64').equals(bytes)) throw new Error('Update channel readback mismatch.');
process.stdout.write(`Published and read back updater manifest v${manifest.version}.\n`);
