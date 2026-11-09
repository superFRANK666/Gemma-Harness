import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const script = path.join(root, 'scripts/release-assets.mjs');
const { version } = JSON.parse(await fs.readFile(path.join(root, 'package.json'), 'utf8'));
const prefix = `Gemma-Harness-v${version}`;
const names = [
  `${prefix}-macOS-arm64.dmg`, `${prefix}-macOS-x64.dmg`,
  `${prefix}-Windows-x64-setup.exe`, `${prefix}-Windows-x64.msi`,
  `${prefix}-Linux-x64.deb`, `${prefix}-Linux-x64.AppImage`,
];

function run(args, directory, extra = {}) {
  return spawnSync(process.execPath, [script, ...args], {
    encoding: 'utf8',
    env: { ...process.env, GITHUB_REF_TYPE: '', HARNESS_RELEASE_ASSET_DIR: directory, ...extra },
  });
}

test('release requires every nonempty installer and verifies uploaded sizes before publication', async () => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'harness-release-test-'));
  try {
    assert.notEqual(run(['verify-local'], dir).status, 0);
    for (const name of names) await fs.writeFile(path.join(dir, name), `fixture:${name}`);
    assert.equal(run(['verify-local'], dir).status, 0);
    const sums = await fs.readFile(path.join(dir, 'SHA256SUMS'), 'utf8');
    const digest = createHash('sha256').update(`fixture:${names[0]}`).digest('hex');
    assert.match(sums, new RegExp(`${digest}  ${names[0].replaceAll('.', '\\.')}\\n`));

    const remotePath = path.join(os.tmpdir(), `harness-remote-${crypto.randomUUID()}.json`);
    try {
      const remote = { isDraft: true, isPrerelease: true, assets: [] };
      for (const name of [...names, 'SHA256SUMS']) {
        remote.assets.push({ name, size: (await fs.stat(path.join(dir, name))).size });
      }
      await fs.writeFile(remotePath, JSON.stringify(remote));
      assert.equal(run(['verify-remote'], dir, { HARNESS_RELEASE_REMOTE: remotePath }).status, 0);
      remote.assets[0].size++;
      await fs.writeFile(remotePath, JSON.stringify(remote));
      assert.notEqual(run(['verify-remote'], dir, { HARNESS_RELEASE_REMOTE: remotePath }).status, 0);
      remote.assets[0].size--;
      remote.isDraft = false;
      await fs.writeFile(remotePath, JSON.stringify(remote));
      assert.notEqual(run(['verify-remote'], dir, { HARNESS_RELEASE_REMOTE: remotePath }).status, 0);
    } finally { await fs.rm(remotePath, { force: true }); }

    await fs.writeFile(path.join(dir, names[0]), '');
    assert.notEqual(run(['verify-local'], dir).status, 0);
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});

test('release tag must match the application version', async () => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'harness-tag-test-'));
  try {
    const result = run(['verify-local'], dir, { GITHUB_REF_TYPE: 'tag', GITHUB_REF_NAME: 'v9.9.9' });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Release tag must be/);
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});
