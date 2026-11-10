export type Json = Record<string, unknown>;
export interface Connection {
  id: string; name: string; baseUrl: string; protocol: string; model: string;
  contextLength: number; apiKey?: string; modelFamily?: string; backend?: string;
  thinkingMode?: 'auto' | 'on' | 'off'; maxOutputTokens?: number | null;
  temperature?: number | null; topP?: number | null; topK?: number | null; minP?: number | null; seed?: number | null;
}
export interface Device { id: string; name: string; url: string; certificatePin: string; token?: string }
export interface McpServer { id: string; name: string; command: string; args: string[]; enabled: boolean }
export interface EngineConfiguration { binaryPath: string; modelPath: string; projectorPath: string; chatTemplatePath?: string; contextLength: number; port: number; gpuLayers: number }
export interface AppearanceConfig {
  theme: 'dark' | 'light' | 'system'; accent: 'sage' | 'blue' | 'violet' | 'amber';
  fontFamily: 'system' | 'sans' | 'serif'; fontSize: number;
  codeFontFamily: 'system' | 'mono'; codeFontSize: number;
}
export interface Project { id: string; name: string; workspace: string; deviceId: string; createdAt: string; updatedAt: string }
export interface Config {
  connections: Connection[]; selectedConnectionId: string; workspace: string; deviceId: string;
  devices: Device[]; mcpServers: McpServer[]; toolPermissions: { terminal: boolean; browser: boolean; desktop: boolean }; mode: string; engineConfig?: EngineConfiguration; diagnostics?: Record<string, Json>;
  appearance?: AppearanceConfig;
}
export interface Session { id: string; title: string; workspace: string; deviceId: string; connectionId: string; createdAt: string; updatedAt: string }
export interface Snapshot { config: Config; sessions: Session[]; projects?: Project[]; engine: Json; platform: string; arch: string; dataDir: string; activeSessions: string[] }
export interface Activity { id: string; role: 'user' | 'assistant' | 'tool' | 'status'; text: string; name?: string; result?: unknown; args?: string; running?: boolean; deviceId?: string; durationMs?: number; reasoning?: string }
export const defaultConfig: Config = {
  connections: [{ id: 'lmstudio', name: 'LM Studio', baseUrl: 'http://127.0.0.1:1234', protocol: 'lmstudio', model: '', contextLength: 8192 }],
  selectedConnectionId: 'lmstudio', workspace: '', deviceId: 'local', devices: [], mcpServers: [],
  toolPermissions: { terminal: true, browser: true, desktop: false }, mode: 'controller',
};
