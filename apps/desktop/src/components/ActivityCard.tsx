import { memo, useState } from 'react';
import { Braces, ChevronDown, Circle, Loader2 } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { pretty } from '../bridge';
import type { Activity, Json } from '../types';

function toolState(item: Activity) {
  const result = item.result as Json | undefined;
  if (result?.indeterminate) return '结果待确认';
  if (result?.cancelled) return '已停止';
  if (result?.timed_out) return '已超时';
  if (result?.isError || result?.error || (typeof result?.exit_code === 'number' && result.exit_code !== 0)) return '执行失败';
  return item.running ? '执行中' : item.durationMs !== undefined ? `${item.durationMs} ms` : '已结束';
}

/** Completed messages retain their object identity while the live tail changes. */
const ActivityCard = memo(function ActivityCard({ item, onCheckOperation, onRestoreDeleted, restoreDisabled }: {
  item: Activity;
  onCheckOperation: (itemId: string, operationId: string) => void;
  onRestoreDeleted: (itemId: string) => void;
  restoreDisabled: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const deleted = item.name === 'file_delete' && (item.result as Json | undefined)?.deleted === true;
  if (item.role === 'tool') return <details className={`tool-card ${item.running ? 'running' : ''}`} onToggle={event => setExpanded(event.currentTarget.open)}>
    <summary>{item.running ? <Loader2 className="spin" size={15} /> : <Braces size={15} />}<strong>{item.name}</strong><span>{item.deviceId === 'local' ? '本机' : '远端'}</span><small>{toolState(item)}</small><ChevronDown size={13} /></summary>
    {expanded && <><pre>{item.args}</pre>{item.text && <pre className="tool-live-output">{item.text}</pre>}{item.result !== undefined && <pre className="tool-result">{pretty(item.result)}</pre>}{deleted && <button className="operation-check" disabled={restoreDisabled} onClick={() => onRestoreDeleted(item.id)}>恢复删除的文件</button>}{(item.result as Json | undefined)?.indeterminate === true && (item.deviceId && item.deviceId !== 'local' && !item.name?.startsWith('mcp_') ? <button className="operation-check" onClick={() => onCheckOperation(item.id, String((item.result as Json).operationId))}>查询远端执行状态</button> : <p className="uncertain-operation">动作可能已发生；请重新观察实际状态后继续。</p>)}</>}
  </details>;
  if (item.role === 'status') return <div className="activity-note"><Circle size={6} />{item.text}</div>;
  return <article className={`message ${item.role}`}>
    <div className="message-avatar">{item.role === 'assistant' ? <span className="brand-mark small"><i /><i /><i /></span> : '你'}</div>
    <div className="message-body"><div className="message-label">{item.role === 'assistant' ? 'Gemma' : '你'}{item.running && <span>正在生成</span>}</div>{item.reasoning && <details className="reasoning"><summary>思考过程</summary><pre>{item.reasoning}</pre></details>}<ReactMarkdown remarkPlugins={[remarkGfm]}>{item.text}</ReactMarkdown>{item.running && !item.text && <span className="typing"><i /><i /><i /></span>}</div>
  </article>;
});

export default ActivityCard;
