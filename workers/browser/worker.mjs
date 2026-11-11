import readline from 'node:readline';
import fs from 'node:fs/promises';
import path from 'node:path';
import crypto from 'node:crypto';
import { pathToFileURL } from 'node:url';

// JSONL on stdout is an interface. Diagnostics belong on stderr.
let context;
let tabs = new Map();
let currentTab;
let snapshots = new Map();
const artifactDir = process.env.HARNESS_ARTIFACT_DIR;

// This function is serialized by Playwright and must not close over Node state.
function elementInfo(el) {
  return {
    tag: el.tagName.toLowerCase(), role: el.getAttribute('role'),
    name: (el.getAttribute('aria-label') || el.getAttribute('alt') || el.getAttribute('placeholder') || el.innerText || el.getAttribute('name') || '').trim().slice(0, 180),
    value: el.type === 'password' ? '[password]' : ('value' in el ? String(el.value).slice(0, 120) : undefined),
    type: el.getAttribute('type'), href: el.getAttribute('href')?.slice(0, 1000),
    disabled: !!el.disabled || el.getAttribute('aria-disabled') === 'true',
    readonly: !!el.readOnly, checked: 'checked' in el ? el.checked : undefined,
    expanded: el.getAttribute('aria-expanded'),
  };
}

async function disposeSnapshot(snapshot) {
  if (snapshot) for (const handle of snapshot.handles.values()) await handle.dispose().catch(() => {});
}

async function discardSnapshot(id) {
  const previous = snapshots.get(id);
  snapshots.delete(id);
  await disposeSnapshot(previous);
}

async function visualState(page) {
  const read = () => {
    const key = '__gemmaHarnessVisualRevision';
    if (!globalThis[key]) {
      const state = { revision: 0 };
      const observer = new MutationObserver(() => state.revision++);
      observer.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
      globalThis[key] = state;
    }
    return { revision: globalThis[key].revision, url: location.href,
      width: innerWidth, height: innerHeight, x: scrollX, y: scrollY, scale: devicePixelRatio };
  };
  const main = await page.mainFrame().evaluate(read);
  const frames = await Promise.all(page.frames().filter(frame => frame !== page.mainFrame()).map(frame => frame.evaluate(read)));
  return { ...main, frames };
}

async function performInput(operation) {
  try { return await operation(); }
  catch (error) {
    // A timeout can occur after the click/key/navigation was already delivered.
    error.indeterminate = true;
    throw error;
  }
}

export function validateUrl(value) {
  const url = new URL(value);
  if (!['http:', 'https:', 'about:'].includes(url.protocol) || (url.protocol === 'about:' && url.href !== 'about:blank')) throw new Error('Only http, https, and about:blank URLs are supported');
  return url.href;
}

async function ensureContext() {
  if (context) return;
  if (!artifactDir) throw new Error('HARNESS_ARTIFACT_DIR is required');
  const { chromium } = await import('playwright').catch(() => { throw new Error('Playwright dependency is missing. Install workers/browser dependencies and Chromium.'); });
  await fs.mkdir(artifactDir, { recursive: true });
  const profile = process.env.HARNESS_BROWSER_PROFILE || path.join(artifactDir, '..', 'browser-profile');
  context = await chromium.launchPersistentContext(profile, {
    headless: process.env.HARNESS_BROWSER_HEADLESS === '1',
    viewport: { width: 1280, height: 800 },
    acceptDownloads: false,
  });
  context.setDefaultTimeout(15000);
  context.setDefaultNavigationTimeout(30000);
  function register(page) {
    if ([...tabs.values()].includes(page)) return;
    const id = crypto.randomUUID();
    tabs.set(id, page); currentTab ||= id;
    page.on('close', () => { tabs.delete(id); void discardSnapshot(id); if (currentTab === id) currentTab = tabs.keys().next().value; });
    page.on('framenavigated', () => { void discardSnapshot(id); });
  }
  context.pages().forEach(register);
  context.on('page', register);
  if (!context.pages().length) register(await context.newPage());
}

async function getPage(params) {
  await ensureContext();
  const id = params.tab_id || currentTab;
  const page = tabs.get(id);
  if (!page || page.isClosed()) throw new Error('Unknown or closed tab; observe the available tabs again');
  currentTab = id;
  return { page, id };
}

