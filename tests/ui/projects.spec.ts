import { test, expect, baseConfig, installDesktopFixture, ipcCalls } from './fixtures';
import type { Page } from '@playwright/test';
import type { Project } from '../../apps/desktop/src/types';

const a = '/tmp/projects-ui-fixture/a/app';
const b = '/tmp/projects-ui-fixture/b/app';
const project = (id: string, name: string, workspace: string, deviceId = 'local'): Project => ({ id, name, workspace, deviceId, createdAt: '2026-11-09', updatedAt: '2026-11-09' });
async function setup(page: Page) {
  const config = structuredClone(baseConfig); config.workspace = a; config.connections[0].model = 'TEST_FIXTURE_MODEL';
  await installDesktopFixture(page, { config, projects: [project('a', '项目 A', a), project('b', '项目 B', b), project('remote', '远端 A', a, 'remote-fixture')] });
  await page.goto('/'); await expect(page.getByRole('button', { name: '管理项目 项目 A', exact: true })).toBeVisible();
}
async function menu(page: Page, name: string, action: '重命名' | '移除项目') {
  await page.getByRole('button', { name: `管理项目 ${name}`, exact: true }).click();
  await page.getByRole('menuitem', { name: action, exact: true }).click();
}

test('renaming a project changes its display name while keeping the conversation and execution path', async ({ page }) => {
  await setup(page);
  await page.getByRole('textbox', { name: '消息', exact: true }).fill('TEST_FIXTURE keep this conversation');
  await page.getByRole('button', { name: '发送', exact: true }).click();
  await expect(page.locator('.message.user')).toContainText('TEST_FIXTURE keep this conversation');
  await expect(page.getByRole('button', { name: '发送', exact: true })).toBeVisible();
  await menu(page, '项目 A', '重命名');
  await page.getByRole('textbox', { name: '项目名称', exact: true }).fill('  我的 Gemma 工程  ');
  await page.getByRole('button', { name: '保存名称', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '管理项目 我的 Gemma 工程', exact: true })).toBeVisible();
  await expect(page.locator('.breadcrumb')).toContainText('我的 Gemma 工程');
  await expect(page.locator('.workspace-path')).toHaveText(a);
  await expect(page.locator('.message.user')).toContainText('TEST_FIXTURE keep this conversation');
  const calls = await ipcCalls(page);
  expect(calls.filter(call => (call.args.request as Record<string, unknown>)?.action === 'rename_project').at(-1)?.args.request).toEqual({ action: 'rename_project', projectId: 'a', name: '我的 Gemma 工程' });
  expect(calls.some(call => ['file_write', 'terminal_run'].includes(String((call.args.request as Record<string, unknown>)?.name)))).toBe(false);
});

test('removing projects keeps other bindings and historical sessions, and explains file preservation', async ({ page }) => {
  await setup(page);
  await page.getByRole('textbox', { name: '消息', exact: true }).fill('TEST_FIXTURE historical task');
  await page.getByRole('button', { name: '发送', exact: true }).click();
  await expect(page.getByRole('button', { name: '发送', exact: true })).toBeVisible();
  await menu(page, '项目 B', '移除项目');
  await expect(page.getByRole('dialog')).toContainText('文件、Git 仓库和历史会话都会保留');
  await page.getByRole('dialog').getByRole('button', { name: '移除项目', exact: true }).click();
  await expect(page.getByRole('button', { name: '管理项目 项目 B', exact: true })).toHaveCount(0);
  await expect(page.locator('.workspace-path')).toHaveText(a);
  await expect(page.locator('.message.user')).toContainText('TEST_FIXTURE historical task');
  await menu(page, '项目 A', '移除项目');
  await page.getByRole('dialog').getByRole('button', { name: '移除项目', exact: true }).click();
  await expect(page.locator('.workspace-path')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '管理项目 项目 A', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '管理项目 远端 A', exact: true })).toBeVisible();
  await expect(page.locator('.session-item')).toHaveCount(1);
});

test('a running manual terminal blocks project changes, new chat and keyboard navigation', async ({ page }) => {
  await setup(page);
  await page.evaluate(() => {
    type Json = Record<string, unknown>;
    const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Json) => Promise<unknown> }; __finishProjectTerminal: () => void };
    const invoke = w.__TAURI_INTERNALS__.invoke.bind(w.__TAURI_INTERNALS__);
    w.__TAURI_INTERNALS__.invoke = (cmd, args) => cmd === 'stream_tool' ? new Promise(resolve => { w.__finishProjectTerminal = () => resolve({ exit_code: 0, cancelled: false, output_complete: true }); }) : invoke(cmd, args);
  });
  await page.getByRole('button', { name: '终端', exact: true }).click();
  await page.getByRole('textbox', { name: '终端命令', exact: true }).fill('TEST_FIXTURE long-running command');
  await page.getByRole('textbox', { name: '终端命令', exact: true }).press('Enter');
  await expect(page.getByRole('button', { name: '停止终端命令', exact: true })).toBeVisible();
  await expect(page.locator('.project-item').filter({ hasText: '项目 B' })).toBeDisabled();
  await page.getByRole('button', { name: /新会话/ }).click();
  await expect(page.getByRole('alert')).toContainText('请先停止当前任务或终端命令');
  await page.keyboard.press('Control+n');
  await expect(page.locator('.workspace-path')).toHaveText(a);
  await page.getByRole('button', { name: '管理项目 项目 A', exact: true }).click();
  await expect(page.getByRole('menuitem', { name: '移除项目', exact: true })).toBeDisabled();
  await page.keyboard.press('Escape');
  await page.evaluate(() => (window as unknown as { __finishProjectTerminal: () => void }).__finishProjectTerminal());
  await expect(page.getByRole('button', { name: '停止终端命令', exact: true })).toHaveCount(0);
  await page.locator('.project-item').filter({ hasText: '项目 B' }).click();
  await expect(page.locator('.workspace-path')).toHaveText(b);
});

test('failed removal stays visible with its binding intact and can be retried', async ({ page }) => {
  await setup(page);
  await page.evaluate(() => {
    type Json = Record<string, unknown>;
    const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Json) => Promise<unknown> } };
    const invoke = w.__TAURI_INTERNALS__.invoke.bind(w.__TAURI_INTERNALS__);
    let rejectOnce = true;
    w.__TAURI_INTERNALS__.invoke = (cmd, args = {}) => {
      if (cmd === 'command' && (args.request as Json)?.action === 'remove_project' && rejectOnce) { rejectOnce = false; return Promise.reject(new Error('TEST_FIXTURE: task started on another window')); }
      return invoke(cmd, args);
    };
  });
  await menu(page, '项目 A', '移除项目');
  await page.getByRole('dialog').getByRole('button', { name: '移除项目', exact: true }).click();
  await expect(page.getByRole('dialog').getByRole('alert')).toContainText('task started on another window');
  await expect(page.locator('.workspace-path')).toHaveText(a);
  await expect(page.getByRole('button', { name: '管理项目 项目 A', exact: true })).toBeVisible();
  await page.getByRole('dialog').getByRole('button', { name: '移除项目', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect(page.locator('.workspace-path')).toHaveCount(0);
});
