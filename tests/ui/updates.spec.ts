import type { Page } from '@playwright/test';
import { test, expect, installDesktopFixture, screenshot } from './fixtures';

type Mode = 'available' | 'current' | 'error' | 'download-error' | 'slow-error';
async function updateFixture(page: Page, mode: Mode) {
  await installDesktopFixture(page, { projects: [] });
  await page.goto('/');
  await page.evaluate(mode => {
    type Json = Record<string, unknown>;
    const target = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Json) => Promise<unknown> }; __UPDATE_TEST_CALLS__: string[] };
    const original = target.__TAURI_INTERNALS__.invoke.bind(target.__TAURI_INTERNALS__);
    const calls: string[] = []; target.__UPDATE_TEST_CALLS__ = calls;
    target.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
      if (cmd === 'plugin:updater|check') {
        calls.push(cmd);
        if (mode === 'error') throw new Error('TEST_FIXTURE update endpoint unavailable');
        if (mode === 'slow-error') return new Promise((_resolve, reject) => setTimeout(() => reject(new Error('TEST_FIXTURE delayed check failed')), 1000));
        return mode === 'current' ? null : { rid: 31, currentVersion: '0.3.1', version: '0.3.2', rawJson: {} };
      }
      if (cmd === 'plugin:updater|download') {
        calls.push(cmd); if (mode === 'download-error') throw new Error('TEST_FIXTURE signature verification failed'); return 32;
      }
      if (cmd === 'prepare_app_update' || cmd === 'plugin:updater|install' || cmd === 'plugin:process|restart' || cmd === 'plugin:resources|close') { calls.push(cmd); return null; }
      return original(cmd, args);
    };
  }, mode);
}
const calls = (page: Page) => page.evaluate(() => (window as unknown as { __UPDATE_TEST_CALLS__: string[] }).__UPDATE_TEST_CALLS__);

test('startup check announces an update without installing it', async ({ page }) => {
  await updateFixture(page, 'available');
  const panel = page.getByRole('region', { name: '应用更新' });
  await expect(panel).toContainText('0.3.2', { timeout: 7000 });
  await expect(panel.getByRole('button', { name: '下载并安装' })).toBeVisible();
  expect(await calls(page)).toEqual(['plugin:updater|check']);
  await screenshot(page, 'update-available');
});

test('a downloaded update waits for unsent input before installing and restarting', async ({ page }) => {
  await updateFixture(page, 'available');
  await page.getByRole('button', { name: '检查更新' }).click();
  const panel = page.getByRole('region', { name: '应用更新' });
  await expect(panel).toContainText('0.3.2');
  const input = page.getByRole('textbox', { name: '消息', exact: true });
  await input.fill('尚未发送');
  await panel.getByRole('button', { name: '下载并安装' }).click();
  await expect(panel).toContainText('请先发送或清空未提交的消息');
  expect(await calls(page)).toEqual(['plugin:updater|check', 'plugin:updater|download']);
  await input.fill('');
  await panel.getByRole('button', { name: '安装并重启' }).click();
  await expect.poll(() => calls(page)).toEqual(['plugin:updater|check', 'plugin:updater|download', 'prepare_app_update', 'plugin:updater|install', 'plugin:process|restart']);
});

test('a failed download never installs or claims success', async ({ page }) => {
  await updateFixture(page, 'download-error');
  await page.getByRole('button', { name: '检查更新' }).click();
  const panel = page.getByRole('region', { name: '应用更新' });
  await panel.getByRole('button', { name: '下载并安装' }).click();
  await expect(panel).toContainText('下载失败');
  await expect(panel).not.toContainText('更新已安装');
  expect(await calls(page)).toEqual(['plugin:updater|check', 'plugin:updater|download']);
});

test('manual checks distinguish a current version from an unavailable endpoint', async ({ page }) => {
  await updateFixture(page, 'current');
  await page.getByRole('button', { name: '检查更新' }).click();
  await expect(page.getByRole('region', { name: '应用更新' })).toContainText('当前已是最新版本');
});

test('manual check errors remain visible instead of reporting a current version', async ({ page }) => {
  await updateFixture(page, 'error');
  await page.getByRole('button', { name: '检查更新' }).click();
  const panel = page.getByRole('region', { name: '应用更新' });
  await expect(panel).toContainText('TEST_FIXTURE update endpoint unavailable');
  await expect(panel).not.toContainText('当前已是最新版本');
});

test('opening a background check shows its eventual error', async ({ page }) => {
  await updateFixture(page, 'slow-error');
  await expect.poll(() => calls(page), { timeout: 7000 }).toEqual(['plugin:updater|check']);
  await page.getByRole('button', { name: '检查更新' }).click();
  const panel = page.getByRole('region', { name: '应用更新' });
  await expect(panel).toContainText('正在检查更新');
  await expect(panel).toContainText('TEST_FIXTURE delayed check failed');
});
