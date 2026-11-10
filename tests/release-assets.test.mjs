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
const installers = [
  `${prefix}-macOS-arm64.dmg`, `${prefix}-macOS-x64.dmg`,
  `${prefix}-Windows-x64-setup.exe`, `${prefix}-Windows-x64.msi`,
  `${prefix}-Linux-x64.deb`, `${prefix}-Linux-x64.AppImage`,
];
const updatePackages = [
  `${prefix}-macOS-arm64.app.tar.gz`, `${prefix}-macOS-x64.app.tar.gz`,
  `${prefix}-Windows-x64-setup.exe`, `${prefix}-Windows-x64.msi`,
  `${prefix}-Linux-x64.deb`, `${prefix}-Linux-x64.AppImage`,
];
const sourceNames = [...installers, ...updatePackages.slice(0, 2), ...updatePackages.map(name => `${name}.sig`)];
const fakeSignature = signedVersion => Buffer.from(`untrusted comment: fixture\nfixture\ntrusted comment: timestamp:1\tversion:${signedVersion}\nfixture\n`).toString('base64');

function run(args, directory, extra = {}) {
  return spawnSync(process.execPath, [script, ...args], {
    encoding: 'utf8',
    env: { ...process.env, GITHUB_REF_TYPE: '', HARNESS_RELEASE_ASSET_DIR: directory, ...extra },
  });
}

async function remoteAsset(dir, name) {
  const bytes = await fs.readFile(path.join(dir, name));
  return { name, size: bytes.length, digest: `sha256:${createHash('sha256').update(bytes).digest('hex')}` };
}

test('release requires installers and version-bound signatures before publishing a complete manifest', async () => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'harness-release-test-'));
  try {
    assert.notEqual(run(['verify-local'], dir).status, 0);
    for (const name of sourceNames) await fs.writeFile(path.join(dir, name), name.endsWith('.sig') ? fakeSignature(version) : `fixture:${name}`);
    assert.equal(run(['verify-local'], dir).status, 0);
    const manifest = JSON.parse(await fs.readFile(path.join(dir, 'latest.json'), 'utf8'));
    assert.equal(manifest.version, version);
    assert.deepEqual(Object.keys(manifest.platforms).sort(), ['darwin-aarch64-app', 'darwin-x86_64-app', 'linux-x86_64-appimage', 'linux-x86_64-deb', 'windows-x86_64-msi', 'windows-x86_64-nsis']);
    assert.match(manifest.platforms['linux-x86_64-deb'].url, /Linux-x64\.deb$/);
    assert.equal(manifest.platforms['linux-x86_64-deb'].signature, fakeSignature(version));
    const sums = await fs.readFile(path.join(dir, 'SHA256SUMS'), 'utf8');
    const digest = createHash('sha256').update(`fixture:${installers[0]}`).digest('hex');
    assert.match(sums, new RegExp(`${digest}  ${installers[0].replaceAll('.', '\\.')}\\n`));

    const remotePath = path.join(dir, 'remote-fixture.json');
    try {
      const remote = { isDraft: true, isPrerelease: true, assets: await Promise.all([...sourceNames, 'latest.json', 'SHA256SUMS'].map(name => remoteAsset(dir, name))) };
      await fs.writeFile(remotePath, JSON.stringify(remote));
      assert.equal(run(['verify-remote'], dir, { HARNESS_RELEASE_REMOTE: remotePath }).status, 0);
      remote.assets[0].digest = `sha256:${'0'.repeat(64)}`;
      await fs.writeFile(remotePath, JSON.stringify(remote));
      assert.notEqual(run(['verify-remote'], dir, { HARNESS_RELEASE_REMOTE: remotePath }).status, 0);
      remote.assets[0] = await remoteAsset(dir, remote.assets[0].name);
      remote.isDraft = false;
      await fs.writeFile(remotePath, JSON.stringify(remote));
      assert.notEqual(run(['verify-remote'], dir, { HARNESS_RELEASE_REMOTE: remotePath }).status, 0);
      assert.equal(run(['verify-remote'], dir, { HARNESS_RELEASE_REMOTE: remotePath, HARNESS_ALLOW_PUBLISHED_RELEASE: '1' }).status, 0);
    } finally { await fs.rm(remotePath, { force: true }); }

    await fs.writeFile(path.join(dir, `${updatePackages[0]}.sig`), fakeSignature('0.0.1'));
    assert.notEqual(run(['verify-local'], dir).status, 0);
    await fs.writeFile(path.join(dir, `${updatePackages[0]}.sig`), fakeSignature(version));
    await fs.rm(path.join(dir, `${updatePackages[1]}.sig`));
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
