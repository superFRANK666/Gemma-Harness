import { applyUsage, initialUsage, numberValue, readMetrics } from '../components/PerformancePanel';
import type { CompactionInfo, ModelMetrics, UsageState } from '../components/PerformancePanel';
import type { Activity, ImageAttachment, Json, Session } from '../types';
import {
  compactionText,
  findLastToolIndex,
  loopGuardText,
  modelRetryText,
  savedTokens,
  terminalResultNotices,
} from './sessionEvents';

export interface Conversation {
  session: Session | null;
  activities: Activity[] | null;
  input: string;
  attachments: ImageAttachment[];
  command: string;
  terminal: string | null;
  running: boolean;
  status: string;
  unverifiedTool: boolean;
  usage: UsageState | null;
  metrics: ModelMetrics | null;
  queueSince: number | null;
  queueWaitMs: number | null;
  compaction: CompactionInfo;
}

export const emptyConversation: Conversation = {
  session: null,
  activities: null,
  input: '',
  attachments: [],
  command: '',
  terminal: null,
  running: false,
  status: '就绪',
  unverifiedTool: false,
  usage: null,
  metrics: null,
  queueSince: null,
  queueWaitMs: null,
  compaction: { count: 0 },
};

export const emptyActivities: Activity[] = [];

export function releaseConversationHistory(chats: Map<string, Conversation>, key: string) {
  const chat = chats.get(key);
  if (!chat || chat.running) return;
  const { session, input, attachments, command, terminal } = chat;
  if (session && (input || attachments.length || command || terminal)) {
    chats.set(key, { ...emptyConversation, session, input, attachments, command, terminal });
  } else {
    chats.delete(key);
  }
}

function appendStatus(chat: Conversation, text: string): Conversation {
  return {
    ...chat,
    activities: [...(chat.activities ?? emptyActivities), { id: crypto.randomUUID(), role: 'status', text }],
  };
}

export function applyConversationEvent(chat: Conversation, event: Json): Conversation {
  const activities = chat.activities ?? emptyActivities;
  switch (event.type) {
    case 'text_delta':
    case 'reasoning_delta': {
      const list = [...activities];
      let last = list.at(-1);
      if (!last || last.role !== 'assistant' || !last.running) {
        last = { id: crypto.randomUUID(), role: 'assistant', text: '', running: true };
        list.push(last);
      }
      const reasoning = event.type === 'reasoning_delta';
      list[list.length - 1] = reasoning
        ? { ...last, reasoning: (last.reasoning || '') + String(event.text) }
        : { ...last, text: last.text + String(event.text) };
      return { ...chat, activities: list, status: reasoning ? '思考中' : '生成中' };
    }
    case 'assistant_completed': {
      const finalText = typeof event.text === 'string' ? event.text : '';
      if (activities.some(item => item.role === 'assistant' && item.running)) {
        return {
          ...chat,
          activities: activities.map(item => item.role === 'assistant' && item.running
            ? { ...item, running: false, text: item.text || finalText }
            : item),
        };
      }
      return finalText ? {
        ...chat,
        activities: [...activities, { id: crypto.randomUUID(), role: 'assistant', text: finalText }],
      } : chat;
    }
    case 'tool_started':
      return {
        ...chat,
        status: `正在执行 ${String(event.name)}`,
        activities: [...activities, {
          id: crypto.randomUUID(),
          callId: String(event.callId),
          role: 'tool',
          name: String(event.name),
          text: '',
          args: String(event.arguments),
          deviceId: String(event.deviceId),
          running: true,
        }],
      };
    case 'tool_output':
    case 'tool_completed': {
      const index = findLastToolIndex(activities, event.callId);
      const list = index === -1 ? activities : [...activities];
      if (event.type === 'tool_output') {
        const text = String(event.text || '');
        if (index !== -1) list[index] = { ...list[index], text: list[index].text + text };
        return { ...chat, activities: list, terminal: (chat.terminal ?? '') + text };
      }
      if (index !== -1) {
        list[index] = {
          ...list[index], running: false, result: event.result, durationMs: Number(event.durationMs),
        };
      }
      if (event.name !== 'terminal_run') return { ...chat, activities: list };
      const notices = terminalResultNotices((event.result ?? {}) as Json, false);
      const text = notices.length > 0 ? notices.join('\n') : '[命令结束，详细结果保存在工具卡片]';
      return { ...chat, activities: list, terminal: `${chat.terminal ?? ''}\n${text}\n` };
    }
    case 'context_usage':
    case 'usage':
      return {
        ...chat,
        usage: applyUsage(chat.usage ?? initialUsage(), event),
        status: event.type === 'context_usage' ? '准备模型响应' : chat.status,
      };
    case 'inference_queued':
      return { ...chat, queueSince: Date.now(), queueWaitMs: null, status: '排队等待推理资源' };
    case 'inference_acquired':
      return { ...chat, queueSince: null, queueWaitMs: numberValue(event.waitMs) ?? null, status: '准备模型响应' };
    case 'model_metrics': {
      const metrics = readMetrics(event);
      return { ...chat, metrics, queueWaitMs: metrics.queueWaitMs ?? chat.queueWaitMs };
    }
    case 'compaction_started':
      return { ...chat, status: '正在压缩上下文' };
    case 'compaction_completed':
      return appendStatus({
        ...chat,
        status: '上下文已压缩',
        compaction: {
          count: chat.compaction.count + 1,
          method: String(event.method || event.reason || ''),
          savedTokens: savedTokens(event),
        },
      }, compactionText(event));
    case 'tool_batch_rejected':
      return appendStatus({ ...chat, status: '正在纠正工具参数' }, `工具调用未执行：${String(event.message || '')}`);
    case 'model_retry':
      return appendStatus({
        ...chat,
        status: event.reason === 'empty_final_after_tool' ? '正在整理工具结果' : '正在增加思考预算并重试',
        activities: activities.map(item => item.role === 'assistant' && item.running
          ? { ...item, running: false }
          : item),
      }, modelRetryText(event));
    case 'capability_warning':
      return appendStatus({ ...chat, unverifiedTool: chat.unverifiedTool || event.capability === 'tool_not_run' }, String(event.message || ''));
    case 'provider_error':
      return appendStatus({ ...chat, status: '推理服务异常' }, `推理异常：${String(event.message || '')}`);
    case 'loop_guard':
      return appendStatus({ ...chat, status: event.action === 'advice_only' ? '正在调整查询方法' : '已拦截无效重复调用' }, loopGuardText(event));
    case 'error':
      return appendStatus(chat, String(event.message));
    case 'status':
      return { ...chat, status: String(event.text) };
    case 'turn_completed':
      return {
        ...chat,
        queueSince: null,
        status: event.status === 'interrupted'
          ? '已停止'
          : event.status === 'completed'
            ? chat.unverifiedTool ? '需要核对' : '回复已结束'
            : '执行失败',
      };
    default:
      return chat;
  }
}
