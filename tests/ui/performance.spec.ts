import type { Page } from '@playwright/test';
import { test, expect, baseConfig, installDesktopFixture, ipcCalls } from './fixtures';

type Json = Record<string, unknown>;
const config = () => { const value = structuredClone(baseConfig); value.workspace = '/tmp/performance-ui-fixture'; value.connections[0].model = 'TEST_FIXTURE_MODEL'; value.connections[0].contextLength = 12288; return value; };

/** Controllable test-only Tauri channels; no model or system actions are performed. */
async function performanceFixture(page: Page, history: Json[] = []) {
  await installDesktopFixture(page, { config: config() }); await page.goto('/');
  await expect(page.locator('.workspace-path')).toHaveText('/tmp/performance-ui-fixture');
  await page.evaluate(({ history }) => {
    type Json = Record<string, unknown>;
    type Stream = { id: number; index: number; resolve: (value: unknown) => void };
    const target = window as unknown as { __TAURI_INTERNALS__: { transformCallback: (callback: (value: unknown) => void, once?: boolean) => number; invoke: (cmd: string, args?: Json) => Promise<unknown> }; __HARNESS_TEST_CALLS__: { cmd: string; args: Json }[]; __PERFORMANCE_FIXTURE__: { pending: boolean; emit: (events: Json[]) => void; finish: () => void } };
    const internals = target.__TAURI_INTERNALS__; const invoke = internals.invoke.bind(internals); const transform = internals.transformCallback.bind(internals);
    const callbacks = new Map<number, (value: unknown) => void>(); let stream: Stream | undefined;
    internals.transformCallback = (callback, once) => { const id = transform(callback, once); callbacks.set(id, callback); return id; };
    const fixture = { pending: false,
      emit(events: Json[]) { if (!stream) throw new Error('TEST_FIXTURE has no pending stream'); for (const message of events) callbacks.get(stream.id)?.({ index: stream.index++, message }); },
      finish() { if (!stream) throw new Error('TEST_FIXTURE has no pending stream'); const current = stream; stream = undefined; fixture.pending = false; callbacks.get(current.id)?.({ index: current.index, end: true }); current.resolve(null); },
    };
    target.__PERFORMANCE_FIXTURE__ = fixture;
    internals.invoke = async (cmd, args = {}) => {
      if (cmd === 'stream_turn') { target.__HARNESS_TEST_CALLS__.push({ cmd, args }); fixture.pending = true; return new Promise(resolve => { stream = { id: (args.onEvent as { id: number }).id, index: 0, resolve }; }); }
      const request = args.request as Json | undefined;
      if (cmd === 'command' && request?.action === 'interrupt') { target.__HARNESS_TEST_CALLS__.push({ cmd, args }); fixture.emit([{ type: 'turn_completed', status: 'interrupted' }]); fixture.finish(); return { ok: true }; }
      if (cmd === 'command' && request?.action === 'load_session') return { events: history };
      const result = await invoke(cmd, args);
      if (cmd === 'command' && request?.action === 'snapshot' && history.length) return { ...result as Json, sessions: [{ id: 'long-fixture', title: 'Long history TEST_FIXTURE', workspace: '/tmp/performance-ui-fixture', deviceId: 'local', connectionId: 'lmstudio', createdAt: '2026-11-09', updatedAt: '2026-11-09' }] };
      return result;
    };
  }, { history });
}

async function emit(page: Page, events: Json[]) { await page.evaluate(events => (window as unknown as { __PERFORMANCE_FIXTURE__: { emit: (events: Json[]) => void } }).__PERFORMANCE_FIXTURE__.emit(events), events); }
async function finish(page: Page, status = 'completed') { await emit(page, [{ type: 'assistant_completed' }, { type: 'turn_completed', status }]); await page.evaluate(() => (window as unknown as { __PERFORMANCE_FIXTURE__: { finish: () => void } }).__PERFORMANCE_FIXTURE__.finish()); }
async function send(page: Page) { await page.getByRole('textbox', { name: '消息', exact: true }).fill('TEST_FIXTURE request'); await page.getByRole('button', { name: '发送', exact: true }).click(); await expect.poll(() => page.evaluate(() => (window as unknown as { __PERFORMANCE_FIXTURE__: { pending: boolean } }).__PERFORMANCE_FIXTURE__.pending)).toBe(true); }
const metric = (page: Page, label: string) => page.locator('.performance-grid > div').filter({ has: page.getByText(label, { exact: true }) });

