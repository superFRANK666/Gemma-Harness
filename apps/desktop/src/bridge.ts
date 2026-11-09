import { Channel, invoke, isTauri } from '@tauri-apps/api/core';
import type { Json } from './types';
export const desktop = isTauri();
export async function api<T = Json>(action: string, data: Json = {}): Promise<T> {
  if (!desktop) throw new Error('请在桌面应用中使用此功能。浏览器页面仅用于界面预览。');
  return invoke<T>('command', { request: { action, ...data } });
}
export async function stream(sessionId: string, input: string, event: (value: Json) => void, images: string[] = []) {
  const onEvent = new Channel<Json>(); onEvent.onmessage = event;
  return invoke('stream_turn', { sessionId, input, images, onEvent });
}
export async function choosePath(directory = true): Promise<string | null> {
  if (!desktop) throw new Error('选择本地文件需要桌面应用');
  const { open } = await import('@tauri-apps/plugin-dialog');
  const value = await open({ directory, multiple: false });
  return typeof value === 'string' ? value : null;
}
export const pretty = (value: unknown) => typeof value === 'string' ? value : JSON.stringify(value, null, 2);

export async function streamTool(request: Json, event: (value: Json) => void): Promise<Json> {
  if (!desktop) throw new Error('终端需要桌面应用');
  const onEvent = new Channel<Json>(); onEvent.onmessage = event;
  return invoke<Json>('stream_tool', { request, onEvent });
}
