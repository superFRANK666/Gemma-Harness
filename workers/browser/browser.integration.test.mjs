import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import os from 'node:os';
import fs from 'node:fs/promises';
import path from 'node:path';
import readline from 'node:readline';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

test('real Chromium observe, click, stale ref rejection, screenshot and cancel', { skip: process.env.HARNESS_BROWSER_INTEGRATION !== '1', timeout: 60000 }, async () => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'harness-browser-test-'));
  const server = http.createServer((_, response) => {
    response.setHeader('content-type', 'text/html');
    response.end('<!doctype html><html><body><h1>Fixture</h1><button onclick="document.querySelector(\'h1\').textContent=\'Clicked\'">Change heading</button><input aria-label="Name"><input aria-label="Password" type="password" value="secret-value-never-expose"></body></html>');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const worker = spawn(process.execPath, [fileURLToPath(new URL('./worker.mjs', import.meta.url))], {
    env: { ...process.env, HARNESS_BROWSER_HEADLESS: '1', HARNESS_ARTIFACT_DIR: directory, HARNESS_BROWSER_PROFILE: path.join(directory, 'profile') },
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  const pending = new Map(); let sequence = 0;
  readline.createInterface({ input: worker.stdout }).on('line', line => {
    const response = JSON.parse(line); const callback = pending.get(response.id);
    if (callback) { pending.delete(response.id); callback(response); }
  });
  const call = (method, params) => new Promise(resolve => {
    const id = String(++sequence); pending.set(id, resolve);
    worker.stdin.write(JSON.stringify({ id, method, params }) + '\n');
  });
  try {
    const observation = await call('observe', { url: `http://127.0.0.1:${server.address().port}` });
    assert.equal(observation.error, undefined, JSON.stringify(observation.error));
    const snapshot = observation.result;
    assert.equal(JSON.stringify(snapshot).includes('secret-value-never-expose'), false);
    const button = snapshot.elements.find(element => element.name === 'Change heading');
    assert.ok(button);
    const clicked = await call('act', { action: 'click', snapshot_id: snapshot.snapshot_id, ref: button.ref });
    assert.match(clicked.result.accessibility, /Clicked/);
    const stale = await call('act', { action: 'click', snapshot_id: snapshot.snapshot_id, ref: button.ref });
    assert.match(stale.error.message, /Stale snapshot/);
    const screenshot = await call('screenshot', {});
    const image = Buffer.from(screenshot.result.content.find(item => item.type === 'image').data, 'base64');
    assert.deepEqual(image.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    assert.deepEqual(await fs.readFile(screenshot.result.artifact), image);
    assert.equal((await call('cancel', {})).result.cancelled, true);
  } finally {
    worker.stdin.end(); worker.kill();
    await new Promise(resolve => server.close(resolve));
    await fs.rm(directory, { recursive: true, force: true });
  }
});
