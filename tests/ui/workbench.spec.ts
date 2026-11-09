import { test, expect, baseConfig, installDesktopFixture, ipcCalls, nativeDiscovery, screenshot } from './fixtures';

test('browser preview shows an honest disconnected empty state', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { level: 1 })).toContainText('让本地模型');
  await expect(page.getByText('桌面界面预览', { exact: false })).toBeVisible();
  await expect(page.getByText('模型未配置', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '发送', exact: true })).toBeDisabled();
  await expect(page.locator('.composer-footnote')).toContainText('就绪');
  await screenshot(page, 'workbench');
  await page.getByRole('textbox', { name: '消息', exact: true }).fill('这是一条预览测试消息');
  await page.getByRole('button', { name: '发送', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('当前为界面预览');
  await expect(page.locator('.composer-footnote')).not.toContainText('已完成');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
});

test('all settings tabs render in browser preview without claiming backend success', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog', { name: '设置与连接', exact: true });
  const checks = [
    { tab: '模型连接', text: '实际加载上下文', image: 'settings-models' },
    { tab: '本地引擎', text: '模型 GGUF 文件', image: 'settings-engine' },
    { tab: '执行设备', text: '一次性配对串', image: 'settings-devices' },
    { tab: '工具与扩展', text: '原生桌面控制', image: 'settings-tools' },
  ];
  for (const check of checks) {
    await dialog.getByRole('button', { name: check.tab, exact: true }).click();
    await expect(dialog.getByRole('heading', { level: 2 })).toHaveText(check.tab);
    await expect(dialog.getByText(check.text, { exact: false }).first()).toBeVisible();
    await screenshot(page, check.image);
  }
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('请在桌面应用中使用此功能');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.diagnostic-report')).toHaveCount(0);
});

test('discovery uses the actual provider response shape and loaded instance context', async ({ page }) => {
  await installDesktopFixture(page, { discovery: nativeDiscovery });
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '发现模型', exact: true }).click();
  await expect(dialog.getByRole('textbox', { name: '模型 ID', exact: true })).toHaveValue('gemma-heretic-i1');
  await expect(dialog.getByRole('spinbutton', { name: /实际加载上下文/ })).toHaveValue('12288');
  await expect(dialog.getByRole('spinbutton', { name: /实际加载上下文/ })).not.toHaveValue('262144');
  await expect(dialog.locator('.discovered-models button')).toHaveCount(1);
});

test('an unloaded model does not substitute its theoretical context limit', async ({ page }) => {
  const discovery = {
    models: { object: 'list', data: [{ id: 'gemma-offline', max_context_length: 262144 }] },
    native: { status: 'unavailable', message: 'TEST_FIXTURE: native API absent' },
  };
  await installDesktopFixture(page, { discovery });
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '发现模型', exact: true }).click();
  await expect(dialog.getByRole('textbox', { name: '模型 ID', exact: true })).toHaveValue('gemma-offline');
  await expect(dialog.getByRole('spinbutton', { name: /实际加载上下文/ })).toHaveValue('8192');
});

test('selecting a second loaded instance preserves its own id and context', async ({ page }) => {
  const discovery = structuredClone(nativeDiscovery);
  discovery.models.data = [
    { id: 'gemma-instance-a', object: 'model', owned_by: 'local' },
    { id: 'gemma-instance-b', object: 'model', owned_by: 'local' },
  ];
  discovery.native.data.models[0].loaded_instances = [
    { id: 'gemma-instance-a', config: { context_length: 8192, parallel: 1 } },
    { id: 'gemma-instance-b', config: { context_length: 16384, parallel: 1 } },
  ];
  await installDesktopFixture(page, { discovery });
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '发现模型', exact: true }).click();
  await expect(dialog.locator('.discovered-models button')).toHaveCount(2);
  await dialog.locator('.discovered-models button').nth(1).click();
  await expect(dialog.getByRole('textbox', { name: '模型 ID', exact: true })).toHaveValue('gemma-instance-b');
  await expect(dialog.getByRole('spinbutton', { name: /实际加载上下文/ })).toHaveValue('16384');
});

