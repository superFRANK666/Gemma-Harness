import { isValidElement, memo, useCallback, useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { Braces, Check, ChevronDown, Circle, Copy, Image as ImageIcon, Loader2 } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { pretty } from '../bridge';
import type { Activity, Json } from '../types';
import { prettyArgs } from '../utils/sessionEvents';
import { Mark } from './Mark';

type ToolTone = 'running' | 'error' | 'warn' | 'done';

function toolState(item: Activity): { label: string; tone: ToolTone } {
  const result = item.result as Json | undefined;
  if (result?.indeterminate) return { label: '结果待确认', tone: 'warn' };
  if (result?.cancelled) return { label: '已停止', tone: 'warn' };
  if (result?.timed_out) return { label: '已超时', tone: 'error' };
  if (
    result?.isError ||
    result?.error ||
    (typeof result?.exit_code === 'number' && result.exit_code !== 0)
  ) {
    return { label: '执行失败', tone: 'error' };
  }
  if (item.running) return { label: '执行中', tone: 'running' };
  return {
    label: item.durationMs !== undefined ? `${item.durationMs} ms` : '已结束',
    tone: 'done',
  };
}

function extractNodeText(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(extractNodeText).join('');
  if (isValidElement<{ children?: ReactNode }>(node)) return extractNodeText(node.props.children);
  return '';
}

function useCopyFeedback() {
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    },
    [],
  );
  const copy = useCallback((text: string) => {
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      if (timerRef.current !== null) window.clearTimeout(timerRef.current);
      timerRef.current = window.setTimeout(() => setCopied(false), 1600);
    }).catch(() => {});
  }, []);
  return { copied, copy };
}

function MarkdownPre({ children }: { children?: ReactNode }) {
  const { copied, copy } = useCopyFeedback();
  const codeElement = isValidElement<{ className?: string; children?: ReactNode }>(children)
    ? children
    : null;
  const className = codeElement?.props?.className || '';
  const langMatch = className.match(/language-([\w-]+)/);
  const lang = langMatch ? langMatch[1] : '';
  const rawCode = extractNodeText(codeElement ? codeElement.props.children : children).replace(/\n$/, '');

  return (
    <div className="code-block">
      <div className="code-block-header">
        <span>{lang || 'code'}</span>
        <button
          type="button"
          className="code-copy-button"
          aria-label="复制代码"
          onClick={() => copy(rawCode)}
        >
          {copied ? <Check size={12} /> : <Copy size={12} />}
          {copied ? '已复制' : '复制'}
        </button>
      </div>
      <pre>{children}</pre>
    </div>
  );
}

const markdownComponents = {
  pre: MarkdownPre,
  a: ({ href, children }: { href?: string; children?: ReactNode }) => (
    <a href={href} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  ),
};

/** Completed messages retain their object identity while the live tail changes. */
const ActivityCard = memo(function ActivityCard({
  item,
  onCheckOperation,
  onRestoreDeleted,
  restoreDisabled,
}: {
  item: Activity;
  onCheckOperation: (itemId: string, operationId: string) => void;
  onRestoreDeleted: (callId: string) => void;
  restoreDisabled: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const { copied, copy } = useCopyFeedback();

  if (item.role === 'tool') {
    const result = item.result as Json | undefined;
    const deleted = item.name === 'file_delete' && result?.deleted === true;
    const state = toolState(item);
    return (
      <details
        className={`tool-card ${item.running ? 'running' : ''} tone-${state.tone}`}
        onToggle={event => setExpanded(event.currentTarget.open)}
      >
        <summary>
          {item.running ? <Loader2 className="spin" size={15} /> : <Braces size={15} />}
          <strong>{item.name}</strong>
          <span>{item.deviceId === 'local' ? '本机' : '远端'}</span>
          <small className={`tool-state-badge tone-${state.tone}`}>{state.label}</small>
          <ChevronDown size={13} />
        </summary>
        {expanded && (
          <>
            {item.args && <pre>{prettyArgs(item.args)}</pre>}
            {item.text && <pre className="tool-live-output">{item.text}</pre>}
            {item.result !== undefined && (
              <pre className="tool-result">{pretty(item.result)}</pre>
            )}
            {deleted && (
              <button
                type="button"
                className="operation-check"
                disabled={restoreDisabled}
                onClick={() => onRestoreDeleted(item.callId ?? item.id)}
              >
                恢复删除的文件
              </button>
            )}
            {result?.indeterminate === true &&
              (item.deviceId && item.deviceId !== 'local' && !item.name?.startsWith('mcp_') ? (
                <button
                  type="button"
                  className="operation-check"
                  onClick={() => onCheckOperation(item.id, String(result.operationId))}
                >
                  查询远端执行状态
                </button>
              ) : (
                <p className="uncertain-operation">
                  动作可能已发生；请重新观察实际状态后继续。
                </p>
              ))}
          </>
        )}
      </details>
    );
  }

  if (item.role === 'status') {
    return (
      <div className="activity-note">
        <Circle size={6} />
        <span>{item.text}</span>
      </div>
    );
  }

  return (
    <article className={`message ${item.role}`}>
      <div className="message-avatar">
        {item.role === 'assistant' ? <Mark small /> : '你'}
      </div>
      <div className="message-body">
        <div className="message-label">
          <strong>{item.role === 'assistant' ? 'Gemma' : '你'}</strong>
          {item.running && <span className="message-generating">正在生成</span>}
          {!item.running && item.text && (
            <button
              type="button"
              className="message-copy-button"
              aria-label="复制消息内容"
              onClick={() => copy(item.text)}
            >
              {copied ? <Check size={12} /> : <Copy size={12} />}
              {copied ? '已复制' : '复制'}
            </button>
          )}
        </div>
        {item.reasoning && (
          <details className="reasoning">
            <summary>思考过程</summary>
            <pre>{item.reasoning}</pre>
          </details>
        )}
        {item.images && item.images.length > 0 && (
          <div className="message-images">
            {item.images.map((label, index) => (
              <span key={`${label}-${index}`} className="message-image-pill">
                <ImageIcon size={12} />
                {label}
              </span>
            ))}
          </div>
        )}
        {item.text && (
          <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
            {item.text}
          </ReactMarkdown>
        )}
        {item.running && !item.text && (
          <span className="typing" aria-label="正在输入">
            <i />
            <i />
            <i />
          </span>
        )}
      </div>
    </article>
  );
});

export default ActivityCard;
