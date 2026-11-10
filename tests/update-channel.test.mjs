import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const script = path.join(root, 'scripts/publish-update-channel.mjs');
const fakeGh = `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
const method = args[2]; const endpoint = args[3];
let state = JSON.parse(fs.readFileSync(process.env.FAKE_GH_STATE, 'utf8'));
if (method === 'GET' && endpoint.includes('/git/ref/')) console.log(JSON.stringify({ object: { sha: 'a'.repeat(40) } }));
else if (method === 'GET' && endpoint.includes('/contents/')) console.log(JSON.stringify({ sha: 'old-blob', content: state.content }));
else if (method === 'PUT') { const body = JSON.parse(fs.readFileSync(0, 'utf8')); state.content = body.content; state.writes++; fs.writeFileSync(process.env.FAKE_GH_STATE, JSON.stringify(state)); console.log(JSON.stringify({ content: { sha: 'new-blob' } })); }
else { console.error('Unexpected fake GH request'); process.exit(2); }
`;

test('update channel cannot move backwards or replace different content for the same version', { skip: process.platform === 'win32' }, async () => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'harness-channel-test-'));
  try {
    const executable = path.join(dir, 'gh'); await fs.writeFile(executable, fakeGh, { mode: 0o755 });
    const stateFile = path.join(dir, 'state.json');
    const incoming = Buffer.from(JSON.stringify({ version: '0.3.1', platforms: { test: true } }));
    await fs.writeFile(path.join(dir, 'latest.json'), incoming);
    const env = { ...process.env, PATH: `${dir}${path.delimiter}${process.env.PATH}`, FAKE_GH_STATE: stateFile, GITHUB_REPOSITORY: 'superFRANK666/Gemma-Harness', GITHUB_SHA: 'a'.repeat(40), HARNESS_RELEASE_ASSET_DIR: dir };
    const run = () => spawnSync(process.execPath, [script], { cwd: root, env, encoding: 'utf8' });
    const setOld = async value => fs.writeFile(stateFile, JSON.stringify({ content: Buffer.from(JSON.stringify(value)).toString('base64'), writes: 0 }));
    await setOld({ version: '0.3.2' });
    assert.notEqual(run().status, 0);
    assert.equal(JSON.parse(await fs.readFile(stateFile, 'utf8')).writes, 0);
    await setOld({ version: '0.3.1', platforms: { test: false } });
    assert.notEqual(run().status, 0);
    assert.equal(JSON.parse(await fs.readFile(stateFile, 'utf8')).writes, 0);
    await setOld({ version: '0.3.0' });
    assert.equal(run().status, 0);
    const result = JSON.parse(await fs.readFile(stateFile, 'utf8'));
    assert.equal(result.writes, 1);
    assert.deepEqual(Buffer.from(result.content, 'base64'), incoming);
    assert.equal(run().status, 0);
    assert.equal(JSON.parse(await fs.readFile(stateFile, 'utf8')).writes, 1);
  } finally { await fs.rm(dir, { recursive: true, force: true }); }
});
