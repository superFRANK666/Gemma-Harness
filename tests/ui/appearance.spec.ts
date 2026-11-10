import type { Page } from '@playwright/test';
import { test, expect, baseConfig, ipcCalls, screenshot } from './fixtures';
import type { AppearanceConfig, Config } from '../../apps/desktop/src/types';

const defaults: AppearanceConfig = { theme: 'dark', accent: 'sage', fontFamily: 'system', fontSize: 13, codeFontFamily: 'system', codeFontSize: 12 };
/** Explicit test-only IPC; saves persist across reload to exercise canonical snapshots. */
async function appearanceFixture(page: Page, appearance: AppearanceConfig = defaults) {
  const initialConfig: Config = { ...structuredClone(baseConfig), appearance, workspace: '/tmp/appearance-TEST_FIXTURE', connections: [{ ...baseConfig.connections[0], model: 'TEST_FIXTURE_MODEL' }] };
  await page.addInitScript(({ initialConfig }) => {
    type Json = Record<string, unknown>;
    const storageKey = 'appearance_TEST_FIXTURE';
    let config = JSON.parse(localStorage.getItem(storageKey) || JSON.stringify(initialConfig)) as typeof initialConfig;
    const sessions: Json[] = [];
    const calls: { cmd: string; args: Json }[] = [];
    const callbacks = new Map<number, (message: unknown) => void>(); let nextCallback = 1;
    let stream: { id: number; index: number; resolve: (value: unknown) => void } | undefined;
    function emit(message: Json) { if (!stream) throw new Error('TEST_FIXTURE has no pending stream'); callbacks.get(stream.id)?.({ index: stream.index++, message }); }
    function finish() { if (!stream) return; const current = stream; stream = undefined; callbacks.get(current.id)?.({ index: current.index, end: true }); current.resolve(null); }
    Object.defineProperty(window, 'isTauri', { value: true, configurable: true });
    Object.defineProperty(window, '__HARNESS_TEST_CALLS__', { value: calls });
    Object.defineProperty(window, '__TAURI_INTERNALS__', { configurable: true, value: {
      transformCallback(callback: (message: unknown) => void) { const id = nextCallback++; callbacks.set(id, callback); return id; },
      unregisterCallback(id: number) { callbacks.delete(id); },
      async invoke(cmd: string, args: Json = {}) {
        calls.push({ cmd, args: JSON.parse(JSON.stringify(args)) as Json });
        if (cmd === 'stream_turn') {
          const promise = new Promise(resolve => { stream = { id: (args.onEvent as { id: number }).id, index: 0, resolve }; });
          emit({ type: 'text_delta', text: 'TEST_FIXTURE live response remains visible' }); return promise;
        }
        if (cmd !== 'command') throw new Error(`TEST_FIXTURE unhandled command ${cmd}`);
        const request = args.request as Json;
        switch (request.action) {
          case 'snapshot': return structuredClone({ config, sessions, engine: { running: false }, platform: 'windows', arch: 'x86_64', dataDir: 'TEST_FIXTURE', activeSessions: stream ? ['appearance-session'] : [] });
          case 'save_appearance': config = { ...config, appearance: structuredClone(request.appearance) as typeof config.appearance }; localStorage.setItem(storageKey, JSON.stringify(config)); return structuredClone(config);
          case 'save_settings': config = structuredClone(request.config) as typeof config; localStorage.setItem(storageKey, JSON.stringify(config)); return structuredClone(config);
          case 'create_session': { const session = { id: 'appearance-session', title: 'TEST_FIXTURE appearance task', workspace: config.workspace, deviceId: config.deviceId, connectionId: config.selectedConnectionId, createdAt: '2026-11-09', updatedAt: '2026-11-09' }; sessions.push(session); return session; }
          case 'interrupt': emit({ type: 'turn_completed', status: 'interrupted' }); finish(); return { ok: true };
          default: throw new Error(`TEST_FIXTURE unhandled action ${String(request.action)}`);
        }
      },
    } });
  }, { initialConfig });
  await page.goto('/');
  await expect(page.locator('.workspace-path')).toHaveText('/tmp/appearance-TEST_FIXTURE');
}
async function openAppearance(page: Page) {
  await page.getByRole('button', { name: /设置与连接/ }).click();
  await page.getByRole('dialog').getByRole('button', { name: '外观', exact: true }).click();
  await expect(page.getByRole('heading', { name: '外观', exact: true })).toBeVisible();
  return page.getByRole('dialog');
}
const actionCalls = async (page: Page, name: string) => (await ipcCalls(page)).filter(call => (call.args.request as { action?: string })?.action === name);

