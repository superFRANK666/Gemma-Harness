import { applyUsage, initialUsage, numberValue, readMetrics } from '../components/PerformancePanel';
import type { CompactionInfo, ModelMetrics, UsageState } from '../components/PerformancePanel';
import type { Activity, Json } from '../types';

export const HISTORY_WINDOW = 80;
export const LIVE_TRUNCATION_NOTICE =
  '\n[Live output preview truncated; check artifacts and output completeness.]\n';

export const basename = (path: string) =>
  path.split(/[\\/]/).filter(Boolean).pop() || '未选择项目';

export function parentDir(path: string): string | null {
  const parts = path.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 1) return parts.length === 1 && path !== '.' ? '.' : null;
  parts.pop();
  return parts.join('/');
}

export function metricKey(
  session: string | undefined,
  connection: string | undefined,
  context: number | undefined,
) {
  return `${session || 'new'}|${connection || ''}|${context || 8192}`;
}

export function savedTokens(event: Json): number | undefined {
  const direct = numberValue(event.tokensSaved) ?? numberValue(event.savedTokens);
  if (direct !== undefined) return direct;
  const prev = numberValue(event.previousEstimatedTokens);
  const curr = numberValue(event.estimatedTokens);
  return prev !== undefined && curr !== undefined ? Math.max(0, prev - curr) : undefined;
}

export function modelRetryText(event: Json): string {
  return event.reason === 'empty_final_after_tool'
    ? '工具已执行，正在重新生成结果说明；本次重试不会再次执行工具。'
    : `本次生成达到输出上限，已在未执行新工具的前提下增加预算重试（${String(event.previousOutputTokens)} → ${String(event.outputTokens)} token）。`;
}

