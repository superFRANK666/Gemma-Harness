import { test as base, expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import path from 'node:path';
import type { Config, Project, Session } from '../../apps/desktop/src/types';

type JsonObject = Record<string, unknown>;
type IpcCall = { cmd: string; args: JsonObject };

export const baseConfig: Config = {
  connections: [{ id: 'lmstudio', name: 'LM Studio', baseUrl: 'http://127.0.0.1:1234', protocol: 'lmstudio', model: '', contextLength: 8192 }],
  selectedConnectionId: 'lmstudio', workspace: '', deviceId: 'local', devices: [], mcpServers: [],
  toolPermissions: { terminal: true, browser: true, desktop: false }, mode: 'controller',
};

export const nativeDiscovery = {
  models: { object: 'list', data: [{ id: 'gemma-heretic-i1', object: 'model', owned_by: 'local' }] },
  native: {
    status: 'available',
    data: {
      models: [{
        key: 'mradermacher/gemma-4-26B-A4B-it-ultra-uncensored-heretic-i1-GGUF',
        display_name: 'Gemma 4 Heretic I1', architecture: 'gemma4',
        quantization: { name: 'IQ3_XS', bits_per_weight: 3 },
        loaded_instances: [{ id: 'gemma-heretic-i1', config: { context_length: 12288, parallel: 1 } }],
        max_context_length: 262144, format: 'gguf',
        capabilities: { vision: true, trained_for_tool_use: true },
      }],
    },
  },
  selectedModel: '', configuredContextLength: 8192,
};

export interface FixtureOptions {
  config?: Config;
  projects?: Project[];
  sessions?: Session[];
  discovery?: unknown;
  diagnosis?: unknown;
  streamEvents?: JsonObject[];
}

/** Test-only IPC replacement installed before product modules initialize. No product mock code. */
export async function installDesktopFixture(page: Page, options: FixtureOptions = {}) {
  await page.addInitScript(({ initialConfig, initialProjects, initialSessions, discovery, diagnosis, streamEvents }) => {
    const calls: { cmd: string; args: Record<string, unknown> }[] = [];
    const state = { config: structuredClone(initialConfig), sessions: structuredClone(initialSessions) as Record<string, unknown>[], projects: initialProjects ? structuredClone(initialProjects) : undefined };
    const callbacks = new Map<number, (message: unknown) => void>();
    let nextCallback = 1;
    Object.defineProperty(window, 'isTauri', { value: true, configurable: true });
    Object.defineProperty(window, '__HARNESS_TEST_CALLS__', { value: calls });
    const internals = {
      transformCallback(callback: (message: unknown) => void) {
        const id = nextCallback++; callbacks.set(id, callback); return id;
      },
      unregisterCallback(id: number) { callbacks.delete(id); },
      async invoke(cmd: string, args: Record<string, unknown> = {}) {
        calls.push({ cmd, args: JSON.parse(JSON.stringify(args)) as Record<string, unknown> });
        if (cmd === 'plugin:dialog|open') return null;
        if (cmd === 'stream_turn') {
          const id = (args.onEvent as { id: number }).id;
          for (let index = 0; index < streamEvents.length; index++) {
            callbacks.get(id)?.({ index, message: streamEvents[index] });
            await Promise.resolve();
          }
          callbacks.get(id)?.({ index: streamEvents.length, end: true });
          return null;
        }
        if (cmd !== 'command') throw new Error(`Unmocked test IPC command: ${cmd}`);
        const request = args.request as Record<string, unknown>;
        switch (request.action) {
          case 'snapshot': return structuredClone({ ...state, engine: { running: false }, platform: 'windows', arch: 'x86_64', dataDir: 'TEST_FIXTURE', activeSessions: [] });
          case 'save_settings': {
            state.config = structuredClone(request.config) as typeof initialConfig;
            state.config.connections.forEach(connection => { delete connection.apiKey; });
            return structuredClone(state.config);
          }
          case 'save_appearance': { state.config.appearance = structuredClone(request.appearance) as typeof state.config.appearance; return structuredClone(state.config); }
          case 'open_project': {
            state.projects ??= [];
            let project = state.projects.find(p => request.projectId ? p.id === request.projectId : p.workspace === request.workspace && p.deviceId === (request.deviceId || state.config.deviceId));
            if (!project && request.projectId) throw new Error('TEST_FIXTURE: 项目不存在');
            if (!project) { const workspace = String(request.workspace); project = { id: `fixture-project-${state.projects.length}`, name: workspace.split(/[\\/]/).filter(Boolean).pop() || workspace, workspace, deviceId: String(request.deviceId || state.config.deviceId), createdAt: '2026-11-09', updatedAt: '2026-11-09' }; state.projects.push(project); }
            state.config.workspace = project.workspace; state.config.deviceId = project.deviceId;
            return structuredClone({ config: state.config, projects: state.projects });
          }
          case 'rename_project': {
            const project = state.projects?.find(p => p.id === request.projectId); if (!project) throw new Error('TEST_FIXTURE: 项目不存在'); project.name = String(request.name);
            return structuredClone({ config: state.config, projects: state.projects });
          }
          case 'remove_project': {
            const project = state.projects?.find(p => p.id === request.projectId); if (!project) throw new Error('TEST_FIXTURE: 项目不存在');
            state.projects = state.projects?.filter(p => p.id !== project.id);
            if (state.config.workspace === project.workspace && state.config.deviceId === project.deviceId) state.config.workspace = '';
            return structuredClone({ config: state.config, projects: state.projects });
          }
          case 'discover': return structuredClone(discovery);
          case 'diagnose': return structuredClone(diagnosis);
          case 'engine_start': return { running: true, source: 'test_fixture' };
          case 'load_session': return { events: [] };
          case 'host_status': return { running: false, status: 'not_started', source: 'test_fixture' };
          case 'host_stop': return { running: false, status: 'stopped', source: 'test_fixture' };
          case 'create_session': {
            const session = { id: 'fixture-session', title: 'Test task', workspace: state.config.workspace, deviceId: state.config.deviceId, connectionId: state.config.selectedConnectionId, createdAt: '2026-11-09T00:00:00Z', updatedAt: '2026-11-09T00:00:00Z' };
            state.sessions.push(session); return session;
          }
          default: throw new Error(`Unmocked test action: ${String(request.action)}`);
        }
      },
    };
    Object.defineProperty(window, '__TAURI_INTERNALS__', { value: internals, configurable: true });
  }, {
    initialConfig: options.config ?? baseConfig,
    initialProjects: options.projects ?? null,
    initialSessions: options.sessions ?? [],
    discovery: options.discovery ?? nativeDiscovery,
    diagnosis: options.diagnosis ?? { status: 'failed', toolCalling: { status: 'failed', tested: true, message: 'TEST_FIXTURE: backend unavailable' }, vision: { status: 'failed', tested: true, imageInput: { status: 'failed' }, imageToolResult: { status: 'unknown', tested: false } }, exactModelArtifact: { status: 'unknown' } },
    streamEvents: options.streamEvents ?? [{ type: 'error', message: 'TEST_FIXTURE: backend unavailable' }, { type: 'turn_completed', status: 'failed' }],
  });
}

export async function ipcCalls(page: Page): Promise<IpcCall[]> {
  return page.evaluate(() => (window as unknown as { __HARNESS_TEST_CALLS__: IpcCall[] }).__HARNESS_TEST_CALLS__);
}

export async function screenshot(page: Page, name: string) {
  const dir = path.resolve('tests/ui/artifacts');
  await mkdir(dir, { recursive: true });
  await page.screenshot({ path: path.join(dir, `${name}.png`), animations: 'disabled' });
}

export const test = base.extend<{ browserErrors: void }>({
  browserErrors: [async ({ page }, use) => {
    const errors: string[] = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
    await use();
    expect(errors, 'No uncaught browser exceptions or console errors').toEqual([]);
  }, { auto: true }],
});
export { expect };