test('measured performance and context fields are visible and reset for a new chat', async ({ page }) => {
  await performanceFixture(page);
  await expect(page.getByRole('button', { name: '推理性能与上下文', exact: true })).toContainText('12K');
  await send(page);
  await emit(page, [
    { type: 'inference_queued', endpoint: 'TEST_FIXTURE' }, { type: 'inference_acquired', waitMs: 1200 },
    { type: 'context_usage', inputTokens: 3000, contextLength: 12288, budget: 7000, outputReserve: 2048, toolCount: 8, toolTokens: 600, isEstimated: true },
    { type: 'text_delta', text: 'TEST_FIXTURE measured response' }, { type: 'usage', inputTokens: 3100, outputTokens: 100 },
    { type: 'model_metrics', step: 0, durationMs: 40000, firstEventMs: 125, firstTokenMs: 875, queueWaitMs: 1200, inputTokens: 3100, outputTokens: 100, tokensPerSecond: 2.5, usageMeasured: true, succeeded: true },
  ]);
  await finish(page);
  await page.getByRole('button', { name: '推理性能与上下文', exact: true }).click();
  await expect(metric(page, '首响应')).toContainText('125 ms'); await expect(metric(page, '首个文本')).toContainText('875 ms');
  await expect(metric(page, '请求吞吐')).toContainText('2.5 tok/s'); await expect(metric(page, '排队耗时')).toContainText('1.20 秒');
  const panel = page.getByRole('region', { name: '推理性能与上下文', exact: true });
  await expect(panel).toContainText('含预填充与网络'); await expect(panel).toContainText('12,288'); await expect(panel).toContainText('600 token / 8 项');
  await expect(panel.getByRole('progressbar', { name: '输入上下文占用', exact: true })).toHaveAttribute('aria-valuemax', '7000');
  await expect(panel.getByRole('progressbar', { name: '输入上下文占用', exact: true })).toHaveAttribute('aria-valuenow', '3100');
  await page.getByRole('button', { name: /新会话/ }).click();
  await expect(metric(page, '请求吞吐')).toContainText('—'); await expect(panel).toContainText('等待一次实际模型请求');
  await expect(page.getByRole('button', { name: '推理性能与上下文', exact: true })).toContainText('约 0 / 12K');
});

test('queue cancellation stays responsive and missing usage never invents throughput', async ({ page }) => {
  await performanceFixture(page); await send(page); await emit(page, [{ type: 'inference_queued', endpoint: 'TEST_FIXTURE' }]);
  await expect(page.locator('.composer-status')).toContainText('排队等待推理资源');
  await expect(page.locator('.composer-status')).toContainText('已等待');
  await page.getByRole('button', { name: '停止', exact: true }).click();
  await expect(page.locator('.composer-status')).toContainText('已停止');
  await send(page); await emit(page, [{ type: 'inference_queued' }, { type: 'inference_acquired', waitMs: 42 }]);
  await expect(page.locator('.composer-status')).toContainText('准备模型响应');
  await expect(page.locator('.composer-status')).not.toContainText('已等待');
  await emit(page, [{ type: 'text_delta', text: 'TEST_FIXTURE no usage' }, { type: 'model_metrics', durationMs: 1000, firstEventMs: 100, tokensPerSecond: 999, usageMeasured: false, succeeded: true }]); await finish(page);
  await page.getByRole('button', { name: '推理性能与上下文', exact: true }).click();
  await expect(metric(page, '请求吞吐')).toContainText('—');
  await expect(page.getByRole('region', { name: '推理性能与上下文', exact: true })).toContainText('后端未返回真实用量');
  await expect(metric(page, '请求吞吐')).not.toContainText('999');
});

