import type { Page } from '@playwright/test';
import { test, expect, baseConfig, installDesktopFixture } from './fixtures';

const workspaceA = '/tmp/binding-fixture-a';
const workspaceB = '/tmp/binding-fixture-b';

/** Explicit test IPC files are namespaced by execution device and workspace. */
async function bindingFixture(page: Page, initialWorkspace = workspaceA, history = false) {
  const config = structuredClone(baseConfig);
  config.workspace = initialWorkspace;
  config.toolPermissions.desktop = true;
  config.devices = [{ id: 'remote-fixture', name: 'Remote fixture', url: 'wss://fixture.invalid:45455', certificatePin: 'TEST_FIXTURE_ONLY' }];
  await installDesktopFixture(page, { config });
  await page.goto('/');
  await expect(page.locator('.workspace-path')).toHaveText(initialWorkspace);
  await page.evaluate(({ initialConfig, a, b, includeHistory }) => {
    type Json = Record<string, unknown>;
    type SavedFile = { content: string; sha256: string };
    type Internals = { invoke: (command: string, args?: Json) => Promise<unknown> };
    const target = window as unknown as {
      __TAURI_INTERNALS__: Internals;
      __HARNESS_TEST_CALLS__: { cmd: string; args: Json }[];
      __BINDING_FIXTURE__: { reads: Json[]; writes: Json[]; pending: boolean; delayNext: (tool: string) => void; release: () => void; chooseDirectory: (path: string) => void };
    };
    let config = structuredClone(initialConfig);
    let delay = '';
    let release: (() => void) | undefined;
    let nextDirectory: string | undefined;
    const files = new Map<string, SavedFile>([
      [`local|${a}`, { content: 'LOCAL A original\n', sha256: 'a'.repeat(64) }],
      [`local|${b}`, { content: 'LOCAL B original\n', sha256: 'b'.repeat(64) }],
      [`remote-fixture|${a}`, { content: 'REMOTE A original\n', sha256: 'c'.repeat(64) }],
    ]);
    const history = { id: 'history-fixture-a', title: 'History fixture A', workspace: a, deviceId: 'local', connectionId: config.selectedConnectionId, createdAt: '2026-11-09T00:00:00Z', updatedAt: '2026-11-09T00:00:00Z' };
    const fixture = {
      reads: [] as Json[], writes: [] as Json[], pending: false,
      delayNext(tool: string) { delay = tool; },
      release() { if (!release) throw new Error('No delayed fixture response'); const finish = release; release = undefined; fixture.pending = false; finish(); },
      chooseDirectory(path: string) { nextDirectory = path; },
    };
    target.__BINDING_FIXTURE__ = fixture;
    const invoke = target.__TAURI_INTERNALS__.invoke.bind(target.__TAURI_INTERNALS__);
    target.__TAURI_INTERNALS__.invoke = async (command, args = {}) => {
      if (command === 'plugin:dialog|open' && nextDirectory !== undefined) {
        const path = nextDirectory; nextDirectory = undefined;
        target.__HARNESS_TEST_CALLS__.push({ cmd: command, args }); return path;
      }
      if (command !== 'command') return invoke(command, args);
      const request = args.request as Json;
      if (request.action === 'open_project') {
        const result = await invoke(command, args) as Json;
        config = structuredClone(result.config) as typeof config;
        return result;
      }
      if (request.action === 'save_settings') { config = structuredClone(request.config) as typeof config; return invoke(command, args); }
      if (request.action === 'snapshot') {
        const snapshot = await invoke(command, args) as Json;
        return includeHistory ? { ...snapshot, sessions: [history] } : snapshot;
      }
      if (request.action === 'load_session') {
        target.__HARNESS_TEST_CALLS__.push({ cmd: command, args }); return { events: [] };
      }
      if (request.action !== 'execute_tool') return invoke(command, args);
      target.__HARNESS_TEST_CALLS__.push({ cmd: command, args: JSON.parse(JSON.stringify(args)) as Json });
      const binding = request.sessionId === history.id ? history : config;
      const deviceId = 'deviceId' in binding ? binding.deviceId : 'local';
      const workspace = binding.workspace;
      const key = `${deviceId}|${workspace}`;
      const file = files.get(key);
      const tool = String(request.name);
      const params = request.args as Json;
      let result: Json;
      switch (tool) {
        case 'file_list': result = { entries: file ? [{ path: 'acceptance.txt', directory: false }] : [] }; break;
        case 'file_read': {
          fixture.reads.push({ deviceId, workspace, path: params.path });
          if (!file) throw new Error('TEST_FIXTURE missing file');
          result = { path: params.path, content: file.content, sha256: file.sha256, raw: true, has_more: false }; break;
        }
        case 'file_write': {
          if (!file || params.expected_sha256 !== file.sha256) throw new Error('TEST_FIXTURE expected hash did not match selected execution binding');
          fixture.writes.push({ deviceId, workspace, ...params });
          const saved = { content: String(params.content), sha256: 'd'.repeat(64) };
          files.set(key, saved); result = { path: params.path, sha256: saved.sha256 }; break;
        }
        case 'desktop_observe': result = { snapshot_id: `TEST_FIXTURE_${key}`, marker: `OBSERVATION ${key}`, elements: [] }; break;
        case 'desktop_screenshot': result = { snapshot_id: `TEST_FIXTURE_${key}`, artifact: 'TEST_FIXTURE_IMAGE', content: [{ type: 'image', mimeType: 'image/png', data: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZKsAAAAASUVORK5CYII=' }] }; break;
        default: throw new Error(`Unmocked binding fixture tool: ${tool}`);
      }
      if (delay === tool) {
        delay = ''; fixture.pending = true;
        return new Promise(resolve => { release = () => resolve(result); });
      }
      return result;
    };
  }, { initialConfig: config, a: workspaceA, b: workspaceB, includeHistory: history });
}

async function changeBinding(page: Page, workspace: string, deviceId?: string) {
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog', { name: '设置与连接' });
  await dialog.getByRole('button', { name: '执行设备', exact: true }).click();
  if (deviceId) await dialog.getByRole('combobox', { name: '执行设备', exact: true }).selectOption(deviceId);
  await dialog.locator('label.field').filter({ hasText: '执行设备上的工作目录' }).locator('input').fill(workspace);
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  if (workspace) await expect(page.locator('.workspace-path')).toHaveText(workspace);
}

async function openFile(page: Page, content: string) {
  await page.locator('.inspector-tabs').getByRole('button', { name: '文件', exact: true }).click();
  await page.locator('.file-list').getByRole('button', { name: 'acceptance.txt', exact: true }).click();
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveValue(content);
}

async function writes(page: Page) {
  return page.evaluate(() => (window as unknown as { __BINDING_FIXTURE__: { writes: Record<string, unknown>[] } }).__BINDING_FIXTURE__.writes);
}

async function delayNext(page: Page, tool: string) {
  await page.evaluate(name => (window as unknown as { __BINDING_FIXTURE__: { delayNext: (tool: string) => void } }).__BINDING_FIXTURE__.delayNext(name), tool);
}

async function releaseDelayed(page: Page) {
  await page.evaluate(() => (window as unknown as { __BINDING_FIXTURE__: { release: () => void } }).__BINDING_FIXTURE__.release());
  await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

test('clearing a workspace removes its dirty editor before a different workspace can save', async ({ page }) => {
  await bindingFixture(page);
  await openFile(page, 'LOCAL A original\n');
  await page.getByRole('textbox', { name: '文件编辑器', exact: true }).fill('UNSAVED A');
  await changeBinding(page, '');
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveCount(0);
  await expect(page.locator('.file-editor').getByRole('button', { name: '保存', exact: true })).toHaveCount(0);
  await expect(page.locator('.project-item')).toHaveCount(0);
  expect(await writes(page)).toEqual([]);

  await changeBinding(page, workspaceB);
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveCount(0);
  await openFile(page, 'LOCAL B original\n');
  await page.getByRole('textbox', { name: '文件编辑器', exact: true }).fill('EDITED B');
  await page.locator('.file-editor').getByRole('button', { name: '保存', exact: true }).click();
  await expect.poll(() => writes(page)).toEqual([{ deviceId: 'local', workspace: workspaceB, path: 'acceptance.txt', content: 'EDITED B', expected_sha256: 'b'.repeat(64) }]);
});

test('switching devices at the same path clears the editor, observation and image', async ({ page }) => {
  await bindingFixture(page);
  await openFile(page, 'LOCAL A original\n');
  await page.getByRole('textbox', { name: '文件编辑器', exact: true }).fill('UNSAVED LOCAL A');
  await page.locator('.inspector-tabs').getByRole('button', { name: '电脑', exact: true }).click();
  await expect(page.locator('.inspection-content')).toContainText(`OBSERVATION local|${workspaceA}`);
  await page.getByRole('button', { name: '截图', exact: true }).click();
  await expect(page.getByRole('img', { name: '当前执行设备的真实截图', exact: true })).toBeVisible();
  await changeBinding(page, workspaceA, 'remote-fixture');
  await expect(page.locator('.inspector-tabs button.selected')).toHaveText('文件');
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveCount(0);
  await expect(page.getByRole('img', { name: '当前执行设备的真实截图', exact: true })).toHaveCount(0);
  await expect(page.locator('.inspection-content')).toHaveCount(0);
  await openFile(page, 'REMOTE A original\n');
  expect(await writes(page)).toEqual([]);
  await page.locator('.inspector-tabs').getByRole('button', { name: '电脑', exact: true }).click();
  await expect(page.locator('.inspection-content')).toContainText(`OBSERVATION remote-fixture|${workspaceA}`);
  await expect(page.getByRole('img', { name: '当前执行设备的真实截图', exact: true })).toHaveCount(0);
});

test('a delayed old file read cannot repopulate the inspector after choosing another workspace', async ({ page }) => {
  await bindingFixture(page);
  await page.locator('.inspector-tabs').getByRole('button', { name: '文件', exact: true }).click();
  await delayNext(page, 'file_read');
  await page.locator('.file-list').getByRole('button', { name: 'acceptance.txt', exact: true }).click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __BINDING_FIXTURE__: { pending: boolean } }).__BINDING_FIXTURE__.pending)).toBe(true);
  await page.evaluate(path => (window as unknown as { __BINDING_FIXTURE__: { chooseDirectory: (path: string) => void } }).__BINDING_FIXTURE__.chooseDirectory(path), workspaceB);
  await page.getByRole('button', { name: '打开项目', exact: true }).click();
  await expect(page.locator('.workspace-path')).toHaveText(workspaceB);
  await openFile(page, 'LOCAL B original\n');
  await releaseDelayed(page);
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveValue('LOCAL B original\n');
  expect(await writes(page)).toEqual([]);
});

