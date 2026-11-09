import { useEffect, useState } from 'react';
import { X } from 'lucide-react';
import type { Json } from '../types';

export interface UsageState {
  input: number; output: number; context: number; estimated: boolean;
  budget?: number; outputReserve?: number; toolCount?: number; toolTokens?: number;
}
export interface ModelMetrics {
  step?: number; durationMs?: number; firstTokenMs?: number; firstEventMs?: number;
  inputTokens?: number; outputTokens?: number; tokensPerSecond?: number; queueWaitMs?: number;
  usageMeasured: boolean; succeeded: boolean;
}
export interface CompactionInfo { count: number; method?: string; savedTokens?: number }

export const numberValue = (value: unknown) => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : undefined;
export const initialUsage = (context = 8192): UsageState => ({ input: 0, output: 0, context, estimated: true });
export function readMetrics(event: Json): ModelMetrics {
  return {
    step: numberValue(event.step), durationMs: numberValue(event.durationMs), firstTokenMs: numberValue(event.firstTokenMs), firstEventMs: numberValue(event.firstEventMs),
    inputTokens: numberValue(event.inputTokens), outputTokens: numberValue(event.outputTokens), tokensPerSecond: numberValue(event.tokensPerSecond), queueWaitMs: numberValue(event.queueWaitMs),
    usageMeasured: event.usageMeasured === true, succeeded: event.succeeded === true,
  };
}
export function applyUsage(previous: UsageState, event: Json): UsageState {
  return { ...previous,
    input: numberValue(event.inputTokens) ?? previous.input,
    output: numberValue(event.outputTokens) ?? previous.output,
    context: numberValue(event.contextLength) || previous.context,
    budget: numberValue(event.budget) ?? previous.budget,
    outputReserve: numberValue(event.outputReserve) ?? previous.outputReserve,
    toolCount: numberValue(event.toolCount) ?? previous.toolCount,
    toolTokens: numberValue(event.toolTokens) ?? previous.toolTokens,
    estimated: event.type === 'context_usage' ? event.isEstimated !== false : numberValue(event.inputTokens) !== undefined ? event.isEstimated === true : previous.estimated,
  };
}
export const timeText = (milliseconds: number | undefined) => milliseconds === undefined ? '—' : milliseconds < 1000 ? `${Math.round(milliseconds)} ms` : `${(milliseconds / 1000).toFixed(2)} 秒`;
const countText = (count: number | undefined) => count === undefined ? '—' : count.toLocaleString();

export function QueueElapsed({ since }: { since: number | null }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (since === null) return;
    setNow(Date.now()); const timer = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(timer);
  }, [since]);
  return since === null ? null : <span className="queue-elapsed"> · 已等待 {Math.max(0, (now - since) / 1000).toFixed(1)} 秒</span>;
}

export default function PerformancePanel({ usage, metrics, queueSince, queueWaitMs, status, compaction, onClose }: {
  usage: UsageState; metrics: ModelMetrics | null; queueSince: number | null; queueWaitMs: number | null;
  status: string; compaction: CompactionInfo; onClose: () => void;
}) {
  const limit = usage.budget || usage.context;
  const percentage = Math.min(100, usage.input / Math.max(1, limit) * 100);
  const rate = metrics?.succeeded && metrics.usageMeasured && metrics.inputTokens !== undefined && metrics.outputTokens !== undefined && metrics.outputTokens > 0 && metrics.durationMs !== undefined && metrics.durationMs > 0 && metrics.tokensPerSecond !== undefined && metrics.tokensPerSecond > 0 ? metrics.tokensPerSecond : undefined;
  return <section className="performance-panel" aria-label="推理性能与上下文">
    <header><div><strong>推理性能与上下文</strong><small>{status}<QueueElapsed since={queueSince} /></small></div><button className="icon-button" aria-label="关闭性能面板" onClick={onClose}><X size={15} /></button></header>
    <p className="performance-caption">{metrics ? `最近完成的请求${metrics.step !== undefined ? ` · 第 ${metrics.step + 1} 次` : ''}` : '等待一次实际模型请求'}</p>
    <dl className="performance-grid">
      <div><dt>首响应</dt><dd>{timeText(metrics?.firstEventMs)}</dd></div>
      <div><dt>首个文本</dt><dd>{timeText(metrics?.firstTokenMs)}</dd></div>
      <div><dt>请求吞吐</dt><dd>{rate === undefined ? '—' : `${rate.toFixed(1)} tok/s`}</dd></div>
      <div><dt>请求耗时</dt><dd>{timeText(metrics?.durationMs)}</dd></div>
      <div><dt>排队耗时</dt><dd>{queueSince !== null ? <>等待中<QueueElapsed since={queueSince} /></> : timeText(queueWaitMs ?? metrics?.queueWaitMs)}</dd></div>
      <div><dt>真实输入 / 输出</dt><dd>{metrics?.usageMeasured ? `${countText(metrics.inputTokens)} / ${countText(metrics.outputTokens)}` : '后端未返回'}</dd></div>
    </dl>
    <p className="performance-caption">{rate !== undefined ? '吞吐按真实输出 token ÷ 请求总耗时计算，含预填充与网络，不代表纯解码速度。' : metrics && !metrics.usageMeasured ? '后端未返回真实用量，吞吐暂不可用。' : metrics && !metrics.succeeded ? '失败请求不计算吞吐。' : '仅在成功请求返回有效真实用量时显示吞吐。'}</p>
    <div className="context-detail"><div><strong>{usage.budget ? '输入预算' : '已配置上下文'}</strong><span>{usage.estimated ? '约 ' : ''}{usage.input.toLocaleString()} / {limit.toLocaleString()} · {percentage.toFixed(0)}%</span></div>
      <div className={`context-meter ${percentage >= 85 ? 'nearly-full' : ''}`} role="progressbar" aria-label="输入上下文占用" aria-valuemin={0} aria-valuemax={limit} aria-valuenow={usage.input}><i style={{ width: `${percentage}%` }} /></div>
      <dl><div><dt>连接配置上下文</dt><dd>{usage.context.toLocaleString()}</dd></div><div><dt>输出预留</dt><dd>{countText(usage.outputReserve)}</dd></div><div><dt>工具定义 · 估算</dt><dd>{countText(usage.toolTokens)} token / {countText(usage.toolCount)} 项</dd></div><div><dt>剩余输入预算</dt><dd>{Math.max(0, limit - usage.input).toLocaleString()}</dd></div></dl>
      {usage.estimated && <p>Token 数为保守估算；实际加载上下文应与推理服务配置一致。</p>}
      {compaction.count > 0 && <p>本会话已整理上下文 {compaction.count} 次{compaction.savedTokens !== undefined ? `，最近节省约 ${compaction.savedTokens.toLocaleString()} token` : ''}；原始历史保留。</p>}
    </div>
  </section>;
}