test('queue status and stop control fit in the smallest window with large text', async ({ page }) => {
  await page.setViewportSize({ width: 1050, height: 680 });
  await performanceFixture(page);
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '外观', exact: true }).click();
  await dialog.getByLabel('界面字号', { exact: true }).selectOption('18');
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  await send(page);
  await emit(page, [{ type: 'inference_queued' }]);
  const composer = (await page.locator('.composer').boundingBox())!;
  const actions = (await page.locator('.composer-actions').boundingBox())!;
  const status = (await page.locator('.composer-status').boundingBox())!;
  const stop = (await page.getByRole('button', { name: '停止', exact: true }).boundingBox())!;
  expect(actions.x + actions.width).toBeLessThanOrEqual(status.x);
  expect(status.x + status.width).toBeLessThanOrEqual(stop.x);
  expect(stop.x + stop.width).toBeLessThanOrEqual(composer.x + composer.width);
  await page.getByRole('button', { name: '停止', exact: true }).click();
});

test('long history is loaded in windows and streaming does not pull a reader to the bottom', async ({ page }) => {
  const history: Json[] = Array.from({ length: 120 }, (_, index) => ({ kind: 'message', payload: { role: index % 2 ? 'assistant' : 'user', content: `TEST_FIXTURE history ${index}\n\nA paragraph for a genuine overflowing conversation.\n\nAnother paragraph.` } }));
  history.push({ kind: 'activity', payload: { type: 'tool_started', callId: 'large-tool', name: 'file_search', arguments: '{}', deviceId: 'local' } }, { kind: 'activity', payload: { type: 'tool_completed', callId: 'large-tool', durationMs: 1, result: { content: 'TEST_FIXTURE large result '.repeat(5000) } } });
  await performanceFixture(page, history);
  await page.getByRole('button', { name: /设置与连接/ }).click(); await page.getByRole('button', { name: '保存设置', exact: true }).click();
  await page.getByRole('button', { name: 'Long history TEST_FIXTURE', exact: true }).click();
  await expect(page.locator('.messages > .message, .messages > .tool-card')).toHaveCount(80);
  await expect(page.locator('.tool-card .tool-result')).toHaveCount(0);
  await page.getByRole('button', { name: '加载更早记录', exact: true }).click();
  await expect(page.locator('.messages > .message, .messages > .tool-card')).toHaveCount(121);
  await send(page); await emit(page, [{ type: 'text_delta', text: 'TEST_FIXTURE live start\n\n' }]);
  const viewport = page.locator('.conversation-scroll');
  await expect.poll(() => viewport.evaluate(element => element.scrollHeight - element.clientHeight - element.scrollTop)).toBeLessThanOrEqual(1);
  await viewport.hover(); await page.mouse.wheel(0, -500);
  await expect.poll(() => viewport.evaluate(element => element.scrollHeight - element.clientHeight - element.scrollTop)).toBeGreaterThan(64);
  const readingTop = await viewport.evaluate(element => element.scrollTop);
  await emit(page, Array.from({ length: 80 }, (_, index) => ({ type: 'text_delta', text: `burst ${index}\n\n` })));
  await expect(page.locator('.message.assistant').last()).toContainText('burst 79');
  await expect.poll(async () => Math.abs(await viewport.evaluate(element => element.scrollTop) - readingTop)).toBeLessThanOrEqual(1);
  await page.getByRole('button', { name: '回到最新', exact: true }).click();
  await expect.poll(() => viewport.evaluate(element => element.scrollHeight - element.clientHeight - element.scrollTop)).toBeLessThanOrEqual(1);
  await finish(page);
});

test('model retry explanations remain visible after reopening a session', async ({ page }) => {
  const history: Json[] = [
    { kind: 'message', payload: { role: 'user', content: 'TEST_FIXTURE request' } },
    { kind: 'activity', payload: { type: 'model_retry', reason: 'output_token_limit', previousOutputTokens: 2048, outputTokens: 4096 } },
    { kind: 'activity', payload: { type: 'model_retry', reason: 'empty_final_after_tool' } },
    { kind: 'activity', payload: { type: 'capability_warning', capability: 'tool_calling', message: 'TEST_FIXTURE tool diagnostic failed' } },
    { kind: 'message', payload: { role: 'assistant', content: 'TEST_FIXTURE result' } },
  ];
  await performanceFixture(page, history);
  await page.getByRole('button', { name: /设置与连接/ }).click(); await page.getByRole('button', { name: '保存设置', exact: true }).click();
  await page.getByRole('button', { name: 'Long history TEST_FIXTURE', exact: true }).click();
  await expect(page.getByText(/增加预算重试（2048 → 4096 token）/)).toBeVisible();
  await expect(page.getByText(/本次重试不会再次执行工具/)).toBeVisible();
  await expect(page.getByText('TEST_FIXTURE tool diagnostic failed', { exact: true })).toBeVisible();
  await expect(page.getByText('TEST_FIXTURE result', { exact: true })).toBeVisible();
});