async function observe(params) {
  const { page, id } = await getPage(params);
  await discardSnapshot(id);
  if (params.url) {
    const url = validateUrl(params.url);
    await performInput(() => page.goto(url, { waitUntil: 'domcontentloaded' }));
  }
  const observedUrl = page.url();
  const handles = new Map();
  const signatures = new Map();
  const elements = [];
  const matches = page.locator('a,button,input,textarea,select,[role],[contenteditable="true"],summary');
  const count = Math.min(await matches.count(), 160);
  for (let i = 0; i < count; i++) {
    const element = await matches.nth(i).elementHandle().catch(() => null);
    if (!element) continue;
    const visible = await element.isVisible().catch(() => false);
    const info = visible ? await element.evaluate(elementInfo).catch(() => null) : null;
    if (!info) { await element.dispose().catch(() => {}); continue; }
    const ref = `e${elements.length + 1}`;
    handles.set(ref, element);
    signatures.set(ref, JSON.stringify(info));
    elements.push({ ref, ...info });
  }
  const snapshot_id = crypto.randomUUID();
  let tree = await page.locator('body').ariaSnapshot({ timeout: 10000 }).catch(() => 'Accessibility snapshot unavailable for this page');
  // Playwright's textual ARIA serializer can include password input values even
  // though the rendered input is masked. Redact before returning or logging.
  const passwordValues = (await Promise.all(page.frames().map(frame =>
    frame.locator('input[type="password"]').evaluateAll(inputs => inputs.map(input => input.value).filter(Boolean)).catch(() => [])
  ))).flat();
  for (const secret of passwordValues) tree = tree.split(secret).join('[password]');
  if (page.url() !== observedUrl) {
    await disposeSnapshot({ handles });
    throw new Error('Page navigated during observation; observe again');
  }
  snapshots.set(id, { snapshot_id, handles, signatures, url: observedUrl, screenshot: false });
  return { snapshot_id, tab_id: id, url: page.url(), title: await page.title(),
    tabs: [...tabs].map(([tab_id, p]) => ({ tab_id, url: p.url() })),
    accessibility: tree.slice(0, 12000), elements,
    truncated: tree.length > 12000 || (await matches.count()) > count,
    note: 'Element refs are valid only for this snapshot. Password values are redacted.' };
}

async function screenshot(params) {
  const { page, id } = await getPage(params);
  await discardSnapshot(id);
  const before = await visualState(page);
  const filename = `browser-${crypto.randomUUID()}.png`;
  const artifact = path.join(artifactDir, filename);
  // Hiding carets mutates input styles in Playwright and would invalidate our own capture.
  const bytes = await page.screenshot({ path: artifact, type: 'png', fullPage: false, caret: 'initial' });
  const view = await visualState(page);
  if (JSON.stringify(before) !== JSON.stringify(view)) {
    await fs.unlink(artifact).catch(() => {});
    throw new Error('Page changed during screenshot capture; capture again or use semantic refs');
  }
  const width = bytes.readUInt32BE(16), height = bytes.readUInt32BE(20);
  const snapshot_id = crypto.randomUUID();
  snapshots.set(id, { snapshot_id, handles: new Map(), url: page.url(), screenshot: true, view, width, height });
  return { snapshot_id, tab_id: id, artifact, width, height,
    coordinate_space: 'screenshot pixels; coordinates are mapped to CSS viewport pixels',
    content: [{ type: 'text', text: `Real browser screenshot: ${page.url()}` }, { type: 'image', data: bytes.toString('base64'), mimeType: 'image/png' }] };
}

