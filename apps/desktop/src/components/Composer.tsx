import { useLayoutEffect, useRef, useState } from 'react';
import type { ClipboardEvent, DragEvent, RefObject } from 'react';
import { ArrowDown, ArrowUp, Code2, ImagePlus, Loader2, Plus, Square, X } from 'lucide-react';
import type { ImageAttachment } from '../types';
import { basename, formatBytes } from '../utils/sessionEvents';
import { QueueElapsed } from './PerformancePanel';

export default function Composer({
  inputRef,
  input,
  attachments,
  workspace,
  running,
  terminalBusy,
  projectBusy,
  status,
  queueSince,
  chatAtBottom,
  onInputChange,
  onAddImages,
  onRemoveAttachment,
  onSelectWorkspace,
  onSend,
  onStop,
  onJumpLatest,
}: {
  inputRef: RefObject<HTMLTextAreaElement | null>;
  input: string;
  attachments: ImageAttachment[];
  workspace: string;
  running: boolean;
  terminalBusy: boolean;
  projectBusy: boolean;
  status: string;
  queueSince: number | null;
  chatAtBottom: boolean;
  onInputChange: (value: string) => void;
  onAddImages: (files: File[]) => Promise<void>;
  onRemoveAttachment: (index: number) => void;
  onSelectWorkspace: () => void;
  onSend: () => Promise<void>;
  onStop: () => Promise<void>;
  onJumpLatest: () => void;
}) {
  const imageInputRef = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);

  useLayoutEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    if (!input) {
      el.style.height = '';
      return;
    }
    el.style.height = 'auto';
    el.style.height = `${Math.min(220, el.scrollHeight)}px`;
  }, [input, inputRef]);

  function handlePaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    const items = event.clipboardData?.items;
    if (!items || running) return;
    const files: File[] = [];
    for (const item of items) {
      if (item.kind === 'file' && (item.type === 'image/png' || item.type === 'image/jpeg')) {
        const file = item.getAsFile();
        if (file) files.push(file);
      }
    }
    if (files.length > 0) {
      event.preventDefault();
      void onAddImages(files);
    }
  }

  function handleDragOver(event: DragEvent<HTMLDivElement>) {
    if (running) return;
    if (event.dataTransfer?.types?.includes('Files')) {
      event.preventDefault();
      setDragging(true);
    }
  }

  function handleDrop(event: DragEvent<HTMLDivElement>) {
    setDragging(false);
    if (running) return;
    const files = Array.from(event.dataTransfer?.files || []);
    if (files.length > 0) {
      event.preventDefault();
      void onAddImages(files);
    }
  }

  return (
    <div className="composer-dock">
      {!chatAtBottom && (
        <button type="button" className="jump-latest" onClick={onJumpLatest}>
          <ArrowDown size={13} />
          回到最新
        </button>
      )}
      <div className="composer-area">
        <div
          className={`composer ${running ? 'is-running' : ''} ${dragging ? 'is-dragging' : ''}`}
          onDragOver={handleDragOver}
          onDragLeave={() => setDragging(false)}
          onDrop={handleDrop}
        >
          <textarea
            ref={inputRef}
            aria-label="消息"
            placeholder={
              workspace
                ? `向 Gemma 描述任务，工作目录：${basename(workspace)}…`
                : '描述你的任务，或先连接模型、选择项目…'
            }
            value={input}
            onChange={e => onInputChange(e.target.value)}
            onPaste={handlePaste}
            onKeyDown={e => {
              if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                void onSend();
              }
            }}
            rows={3}
          />
          {attachments.length > 0 && (
            <div className="composer-attachments">
              {attachments.map((image, index) => (
                <span className="attachment-chip" key={`${image.name}-${index}`}>
                  <img src={image.dataUrl} alt="" className="attachment-thumb" />
                  <span className="attachment-meta">
                    <span className="attachment-name">{image.name}</span>
                    <small>{formatBytes(image.size)}</small>
                  </span>
                  <button
                    type="button"
                    aria-label={`移除图片 ${image.name}`}
                    onClick={() => onRemoveAttachment(index)}
                  >
                    <X size={12} />
                  </button>
                </span>
              ))}
            </div>
          )}
          <div className="composer-toolbar">
            <div className="composer-actions">
              <button type="button" onClick={onSelectWorkspace}>
                <Plus size={16} />
                <span>项目上下文</span>
              </button>
              <button
                type="button"
                disabled={running}
                onClick={() => imageInputRef.current?.click()}
              >
                <ImagePlus size={15} />
                <span>添加图片</span>
              </button>
              <input
                ref={imageInputRef}
                type="file"
                accept="image/png,image/jpeg"
                multiple
                hidden
                aria-label="选择图片"
                onChange={e => {
                  const files = Array.from(e.currentTarget.files || []);
                  e.currentTarget.value = '';
                  void onAddImages(files);
                }}
              />
            </div>
            <div className="composer-meta">
              <span className={`composer-status ${status === '就绪' ? 'is-idle' : ''}`}>
                {running && <Loader2 size={11} className="spin" />}
                {status === '就绪' ? (
                  <>
                    <Code2 size={13} /> 编程与电脑操作
                  </>
                ) : (
                  <>
                    <span role="status">{status}</span>
                    <QueueElapsed since={queueSince} />
                  </>
                )}
              </span>
              {running ? (
                <button
                  type="button"
                  className="send-button stop"
                  aria-label="停止"
                  onClick={() => void onStop()}
                >
                  <Square size={14} fill="currentColor" />
                </button>
              ) : (
                <button
                  type="button"
                  className="send-button"
                  aria-label="发送"
                  disabled={(!input.trim() && attachments.length === 0) || terminalBusy || projectBusy}
                  onClick={() => void onSend()}
                >
                  <ArrowUp size={19} />
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
