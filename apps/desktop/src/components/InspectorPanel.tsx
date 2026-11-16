import { useEffect, useState } from 'react';
import {
  ArrowLeft,
  ArrowUpRight,
  Camera,
  Cpu,
  Eye,
  FileCode2,
  FileDiff,
  Folder,
  FolderOpen,
  Monitor,
  RefreshCw,
  X,
} from 'lucide-react';
import type { Connection, Json } from '../types';
import { basename, parentDir, parseGitDiffResult } from '../utils/sessionEvents';
import type { ParsedDiffResult } from '../utils/sessionEvents';
import { IconButton } from './Mark';

export interface OpenFileState {
  path: string;
  text: string;
  hash: string;
}

export default function InspectorPanel({
  panel,
  workspace,
  workspaceName,
  currentDir,
  files,
  file,
  fileText,
  diffResult,
  inspection,
  inspecting,
  computerKind,
  previewImage,
  modelId,
  connection,
  onInspect,
  onSelectWorkspace,
  onReadFile,
  onFileTextChange,
  onSaveFile,
  onCloseFile,
  onComputerKindChange,
  onCapture,
  onOpenSettings,
}: {
  panel: string;
  workspace: string;
  workspaceName: string;
  currentDir: string;
  files: Json | null;
  file: OpenFileState | null;
  fileText: string;
  diffResult: Json | null;
  inspection: string;
  inspecting: boolean;
  computerKind: string;
  previewImage: string;
  modelId: string;
  connection?: Connection;
  onInspect: (kind: string, path?: string) => Promise<void>;
  onSelectWorkspace: () => void;
  onReadFile: (path: string) => Promise<void>;
  onFileTextChange: (text: string) => void;
  onSaveFile: () => Promise<void>;
  onCloseFile: () => void;
  onComputerKindChange: (kind: string) => void;
  onCapture: () => Promise<void>;
  onOpenSettings: (tab: string) => void;
}) {
  const [discardConfirm, setDiscardConfirm] = useState(false);
  const [lightboxOpen, setLightboxOpen] = useState(false);

  const dirty = Boolean(file && fileText !== file.text);
  useEffect(() => {
    if (!dirty) setDiscardConfirm(false);
  }, [dirty, file?.path]);

  useEffect(() => {
    if (!lightboxOpen) return;
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setLightboxOpen(false);
    };
    document.addEventListener('keydown', onEsc);
    return () => document.removeEventListener('keydown', onEsc);
  }, [lightboxOpen]);

  const entries = Array.isArray(files?.entries) ? (files.entries as Json[]) : [];
  const parent = parentDir(currentDir);
  const parsedDiff: ParsedDiffResult | null = diffResult
    ? parseGitDiffResult(diffResult)
    : null;

  function requestCloseFile() {
    if (dirty && !discardConfirm) {
      setDiscardConfirm(true);
      return;
    }
    setDiscardConfirm(false);
    onCloseFile();
  }

  return (
    <aside className="inspector">
      <div className="inspector-tabs">
        {[
          { id: 'files', icon: <Folder size={14} />, label: '文件' },
          { id: 'diff', icon: <FileDiff size={14} />, label: '差异' },
          { id: 'desktop', icon: <Monitor size={14} />, label: '电脑' },
        ].map(tab => (
          <button
            type="button"
            className={panel === tab.id ? 'selected' : ''}
            key={tab.id}
            onClick={() => void onInspect(tab.id)}
          >
            {tab.icon}
            {tab.label}
          </button>
        ))}
      </div>
      <div className="inspector-body">
        {panel === 'files' ? (
          <>
            <div className="inspector-heading">
              <span title={currentDir !== '.' ? currentDir : workspace}>
                {workspace
                  ? currentDir && currentDir !== '.'
                    ? `${workspaceName} / ${currentDir}`
                    : workspaceName
                  : '项目文件'}
              </span>
              <IconButton
                label="刷新文件"
                disabled={inspecting || !workspace}
                onClick={() => void onInspect('files', currentDir)}
              >
                <RefreshCw size={14} className={inspecting ? 'spin' : ''} />
              </IconButton>
            </div>
            {file ? (
              <div className="file-editor">
                <div className="file-editor-bar">
                  <button type="button" onClick={requestCloseFile}>
                    ← 文件
                  </button>
                  <span title={file.path}>
                    {basename(file.path)}
                    {dirty && (
                      <em className="file-dirty-badge" title="有未保存的修改">
                        ● 未保存
                      </em>
                    )}
                  </span>
                  <button
                    type="button"
                    className={dirty ? 'is-dirty' : ''}
                    disabled={!dirty}
                    onClick={() => void onSaveFile()}
                  >
                    保存
                  </button>
                </div>
                {discardConfirm && (
                  <div className="file-discard-bar" role="status">
                    <span>有未保存修改</span>
                    <button
                      type="button"
                      className="text-danger"
                      onClick={() => {
                        setDiscardConfirm(false);
                        onCloseFile();
                      }}
                    >
                      放弃修改
                    </button>
                    <button type="button" onClick={() => setDiscardConfirm(false)}>
                      继续编辑
                    </button>
                  </div>
                )}
                <textarea
                  aria-label="文件编辑器"
                  spellCheck={false}
                  value={fileText}
                  onChange={e => onFileTextChange(e.target.value)}
                  onKeyDown={e => {
                    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') {
                      e.preventDefault();
                      if (dirty) void onSaveFile();
                    }
                  }}
                />
              </div>
            ) : files ? (
              <div className="file-list">
                {parent !== null && (
                  <button
                    type="button"
                    className="file-parent-button"
                    onClick={() => void onInspect('files', parent)}
                  >
                    <ArrowLeft size={14} />
                    <span>返回上级 ({parent === '.' ? workspaceName : parent})</span>
                  </button>
                )}
                {entries.length > 0 ? (
                  entries.map(entry => {
                    const isDir = Boolean(
                      entry.directory ||
                        entry.is_dir ||
                        entry.isDir ||
                        entry.kind === 'directory',
                    );
                    const entryPath = String(entry.path || entry.name);
                    const entryLabel = String(entry.name || entry.path);
                    return (
                      <button
                        type="button"
                        key={entryPath}
                        title={entryPath}
                        onClick={() =>
                          isDir
                            ? void onInspect('files', entryPath)
                            : void onReadFile(entryPath)
                        }
                      >
                        {isDir ? <Folder size={14} /> : <FileCode2 size={14} />}
                        <span>{entryLabel}</span>
                      </button>
                    );
                  })
                ) : (
                  <div className="inspector-empty compact">
                    <FolderOpen size={24} strokeWidth={1.2} />
                    <p>目录为空</p>
                  </div>
                )}
              </div>
            ) : (
              <div className="inspector-empty">
                <FolderOpen size={28} strokeWidth={1.2} />
                <p>{workspace ? '浏览项目文件' : '打开你的项目'}</p>
                <small>
                  {workspace
                    ? '读取当前执行设备上的目录'
                    : '文件、终端与差异将在这里呈现'}
                </small>
                <button
                  type="button"
                  onClick={() =>
                    workspace ? void onInspect('files', '.') : onSelectWorkspace()
                  }
                >
                  {workspace ? '加载文件' : '选择文件夹'}
                  <ArrowUpRight size={13} />
                </button>
              </div>
            )}
          </>
        ) : panel === 'diff' ? (
          <>
            <div className="inspector-heading">
              <span>工作区差异</span>
              <IconButton
                label="刷新"
                disabled={inspecting || !workspace}
                onClick={() => void onInspect('diff')}
              >
                <RefreshCw size={14} className={inspecting ? 'spin' : ''} />
              </IconButton>
            </div>
            {parsedDiff ? (
              parsedDiff.files.length > 0 ? (
                <div className="diff-viewer">
                  <div className="diff-summary">
                    <span>{parsedDiff.files.length} 个文件变更</span>
                    <span className="diff-stats">
                      <em className="add">+{parsedDiff.additions}</em>
                      <em className="del">-{parsedDiff.deletions}</em>
                    </span>
                  </div>
                  {(parsedDiff.truncated || parsedDiff.error) && (
                    <div className="diff-notice" role="status">
                      {parsedDiff.error && <p>{parsedDiff.error}</p>}
                      {parsedDiff.truncated && (
                        <p>
                          差异预览已截断
                          {parsedDiff.artifact ? ` · 完整输出：${parsedDiff.artifact}` : ''}
                        </p>
                      )}
                    </div>
                  )}
                  {parsedDiff.files.map((item, idx) => (
                    <section className="diff-file" key={`${item.path}-${idx}`}>
                      <header className="diff-file-header" title={item.path}>
                        <span>{item.path}</span>
                        <small>
                          +{item.additions} -{item.deletions}
                        </small>
                      </header>
                      <div className="diff-lines">
                        {item.lines.map((line, lineIdx) => (
                          <div key={lineIdx} className={`diff-line ${line.type}`}>
                            {line.text}
                          </div>
                        ))}
                      </div>
                    </section>
                  ))}
                </div>
              ) : (
                <div className="inspector-empty">
                  <FileDiff size={28} strokeWidth={1.2} />
                  <p>{parsedDiff.error ? '无法获取 Git 差异' : '工作区暂无未暂存修改'}</p>
                  <small>
                    {parsedDiff.error || '当前目录没有未暂存的变更'}
                  </small>
                  <button type="button" onClick={() => void onInspect('diff')}>
                    重新检查
                    <ArrowUpRight size={13} />
                  </button>
                </div>
              )
            ) : (
              <div className="inspector-empty">
                <FileDiff size={28} strokeWidth={1.2} />
                <p>修改清晰可见</p>
                <small>选择项目后查看真实 Git 差异</small>
                <button type="button" onClick={() => void onInspect('diff')}>
                  查看差异
                  <ArrowUpRight size={13} />
                </button>
              </div>
            )}
          </>
        ) : (
          <>
            <div className="inspector-heading">
              <span>当前设备观测</span>
              <IconButton
                label="刷新"
                disabled={inspecting || !workspace}
                onClick={() => void onInspect('desktop')}
              >
                <RefreshCw size={14} className={inspecting ? 'spin' : ''} />
              </IconButton>
            </div>
            <div className="computer-controls">
              <select
                aria-label="电脑控制类型"
                value={computerKind}
                onChange={e => onComputerKindChange(e.target.value)}
              >
                <option value="desktop">原生桌面</option>
                <option value="browser">浏览器</option>
              </select>
              <button type="button" onClick={() => void onInspect('desktop')}>
                <Eye size={12} /> 观测
              </button>
              <button type="button" onClick={() => void onCapture()}>
                <Camera size={12} /> 截图
              </button>
            </div>
            {previewImage && (
              <button
                type="button"
                className="computer-preview-trigger"
                aria-label="放大查看截图"
                onClick={() => setLightboxOpen(true)}
              >
                <img
                  className="computer-preview"
                  src={previewImage}
                  alt="当前执行设备的真实截图"
                />
              </button>
            )}
            {inspection ? (
              <pre className="inspection-content">{inspection}</pre>
            ) : (
              <div className="inspector-empty">
                <Monitor size={28} strokeWidth={1.2} />
                <p>先观察，再操作</p>
                <small>在工具设置中启用桌面控制</small>
                <button type="button" onClick={() => onOpenSettings('tools')}>
                  检查工具设置
                  <ArrowUpRight size={13} />
                </button>
              </div>
            )}
          </>
        )}
        <div className="model-info">
          <div className="tiny-label">模型状态</div>
          <div>
            <Cpu size={15} />
            <strong>{modelId || '尚未配置模型'}</strong>
          </div>
          <p>
            {modelId
              ? `连接：${connection?.name}`
              : '配置模型后可在这里查看模型 ID'}
          </p>
          {modelId && (
            <span>
              已配置 · 当前连接状态未知
              {connection?.contextLength
                ? ` · ${Math.round(connection.contextLength / 1024)}K`
                : ''}
            </span>
          )}
          <button type="button" onClick={() => onOpenSettings('models')}>
            {modelId ? '查看模型设置' : '配置模型'} <ArrowUpRight size={12} />
          </button>
        </div>
      </div>

      {lightboxOpen && previewImage && (
        <div
          className="modal-backdrop image-lightbox-backdrop"
          onMouseDown={e => {
            if (e.target === e.currentTarget) setLightboxOpen(false);
          }}
        >
          <div
            className="image-lightbox"
            role="dialog"
            aria-modal="true"
            aria-label="设备截图预览"
          >
            <button
              type="button"
              className="icon-button"
              aria-label="关闭截图预览"
              onClick={() => setLightboxOpen(false)}
            >
              <X size={16} />
            </button>
            <img src={previewImage} alt="当前执行设备的真实截图放大预览" />
          </div>
        </div>
      )}
    </aside>
  );
}
