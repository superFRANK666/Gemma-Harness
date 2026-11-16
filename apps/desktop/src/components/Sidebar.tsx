import { useEffect, useRef, useState } from 'react';
import {
  ChevronDown,
  Circle,
  Download,
  FolderOpen,
  Monitor,
  MoreHorizontal,
  Pencil,
  Plus,
  Search,
  Settings2,
  X,
} from 'lucide-react';
import type { Project, Session } from '../types';
import { IconButton, Mark } from './Mark';
import { ProjectList } from './Projects';

export default function Sidebar({
  collapsed,
  inert,
  projects,
  sessions,
  current,
  workspace,
  workspaceName,
  deviceId,
  deviceName,
  shortcut,
  filter,
  searchOpen,
  busy,
  onNewChat,
  onToggleSearch,
  onFilterChange,
  onSelectWorkspace,
  onOpenProject,
  onManageProject,
  onOpenSession,
  onRequestRenameSession,
  onExportSession,
  onOpenSettings,
}: {
  collapsed: boolean;
  inert: boolean;
  projects: Project[] | null;
  sessions: Session[];
  current: Session | null;
  workspace: string;
  workspaceName: string;
  deviceId: string;
  deviceName: string;
  shortcut: string;
  filter: string;
  searchOpen: boolean;
  busy: boolean;
  onNewChat: () => void;
  onToggleSearch: () => void;
  onFilterChange: (value: string) => void;
  onSelectWorkspace: () => void;
  onOpenProject: (project: Project) => void;
  onManageProject: (project: Project, action: 'rename' | 'remove') => void;
  onOpenSession: (session: Session) => void;
  onRequestRenameSession: (session: Session) => void;
  onExportSession: (session: Session) => void;
  onOpenSettings: (tab: string) => void;
}) {
  const [sessionMenu, setSessionMenu] = useState<string | null>(null);
  const sessionRootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!sessionMenu) return;
    const dismiss = (event: MouseEvent) => {
      if (!sessionRootRef.current?.contains(event.target as Node)) setSessionMenu(null);
    };
    const onEsc = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setSessionMenu(null);
    };
    document.addEventListener('mousedown', dismiss);
    document.addEventListener('keydown', onEsc);
    return () => {
      document.removeEventListener('mousedown', dismiss);
      document.removeEventListener('keydown', onEsc);
    };
  }, [sessionMenu]);

  const query = filter.trim().toLowerCase();
  const visibleSessions = query
    ? sessions.filter(s => s.title.toLowerCase().includes(query))
    : sessions;

  return (
    <aside
      className={`sidebar ${collapsed ? 'collapsed' : ''}`}
      inert={collapsed || inert || undefined}
      aria-hidden={collapsed || undefined}
    >
      <div className="traffic-space" data-tauri-drag-region />
      <div className="brand">
        <Mark small />
        <span>harness</span>
      </div>
      <button type="button" className="new-chat" onClick={onNewChat}>
        <Plus size={17} /> 新会话 <kbd>{shortcut} N</kbd>
      </button>
      <button
        type="button"
        className={`nav-item ${searchOpen ? 'active' : ''}`}
        aria-expanded={searchOpen}
        onClick={onToggleSearch}
      >
        <Search size={16} /> 搜索会话 <span>{shortcut} K</span>
      </button>
      {searchOpen && (
        <div className="session-search-wrap">
          <input
            className="session-search"
            autoFocus
            aria-label="搜索会话"
            placeholder="按标题查找…"
            value={filter}
            onChange={e => onFilterChange(e.target.value)}
            onKeyDown={e => {
              if (e.key === 'Escape') {
                e.stopPropagation();
                onFilterChange('');
                onToggleSearch();
              }
            }}
          />
          {filter && (
            <button
              type="button"
              className="session-search-clear"
              aria-label="清空搜索"
              onClick={() => onFilterChange('')}
            >
              <X size={12} />
            </button>
          )}
        </div>
      )}
      <div className="sidebar-section">
        <span>项目</span>
        <IconButton label="打开项目" onClick={onSelectWorkspace}>
          <Plus size={14} />
        </IconButton>
      </div>
      {projects !== null ? (
        <ProjectList
          projects={projects}
          workspace={workspace}
          deviceId={deviceId}
          disabled={busy}
          onOpen={onOpenProject}
          onManage={onManageProject}
        />
      ) : (
        workspace && (
          <button type="button" className="project-item selected" onClick={onSelectWorkspace}>
            <FolderOpen size={17} />
            <span>{workspaceName}</span>
            <ChevronDown size={13} />
          </button>
        )
      )}
      {workspace && (
        <div className="workspace-path" title={workspace}>
          {workspace}
        </div>
      )}
      <div className="sidebar-section sessions-heading">
        <span>最近会话</span>
        {sessions.length > 0 && (
          <span className="sidebar-count" title={`共 ${sessions.length} 条会话`}>
            {query ? `${visibleSessions.length}/${sessions.length}` : sessions.length}
          </span>
        )}
      </div>
      <div className="session-list" ref={sessionRootRef}>
        {visibleSessions.map(session => (
          <div className="session-row" key={session.id}>
            <button
              type="button"
              className={`session-item ${current?.id === session.id ? 'selected' : ''}`}
              onClick={() => {
                setSessionMenu(null);
                onOpenSession(session);
              }}
            >
              <Circle size={7} fill="currentColor" aria-hidden="true" />
              <span>{session.title}</span>
              {session.deviceId !== 'local' && <Monitor size={12} aria-hidden="true" />}
            </button>
            <button
              type="button"
              className="session-menu-button icon-button"
              aria-label="会话操作"
              title={`管理会话 ${session.title}`}
              aria-expanded={sessionMenu === session.id}
              aria-haspopup="menu"
              onClick={() =>
                setSessionMenu(curr => (curr === session.id ? null : session.id))
              }
            >
              <MoreHorizontal size={14} />
            </button>
            {sessionMenu === session.id && (
              <div
                className="project-menu session-menu"
                role="menu"
                aria-label={`会话操作 ${session.title}`}
              >
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    setSessionMenu(null);
                    onRequestRenameSession(session);
                  }}
                >
                  <Pencil size={13} />
                  重命名
                </button>
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    setSessionMenu(null);
                    onExportSession(session);
                  }}
                >
                  <Download size={13} />
                  导出 JSON
                </button>
              </div>
            )}
          </div>
        ))}
        {sessions.length > 0 && visibleSessions.length === 0 && (
          <div className="session-empty">没有匹配的会话</div>
        )}
      </div>
      <div className="sidebar-bottom">
        <button
          type="button"
          className="device-button"
          onClick={() => onOpenSettings('devices')}
        >
          <span className="device-icon">
            <Monitor size={16} />
          </span>
          <span>
            <strong>{deviceName}执行</strong>
            <small>
              {deviceId === 'local' ? '文件、终端与桌面' : '已配对的执行设备'}
            </small>
          </span>
          <ChevronDown size={13} />
        </button>
        <button
          type="button"
          className="nav-item settings-nav"
          onClick={() => onOpenSettings('models')}
        >
          <Settings2 size={16} /> 设置与连接 <span>{shortcut} ,</span>
        </button>
      </div>
    </aside>
  );
}