test('local and MCP uncertain results require observation while remote results offer status lookup', async ({ page }) => {
  await performanceFixture(page); await send(page);
  const events: Json[] = [];
  for (const [callId, name, deviceId] of [['local', 'browser_act', 'local'], ['mcp', 'mcp_fixture_write', 'remote-fixture'], ['remote', 'terminal_run', 'remote-fixture']]) {
    events.push({ type: 'tool_started', callId, name, deviceId, arguments: '{}' }, { type: 'tool_completed', callId, name, deviceId, result: { isError: true, indeterminate: true, operationId: `TEST_FIXTURE_${callId}` }, durationMs: 1 });
  }
  await emit(page, events); await finish(page, 'failed');
  const cards = page.locator('.tool-card');
  await expect(cards).toHaveCount(3);
  for (const card of await cards.all()) await card.locator('summary').click();
  await expect(page.getByText('动作可能已发生；请重新观察实际状态后继续。', { exact: true })).toHaveCount(2);
  await expect(page.getByRole('button', { name: '查询远端执行状态', exact: true })).toHaveCount(1);
  expect((await ipcCalls(page)).some(call => (call.args.request as Json | undefined)?.action === 'remote_operation_status')).toBe(false);
});

test('closing a pending diagnosis interrupts its operation after saving its connection', async ({ page }) => {
  await installDesktopFixture(page, { config: config() }); await page.goto('/');
  await page.evaluate(() => {
    type Json = Record<string, unknown>;
    const target = window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Json) => Promise<unknown> }; __HARNESS_TEST_CALLS__: { cmd: string; args: Json }[] };
    const invoke = target.__TAURI_INTERNALS__.invoke.bind(target.__TAURI_INTERNALS__); let cancel: (() => void) | undefined;
    target.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
      const request = args.request as Json | undefined;
      if (cmd === 'command' && request?.action === 'diagnose') { target.__HARNESS_TEST_CALLS__.push({ cmd, args }); return new Promise((_resolve, reject) => { cancel = () => reject(new Error('TEST_FIXTURE diagnosis cancelled')); }); }
      if (cmd === 'command' && request?.action === 'interrupt') { target.__HARNESS_TEST_CALLS__.push({ cmd, args }); cancel?.(); return { ok: true }; }
      return invoke(cmd, args);
    };
  });
  await page.getByRole('button', { name: /设置与连接/ }).click();
  await page.getByPlaceholder('留空则保留已保存的密钥').fill('TEST_FIXTURE_SECRET');
  await page.getByRole('button', { name: '保存并运行诊断', exact: true }).click();
  await expect.poll(async () => (await ipcCalls(page)).some(call => (call.args.request as Json | undefined)?.action === 'diagnose')).toBe(true);
  await expect(page.locator('.diagnostic-report')).toHaveCount(0);
  await page.getByRole('button', { name: '关闭设置', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await expect.poll(async () => (await ipcCalls(page)).filter(call => (call.args.request as Json | undefined)?.action === 'interrupt').length).toBe(1);
  const calls = await ipcCalls(page); const diagnosis = calls.find(call => (call.args.request as Json | undefined)?.action === 'diagnose')!.args.request as Json;
  const interrupted = calls.find(call => (call.args.request as Json | undefined)?.action === 'interrupt')!.args.request as Json;
  expect(typeof diagnosis.operationId).toBe('string'); expect(interrupted.sessionId).toBe(diagnosis.operationId);
  expect((diagnosis.connection as Json).apiKey).toBeUndefined();
  expect(calls.findIndex(call => (call.args.request as Json | undefined)?.action === 'save_settings')).toBeLessThan(calls.findIndex(call => (call.args.request as Json | undefined)?.action === 'diagnose'));
  await expect(page.getByRole('alert')).toHaveCount(0);
});
