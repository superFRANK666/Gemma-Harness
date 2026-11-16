import type { Page } from '@playwright/test';
import { test, expect, baseConfig, installDesktopFixture, ipcCalls, screenshot } from './fixtures';
import type { Session } from '../../apps/desktop/src/types';

function configuredState() {
  const config = structuredClone(baseConfig);
  config.workspace = '/tmp/upgrades-fixture';
  config.connections[0].model = 'gemma-4-upgrade-test';
  return config;
}

async function installUpgradeFixture(
  page: Page,
  {
    sessions = [],
    diffStdout = '',
    events = [],
  }: {
    sessions?: Session[];
    diffStdout?: string;
    events?: Record<string, unknown>[];
  } = {},
) {
  await installDesktopFixture(page, { config: configuredState(), sessions });
  await page.goto('/');
  await page.evaluate(
    ({ diffStdout, events }) => {
      type Json = Record<string, unknown>;
      const target = window as unknown as {
        __TAURI_INTERNALS__: { invoke: (cmd: string, args?: Json) => Promise<unknown> };
        __HARNESS_TEST_CALLS__: { cmd: string; args: Json }[];
      };
      const original = target.__TAURI_INTERNALS__.invoke.bind(target.__TAURI_INTERNALS__);
      target.__TAURI_INTERNALS__.invoke = async (cmd, args = {}) => {
        if (cmd !== 'command') return original(cmd, args);
        const request = args.request as Json;
        if (request.action === 'rename_session') {
          target.__HARNESS_TEST_CALLS__.push({ cmd, args });
          return { ok: true };
        }
        if (request.action === 'export_session') {
          target.__HARNESS_TEST_CALLS__.push({ cmd, args });
          return { session: { id: request.sessionId }, events };
        }
        if (request.action === 'load_session') {
          target.__HARNESS_TEST_CALLS__.push({ cmd, args });
          return { events };
        }
        if (request.action === 'execute_tool') {
          target.__HARNESS_TEST_CALLS__.push({ cmd, args });
          const name = String(request.name);
          const toolArgs = request.args as Json;
          if (name === 'file_list') {
            if (toolArgs.path === 'src') {
              return { entries: [{ path: 'src/App.tsx', directory: false }] };
            }
            return {
              entries: [
                { path: 'src', directory: true },
                { path: 'README.md', directory: false },
              ],
            };
          }
          if (name === 'file_read') {
            return { path: toolArgs.path, content: 'export const ok = true;\n', sha256: 'a'.repeat(64) };
          }
          if (name === 'git_diff') {
            return {
              exit_code: 0,
              cancelled: false,
              timed_out: false,
              output_complete: true,
              stdout: diffStdout,
              stderr: '',
            };
          }
        }
        return original(cmd, args);
      };
    },
    { diffStdout, events },
  );
}

test('sidebar collapse, session search reset on Escape, and session rename work cleanly', async ({ page }) => {
  const sessions: Session[] = [
    {
      id: 's1',
      title: '重构前端组件',
      workspace: '/tmp/upgrades-fixture',
      deviceId: 'local',
      connectionId: 'lmstudio',
      createdAt: '2026-11-16',
      updatedAt: '2026-11-16',
    },
    {
      id: 's2',
      title: '修复 Git 差异视图',
      workspace: '/tmp/upgrades-fixture',
      deviceId: 'local',
      connectionId: 'lmstudio',
      createdAt: '2026-11-16',
      updatedAt: '2026-11-16',
    },
  ];
  await installUpgradeFixture(page, { sessions });

  // Sidebar collapse & expand
  await page.getByRole('button', { name: '收起侧边栏', exact: true }).click();
  await expect(page.locator('.sidebar')).toHaveClass(/collapsed/);
  await page.getByRole('button', { name: '展开侧边栏', exact: true }).click();
  await expect(page.locator('.sidebar')).not.toHaveClass(/collapsed/);

  // Session search filters and clears on Escape
  await page.getByRole('button', { name: /搜索会话/ }).click();
  const search = page.getByRole('textbox', { name: '搜索会话', exact: true });
  await search.fill('差异');
  await expect(page.locator('.session-item')).toHaveCount(1);
  await search.press('Escape');
  await expect(search).toHaveCount(0);
  await expect(page.locator('.session-item')).toHaveCount(2);

  // Rename session
  await page.locator('.session-row').filter({ hasText: '重构前端组件' }).getByRole('button', { name: '会话操作', exact: true }).click();
  await page.getByRole('menuitem', { name: '重命名', exact: true }).click();
  await page.getByRole('textbox', { name: '会话标题', exact: true }).fill('前端架构升级');
  await page.getByRole('button', { name: '保存标题', exact: true }).click();
  await expect(page.getByRole('button', { name: '前端架构升级', exact: true })).toBeVisible();
  const calls = await ipcCalls(page);
  expect(
    calls.find(c => (c.args.request as Record<string, unknown>)?.action === 'rename_session')?.args.request,
  ).toEqual({ action: 'rename_session', sessionId: 's1', title: '前端架构升级' });
});