export function SessionRenameDialog({
  session,
  onClose,
  onConfirm,
}: {
  session: Session;
  onClose: () => void;
  onConfirm: (title: string) => Promise<void>;
}) {
  const [title, setTitle] = useState(session.title);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !busy) onClose();
    };
    document.addEventListener('keydown', escape);
    return () => document.removeEventListener('keydown', escape);
  }, [busy, onClose]);

  async function submit() {
    const trimmed = title.trim();
    if (busy || !trimmed) return;
    setBusy(true);
    setError('');
    try {
      await onConfirm(trimmed);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="modal-backdrop"
      onMouseDown={e => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <form
        className="project-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="重命名会话"
        onSubmit={e => {
          e.preventDefault();
          void submit();
        }}
      >
        <header>
          <h2>重命名会话</h2>
          <button
            type="button"
            className="icon-button"
            aria-label="关闭会话重命名"
            disabled={busy}
            onClick={onClose}
          >
            <X size={17} />
          </button>
        </header>
        <p className="project-dialog-path">{session.workspace}</p>
        <label className="field">
          会话标题
          <input
            ref={inputRef}
            value={title}
            maxLength={120}
            aria-label="会话标题"
            onChange={e => setTitle(e.target.value)}
          />
        </label>
        {error && (
          <p className="project-dialog-error" role="alert">
            {error}
          </p>
        )}
        <footer>
          <button
            type="button"
            className="secondary-button"
            disabled={busy}
            onClick={onClose}
          >
            取消
          </button>
          <button
            type="submit"
            className="primary-button"
            disabled={busy || !title.trim()}
          >
            {busy ? '正在保存…' : '保存标题'}
          </button>
        </footer>
      </form>
    </div>
  );
}