test('failed diagnostics retain failed and untested capability states', async ({ page }) => {
  await installDesktopFixture(page);
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox', { name: '模型 ID', exact: true }).fill('gemma-heretic-i1');
  await dialog.getByRole('button', { name: '保存并运行诊断', exact: true }).click();
  const report = dialog.locator('.diagnostic-report');
  await expect(report).toContainText('failed');
  await expect(report).toContainText('unknown');
  await expect(report).toContainText('"tested": false');
  await expect(report).not.toContainText('passed');
  const calls = await ipcCalls(page);
  const saved = calls.findIndex(call => (call.args.request as { action?: string })?.action === 'save_settings');
  const diagnosed = calls.findIndex(call => (call.args.request as { action?: string })?.action === 'diagnose');
  expect(saved).toBeGreaterThanOrEqual(0);
  expect(diagnosed).toBeGreaterThan(saved);
});

test('remote inference and remote execution settings save as separate selections', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.workspace = '/tmp/local-fixture-project';
  config.devices = [{ id: 'windows-agent', name: 'Windows 11 工作站', url: 'wss://192.168.1.20:45455', certificatePin: 'TEST_FIXTURE_ONLY' }];
  await installDesktopFixture(page, { config });
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox', { name: '服务地址', exact: true }).fill('http://192.168.1.30:1234/v1');
  await dialog.getByRole('combobox', { name: '接口协议', exact: true }).selectOption('responses');
  await dialog.getByRole('textbox', { name: '模型 ID', exact: true }).fill('gemma-heretic-i1');
  await dialog.getByRole('button', { name: '执行设备', exact: true }).click();
  await dialog.getByRole('combobox', { name: '执行设备', exact: true }).selectOption('windows-agent');
  const workspace = dialog.getByPlaceholder('例如 C:\\Projects\\app 或 /home/user/project');
  await expect(workspace).toHaveValue('');
  await expect(dialog.getByRole('button', { name: '选择工作目录', exact: true })).toBeDisabled();
  await workspace.fill('C:\\Projects\\Harness');
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  const saved = (await ipcCalls(page)).find(call => (call.args.request as { action?: string })?.action === 'save_settings');
  const savedConfig = (saved?.args.request as { config: typeof config }).config;
  expect(savedConfig.deviceId).toBe('windows-agent');
  expect(savedConfig.workspace).toBe('C:\\Projects\\Harness');
  expect(savedConfig.connections[0].baseUrl).toBe('http://192.168.1.30:1234/v1');
  expect(savedConfig.connections[0].protocol).toBe('responses');
  expect((await ipcCalls(page)).some(call => (call.args.request as { action?: string })?.action === 'execute_tool')).toBe(false);
  await expect(page.getByText('已配置模型', { exact: true })).toBeVisible();
  await expect(page.getByText('模型已连接', { exact: true })).toHaveCount(0);
  await screenshot(page, 'remote-workbench');
});

test('Tauri Channel failure events do not turn into a completed task', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.workspace = 'C:\\Projects\\Harness';
  config.connections[0].model = 'gemma-heretic-i1';
  await installDesktopFixture(page, { config });
  await page.goto('/');
  await expect(page.getByRole('textbox', { name: '消息', exact: true })).toHaveAttribute('placeholder', /Harness/);
  await page.getByRole('textbox', { name: '消息', exact: true }).fill('运行一个连接测试');
  await page.getByRole('button', { name: '发送', exact: true }).click();
  await expect(page.locator('.composer-footnote')).toContainText('执行失败');
  await expect(page.getByRole('alert')).toContainText('TEST_FIXTURE: backend unavailable');
  await expect(page.locator('.composer-footnote')).not.toContainText('已完成');
  expect((await ipcCalls(page)).filter(call => call.cmd === 'stream_turn')).toHaveLength(1);
});

test('image attachment and exact user whitespace reach the desktop stream', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.workspace = '/tmp/image-fixture';
  config.connections[0].model = 'gemma-4-fixture';
  await installDesktopFixture(page, { config, streamEvents: [{ type: 'turn_completed', status: 'completed' }] });
  await page.goto('/');
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6n1cAAAAASUVORK5CYII=', 'base64');
  await page.locator('input[aria-label="选择图片"]').setInputFiles({ name: 'fixture.png', mimeType: 'image/png', buffer: png });
  await expect(page.getByText('fixture.png', { exact: true })).toBeVisible();
  await page.getByRole('textbox', { name: '消息', exact: true }).fill('  compare this\n');
  await page.getByRole('button', { name: '发送', exact: true }).click();
  await expect.poll(async () => (await ipcCalls(page)).filter(call => call.cmd === 'stream_turn').length).toBe(1);
  const request = (await ipcCalls(page)).find(call => call.cmd === 'stream_turn')!;
  expect(request.args.input).toBe('  compare this\n');
  expect((request.args.images as string[])[0]).toMatch(/^data:image\/png;base64,/);
});

