import { useLayoutEffect, useRef, useState } from 'react';
import { Square, Terminal, Trash2, X } from 'lucide-react';
import { IconButton } from './Mark';

export default function TerminalPanel({
  terminal,
  command,
  deviceName,
  workspace,
  terminalBusy,
  running,
  projectBusy,
  onCommandChange,
  onRunCommand,
  onStopCommand,
  onClearTerminal,
  onClose,
}: {
  terminal: string | null;
  command: string;
  deviceName: string;
  workspace: string;
  terminalBusy: boolean;
  running: boolean;
  projectBusy: boolean;
  onCommandChange: (value: string) => void;
  onRunCommand: (overrideCommand?: string) => Promise<void>;
  onStopCommand: () => void;
  onClearTerminal: () => void;
  onClose: () => void;
}) {
  const outputRef = useRef<HTMLPreElement>(null);
  const followRef = useRef(true);
  const scrollTopRef = useRef(0);
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const draftCommandRef = useRef('');

  useLayoutEffect(() => {
    followRef.current = true;
  }, []);

  useLayoutEffect(() => {
    const output = outputRef.current;
    if (!output || !followRef.current) return;
    const frame = requestAnimationFrame(() => {
      if (followRef.current && outputRef.current === output) {
        output.scrollTop = output.scrollHeight;
        scrollTopRef.current = output.scrollTop;
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [terminal]);

  function submitCommand() {
    const trimmed = command.trim();
    if (!trimmed) return;
    followRef.current = true;
    if (trimmed !== 'clear') {
      setHistory(old => (old[0] === trimmed ? old : [trimmed, ...old.slice(0, 49)]));
    }
    setHistoryIndex(-1);
    draftCommandRef.current = '';
    void onRunCommand();
  }

  return (
    <section className="terminal-panel">
      <div className="terminal-heading">
        <span>
          <Terminal size={13} /> 终端 · {deviceName}
        </span>
        <div className="terminal-heading-actions">
          <IconButton
            label="清空终端"
            disabled={terminalBusy || !terminal}
            onClick={onClearTerminal}
          >
            <Trash2 size={13} />
          </IconButton>
          <IconButton label="关闭终端" onClick={onClose}>
            <X size={14} />
          </IconButton>
        </div>
      </div>
      <pre
        ref={outputRef}
        onScroll={event => {
          const output = event.currentTarget;
          const nearBottom =
            output.scrollHeight - output.clientHeight - output.scrollTop <= 24;
          // A queued programmatic scroll can arrive after more text was appended.
          // Only a changed scroll position should count as scrolling away.
          if (nearBottom || output.scrollTop !== scrollTopRef.current) {
            followRef.current = nearBottom;
          }
          scrollTopRef.current = output.scrollTop;
        }}
      >
        {terminal ?? '命令将在当前会话绑定的执行设备上运行。'}
      </pre>
      <form
        onSubmit={e => {
          e.preventDefault();
          submitCommand();
        }}
      >
        <span aria-hidden="true">❯</span>
        <input
          aria-label="终端命令"
          placeholder={workspace ? '输入命令…' : '请先选择项目'}
          value={command}
          onChange={e => {
            if (historyIndex !== -1) setHistoryIndex(-1);
            onCommandChange(e.target.value);
          }}
          onKeyDown={e => {
            if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'l') {
              e.preventDefault();
              if (!terminalBusy) onClearTerminal();
              return;
            }
            if (e.key === 'ArrowUp' && history.length > 0) {
              e.preventDefault();
              if (historyIndex === -1) draftCommandRef.current = command;
              const next = Math.min(history.length - 1, historyIndex + 1);
              setHistoryIndex(next);
              onCommandChange(history[next]);
            } else if (e.key === 'ArrowDown' && historyIndex !== -1) {
              e.preventDefault();
              const next = historyIndex - 1;
              setHistoryIndex(next);
              onCommandChange(next === -1 ? draftCommandRef.current : history[next]);
            }
          }}
          disabled={!workspace || terminalBusy || running || projectBusy}
        />
        {terminalBusy && (
          <button
            type="button"
            className="terminal-stop"
            aria-label="停止终端命令"
            onClick={onStopCommand}
          >
            <Square size={12} fill="currentColor" />
            停止
          </button>
        )}
      </form>
    </section>
  );
}
