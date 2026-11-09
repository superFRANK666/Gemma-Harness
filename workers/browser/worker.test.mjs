import test from 'node:test';
import assert from 'node:assert/strict';
import { validateUrl } from './worker.mjs';

test('browser navigation rejects local files and script URLs', () => {
  assert.equal(validateUrl('https://example.org'), 'https://example.org/');
  assert.equal(validateUrl('about:blank'), 'about:blank');
  for (const url of ['file:///etc/passwd', 'javascript:alert(1)', 'data:text/html,hello', 'about:config']) assert.throws(() => validateUrl(url));
});