test('Gemma profile and thinking settings are saved together', async ({ page }) => {
  await installDesktopFixture(page);
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox', { name: '模型 ID', exact: true }).fill('gemma-4-fixture');
  await dialog.getByRole('combobox', { name: '接口协议' }).selectOption('chat_completions');
  await dialog.getByRole('combobox', { name: '模型系列' }).selectOption('gemma4');
  await dialog.getByRole('combobox', { name: '推理后端' }).selectOption('llama_cpp');
  await dialog.getByRole('combobox', { name: '思考模式' }).selectOption('on');
  await dialog.getByRole('spinbutton', { name: '单次最大输出 token' }).fill('3072');
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  const saved = (await ipcCalls(page)).find(call => (call.args.request as { action?: string })?.action === 'save_settings');
  const connection = (saved?.args.request as { config: { connections: { modelFamily?: string; backend?: string; thinkingMode?: string; maxOutputTokens?: number | null }[] } }).config.connections[0];
  expect(connection.modelFamily).toBe('gemma4');
  expect(connection.backend).toBe('llama_cpp');
  expect(connection.thinkingMode).toBe('on');
  expect(connection.maxOutputTokens).toBe(3072);
});

test('changing the model clears the previous Gemma profile and thinking mode', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.connections[0] = { ...config.connections[0], model: 'gemma-4-old', modelFamily: 'gemma4', backend: 'llama_cpp', thinkingMode: 'on' };
  await installDesktopFixture(page, { config });
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('textbox', { name: '模型 ID' }).fill('functiongemma-new');
  await expect(dialog.getByRole('combobox', { name: '模型系列' })).toHaveValue('auto');
  await expect(dialog.getByRole('combobox', { name: '思考模式' })).toHaveValue('auto');
});

test('restarting the managed engine drops stale model settings and oversized output limit', async ({ page }) => {
  const config = structuredClone(baseConfig);
  config.connections.push({ id: 'managed-llama', name: '内置 llama.cpp', baseUrl: 'http://127.0.0.1:1235', protocol: 'chat_completions', model: 'gemma-local', contextLength: 16384, modelFamily: 'gemma4', backend: 'llama_cpp', thinkingMode: 'on', maxOutputTokens: 8192 });
  config.engineConfig = { binaryPath: '/fixture/llama-server', modelPath: '/fixture/new.gguf', projectorPath: '', contextLength: 8192, port: 1235, gpuLayers: 0 };
  await installDesktopFixture(page, { config });
  await page.goto('/');
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: '本地引擎', exact: true }).click();
  await dialog.getByRole('button', { name: '启动推理', exact: true }).click();
  await expect(dialog.locator('.diagnostic-report')).toContainText('test_fixture');
  await dialog.getByRole('button', { name: '保存设置', exact: true }).click();
  const saved = (await ipcCalls(page)).find(call => (call.args.request as { action?: string })?.action === 'save_settings');
  const managed = (saved?.args.request as { config: { connections: { id: string; modelFamily?: string; thinkingMode?: string; maxOutputTokens?: number | null }[] } }).config.connections.find(c => c.id === 'managed-llama');
  expect(managed?.modelFamily).toBe('auto');
  expect(managed?.thinkingMode).toBe('auto');
  expect(managed?.maxOutputTokens).toBeNull();
});

test('the desktop layout stays within a supported narrow window', async ({ page }) => {
  await page.setViewportSize({ width: 1120, height: 760 });
  await page.goto('/');
  await expect(page.getByRole('textbox', { name: '消息', exact: true })).toBeVisible();
  await expect(page.locator('.statusbar')).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
  expect(overflow).toBe(false);
  await page.getByRole('button', { name: /设置与连接/ }).click();
  const dialog = page.getByRole('dialog');
  const bounds = await dialog.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(1120);
  await expect(dialog.getByRole('button', { name: '保存设置', exact: true })).toBeVisible();
  await screenshot(page, 'narrow-settings');
});