export function prettyArgs(raw?: string): string {
  if (!raw) return '';
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

export function extractUserMessageView(content: unknown): { text: string; images: string[] } {
  if (typeof content === 'string') return { text: content, images: [] };
  if (!Array.isArray(content)) return { text: '', images: [] };
  const textParts: string[] = [];
  const images: string[] = [];
  for (const part of content) {
    if (!part || typeof part !== 'object') continue;
    const item = part as Json;
    if (item.type === 'image_url') {
      images.push('图片附件');
      continue;
    }
    if (item.type === 'text' && typeof item.text === 'string') {
      const artifactMatch = item.text.match(/^\[Saved image artifact:\s*([^;\]]+)/);
      if (artifactMatch) {
        images.push(artifactMatch[1].trim());
      } else {
        textParts.push(item.text);
      }
    }
  }
  return { text: textParts.join('\n'), images };
}

/** Find the most recent tool activity matching callId (prefers still-running entries). */
export function findLastToolIndex(list: Activity[], callId: unknown): number {
  const target = String(callId ?? '');
  let fallback = -1;
  for (let i = list.length - 1; i >= 0; i--) {
    const item = list[i];
    if (item.role !== 'tool') continue;
    if ((item.callId ?? item.id) === target) {
      if (item.running) return i;
      if (fallback === -1) fallback = i;
    }
  }
  return fallback;
}

export function terminalResultNotices(result: Json, truncated: boolean): string[] {
  const notices: string[] = [];
  if (result.cancelled === true) notices.push('[命令已中断]');
  if (result.timed_out === true) notices.push('[命令超时]');
  if (result.cancelled !== true && result.timed_out !== true) {
    if (typeof result.exit_code === 'number' && result.exit_code !== 0) {
      notices.push(`[命令退出，退出码：${result.exit_code}]`);
    } else if (result.exit_code === null) {
      notices.push('[命令异常结束，没有退出码]');
    }
  }
  const incomplete = result.output_complete === false || result.isError === true;
  if (incomplete) {
    notices.push('[命令输出采集不完整]');
    for (const key of ['stdout_error', 'stderr_error'] as const) {
      if (typeof result[key] === 'string' && result[key]) notices.push(result[key]);
    }
  }
  if (truncated || incomplete) {
    for (const [key, label] of [
      ['stdout_artifact', '标准输出'],
      ['stderr_artifact', '标准错误'],
    ] as const) {
      if (typeof result[key] === 'string' && result[key]) {
        notices.push(`${label}文件（执行设备）：${result[key]}`);
      }
    }
  }
  return notices;
}

export function buildActivitiesFromEvents(
  events: { kind: string; payload: Json }[],
  defaultContext: number,
): {
  list: Activity[];
  usage: UsageState;
  metrics: ModelMetrics | null;
  waitMs: number | null;
  compaction: CompactionInfo;
} {
  const list: Activity[] = [];
  let usage = initialUsage(defaultContext);
  let metrics: ModelMetrics | null = null;
  let waitMs: number | null = null;
  let compaction: CompactionInfo = { count: 0 };

  for (const event of events) {
    const p = event.payload;
    if (event.kind === 'activity' && (p.type === 'context_usage' || p.type === 'usage')) {
      usage = applyUsage(usage, p);
    }
    if (event.kind === 'activity' && p.type === 'model_metrics') {
      metrics = readMetrics(p);
      waitMs = metrics.queueWaitMs ?? waitMs;
      if (metrics.usageMeasured) {
        usage = applyUsage(usage, {
          type: 'usage',
          inputTokens: metrics.inputTokens,
          outputTokens: metrics.outputTokens,
          isEstimated: false,
        });
      }
    }
    if (event.kind === 'activity' && p.type === 'inference_acquired') {
      waitMs = numberValue(p.waitMs) ?? waitMs;
    }
    if (event.kind === 'context_compacted') {
      compaction = {
        count: compaction.count + 1,
        method: String(p.method || ''),
        savedTokens: savedTokens(p),
      };
    }

    if (event.kind === 'message' && (p.role === 'user' || p.role === 'assistant')) {
      const view = extractUserMessageView(p.content);
      if (view.text || view.images.length > 0) {
        list.push({
          id: crypto.randomUUID(),
          role: p.role,
          text: view.text,
          images: view.images.length > 0 ? view.images : undefined,
        });
      }
    } else if (event.kind === 'activity' && p.type === 'assistant_partial') {
      list.push({
        id: crypto.randomUUID(),
        role: 'assistant',
        text: `${String(p.text || '')}\n\n[生成未完成]`,
      });
    } else if (event.kind === 'activity' && p.type === 'tool_batch_rejected') {
      list.push({
        id: crypto.randomUUID(),
        role: 'status',
        text: `工具调用未执行：${String(p.message || '')}`,
      });
    } else if (event.kind === 'activity' && p.type === 'model_retry') {
      list.push({ id: crypto.randomUUID(), role: 'status', text: modelRetryText(p) });
    } else if (event.kind === 'activity' && p.type === 'capability_warning') {
      list.push({ id: crypto.randomUUID(), role: 'status', text: String(p.message || '') });
    } else if (event.kind === 'activity' && p.type === 'manual_file_restore_started') {
      list.push({
        id: crypto.randomUUID(),
        role: 'status',
        text: `开始恢复 ${String(p.path || '')}；请以完成记录和实际文件为准。`,
      });
    } else if (event.kind === 'activity' && p.type === 'manual_file_restore_completed') {
      list.push({
        id: crypto.randomUUID(),
        role: 'status',
        text: `已恢复 ${String(p.path || '')}`,
      });
    } else if (event.kind === 'activity' && p.type === 'manual_file_restore_failed') {
      list.push({
        id: crypto.randomUUID(),
        role: 'status',
        text: `恢复未完成：${String(p.message || '')}`,
      });
    } else if (event.kind === 'activity' && p.type === 'tool_started') {
      list.push({
        id: crypto.randomUUID(),
        callId: String(p.callId),
        role: 'tool',
        name: String(p.name),
        text: '',
        args: String(p.arguments),
        running: true,
        deviceId: String(p.deviceId),
      });
    } else if (event.kind === 'activity' && p.type === 'tool_completed') {
      const idx = findLastToolIndex(list, p.callId);
      if (idx !== -1) {
        const item = list[idx];
        item.running = false;
        item.result = p.result;
        item.durationMs = Number(p.durationMs);
      }
    } else if (event.kind === 'activity' && p.type === 'tool_output') {
      const idx = findLastToolIndex(list, p.callId);
      if (idx !== -1) list[idx].text += String(p.text || '');
    } else if (event.kind === 'activity' && p.type === 'error') {
      list.push({ id: crypto.randomUUID(), role: 'status', text: String(p.message) });
    }
  }

  for (const item of list) {
    if (item.running) {
      item.running = false;
      item.result = { status: '结果待确认，请检查实际状态' };
    }
  }

  return { list, usage, metrics, waitMs, compaction };
}

export interface DiffLine {
  type: 'meta' | 'hunk' | 'add' | 'del' | 'ctx';
  text: string;
}

export interface DiffFile {
  path: string;
  additions: number;
  deletions: number;
  lines: DiffLine[];
}

export interface ParsedDiffResult {
  files: DiffFile[];
  additions: number;
  deletions: number;
  error?: string;
  truncated: boolean;
  artifact?: string;
}

export function parseGitDiffResult(result: Json): ParsedDiffResult {
  const stdout = typeof result.stdout === 'string' ? result.stdout : '';
  const stderr = typeof result.stderr === 'string' ? result.stderr.trim() : '';
  const exitCode = typeof result.exit_code === 'number' ? result.exit_code : null;
  const truncated =
    result.output_complete === false || result.stdout_artifact_truncated === true;
  const artifact =
    typeof result.stdout_artifact === 'string' ? result.stdout_artifact : undefined;
  const error =
    exitCode !== null && exitCode !== 0
      ? stderr || `git diff 退出码 ${exitCode}`
      : stderr || undefined;

  if (!stdout.trim()) {
    return { files: [], additions: 0, deletions: 0, error, truncated, artifact };
  }

  const files: DiffFile[] = [];
  let current: DiffFile | null = null;
  let totalAdd = 0;
  let totalDel = 0;

  const ensureFile = (path: string) => {
    current = { path, additions: 0, deletions: 0, lines: [] };
    files.push(current);
    return current;
  };

  for (const line of stdout.split('\n')) {
    if (line.startsWith('diff --git ')) {
      const match = line.match(/^diff --git a\/(.+?) b\/(.+)$/);
      ensureFile(match ? match[2] : line.slice('diff --git '.length));
      continue;
    }
    const target = current ?? ensureFile('工作区变更');
    if (line.startsWith('+++ ') || line.startsWith('--- ')) {
      if (line.startsWith('+++ b/')) target.path = line.slice(6);
      target.lines.push({ type: 'meta', text: line });
    } else if (
      line.startsWith('index ') ||
      line.startsWith('new file') ||
      line.startsWith('deleted file') ||
      line.startsWith('rename ') ||
      line.startsWith('Binary files')
    ) {
      target.lines.push({ type: 'meta', text: line });
    } else if (line.startsWith('@@')) {
      target.lines.push({ type: 'hunk', text: line });
    } else if (line.startsWith('+')) {
      target.additions += 1;
      totalAdd += 1;
      target.lines.push({ type: 'add', text: line });
    } else if (line.startsWith('-')) {
      target.deletions += 1;
      totalDel += 1;
      target.lines.push({ type: 'del', text: line });
    } else if (line !== '') {
      target.lines.push({ type: 'ctx', text: line });
    }
  }

  return { files, additions: totalAdd, deletions: totalDel, error, truncated, artifact };
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}
