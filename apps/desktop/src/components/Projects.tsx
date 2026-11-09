import { useEffect, useRef, useState } from 'react';
import { FolderOpen, Monitor, MoreHorizontal, Pencil, Trash2, X } from 'lucide-react';
import type { Project } from '../types';
import '../projects.css';

export function ProjectList({ projects, workspace, deviceId, disabled, onOpen, onManage }: {
  projects: Project[]; workspace: string; deviceId: string; disabled: boolean;
  onOpen: (project: Project) => void; onManage: (project: Project, action: 'rename' | 'remove') => void;
}) {
  const [menu, setMenu] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const dismiss = (event: MouseEvent) => { if (!root.current?.contains(event.target as Node)) setMenu(null); };
    const escape = (event: KeyboardEvent) => { if (event.key === 'Escape') setMenu(null); };
    document.addEventListener('mousedown', dismiss); document.addEventListener('keydown', escape);
    return () => { document.removeEventListener('mousedown', dismiss); document.removeEventListener('keydown', escape); };
  }, [menu]);
  return <div className="project-list" ref={root} aria-label="项目列表">{projects.map(project => <div className="project-row" key={project.id}>
    <button className={`project-item ${workspace === project.workspace && deviceId === project.deviceId ? 'selected' : ''}`} disabled={disabled} title={project.workspace} onClick={() => { setMenu(null); onOpen(project); }}>
      <FolderOpen size={16} /><span>{project.name}</span>{project.deviceId !== 'local' && <Monitor size={12} />}
    </button>
    <button className="project-menu-button icon-button" aria-label={`管理项目 ${project.name}`} aria-expanded={menu === project.id} aria-haspopup="menu" onClick={() => setMenu(current => current === project.id ? null : project.id)}><MoreHorizontal size={15} /></button>
    {menu === project.id && <div className="project-menu" role="menu" aria-label={`项目操作 ${project.name}`}>
      <button role="menuitem" onClick={() => { setMenu(null); onManage(project, 'rename'); }}><Pencil size={13} />重命名</button>
      <button role="menuitem" disabled={disabled} onClick={() => { setMenu(null); onManage(project, 'remove'); }}><Trash2 size={13} />移除项目</button>
    </div>}
  </div>)}{projects.length === 0 && <p className="no-projects">尚未添加项目<br />点击 + 打开项目文件夹</p>}</div>;
}

export function ProjectDialog({ project, action, onClose, onConfirm }: {
  project: Project; action: 'rename' | 'remove'; onClose: () => void; onConfirm: (name: string) => Promise<void>;
}) {
  const [name, setName] = useState(project.name);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => { input.current?.focus(); input.current?.select(); }, []);
  useEffect(() => { const escape = (event: KeyboardEvent) => { if (event.key === 'Escape' && !busy) onClose(); }; document.addEventListener('keydown', escape); return () => document.removeEventListener('keydown', escape); }, [busy, onClose]);
  async function submit() {
    if (busy || (action === 'rename' && !name.trim())) return;
    setBusy(true); setError('');
    try { await onConfirm(name.trim()); onClose(); }
    catch (error) { setError(error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  }
  return <div className="modal-backdrop" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose(); }}>
    <form className="project-dialog" role="dialog" aria-modal="true" aria-label={action === 'rename' ? '重命名项目' : '移除项目'} onSubmit={event => { event.preventDefault(); void submit(); }}>
      <header><h2>{action === 'rename' ? '重命名项目' : '移除项目'}</h2><button type="button" className="icon-button" aria-label="关闭项目操作" disabled={busy} onClick={onClose}><X size={17} /></button></header>
      <p className="project-dialog-path">{project.workspace}</p>
      {action === 'rename' ? <><label className="field">项目名称<input ref={input} value={name} maxLength={100} aria-label="项目名称" onChange={event => setName(event.target.value)} /></label><p className="project-dialog-note">更改列表中的显示名称，文件夹路径保持不变。</p></> : <p className="project-dialog-note">将“{project.name}”从项目列表移除。本地或远端文件、Git 仓库和历史会话都会保留，之后可以重新添加。</p>}
      {error && <p className="project-dialog-error" role="alert">{error}</p>}
      <footer><button type="button" className="secondary-button" disabled={busy} onClick={onClose}>取消</button><button type="submit" className="primary-button" disabled={busy || (action === 'rename' && !name.trim())}>{busy ? '正在保存…' : action === 'rename' ? '保存名称' : '移除项目'}</button></footer>
    </form>
  </div>;
}