test('inspector navigates directories back to root and guards unsaved editor close', async ({ page }) => {
  await installUpgradeFixture(page);
  await page.locator('.inspector-tabs').getByRole('button', { name: '文件', exact: true }).click();
  await page.locator('.file-list').getByRole('button', { name: 'src', exact: true }).click();
  await expect(page.getByRole('button', { name: /返回上级/ })).toBeVisible();
  await page.locator('.file-list').getByRole('button', { name: 'src/App.tsx', exact: true }).click();
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveValue('export const ok = true;\n');

  await page.getByRole('textbox', { name: '文件编辑器', exact: true }).fill('export const ok = false;\n');
  await expect(page.locator('.file-dirty-badge')).toContainText('未保存');
  await page.locator('.file-editor-bar').getByRole('button', { name: '← 文件', exact: true }).click();
  await expect(page.getByText('有未保存修改')).toBeVisible();
  await page.getByRole('button', { name: '放弃修改', exact: true }).click();
  await expect(page.getByRole('textbox', { name: '文件编辑器', exact: true })).toHaveCount(0);

  await page.getByRole('button', { name: /返回上级/ }).click();
  await expect(page.locator('.file-list').getByRole('button', { name: 'README.md', exact: true })).toBeVisible();
});

test('inspector diff tab parses git diff hunks and active conversation renders code copy and tool badges', async ({ page }) => {
  const diffStdout = [
    'diff --git a/apps/desktop/src/App.tsx b/apps/desktop/src/App.tsx',
    'index 1111111..2222222 100644',
    '--- a/apps/desktop/src/App.tsx',
    '+++ b/apps/desktop/src/App.tsx',
    '@@ -10,3 +10,4 @@',
    ' const keep = true;',
    '-const oldName = "x";',
    '+const newName = "y";',
    '+export { newName };',
  ].join('\n');

  const events = [
    {
      kind: 'message',
      payload: {
        role: 'user',
        content: [
          { type: 'text', text: '请优化前端组件并检查差异' },
          { type: 'text', text: '[Saved image artifact: abc123.png; MIME: image/png; bytes: 1024; SHA256: 0000.]' },
        ],
      },
    },
    {
      kind: 'activity',
      payload: { type: 'tool_started', callId: 'c1', name: 'file_read', arguments: '{"path":"App.tsx"}', deviceId: 'local' },
    },
    {
      kind: 'activity',
      payload: { type: 'tool_completed', callId: 'c1', name: 'file_read', durationMs: 14, result: { ok: true } },
    },
    {
      kind: 'activity',
      payload: { type: 'tool_started', callId: 'c1', name: 'terminal_run', arguments: '{"command":"npm test"}', deviceId: 'local' },
    },
    {
      kind: 'activity',
      payload: { type: 'tool_completed', callId: 'c1', name: 'terminal_run', durationMs: 32, result: { exit_code: 1 } },
    },
    {
      kind: 'message',
      payload: {
        role: 'assistant',
        content: '已完成审查与升级：\n\n```ts\nexport const upgraded = true;\n```',
      },
    },
  ];

  const sessions: Session[] = [
    {
      id: 'active-ui',
      title: '前端页面优化审查',
      workspace: '/tmp/upgrades-fixture',
      deviceId: 'local',
      connectionId: 'lmstudio',
      createdAt: '2026-11-16',
      updatedAt: '2026-11-16',
    },
  ];

  await installUpgradeFixture(page, { sessions, diffStdout, events });
  await page.getByRole('button', { name: '前端页面优化审查', exact: true }).click();
  await expect(page.locator('.message.user')).toContainText('请优化前端组件并检查差异');
  await expect(page.locator('.message-image-pill')).toContainText('abc123.png');
  await expect(page.locator('.message.user')).not.toContainText('Use artifact_view');
  await expect(page.locator('.tool-card')).toHaveCount(2);
  await expect(page.locator('.tool-state-badge.tone-done')).toHaveText('14 ms');
  await expect(page.locator('.tool-state-badge.tone-error')).toHaveText('执行失败');
  await expect(page.locator('.code-block-header')).toContainText('ts');
  await expect(page.getByRole('button', { name: '复制代码', exact: true })).toBeVisible();

  await page.locator('.inspector-tabs').getByRole('button', { name: '差异', exact: true }).click();
  await expect(page.locator('.diff-summary')).toContainText('1 个文件变更');
  await expect(page.locator('.diff-line.add')).toHaveCount(2);
  await expect(page.locator('.diff-line.del')).toHaveCount(1);
  await screenshot(page, 'conversation-and-diff');
});

test('Escape dismisses Settings and PerformancePanel and diagnostic pills display', async ({ page }) => {
  await installUpgradeFixture(page);
  await page.getByRole('button', { name: '推理性能与上下文', exact: true }).click();
  await expect(page.getByRole('region', { name: '推理性能与上下文', exact: true })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(page.getByRole('region', { name: '推理性能与上下文', exact: true })).toHaveCount(0);

  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog', { name: '设置与连接', exact: true });
  await dialog.getByRole('button', { name: '保存并运行诊断', exact: true }).click();
  await expect(dialog.locator('.diagnostic-pills')).toContainText('工具调用: failed');
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
});
