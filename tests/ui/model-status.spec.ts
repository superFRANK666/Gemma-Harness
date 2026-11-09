import { test, expect, baseConfig, installDesktopFixture } from './fixtures';

test('an unconfigured model does not display a fabricated profile', async ({ page }) => {
  await installDesktopFixture(page);
  await page.goto('/');

  const status = page.locator('.model-info');
  await expect(status).toContainText('尚未配置模型');
  await expect(status).not.toContainText('MODEL PROFILE');
  await expect(status).not.toContainText('Gemma 4');
  await expect(status).not.toContainText('Heretic');
  await expect(page.locator('.statusbar-connection')).toContainText('模型未配置');

  await status.getByRole('button', { name: '配置模型' }).click();
  const dialog = page.getByRole('dialog', { name: '设置与连接' });
  await expect(dialog.locator('.profile-notice')).toContainText('预设适配目标');
  await expect(dialog.locator('.profile-notice')).toContainText('不代表已连接该模型');
});

test('model status follows saved configuration without claiming a live connection', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.connections[0].model = 'first-model';
  config.connections.push({ id: 'second', name: '第二服务', baseUrl: 'http://127.0.0.1:5678', protocol: 'chat_completions', model: 'other-provider/actual-model-id', contextLength: 8192 });
  await installDesktopFixture(page, { config });
  await page.goto('/');

  const status = page.locator('.model-info');
  await expect(status).toContainText('first-model');
  await expect(status).toContainText('连接：LM Studio');
  await expect(status).toContainText('已配置 · 当前连接状态未知');
  await expect(status).not.toContainText('Gemma 4');

  await status.getByRole('button', { name: '查看模型设置' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '第二服务' }).click();
  await dialog.getByRole('radio', { name: '作为默认连接' }).check();
  await dialog.getByRole('button', { name: '保存设置' }).click();
  await expect(status).toContainText('other-provider/actual-model-id');
  await expect(status).toContainText('连接：第二服务');
  await expect(status).not.toContainText('first-model');
  await expect(status).toContainText('当前连接状态未知');

  await status.getByRole('button', { name: '查看模型设置' }).click();
  await dialog.getByRole('textbox', { name: '模型 ID' }).fill('   ');
  await expect(dialog.getByRole('button', { name: '保存并运行诊断' })).toBeDisabled();
  await dialog.getByRole('button', { name: '保存设置' }).click();
  await expect(status).toContainText('尚未配置模型');
  await expect(status).not.toContainText('other-provider/actual-model-id');
  await expect(page.locator('.statusbar-connection')).toContainText('模型未配置');
});

test('model status uses the active session connection, including a removed connection', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.connections[0].model = 'default-model';
  config.connections.push({ id: 'history', name: '历史服务', baseUrl: 'http://127.0.0.1:5678', protocol: 'chat_completions', model: 'history-model', contextLength: 8192 });
  const session = (id: string, connectionId: string) => ({ id, title: id, workspace: '/tmp/project', deviceId: 'local', connectionId, createdAt: '2026-11-09', updatedAt: '2026-11-09' });
  await installDesktopFixture(page, { config, sessions: [session('历史会话', 'history'), session('已删除连接的会话', 'removed')] });
  await page.goto('/');

  const status = page.locator('.model-info');
  await expect(status).toContainText('default-model');
  await page.getByRole('button', { name: '历史会话' }).click();
  await expect(status).toContainText('history-model');
  await expect(status).toContainText('连接：历史服务');
  await page.getByRole('button', { name: '已删除连接的会话' }).click();
  await expect(status).toContainText('尚未配置模型');
  await expect(status).not.toContainText('default-model');
  await page.getByRole('button', { name: /新会话/ }).click();
  await expect(status).toContainText('default-model');
});

test('long configured model identifiers fit inside the inspector', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.connections[0].model = 'long-model-identifier-'.repeat(12);
  await installDesktopFixture(page, { config });
  await page.setViewportSize({ width: 1120, height: 760 });
  await page.goto('/');

  const status = page.locator('.model-info');
  await expect(status).toContainText(config.connections[0].model);
  await expect.poll(() => status.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});