test('appearance previews theme, accent and local fonts across the workbench, then Cancel restores saved values', async ({ page }) => {
  await appearanceFixture(page);
  await page.getByRole('button', { name: '终端', exact: true }).click();
  const original = await page.locator('.terminal-panel pre').evaluate(element => getComputedStyle(element).fontSize);
  const dialog = await openAppearance(page);
  await dialog.getByRole('button', { name: '浅色', exact: true }).click();
  await dialog.getByRole('button', { name: '海蓝', exact: true }).click();
  await dialog.getByLabel('界面字体', { exact: true }).selectOption('serif');
  await dialog.getByLabel('界面字号', { exact: true }).selectOption('16');
  await dialog.getByLabel('代码字体', { exact: true }).selectOption('mono');
  await dialog.getByLabel('代码字号', { exact: true }).selectOption('17');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(page.locator('html')).toHaveAttribute('data-accent', 'blue');
  expect(await page.locator('.sidebar').evaluate(element => getComputedStyle(element).fontFamily)).toContain('Georgia');
  await expect(page.locator('.terminal-panel pre')).toHaveCSS('font-size', '17px');
  for (const selector of ['.main-area', '.sidebar', '.terminal-panel', '.settings-modal']) {
    const rgb = await page.locator(selector).evaluate(element => getComputedStyle(element).backgroundColor.match(/\d+/g)?.slice(0, 3).map(Number) || []);
    expect(rgb.length).toBe(3); expect(Math.min(...rgb)).toBeGreaterThan(200);
  }
  await screenshot(page, 'appearance-light-blue-preview');
  await dialog.getByRole('button', { name: '取消', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark'); await expect(page.locator('html')).toHaveAttribute('data-accent', 'sage');
  await expect(page.locator('.terminal-panel pre')).toHaveCSS('font-size', original);
  expect(await page.locator('.sidebar').evaluate(element => getComputedStyle(element).fontFamily)).not.toContain('Georgia');
  expect(await actionCalls(page, 'save_appearance')).toHaveLength(0); expect(await actionCalls(page, 'save_settings')).toHaveLength(0);
});

test('appearance-only save survives reopening and reload, preserving its canonical settings', async ({ page }) => {
  await appearanceFixture(page); let dialog = await openAppearance(page);
  await dialog.getByRole('button', { name: '浅色', exact: true }).click(); await dialog.getByRole('button', { name: '紫罗兰', exact: true }).click();
  await dialog.getByLabel('界面字号', { exact: true }).selectOption('18'); await dialog.getByLabel('代码字号', { exact: true }).selectOption('18');
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await actionCalls(page, 'save_appearance')).toHaveLength(1); expect(await actionCalls(page, 'save_settings')).toHaveLength(0);
  dialog = await openAppearance(page); await expect(dialog.getByRole('button', { name: '紫罗兰', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await dialog.getByRole('button', { name: '深色', exact: true }).click(); await dialog.getByRole('button', { name: '关闭设置', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.reload(); await expect(page.locator('.workspace-path')).toHaveText('/tmp/appearance-TEST_FIXTURE');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light'); await expect(page.locator('html')).toHaveAttribute('data-accent', 'violet');
  dialog = await openAppearance(page); await expect(dialog.getByLabel('界面字号', { exact: true })).toHaveValue('18'); await expect(dialog.getByLabel('代码字号', { exact: true })).toHaveValue('18');
  await expect(dialog.getByRole('button', { name: '保存设置', exact: true })).toBeVisible();
});

test('system theme follows live changes and restores its listener after closing a fixed-theme preview', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' }); await appearanceFixture(page, { ...defaults, theme: 'system' });
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  const dialog = await openAppearance(page); await expect(dialog.getByRole('button', { name: '跟随系统', exact: true })).toHaveAttribute('aria-pressed', 'true');
  await page.emulateMedia({ colorScheme: 'light' }); await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await dialog.getByRole('button', { name: '深色', exact: true }).click();
  await page.emulateMedia({ colorScheme: 'dark' }); await page.emulateMedia({ colorScheme: 'light' }); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await dialog.getByRole('button', { name: '取消', exact: true }).click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await page.emulateMedia({ colorScheme: 'dark' }); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});

test('mixed appearance and model edits use full configuration save', async ({ page }) => {
  await appearanceFixture(page); const dialog = await openAppearance(page);
  await dialog.getByRole('button', { name: '琥珀', exact: true }).click();
  await dialog.getByRole('button', { name: '模型连接', exact: true }).click(); await dialog.getByLabel('服务地址', { exact: true }).fill('http://127.0.0.1:4321');
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click(); await expect(page.getByRole('dialog')).toHaveCount(0);
  expect(await actionCalls(page, 'save_appearance')).toHaveLength(0);
  const request = (await actionCalls(page, 'save_settings'))[0].args.request as { config: Config };
  expect(request.config.appearance?.accent).toBe('amber'); expect(request.config.connections[0].baseUrl).toBe('http://127.0.0.1:4321');
});

test('appearance saves during an active response without clearing the conversation or stop control', async ({ page }) => {
  await appearanceFixture(page);
  await page.getByRole('textbox', { name: '消息', exact: true }).fill('TEST_FIXTURE running task'); await page.getByRole('button', { name: '发送', exact: true }).click();
  await expect(page.locator('.message.assistant')).toContainText('TEST_FIXTURE live response remains visible');
  const dialog = await openAppearance(page); await dialog.getByRole('button', { name: '浅色', exact: true }).click(); await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0); await expect(page.locator('.message.user')).toContainText('TEST_FIXTURE running task'); await expect(page.locator('.message.assistant')).toContainText('TEST_FIXTURE live response remains visible');
  await expect(page.getByRole('button', { name: '停止', exact: true })).toBeVisible(); expect(await actionCalls(page, 'save_settings')).toHaveLength(0);
  await page.getByRole('button', { name: '停止', exact: true }).click(); await expect(page.locator('.composer-status')).toContainText('已停止');
});

test('background session refresh does not override an unsaved appearance preview', async ({ page }) => {
  await appearanceFixture(page);
  await page.getByRole('textbox', { name: '消息', exact: true }).fill('TEST_FIXTURE preview during background completion');
  await page.getByRole('button', { name: '发送', exact: true }).click();
  await expect(page.locator('.message.assistant')).toContainText('TEST_FIXTURE live response remains visible');
  const dialog = await openAppearance(page);
  await dialog.getByRole('button', { name: '浅色', exact: true }).click();
  await page.evaluate(async () => {
    const target = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args: unknown) => Promise<unknown> } };
    await target.__TAURI_INTERNALS__.invoke('command', { request: { action: 'interrupt', sessionId: 'appearance-session' } });
  });
  await expect(page.locator('.composer-status')).toContainText('已停止');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await dialog.getByRole('button', { name: '取消', exact: true }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(page.locator('.message.user')).toContainText('TEST_FIXTURE preview during background completion');
});

test('small viewport and maximum font settings keep controls accessible and default reset is preview-only', async ({ page }) => {
  await page.setViewportSize({ width: 1120, height: 760 }); await appearanceFixture(page, { ...defaults, theme: 'light', accent: 'amber', fontSize: 18, codeFontSize: 18 });
  const dialog = await openAppearance(page);
  const bounds = await dialog.boundingBox(); expect(bounds).not.toBeNull(); expect(bounds!.x).toBeGreaterThanOrEqual(0); expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(1120);
  await expect(dialog.getByRole('button', { name: '取消', exact: true })).toBeInViewport(); await expect(dialog.getByRole('button', { name: '保存设置', exact: true })).toBeInViewport();
  await dialog.getByRole('button', { name: '恢复默认外观', exact: true }).click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark'); await expect(page.locator('html')).toHaveAttribute('data-accent', 'sage'); await expect(dialog.getByLabel('界面字号', { exact: true })).toHaveValue('13');
  await dialog.getByRole('button', { name: '取消', exact: true }).click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'light'); await expect(page.locator('html')).toHaveAttribute('data-accent', 'amber');
  expect(await actionCalls(page, 'save_appearance')).toHaveLength(0);
});

test('a failed appearance save keeps the preview open and cancellation restores the saved theme', async ({ page }) => {
  await appearanceFixture(page);
  await page.evaluate(() => {
    type Json = Record<string, unknown>;
    const target = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Json) => Promise<unknown> } };
    const invoke = target.__TAURI_INTERNALS__.invoke.bind(target.__TAURI_INTERNALS__);
    target.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
      if (cmd === 'command' && (args.request as Json)?.action === 'save_appearance') throw new Error('TEST_FIXTURE appearance storage unavailable');
      return invoke(cmd, args);
    };
  });
  const dialog = await openAppearance(page); await dialog.getByRole('button', { name: '浅色', exact: true }).click();
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click(); await expect(page.getByRole('alert')).toContainText('TEST_FIXTURE appearance storage unavailable');
  await expect(dialog).toBeVisible(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await dialog.getByRole('button', { name: '取消', exact: true }).click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
});

test('unexpected persisted appearance values normalize safely without out-of-range font controls', async ({ page }) => {
  await appearanceFixture(page, { theme: 'unexpected', accent: 'unknown', fontFamily: 'remote-url', fontSize: 400, codeFontFamily: 'unknown', codeFontSize: -10 } as unknown as AppearanceConfig);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark'); await expect(page.locator('html')).toHaveAttribute('data-accent', 'sage');
  const dialog = await openAppearance(page); await expect(dialog.getByLabel('界面字号', { exact: true })).toHaveValue('18'); await expect(dialog.getByLabel('代码字号', { exact: true })).toHaveValue('10');
  await expect(dialog.getByLabel('界面字体', { exact: true })).toHaveValue('system'); await expect(dialog.getByLabel('代码字体', { exact: true })).toHaveValue('system');
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click(); await expect(page.getByRole('dialog')).toHaveCount(0);
  const appearance = ((await actionCalls(page, 'save_appearance'))[0].args.request as { appearance: AppearanceConfig }).appearance;
  expect(appearance).toEqual({ ...defaults, fontSize: 18, codeFontSize: 10 });
});