test('a delayed old save does not issue a refresh against the newly selected workspace', async ({ page }) => {
  await bindingFixture(page);
  await openFile(page, 'LOCAL A original\n');
  await page.getByRole('textbox', { name: '文件编辑器', exact: true }).fill('SAVE A IN FLIGHT');
  await delayNext(page, 'file_write');
  await page.locator('.file-editor').getByRole('button', { name: '保存', exact: true }).click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __BINDING_FIXTURE__: { pending: boolean } }).__BINDING_FIXTURE__.pending)).toBe(true);
  await changeBinding(page, workspaceB);
  await openFile(page, 'LOCAL B original\n');
  const readsBefore = await page.evaluate(() => (window as unknown as { __BINDING_FIXTURE__: { reads: unknown[] } }).__BINDING_FIXTURE__.reads.length);
  await releaseDelayed(page);
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveValue('LOCAL B original\n');
  expect(await page.evaluate(() => (window as unknown as { __BINDING_FIXTURE__: { reads: unknown[] } }).__BINDING_FIXTURE__.reads.length)).toBe(readsBefore);
  await expect(page.getByRole('alert')).toHaveCount(0);
  expect((await writes(page)).map(write => write.workspace)).toEqual([workspaceA]);
});

test('starting a new chat from a historical binding removes that history workspace editor', async ({ page }) => {
  await bindingFixture(page, workspaceB, true);
  await changeBinding(page, workspaceB);
  await page.getByRole('button', { name: 'History fixture A', exact: true }).click();
  await expect(page.locator('.workspace-path')).toHaveText(workspaceA);
  await openFile(page, 'LOCAL A original\n');
  await page.getByRole('textbox', { name: '文件编辑器', exact: true }).fill('UNSAVED HISTORICAL A');
  await page.getByRole('button', { name: /新会话/ }).click();
  await expect(page.locator('.workspace-path')).toHaveText(workspaceB);
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveCount(0);
  await openFile(page, 'LOCAL B original\n');
  expect(await writes(page)).toEqual([]);
});