async function act(params) {
  const { page, id } = await getPage(params);
  const action = params.action;
  if (action === 'new_tab') {
    const url = params.url ? validateUrl(params.url) : undefined;
    const newPage = await performInput(() => context.newPage());
    if (url) await performInput(() => newPage.goto(url, { waitUntil: 'domcontentloaded' }));
    const tab_id = [...tabs].find(([, p]) => p === newPage)[0]; currentTab = tab_id;
    return observeAfterInput(tab_id);
  }
  const snapshot = snapshots.get(id);
  if (!snapshot || snapshot.snapshot_id !== params.snapshot_id || snapshot.url !== page.url()) throw new Error('Stale snapshot_id; observe the page again');
  let element;
  if (params.ref) {
    element = snapshot.handles.get(params.ref);
    if (!element || !(await element.evaluate(el => el.isConnected).catch(() => false))) throw new Error('Stale element ref; observe again');
    const info = await element.evaluate(elementInfo).catch(() => null);
    if (!info || JSON.stringify(info) !== snapshot.signatures.get(params.ref) || !(await element.isVisible().catch(() => false))) throw new Error('Element changed since observation; observe again');
    if (info.disabled) throw new Error('Element is disabled; observe before choosing another action');
  }
  if (!['click', 'fill', 'press', 'scroll', 'back', 'forward', 'close_tab'].includes(action)) throw new Error(`Unsupported browser action: ${action}`);
  if (action === 'click' && !element) {
    if (!snapshot.screenshot || !Number.isFinite(params.x) || !Number.isFinite(params.y)) throw new Error('click requires an element ref or coordinates from a current screenshot');
    if (params.x < 0 || params.y < 0 || params.x >= snapshot.width || params.y >= snapshot.height) throw new Error('Coordinates are outside the screenshot');
    if (JSON.stringify(snapshot.view) !== JSON.stringify(await visualState(page))) throw new Error('Page changed since screenshot; capture again or use semantic refs');
  }
  if (action === 'fill' && (!element || typeof params.text !== 'string')) throw new Error('fill requires an element ref and text');
  if (action === 'press' && (typeof params.key !== 'string' || !params.key.trim())) throw new Error('press requires key');
  if (snapshots.get(id) !== snapshot) throw new Error('Snapshot invalidated during action preparation; observe again');
  // Consume before dispatch, including partial failures. Old refs cannot retry input.
  snapshots.delete(id);
  try { await performInput(async () => { switch (action) {
    case 'click':
      if (element) await element.click();
      else await page.mouse.click(params.x * snapshot.view.width / snapshot.width, params.y * snapshot.view.height / snapshot.height);
      break;
    case 'fill': await element.fill(params.text); break;
    case 'press': if (element) await element.press(String(params.key)); else await page.keyboard.press(String(params.key)); break;
    case 'scroll': await page.mouse.wheel(Number(params.x ?? 0), Number(params.y ?? 600)); break;
    case 'back': await page.goBack({ waitUntil: 'domcontentloaded' }); break;
    case 'forward': await page.goForward({ waitUntil: 'domcontentloaded' }); break;
    case 'close_tab': await page.close(); break;
  } }); } finally { await disposeSnapshot(snapshot); }
  if (action === 'close_tab') return { closed: true, tab_id: id, tabs: [...tabs.keys()] };
  return observeAfterInput(id);
}

async function observeAfterInput(tab_id) {
  try { return await observe({ tab_id }); }
  catch (error) {
    return { action_accepted: true, requires_observation: true, tab_id,
      observation_error: String(error.message || error),
      instruction: 'Input was accepted but the resulting page could not be observed. Observe again; do not repeat the input.' };
  }
}

async function cancel() {
  if (context) await context.close().catch(() => {});
  context = undefined; tabs.clear(); snapshots.clear(); currentTab = undefined;
  return { cancelled: true };
}

async function main() {
  const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  let queue = Promise.resolve();
  rl.on('line', line => {
    let request;
    try { request = JSON.parse(line); } catch { process.stdout.write(JSON.stringify({ id: null, error: { message: 'Invalid JSON' } }) + '\n'); return; }
    const run = async () => {
      try {
        const handler = { observe, screenshot, act, cancel }[request.method];
        if (!handler) throw new Error('Unknown browser method');
        process.stdout.write(JSON.stringify({ id: request.id, result: await handler(request.params || {}) }) + '\n');
      } catch (error) { process.stdout.write(JSON.stringify({ id: request.id, error: { message: String(error.message || error), indeterminate: error.indeterminate === true } }) + '\n'); }
    };
    // Cancellation does not wait behind a navigation or click.
    if (request.method === 'cancel') void run(); else queue = queue.then(run, run);
  });
  rl.on('close', () => { void cancel().finally(() => process.exit(0)); });
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main().catch(error => { console.error(error); process.exit(1); });
