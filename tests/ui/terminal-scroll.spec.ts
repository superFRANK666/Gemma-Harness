import type { Locator, Page } from '@playwright/test';
import { test, expect, baseConfig, installDesktopFixture } from './fixtures';

/** Test-only Tauri Channel stream controlled from this spec, never product code. */
async function terminalFixture(page: Page) {
  const config = structuredClone(baseConfig);
  config.workspace = '/tmp/terminal-scroll-test-fixture';
  await installDesktopFixture(page, { config });
  await page.goto('/');
  await page.getByRole('button', { name: '终端', exact: true }).click();
  await expect(page.getByRole('textbox', { name: '终端命令', exact: true })).toBeEnabled();
  await page.evaluate(() => {
    type Json = Record<string, unknown>;
    type Internals = { transformCallback: (callback: (message: unknown) => void, once?: boolean) => number; invoke: (command: string, args?: Json) => Promise<unknown> };
    type Stream = { id: number; index: number; resolve: (result: unknown) => void };
    const target = window as unknown as { __TAURI_INTERNALS__: Internals; __HARNESS_TEST_CALLS__: { cmd: string; args: Json }[]; __TERMINAL_SCROLL_FIXTURE__: { emit: (text: string, stream?: string, truncated?: boolean) => void; finish: (result?: Json) => void; started: number } };
    const internals = target.__TAURI_INTERNALS__;
    const transform = internals.transformCallback.bind(internals);
    const invoke = internals.invoke.bind(internals);
    const callbacks = new Map<number, (message: unknown) => void>();
    let pending: Stream | undefined;
    internals.transformCallback = (callback, once) => {
      const id = transform(callback, once); callbacks.set(id, callback); return id;
    };
    const fixture = {
      started: 0,
      emit(text: string, stream = 'stdout', truncated = false) {
        if (!pending) throw new Error('No pending terminal stream in test fixture');
        callbacks.get(pending.id)?.({ index: pending.index++, message: { type: 'tool_output', stream, text, truncated } });
      },
      finish(result: Json = {}) {
        if (!pending) throw new Error('No pending terminal stream in test fixture');
        const current = pending; pending = undefined;
        callbacks.get(current.id)?.({ index: current.index, end: true });
        current.resolve({ exit_code: 0, cancelled: false, timed_out: false, output_complete: true, stdout: '', stderr: '', stdout_artifact: 'TEST_FIXTURE_STDOUT', stderr_artifact: 'TEST_FIXTURE_STDERR', ...result });
      },
    };
    target.__TERMINAL_SCROLL_FIXTURE__ = fixture;
    internals.invoke = (command, args = {}) => {
      if (command !== 'stream_tool') return invoke(command, args);
      target.__HARNESS_TEST_CALLS__.push({ cmd: command, args: JSON.parse(JSON.stringify(args)) as Json });
      if (pending) throw new Error('Test fixture received overlapping terminal commands');
      fixture.started++;
      return new Promise(resolve => { pending = { id: (args.onEvent as { id: number }).id, index: 0, resolve }; });
    };
  });
  return page.locator('.terminal-panel pre');
}

async function emit(page: Page, text: string, stream = 'stdout', truncated = false) {
  await page.evaluate(({ text, stream, truncated }) => (window as unknown as { __TERMINAL_SCROLL_FIXTURE__: { emit: (text: string, stream: string, truncated: boolean) => void } }).__TERMINAL_SCROLL_FIXTURE__.emit(text, stream, truncated), { text, stream, truncated });
}

async function finish(page: Page, result: Record<string, unknown> = {}) {
  await page.evaluate(result => (window as unknown as { __TERMINAL_SCROLL_FIXTURE__: { finish: (result: Record<string, unknown>) => void } }).__TERMINAL_SCROLL_FIXTURE__.finish(result), result);
  await expect(page.getByRole('textbox', { name: '终端命令', exact: true })).toBeEnabled();
}

async function start(page: Page, command: string, started: number) {
  const input = page.getByRole('textbox', { name: '终端命令', exact: true });
  await input.fill(command); await input.press('Enter');
  await expect.poll(() => page.evaluate(() => (window as unknown as { __TERMINAL_SCROLL_FIXTURE__: { started: number } }).__TERMINAL_SCROLL_FIXTURE__.started)).toBe(started);
  await expect(page.getByRole('button', { name: '停止终端命令', exact: true })).toBeVisible();
}

const lines = (prefix: string, count = 80) => Array.from({ length: count }, (_, index) => `${prefix} ${index}`).join('\n') + '\n';
const gap = (output: Locator) => output.evaluate(element => element.scrollHeight - element.clientHeight - element.scrollTop);
const top = (output: Locator) => output.evaluate(element => element.scrollTop);
const atBottom = async (output: Locator) => { await expect.poll(() => gap(output)).toBeLessThanOrEqual(1); };

