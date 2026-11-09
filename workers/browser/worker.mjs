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
    page.on('close', () => { tabs.delete(id); snapshots.delete(id); if (currentTab === id) currentTab = tabs.keys().next().value; });
    page.on('framenavigated', frame => { if (frame === page.mainFrame()) snapshots.delete(id); });
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
  if (params.url) await page.goto(validateUrl(params.url), { waitUntil: 'domcontentloaded' });
  const handles = new Map();
  const elements = [];
  const matches = page.locator('a,button,input,textarea,select,[role],[contenteditable="true"],summary');
  const count = Math.min(await matches.count(), 160);
  for (let i = 0; i < count; i++) {
    const element = await matches.nth(i).elementHandle().catch(() => null);
    if (!element || !(await element.isVisible().catch(() => false))) continue;
    const info = await element.evaluate(el => ({
      tag: el.tagName.toLowerCase(), role: el.getAttribute('role'),
      name: (el.getAttribute('aria-label') || el.getAttribute('alt') || el.getAttribute('placeholder') || el.innerText || el.getAttribute('name') || '').trim().slice(0, 180),
      value: el.type === 'password' ? '[password]' : ('value' in el ? String(el.value).slice(0, 120) : undefined),
      disabled: !!el.disabled,
    })).catch(() => null);
    if (!info) continue;
    const ref = `e${elements.length + 1}`;
    handles.set(ref, element);
    elements.push({ ref, ...info });
  }
  const previous = snapshots.get(id);
  if (previous) for (const handle of previous.handles.values()) await handle.dispose().catch(() => {});
  const snapshot_id = crypto.randomUUID();
  snapshots.set(id, { snapshot_id, handles, url: page.url(), screenshot: false });
  let tree = await page.locator('body').ariaSnapshot({ timeout: 10000 }).catch(() => 'Accessibility snapshot unavailable for this page');
  // Playwright's textual ARIA serializer can include password input values even
  // though the rendered input is masked. Redact before returning or logging.
  const passwordValues = (await Promise.all(page.frames().map(frame =>
    frame.locator('input[type="password"]').evaluateAll(inputs => inputs.map(input => input.value).filter(Boolean)).catch(() => [])
  ))).flat();
  for (const secret of passwordValues) tree = tree.split(secret).join('[password]');
  return { snapshot_id, tab_id: id, url: page.url(), title: await page.title(),
    tabs: [...tabs].map(([tab_id, p]) => ({ tab_id, url: p.url() })),
    accessibility: tree.slice(0, 12000), elements,
    truncated: tree.length > 12000 || (await matches.count()) > count,
    note: 'Element refs are valid only for this snapshot. Password values are redacted.' };
}

async function screenshot(params) {
  const { page, id } = await getPage(params);
  const previous = snapshots.get(id);
  if (previous) for (const handle of previous.handles.values()) await handle.dispose().catch(() => {});
  const filename = `browser-${crypto.randomUUID()}.png`;
  const artifact = path.join(artifactDir, filename);
  const bytes = await page.screenshot({ path: artifact, type: 'png', fullPage: false });
  const snapshot_id = crypto.randomUUID();
  snapshots.set(id, { snapshot_id, handles: new Map(), url: page.url(), screenshot: true });
  return { snapshot_id, tab_id: id, artifact,
    content: [{ type: 'text', text: `Real browser screenshot: ${page.url()}` }, { type: 'image', data: bytes.toString('base64'), mimeType: 'image/png' }] };
}

async function act(params) {
  const { page, id } = await getPage(params);
  const action = params.action;
  if (action === 'new_tab') {
    const newPage = await context.newPage();
    if (params.url) await newPage.goto(validateUrl(params.url), { waitUntil: 'domcontentloaded' });
    const tab_id = [...tabs].find(([, p]) => p === newPage)[0]; currentTab = tab_id;
    return observe({ tab_id });
  }
  const snapshot = snapshots.get(id);
  if (!snapshot || snapshot.snapshot_id !== params.snapshot_id || snapshot.url !== page.url()) throw new Error('Stale snapshot_id; observe the page again');
  let element;
  if (params.ref) {
    element = snapshot.handles.get(params.ref);
    if (!element || !(await element.evaluate(el => el.isConnected).catch(() => false))) throw new Error('Stale element ref; observe again');
  }
  switch (action) {
    case 'click':
      if (element) await element.click();
      else if (snapshot.screenshot && Number.isFinite(params.x) && Number.isFinite(params.y)) await page.mouse.click(params.x, params.y);
      else throw new Error('click requires an element ref or coordinates from a current screenshot');
      break;
    case 'fill': if (!element) throw new Error('fill requires an element ref'); await element.fill(String(params.text ?? '')); break;
    case 'press': if (element) await element.press(String(params.key)); else await page.keyboard.press(String(params.key)); break;
    case 'scroll': await page.mouse.wheel(Number(params.x || 0), Number(params.y || 600)); break;
    case 'back': await page.goBack({ waitUntil: 'domcontentloaded' }); break;
    case 'forward': await page.goForward({ waitUntil: 'domcontentloaded' }); break;
    case 'close_tab': await page.close(); return { closed: true, tabs: [...tabs.keys()] };
    default: throw new Error(`Unsupported browser action: ${action}`);
  }
  // observe disposes the handles from the previous snapshot before replacing it.
  return observe({ tab_id: id });
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
      } catch (error) { process.stdout.write(JSON.stringify({ id: request.id, error: { message: String(error.message || error) } }) + '\n'); }
    };
    // Cancellation does not wait behind a navigation or click.
    if (request.method === 'cancel') void run(); else queue = queue.then(run, run);
  });
  rl.on('close', () => { void cancel().finally(() => process.exit(0)); });
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main().catch(error => { console.error(error); process.exit(1); });
