import { useLayoutEffect, useRef, useState } from 'react';
import { Square, Terminal, Trash2, X } from 'lucide-react';
import { IconButton } from './Mark';

/**
 * Reverses macOS/WebKit smart punctuation and autocorrect mutations inside the
 * CLI input while leaving pastes, undo/redo, and IME composition untouched.
 */
function normalizeTerminalInputEdit(
  prev: string,
  next: string,
  inputType: string,
  isComposing: boolean,
): { value: string; caret: number } | null {
  if (
    isComposing ||
    inputType === 'insertFromPaste' ||
    inputType === 'insertFromDrop' ||
    inputType === 'historyUndo' ||
    inputType === 'historyRedo'
  ) {
    return null;
  }

  let prefix = 0;
  const minLen = Math.min(prev.length, next.length);
  while (prefix < minLen && prev[prefix] === next[prefix]) prefix++;
  let suffix = 0;
  while (
    suffix < prev.length - prefix &&
    suffix < next.length - prefix &&
    prev[prev.length - 1 - suffix] === next[next.length - 1 - suffix]
  ) {
    suffix++;
  }

  const removed = prev.slice(prefix, prev.length - suffix);
  const inserted = next.slice(prefix, next.length - suffix);

  let replacement: string | null = null;
  if (
    (removed === '-' && (inserted === '\u2014' || inserted === '\u2013')) ||
    (removed.startsWith('--') &&
      (inserted.startsWith('\u2014') || inserted.startsWith('\u2013')) &&
      inserted.slice(1).startsWith(removed.slice(2)))
  ) {
    replacement = '--' + inserted.slice(1);
  } else if (inputType === 'insertReplacementText') {
    if (inserted === '\u2014' || inserted === '\u2013') replacement = '--';
    else if (inserted === '\u201C' || inserted === '\u201D') replacement = '"';
    else if (inserted === '\u2018' || inserted === '\u2019') replacement = "'";
    else if (
      removed &&
      removed !== inserted &&
      removed.toLowerCase() === inserted.toLowerCase()
    ) {
      replacement = removed;
    }
  }

  if (replacement === null) return null;
  return {
    value: next.slice(0, prefix) + replacement + next.slice(next.length - suffix),
    caret: prefix + replacement.length,
  };
}

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
  const inputRef = useRef<HTMLInputElement>(null);
  const pendingCaretRef = useRef<number | null>(null);
  const followRef = useRef(true);
  const scrollTopRef = useRef(0);
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const draftCommandRef = useRef('');

  useLayoutEffect(() => {
    followRef.current = true;
  }, []);

  useLayoutEffect(() => {
    const caret = pendingCaretRef.current;
    pendingCaretRef.current = null;
    if (caret !== null && inputRef.current) {
      inputRef.current.setSelectionRange(caret, caret);
    }
  }, [command]);

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
          {workspace && (
            <code className="terminal-cwd" title={workspace}>
              {workspace}
            </code>
          )}
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
          ref={inputRef}
          type="text"
          aria-label="终端命令"
          autoCapitalize="none"
          autoCorrect="off"
          autoComplete="off"
          spellCheck={false}
          placeholder={workspace ? '输入命令…' : '请先选择项目'}
          value={command}
          onChange={e => {
            if (historyIndex !== -1) setHistoryIndex(-1);
            const native = e.nativeEvent as Partial<InputEvent>;
            const normalized = normalizeTerminalInputEdit(
              command,
              e.target.value,
              typeof native.inputType === 'string' ? native.inputType : '',
              native.isComposing === true,
            );
            if (normalized) {
              pendingCaretRef.current = normalized.caret;
              onCommandChange(normalized.value);
            } else {
              onCommandChange(e.target.value);
            }
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