test('clear empties the terminal without executing a shell command', async ({ page }) => {
  const output = await terminalFixture(page);
  await start(page, 'pwd', 1);
  await emit(page, '/tmp/terminal-scroll-test-fixture\n');
  await finish(page);

  const input = page.getByRole('textbox', { name: '终端命令', exact: true });
  await input.fill('  clear  '); await input.press('Enter');
  await expect(output).toHaveText('');
  await expect(input).toHaveValue('');
  expect(await page.evaluate(() => (window as unknown as { __TERMINAL_SCROLL_FIXTURE__: { started: number } }).__TERMINAL_SCROLL_FIXTURE__.started)).toBe(1);

  // Only a standalone clear is handled locally; compound commands still run.
  await start(page, 'clear; echo after', 2);
  await emit(page, 'after\n');
  await finish(page);
  await expect(output).toHaveText('\n$ clear; echo after\nafter\n');
});

test('successful commands show streamed output once and silent commands add no metadata', async ({ page }) => {
  const output = await terminalFixture(page);
  await start(page, 'pwd', 1);
  await emit(page, '/Users/raven/Desktop\n');
  await finish(page, { stdout: '/Users/raven/Desktop\n' });
  await start(page, 'ls', 2);
  await finish(page);
  await expect(output).toHaveText('\n$ pwd\n/Users/raven/Desktop\n\n$ ls\n');
});

test('terminal reports failed, interrupted, timed out and incomplete commands', async ({ page }) => {
  const output = await terminalFixture(page);
  await start(page, 'missing-command', 1);
  await emit(page, 'command not found\n', 'stderr');
  await finish(page, { exit_code: 127, stderr: 'command not found\n' });
  await expect(output).toContainText('command not found\n[命令退出，退出码：127]');

  await start(page, 'interrupt-me', 2);
  await finish(page, { cancelled: true, exit_code: null });
  await expect(output).toContainText('[命令已中断]');

  await start(page, 'timeout-me', 3);
  await finish(page, { timed_out: true, exit_code: null });
  await expect(output).toContainText('[命令超时]');

  await start(page, 'capture-failure', 4);
  await finish(page, { output_complete: false, isError: true, stdout_error: 'stdout capture failed' });
  await expect(output).toContainText('[命令输出采集不完整]\nstdout capture failed');
  await expect(output).toContainText('标准输出文件（执行设备）：TEST_FIXTURE_STDOUT');
  await expect(output).not.toContainText('"exit_code"');
});

test('truncated terminal output keeps the available artifact paths', async ({ page }) => {
  const output = await terminalFixture(page);
  await start(page, 'long-output', 1);
  await emit(page, 'partial output');
  await emit(page, '\n[Live output preview truncated; check artifacts and output completeness.]\n', 'stdout', true);
  await expect(output).toContainText('partial output\n[实时输出已截断；输出文件每个流最多保留 16 MiB。]');
  await finish(page);
  await expect(output).toContainText('标准输出文件（执行设备）：TEST_FIXTURE_STDOUT');
  await expect(output).toContainText('标准错误文件（执行设备）：TEST_FIXTURE_STDERR');
  await expect(output).not.toContainText('check artifacts and output completeness');
});

test('terminal follows live chunks, preserves reading history, and resumes near the bottom', async ({ page }) => {
  const output = await terminalFixture(page);
  await start(page, 'TEST_FIXTURE live output', 1);
  await emit(page, lines('seed'));
  await expect.poll(() => output.evaluate(element => element.scrollHeight - element.clientHeight)).toBeGreaterThan(200);
  await atBottom(output);
  await emit(page, lines('live chunk', 20));
  await atBottom(output);

  await output.hover(); await page.mouse.wheel(0, -320);
  await expect.poll(() => gap(output)).toBeGreaterThan(24);
  const readingPosition = await top(output);
  await emit(page, lines('arrived while reading', 20));
  await expect(output).toContainText('arrived while reading 19');
  await expect.poll(async () => Math.abs(await top(output) - readingPosition)).toBeLessThanOrEqual(1);
  expect(await gap(output)).toBeGreaterThan(24);

  await output.hover(); await page.mouse.wheel(0, 100000); await atBottom(output);
  await page.mouse.wheel(0, -20);
  await expect.poll(() => gap(output)).toBeGreaterThan(0);
  expect(await gap(output)).toBeLessThanOrEqual(24);
  await emit(page, lines('near bottom resumes', 10));
  await atBottom(output);
  await finish(page);
  await atBottom(output);
});

test('a new manual command and reopening the terminal restore following', async ({ page }) => {
  const output = await terminalFixture(page);
  await start(page, 'TEST_FIXTURE first command', 1);
  await emit(page, lines('first command output')); await atBottom(output);
  await output.hover(); await page.mouse.wheel(0, -320);
  await expect.poll(() => gap(output)).toBeGreaterThan(24);
  await finish(page);

  await start(page, 'TEST_FIXTURE second command', 2);
  await atBottom(output);
  await emit(page, lines('second command output', 30)); await atBottom(output);
  await output.hover(); await page.mouse.wheel(0, -320);
  await expect.poll(() => gap(output)).toBeGreaterThan(24);
  await page.getByRole('button', { name: '关闭终端', exact: true }).click();
  await expect(output).toHaveCount(0);
  await emit(page, lines('arrived while closed', 20));
  await page.getByRole('button', { name: '终端', exact: true }).click();
  await expect(output).toContainText('arrived while closed 19');
  await atBottom(output);
  await finish(page);
  await atBottom(output);
});
