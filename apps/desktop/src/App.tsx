import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import { ArrowDown, ArrowUp, ArrowUpRight, Check, ChevronDown, ChevronRight, Circle, Code2, Cpu, FileCode2, FileDiff, Folder, FolderOpen, Globe2, HardDrive, ImagePlus, Loader2, Menu, Monitor, MoreHorizontal, PanelRightClose, Plus, Search, Settings2, ShieldCheck, Square, Terminal, Unplug, X } from 'lucide-react';
import { api, choosePath, desktop, pretty, stream, streamTool } from './bridge';
import { defaultConfig } from './types';
import type { Activity, Config, Json, Project, Session, Snapshot } from './types';
import Settings from './components/Settings';
import { ProjectDialog, ProjectList } from './components/Projects';
import { applyAppearance } from './appearance';
import ActivityCard from './components/ActivityCard';
import PerformancePanel, { applyUsage, initialUsage, numberValue, QueueElapsed, readMetrics } from './components/PerformancePanel';
import type { CompactionInfo, ModelMetrics } from './components/PerformancePanel';
import UpdateControl, { useAppUpdates } from './components/UpdateControl';

const basename = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() || '未选择项目';
function Mark({ small = false }: { small?: boolean }) { return <span className={`brand-mark ${small ? 'small' : ''}`}><i /><i /><i /></span>; }
function IconButton({ label, onClick, children, active = false }: { label: string; onClick: () => void; children: ReactNode; active?: boolean }) { return <button title={label} aria-label={label} className={`icon-button ${active ? 'active' : ''}`} onClick={onClick}>{children}</button>; }
function getText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) return content.filter(v => v.type === 'text').map(v => v.text).join('\n');
  return '';
}
const HISTORY_WINDOW = 80;
const metricKey = (session: string | undefined, connection: string | undefined, context: number | undefined) => `${session || 'new'}|${connection || ''}|${context || 8192}`;
const withoutAppearance = (config: Config) => { const copy = { ...config }; delete copy.appearance; return JSON.stringify(copy); };
const savedTokens = (event: Json) => numberValue(event.tokensSaved) ?? numberValue(event.savedTokens) ?? (numberValue(event.previousEstimatedTokens) !== undefined && numberValue(event.estimatedTokens) !== undefined ? Math.max(0, Number(event.previousEstimatedTokens) - Number(event.estimatedTokens)) : undefined);
const modelRetryText = (event: Json) => event.reason === 'empty_final_after_tool'
  ? '工具已执行，正在重新生成结果说明；本次重试不会再次执行工具。'
  : `本次生成达到输出上限，已在未执行新工具的前提下增加预算重试（${String(event.previousOutputTokens)} → ${String(event.outputTokens)} token）。`;
const liveTruncationNotice = '\n[Live output preview truncated; check artifacts and output completeness.]\n';
function terminalResultNotices(result: Json, truncated: boolean): string[] {
  const notices: string[] = [];
  if (result.cancelled === true) notices.push('[命令已中断]');
  if (result.timed_out === true) notices.push('[命令超时]');
  if (result.cancelled !== true && result.timed_out !== true) {
    if (typeof result.exit_code === 'number' && result.exit_code !== 0) notices.push(`[命令退出，退出码：${result.exit_code}]`);
    else if (result.exit_code === null) notices.push('[命令异常结束，没有退出码]');
  }
  const incomplete = result.output_complete === false || result.isError === true;
  if (incomplete) {
    notices.push('[命令输出采集不完整]');
    for (const key of ['stdout_error', 'stderr_error'] as const) {
      if (typeof result[key] === 'string' && result[key]) notices.push(result[key]);
    }
  }
  if (truncated || incomplete) {
    for (const [key, label] of [['stdout_artifact', '标准输出'], ['stderr_artifact', '标准错误']] as const) {
      if (typeof result[key] === 'string' && result[key]) notices.push(`${label}文件（执行设备）：${result[key]}`);
    }
  }
  return notices;
}
export default function App() {
  const [config, setConfig] = useState<Config>(structuredClone(defaultConfig));
  const [sessions, setSessions] = useState<Session[]>([]);
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [projectBusy, setProjectBusy] = useState(false);
  const [restoreBusy, setRestoreBusy] = useState(false);
  const [projectDialog, setProjectDialog] = useState<{ project: Project; action: 'rename' | 'remove' } | null>(null);
  const [current, setCurrent] = useState<Session | null>(null);
  const [activities, setActivities] = useState<Activity[]>([]);
  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<{ name: string; size: number; dataUrl: string }[]>([]);
  const [running, setRunning] = useState(false);
  const [settings, setSettings] = useState<string | null>(null);
  const [toast, setToast] = useState('');
  const [panel, setPanel] = useState('files');
  const [panelOpen, setPanelOpen] = useState(true);
  const [files, setFiles] = useState<Json | null>(null);
  const [file, setFile] = useState<{ path: string; text: string; hash: string } | null>(null);
  const [fileText, setFileText] = useState('');
  const [inspection, setInspection] = useState('');
  const [computerKind, setComputerKind] = useState('desktop');
  const [previewImage, setPreviewImage] = useState('');
  const [usage, setUsage] = useState(initialUsage());
  const [metrics, setMetrics] = useState<ModelMetrics | null>(null);
  const [queueSince, setQueueSince] = useState<number | null>(null);
  const [queueWaitMs, setQueueWaitMs] = useState<number | null>(null);
  const [compaction, setCompaction] = useState<CompactionInfo>({ count: 0 });
  const [performanceOpen, setPerformanceOpen] = useState(false);
  const [historyStart, setHistoryStart] = useState(0);
  const [chatAtBottom, setChatAtBottom] = useState(true);
  const [status, setStatus] = useState('就绪');
  const [terminal, setTerminal] = useState<string | null>(null);
  const [command, setCommand] = useState('');
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [terminalBusy, setTerminalBusy] = useState(false);
  const [updateInstalling, setUpdateInstalling] = useState(false);
  const [terminalOperation, setTerminalOperation] = useState('');
  const [filter, setFilter] = useState('');
  const [searchOpen, setSearchOpen] = useState(false);
  const shortcut = navigator.platform.toLowerCase().includes('mac') ? '⌘' : 'Ctrl';
  useEffect(() => {
    const handler = (event: KeyboardEvent) => { if (settings || projectDialog) return; if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      if (event.key.toLowerCase() === 'n') { event.preventDefault(); startNewChat(); }
      else if (event.key.toLowerCase() === 'k') { event.preventDefault(); setSearchOpen(v => !v); }
      else if (event.key === ',') { event.preventDefault(); setSettings('models'); }
    }; window.addEventListener('keydown', handler); return () => window.removeEventListener('keydown', handler);
  }, [running, terminalBusy, projectBusy, settings, projectDialog]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const imageInputRef = useRef<HTMLInputElement>(null);
  const terminalOutputRef = useRef<HTMLPreElement>(null);
  const terminalFollowRef = useRef(true);
  const unverifiedToolRef = useRef(false);
  const terminalScrollTopRef = useRef(0);
  const inspectorBindingRef = useRef(0);
  const metricBindingRef = useRef('');
  const chatFollowRef = useRef(true);
  const chatScrollTopRef = useRef(0);
  const chatResetFollowRef = useRef(false);
  const historyAnchorRef = useRef<{ height: number; top: number } | null>(null);
  const pendingEventsRef = useRef<Json[]>([]);
  const pendingTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const bindingBusyRef = useRef(false);
  const settingsDirtyRef = useRef(true);
  const recordSettingsDirty = useCallback((dirty: boolean) => { settingsDirtyRef.current = dirty; }, []);
  bindingBusyRef.current = running || terminalBusy || projectBusy || restoreBusy || updateInstalling;
  const configRef = useRef(config); configRef.current = config;
  const connection = config.connections.find(c => c.id === (current?.connectionId || config.selectedConnectionId));
  const modelId = connection?.model?.trim() || '';
  const workspace = current?.workspace || config.workspace;
  const deviceId = current?.deviceId || config.deviceId;
  const deviceName = deviceId === 'local' ? '本机' : config.devices.find(d => d.id === deviceId)?.name || '远端';
  const workspaceName = projects?.find(p => p.workspace === workspace && p.deviceId === deviceId)?.name || basename(workspace);
  useLayoutEffect(() => { if (!settings) return applyAppearance(config.appearance); }, [config.appearance, settings]);
  const notify = useCallback((error: unknown) => setToast(error instanceof Error ? error.message : String(error)), []);
  const performanceBinding = metricKey(current?.id, connection?.id, connection?.contextLength);
  function resetPerformance(context = connection?.contextLength || 8192) {
    setUsage(initialUsage(context)); setMetrics(null); setQueueSince(null); setQueueWaitMs(null); setCompaction({ count: 0 });
  }
  useLayoutEffect(() => {
    if (metricBindingRef.current === performanceBinding) return;
    metricBindingRef.current = performanceBinding; resetPerformance();
  }, [performanceBinding]);
  useLayoutEffect(() => {
    inspectorBindingRef.current += 1;
    setFiles(null); setFile(null); setFileText('');
    setInspection(''); setPreviewImage(''); setPanel('files');
    setAttachments([]);
  }, [workspace, deviceId]);
  async function refresh() {
    if (!desktop) return;
    try { const snapshot = await api<Snapshot>('snapshot'); setConfig(snapshot.config); setSessions(snapshot.sessions); setProjects(snapshot.projects ?? null); } catch (error) { notify(error); }
  }
  useEffect(() => { void refresh(); }, []);
  useEffect(() => { if (toast) { const timer = setTimeout(() => setToast(''), 7000); return () => clearTimeout(timer); } }, [toast]);
  useLayoutEffect(() => {
    const output = scrollRef.current; if (!output) return;
    if (chatResetFollowRef.current) {
      chatResetFollowRef.current = false; chatFollowRef.current = true;
      output.scrollTop = output.scrollHeight; chatScrollTopRef.current = output.scrollTop; return;
    }
    const anchor = historyAnchorRef.current;
    if (anchor) {
      output.scrollTop = anchor.top + output.scrollHeight - anchor.height;
      chatScrollTopRef.current = output.scrollTop; historyAnchorRef.current = null; return;
    }
    if (!chatFollowRef.current) return;
    const frame = requestAnimationFrame(() => {
      if (chatFollowRef.current && scrollRef.current === output) { output.scrollTop = output.scrollHeight; chatScrollTopRef.current = output.scrollTop; }
    });
    return () => cancelAnimationFrame(frame);
  }, [activities, historyStart]);
  useEffect(() => () => { if (pendingTimerRef.current !== null) clearTimeout(pendingTimerRef.current); pendingEventsRef.current = []; }, []);
  useLayoutEffect(() => {
    if (terminalOpen) terminalFollowRef.current = true;
  }, [terminalOpen]);
  useLayoutEffect(() => {
    const output = terminalOutputRef.current;
    if (!terminalOpen || !output || !terminalFollowRef.current) return;
    const frame = requestAnimationFrame(() => {
      if (terminalFollowRef.current && terminalOutputRef.current === output) {
        output.scrollTop = output.scrollHeight;
        terminalScrollTopRef.current = output.scrollTop;
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [terminal, terminalOpen]);
  async function selectWorkspace() {
    if (bindingBusyRef.current) { notify('请先停止当前任务或终端命令，再切换项目'); return; }
    if (file && fileText !== file.text) { notify('请先保存编辑内容，再切换项目'); return; }
    bindingBusyRef.current = true; setProjectBusy(true);
    try {
      let selected: string | null;
      if (configRef.current.deviceId === 'local') selected = await choosePath(true);
      else { setSettings('devices'); return; }
      if (selected) {
        const result = await api<{ config: Config; projects: Project[] }>('open_project', { workspace: selected, deviceId: 'local' });
        applyProjectSelection(result);
      }
    } catch (error) { notify(error); }
    finally { bindingBusyRef.current = false; setProjectBusy(false); }
  }
  function applyProjectSelection(result: { config: Config; projects: Project[] }) {
    setConfig(result.config); setProjects(result.projects); setCurrent(null); setActivities([]); setHistoryStart(0); setFiles(null);
    setStatus('就绪'); chatFollowRef.current = true; chatResetFollowRef.current = true; setChatAtBottom(true);
  }
  async function openProject(project: Project) {
    if (bindingBusyRef.current) { notify('请先停止当前任务或终端命令，再切换项目'); return; }
    if (file && fileText !== file.text && (project.workspace !== workspace || project.deviceId !== deviceId)) { notify('请先保存编辑内容，再切换项目'); return; }
    bindingBusyRef.current = true; setProjectBusy(true);
    try { applyProjectSelection(await api<{ config: Config; projects: Project[] }>('open_project', { projectId: project.id })); }
    catch (error) { notify(error); }
    finally { bindingBusyRef.current = false; setProjectBusy(false); }
  }
  async function manageProject(name: string) {
    if (!projectDialog) return;
    const { project, action } = projectDialog;
    if (action === 'remove' && bindingBusyRef.current) throw new Error('请先停止当前任务或终端命令，再移除项目');
    if (action === 'remove' && project.workspace === workspace && project.deviceId === deviceId && file && fileText !== file.text) throw new Error('请先保存编辑内容，再移除项目');
    const result = await api<{ config: Config; projects: Project[] }>(action === 'rename' ? 'rename_project' : 'remove_project', { projectId: project.id, ...(action === 'rename' ? { name } : {}) });
    setConfig(result.config); setProjects(result.projects);
    if (action === 'remove' && project.workspace === workspace && project.deviceId === deviceId) {
      setCurrent(null); setActivities([]); setHistoryStart(0); setFiles(null); setStatus('就绪');
    }
    notify(action === 'rename' ? '项目名称已更新' : '项目已从列表移除，文件和历史会话已保留');
  }
  function startNewChat() {
    if (bindingBusyRef.current) { notify('请先停止当前任务或终端命令'); return; }
    if (pendingTimerRef.current !== null) clearTimeout(pendingTimerRef.current);
    pendingTimerRef.current = null; pendingEventsRef.current = [];
    const selected = config.connections.find(c => c.id === config.selectedConnectionId);
    metricBindingRef.current = metricKey(undefined, selected?.id, selected?.contextLength);
    resetPerformance(selected?.contextLength || 8192);
    chatFollowRef.current = true; chatResetFollowRef.current = true; setChatAtBottom(true); historyAnchorRef.current = null;
    setCurrent(null); setActivities([]); setHistoryStart(0); setStatus('就绪'); inputRef.current?.focus();
  }
  async function openSession(session: Session) {
    if (bindingBusyRef.current) { notify('请先停止当前任务或终端命令'); return; }
    if (file && fileText !== file.text && (session.workspace !== workspace || session.deviceId !== deviceId)) { notify('请先保存编辑内容，再切换会话'); return; }
    bindingBusyRef.current = true; setProjectBusy(true);
    try {
      const result = await api<{ events: { kind: string; payload: Json }[] }>('load_session', { sessionId: session.id });
      const list: Activity[] = [];
      const selected = config.connections.find(c => c.id === session.connectionId);
      let restoredUsage = initialUsage(selected?.contextLength || 8192);
      let restoredMetrics: ModelMetrics | null = null;
      let restoredWait: number | null = null;
      let restoredCompaction: CompactionInfo = { count: 0 };
      for (const event of result.events) {
        const p = event.payload;
        if (event.kind === 'activity' && (p.type === 'context_usage' || p.type === 'usage')) restoredUsage = applyUsage(restoredUsage, p);
        if (event.kind === 'activity' && p.type === 'model_metrics') { restoredMetrics = readMetrics(p); restoredWait = restoredMetrics.queueWaitMs ?? restoredWait; if (restoredMetrics.usageMeasured) restoredUsage = applyUsage(restoredUsage, { type: 'usage', inputTokens: restoredMetrics.inputTokens, outputTokens: restoredMetrics.outputTokens, isEstimated: false }); }
        if (event.kind === 'activity' && p.type === 'inference_acquired') restoredWait = numberValue(p.waitMs) ?? restoredWait;
        if (event.kind === 'context_compacted') restoredCompaction = { count: restoredCompaction.count + 1, method: String(p.method || ''), savedTokens: savedTokens(p) };
        if (event.kind === 'message' && (p.role === 'user' || p.role === 'assistant')) {
          const text = getText(p.content); if (text) list.push({ id: crypto.randomUUID(), role: p.role, text });
        } else if (event.kind === 'activity' && p.type === 'assistant_partial') {
          list.push({ id: crypto.randomUUID(), role: 'assistant', text: `${String(p.text || '')}\n\n[生成未完成]` });
        } else if (event.kind === 'activity' && p.type === 'tool_batch_rejected') {
          list.push({ id: crypto.randomUUID(), role: 'status', text: `工具调用未执行：${String(p.message || '')}` });
        } else if (event.kind === 'activity' && p.type === 'model_retry') {
          list.push({ id: crypto.randomUUID(), role: 'status', text: modelRetryText(p) });
        } else if (event.kind === 'activity' && p.type === 'capability_warning') {
          list.push({ id: crypto.randomUUID(), role: 'status', text: String(p.message || '') });
        } else if (event.kind === 'activity' && p.type === 'manual_file_restore_started') {
          list.push({ id: crypto.randomUUID(), role: 'status', text: `开始恢复 ${String(p.path || '')}；请以完成记录和实际文件为准。` });
        } else if (event.kind === 'activity' && p.type === 'manual_file_restore_completed') {
          list.push({ id: crypto.randomUUID(), role: 'status', text: `已恢复 ${String(p.path || '')}` });
        } else if (event.kind === 'activity' && p.type === 'manual_file_restore_failed') {
          list.push({ id: crypto.randomUUID(), role: 'status', text: `恢复未完成：${String(p.message || '')}` });
        } else if (event.kind === 'activity' && p.type === 'tool_started') {
          list.push({ id: String(p.callId), role: 'tool', name: String(p.name), text: '', args: String(p.arguments), running: true, deviceId: String(p.deviceId) });
        } else if (event.kind === 'activity' && p.type === 'tool_completed') {
          const item = list.find(i => i.id === p.callId); if (item) { item.running = false; item.result = p.result; item.durationMs = Number(p.durationMs); }
        } else if (event.kind === 'activity' && p.type === 'tool_output') {
          const item = list.find(i => i.id === p.callId); if (item) item.text += String(p.text || '');
        } else if (event.kind === 'activity' && p.type === 'error') list.push({ id: crypto.randomUUID(), role: 'status', text: String(p.message) });
      }
      list.forEach(item => { if (item.running) { item.running = false; item.result = { status: '结果待确认，请检查实际状态' }; } });
      metricBindingRef.current = metricKey(session.id, selected?.id, selected?.contextLength);
      setUsage(restoredUsage); setMetrics(restoredMetrics); setQueueSince(null); setQueueWaitMs(restoredWait); setCompaction(restoredCompaction);
      chatFollowRef.current = true; chatResetFollowRef.current = true; setChatAtBottom(true); historyAnchorRef.current = null;
      setHistoryStart(Math.max(0, list.length - HISTORY_WINDOW));
      setActivities(list); setCurrent(session); setFile(null); setFiles(null); setStatus('就绪');
    } catch (error) { notify(error); }
    finally { bindingBusyRef.current = false; setProjectBusy(false); }
  }
  const checkOperation = useCallback((itemId: string, operationId: string) => {
    void api('remote_operation_status', { sessionId: current?.id, operationId }).then(result => setActivities(old => old.map(item => item.id === itemId ? { ...item, result: { ...result, indeterminate: result.state === 'indeterminate' || result.state === 'dispatching' || result.state === 'accepted' } } : item))).catch(notify);
  }, [current?.id, notify]);
  const restoreDeleted = useCallback((callId: string) => {
    if (!current?.id || bindingBusyRef.current) return;
    bindingBusyRef.current = true; setRestoreBusy(true);
    void api('restore_deleted_file', { sessionId: current.id, callId }).then(result => {
      if (result.restored !== true) throw new Error('没有收到恢复成功的结果，请检查文件状态');
      setFiles(null); setActivities(old => [...old, { id: crypto.randomUUID(), role: 'status', text: `已恢复 ${String(result.path || '')}` }]);
      if (result.historyWarning) notify(result.historyWarning);
    }).catch(notify).finally(() => { bindingBusyRef.current = false; setRestoreBusy(false); });
  }, [current?.id, notify]);
  function applyEvent(event: Json) {
    const type = String(event.type);
    if (type === 'text_delta' || type === 'reasoning_delta') {
      setActivities(old => {
        const list = [...old]; let last = list.at(-1);
        if (!last || last.role !== 'assistant' || !last.running) { last = { id: crypto.randomUUID(), role: 'assistant', text: '', running: true }; list.push(last); }
        list[list.length - 1] = type === 'text_delta' ? { ...last, text: last.text + String(event.text) } : { ...last, reasoning: (last.reasoning || '') + String(event.text) };
        return list;
      }); setStatus(type === 'reasoning_delta' ? '思考中' : '生成中');
    } else if (type === 'assistant_completed') setActivities(old => old.map(item => item.role === 'assistant' && item.running ? { ...item, running: false } : item));
    else if (type === 'tool_started') {
      setStatus(`正在执行 ${String(event.name)}`);
      setActivities(old => [...old, { id: String(event.callId), role: 'tool', name: String(event.name), text: '', args: String(event.arguments), deviceId: String(event.deviceId), running: true }]);
    } else if (type === 'tool_output') {
      setActivities(old => old.map(item => item.id === event.callId ? { ...item, text: item.text + String(event.text || '') } : item));
      setTerminal(old => (old ?? '') + String(event.text || ''));
    } else if (type === 'tool_completed') {
      setActivities(old => old.map(item => item.id === event.callId ? { ...item, running: false, result: event.result, durationMs: Number(event.durationMs) } : item));
      if (String(event.name).startsWith('file_')) setFiles(null);
      if (event.name === 'terminal_run') setTerminal(old => (old ?? '') + '\n[命令结束，详细结果保存在工具卡片]\n');
    } else if (type === 'context_usage' || type === 'usage') {
      setUsage(u => applyUsage(u, event));
      if (type === 'context_usage') setStatus('准备模型响应');
    }
    else if (type === 'inference_queued') { setQueueSince(Date.now()); setQueueWaitMs(null); setStatus('排队等待推理资源'); }
    else if (type === 'inference_acquired') { setQueueSince(null); setQueueWaitMs(numberValue(event.waitMs) ?? null); setStatus('准备模型响应'); }
    else if (type === 'model_metrics') { const measured = readMetrics(event); setMetrics(measured); setQueueWaitMs(old => measured.queueWaitMs ?? old); }
    else if (type === 'compaction_started') setStatus('正在压缩上下文');
    else if (type === 'tool_batch_rejected') { setStatus('正在纠正工具参数'); setActivities(old => [...old, { id: crypto.randomUUID(), role: 'status', text: `工具调用未执行：${String(event.message || '')}` }]); }
    else if (type === 'model_retry') { const empty = event.reason === 'empty_final_after_tool'; setStatus(empty ? '正在整理工具结果' : '正在增加思考预算并重试'); setActivities(old => [...old.map(item => item.role === 'assistant' && item.running ? { ...item, running: false } : item), { id: crypto.randomUUID(), role: 'status', text: modelRetryText(event) }]); }
    else if (type === 'capability_warning') { if (event.capability === 'tool_not_run') unverifiedToolRef.current = true; setActivities(old => [...old, { id: crypto.randomUUID(), role: 'status', text: String(event.message || '') }]); }
    else if (type === 'compaction_completed') { setStatus('上下文已压缩'); setCompaction(old => ({ count: old.count + 1, method: String(event.method || event.reason || ''), savedTokens: savedTokens(event) })); setActivities(old => [...old, { id: crypto.randomUUID(), role: 'status', text: `已整理上下文${savedTokens(event) !== undefined ? ` · 节省约 ${savedTokens(event)!.toLocaleString()} token` : ''} · 完整历史保留在本地` }]); }
    else if (type === 'error') { notify(event.message); setActivities(old => [...old, { id: crypto.randomUUID(), role: 'status', text: String(event.message) }]); }
    else if (type === 'status') setStatus(String(event.text));
    else if (type === 'turn_completed') { setRunning(false); setQueueSince(null); setStatus(event.status === 'completed' ? (unverifiedToolRef.current ? '需要核对' : '回复已结束') : event.status === 'interrupted' ? '已停止' : '执行失败'); }
  }
  function flushStreamEvents() {
    if (pendingTimerRef.current !== null) clearTimeout(pendingTimerRef.current);
    pendingTimerRef.current = null;
    const events = pendingEventsRef.current; pendingEventsRef.current = [];
    for (const event of events) applyEvent(event);
  }
  function onEvent(event: Json) {
    if (event.type === 'text_delta' || event.type === 'reasoning_delta' || event.type === 'tool_output') {
      const last = pendingEventsRef.current.at(-1);
      if (last?.type === event.type && last.callId === event.callId && last.stream === event.stream) last.text = String(last.text || '') + String(event.text || '');
      else pendingEventsRef.current.push({ ...event });
      if (pendingTimerRef.current === null) pendingTimerRef.current = setTimeout(flushStreamEvents, 32);
      return;
    }
    flushStreamEvents(); applyEvent(event);
  }
  async function addImages(files: File[]) {
    if (!files.length) return;
    const binding = inspectorBindingRef.current;
    if (attachments.length + files.length > 2 || attachments.reduce((n, file) => n + file.size, 0) + files.reduce((n, file) => n + file.size, 0) > 8 * 1024 * 1024 || files.some(file => !['image/png', 'image/jpeg'].includes(file.type))) {
      notify('最多选择两张 PNG/JPEG 图片，总大小不超过 8 MiB。'); return;
    }
    try {
      const added = await Promise.all(files.map(file => new Promise<{ name: string; size: number; dataUrl: string }>((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => typeof reader.result === 'string' ? resolve({ name: file.name, size: file.size, dataUrl: reader.result }) : reject(new Error('无法读取图片'));
        reader.onerror = () => reject(new Error('无法读取图片'));
        reader.readAsDataURL(file);
      })));
      if (binding !== inspectorBindingRef.current) return;
      setAttachments(old => old.length + added.length <= 2 && old.reduce((n, file) => n + file.size, 0) + added.reduce((n, file) => n + file.size, 0) <= 8 * 1024 * 1024 ? [...old, ...added] : old);
    } catch (error) { notify(error); }
  }
  async function send() {
    if ((!input.trim() && attachments.length === 0) || bindingBusyRef.current) return;
    if (!desktop) { notify('当前为界面预览。请启动 Gemma Harness 桌面应用来连接模型与执行工具。'); return; }
    if (!workspace) { notify('请先选择项目目录'); return; }
    if (!connection || !modelId) { setSettings('models'); notify('请先选择推理服务中的模型'); return; }
    bindingBusyRef.current = true;
    const text = input; const images = attachments; unverifiedToolRef.current = false; resetPerformance(); chatFollowRef.current = true; chatResetFollowRef.current = true; setChatAtBottom(true); historyAnchorRef.current = null; setHistoryStart(Math.max(0, activities.length - HISTORY_WINDOW)); setInput(''); setAttachments([]); setRunning(true); setStatus('正在连接');
    try {
      let session = current;
      if (!session) { session = await api<Session>('create_session'); metricBindingRef.current = metricKey(session.id, connection.id, connection.contextLength); setCurrent(session); }
      setActivities(old => [...old, { id: crypto.randomUUID(), role: 'user', text: [text, ...images.map(image => `[图片：${image.name}]`)].filter(Boolean).join('\n') }]);
      await stream(session.id, text, onEvent, images.map(image => image.dataUrl));
    } catch (error) { notify(error); setStatus('执行失败'); }
    finally { bindingBusyRef.current = false; flushStreamEvents(); setRunning(false); setQueueSince(null); setActivities(old => old.map(item => item.running ? { ...item, running: false } : item)); void refresh(); }
  }
  async function stop() { if (current) { setStatus('正在停止'); try { await api('interrupt', { sessionId: current.id }); } catch (error) { notify(error); } } }
  async function inspect(kind: string, path?: string) {
    const binding = inspectorBindingRef.current;
    setPanel(kind); setPanelOpen(true);
    if (!workspace || !desktop) return;
    try {
      const result = await api('execute_tool', { sessionId: current?.id, name: kind === 'diff' ? 'git_diff' : kind === 'desktop' ? `${computerKind}_observe` : 'file_list', args: kind === 'files' ? { path: path || '.', limit: 150 } : {} });
      if (binding !== inspectorBindingRef.current) return;
      if (kind === 'files') setFiles(result); else setInspection(pretty(result));
    } catch (error) { if (binding === inspectorBindingRef.current) notify(error); }
  }
  async function capture() {
    const binding = inspectorBindingRef.current;
    try {
      const result = await api('execute_tool', { sessionId: current?.id, name: `${computerKind}_screenshot`, args: {} });
      if (binding !== inspectorBindingRef.current) return;
      const parts = Array.isArray(result.content) ? result.content as Json[] : [];
      const image = parts.find(p => p.type === 'image' && typeof p.data === 'string');
      if (image) setPreviewImage(`data:${String(image.mimeType)};base64,${String(image.data)}`);
      setInspection(pretty({ ...result, content: parts.map(p => p.type === 'image' ? { ...p, data: '[图像已展示]' } : p) }));
    } catch (error) { if (binding === inspectorBindingRef.current) notify(error); }
  }
  async function readFile(path: string) {
    const binding = inspectorBindingRef.current;
    try {
      const result = await api('execute_tool', { sessionId: current?.id, name: 'file_read', args: { path, raw: true } });
      if (binding !== inspectorBindingRef.current) return;
      const text = String(result.content ?? result.text ?? ''); const hash = String(result.sha256 ?? '');
      setFile({ path, text, hash }); setFileText(text);
    } catch (error) { if (binding === inspectorBindingRef.current) notify(error); }
  }
  async function saveFile() {
    if (!file) return;
    const binding = inspectorBindingRef.current;
    try {
      await api('execute_tool', { sessionId: current?.id, name: 'file_write', args: { path: file.path, content: fileText, expected_sha256: file.hash } });
      if (binding !== inspectorBindingRef.current) return;
      await readFile(file.path);
      if (binding === inspectorBindingRef.current) notify('文件已保存');
    } catch (error) { notify(error); }
  }
  async function runCommand() {
    if (!command.trim() || bindingBusyRef.current) return;
    terminalFollowRef.current = true;
    if (command.trim() === 'clear') {
      setCommand(''); setTerminal('');
      return;
    }
    bindingBusyRef.current = true;
    const operationId = crypto.randomUUID(); setTerminalOperation(operationId); setTerminalBusy(true);
    const cmd = command; setCommand(''); setTerminal(t => (t ?? '') + `\n$ ${cmd}\n`);
    try {
      let truncated = false;
      const result = await streamTool({ operationId, sessionId: current?.id, name: 'terminal_run', args: { command: cmd, timeout_ms: 600000 } }, event => {
        if (event.type !== 'tool_output') return;
        let text = typeof event.text === 'string' ? event.text : '';
        if (event.truncated === true) {
          text = text.replace(liveTruncationNotice, '');
          if (!truncated) {
            truncated = true;
            setTerminal(t => {
              const output = (t ?? '') + text;
              return output + `${output.endsWith('\n') ? '' : '\n'}[实时输出已截断；输出文件每个流最多保留 16 MiB。]\n`;
            });
            return;
          }
        }
        if (text) setTerminal(t => (t ?? '') + text);
      });
      const notices = terminalResultNotices(result, truncated);
      if (notices.length) setTerminal(t => (t ?? '') + `${!t || t.endsWith('\n') ? '' : '\n'}${notices.join('\n')}\n`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setTerminal(t => (t ?? '') + `${!t || t.endsWith('\n') ? '' : '\n'}[终端错误] ${message}\n`);
    }
    finally { bindingBusyRef.current = false; setTerminalBusy(false); setTerminalOperation(''); }
  }

  function beginUpdateInstall(fromSettings: boolean): string | null {
    if (bindingBusyRef.current) return '请先停止当前任务或终端命令，再安装更新。';
    if (file && fileText !== file.text) return '请先保存正在编辑的文件，再安装更新。';
    if (input || attachments.length || command) return '请先发送或清空未提交的消息、图片和终端命令。';
    if (projectDialog || (settings && !fromSettings)) return '请先关闭设置或项目操作，再安装更新。';
    if (settings && settingsDirtyRef.current) return '请先保存或取消未完成的设置，再安装更新。';
    if (settings && fromSettings) setSettings(null);
    bindingBusyRef.current = true; setUpdateInstalling(true); return null;
  }
  function endUpdateInstall() {
    bindingBusyRef.current = false; setUpdateInstalling(false);
  }
  const update = useAppUpdates(beginUpdateInstall, endUpdateInstall);

  const entries = Array.isArray(files?.entries) ? files.entries as Json[] : [];
  return <div className="app-shell">
    <aside className="sidebar" inert={updateInstalling || undefined}>
      <div className="traffic-space" data-tauri-drag-region />
      <div className="brand"><Mark small /><span>harness</span></div>
      <button className="new-chat" onClick={startNewChat}><Plus size={17} /> 新会话 <kbd>{shortcut} N</kbd></button>
      <button className="nav-item" onClick={() => setSearchOpen(!searchOpen)}><Search size={16} /> 搜索会话 <span>{shortcut} K</span></button>
      {searchOpen && <input className="session-search" autoFocus placeholder="按标题查找…" value={filter} onChange={e => setFilter(e.target.value)} />}
      <div className="sidebar-section"><span>项目</span><IconButton label="打开项目" onClick={selectWorkspace}><Plus size={14} /></IconButton></div>
      {projects !== null ? <ProjectList projects={projects} workspace={workspace} deviceId={deviceId} disabled={running || terminalBusy || projectBusy} onOpen={project => void openProject(project)} onManage={(project, action) => setProjectDialog({ project, action })} /> : workspace && <button className="project-item selected" onClick={selectWorkspace}><FolderOpen size={17} /><span>{workspaceName}</span><ChevronDown size={13} /></button>}
      {workspace && <div className="workspace-path" title={workspace}>{workspace}</div>}
      <div className="sidebar-section sessions-heading"><span>最近会话</span><MoreHorizontal size={15} /></div>
      <div className="session-list">{sessions.filter(s => s.title.toLowerCase().includes(filter.toLowerCase())).map(session => <button key={session.id} className={`session-item ${current?.id === session.id ? 'selected' : ''}`} onClick={() => void openSession(session)}><Circle size={7} fill="currentColor" /><span>{session.title}</span>{session.deviceId !== 'local' && <Monitor size={12} />}</button>)}</div>
      <div className="sidebar-bottom"><button className="device-button" onClick={() => setSettings('devices')}><span className="device-icon"><Monitor size={16} /></span><span><strong>{deviceName}执行</strong><small>{deviceId === 'local' ? '文件、终端与桌面' : '已配对的执行设备'}</small></span><ChevronDown size={13} /></button><button className="nav-item settings-nav" onClick={() => setSettings('models')}><Settings2 size={16} /> 设置与连接 <span>{shortcut} ,</span></button></div>
    </aside>
    <main className="main-area" inert={updateInstalling || undefined}>
      <header className="topbar"><div className="breadcrumb"><Menu size={16} /><span>{workspace ? workspaceName : '工作台'}</span><ChevronRight size={13} /><strong>{current?.title || '新会话'}</strong></div><div className="topbar-actions"><IconButton label={panelOpen ? '隐藏详情' : '显示详情'} onClick={() => setPanelOpen(!panelOpen)} active={panelOpen}><PanelRightClose size={17} /></IconButton></div></header>
      {!desktop && <div className="preview-banner"><Monitor size={13} /> 桌面界面预览 <span>模型与系统工具在桌面应用中连接</span></div>}
      <div className="work-area"><section className="conversation">
        <div className="conversation-scroll" ref={scrollRef} onScroll={event => {
          const viewport = event.currentTarget; const nearBottom = viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop <= 64;
          if (nearBottom || viewport.scrollTop !== chatScrollTopRef.current) { chatFollowRef.current = nearBottom; setChatAtBottom(nearBottom); }
          chatScrollTopRef.current = viewport.scrollTop;
        }}>{activities.length === 0 ? <div className="welcome">
          <div className="welcome-emblem"><Mark /></div><div className="eyebrow">YOUR MODEL. YOUR WORKSPACE.</div><h1>让本地模型，<br /><span>真正开始工作。</span></h1><p className="welcome-description">为 Gemma 构建的编程与电脑操作工作台。<br />连接你的模型，把想法交给工具。</p>
          <div className="setup-path"><button className={modelId ? 'done' : ''} onClick={() => setSettings('models')}><span>{modelId ? <Check size={12} /> : '1'}</span>配置模型<ArrowUpRight size={14} /></button><i /><button className={workspace ? 'done' : ''} onClick={selectWorkspace}><span>{workspace ? <Check size={12} /> : '2'}</span>选择项目<ArrowUpRight size={14} /></button></div>
          <div className="starter-grid">{[
            { icon: <Code2 size={19} />, title: '理解与修改代码', sub: '检查项目、编辑文件、运行测试', prompt: '请先检查这个项目的结构和说明，概括当前架构，再给出最值得改进的地方。' },
            { icon: <Globe2 size={19} />, title: '操作浏览器与桌面', sub: '观察界面、执行动作、验证结果', prompt: '请检查当前设备的 Computer Use 能力和权限，先观察状态，暂时不要执行点击。' },
          ].map(card => <button className="starter-card" key={card.title} onClick={() => { setInput(card.prompt); inputRef.current?.focus(); }}><span className="starter-icon">{card.icon}</span><strong>{card.title}</strong><small>{card.sub}</small><ArrowUpRight size={15} /></button>)}</div>
          <div className="local-note"><ShieldCheck size={13} /> 本地保存会话 · 自主选择推理与执行位置</div>
        </div>  : <div className="messages">{historyStart > 0 && <button className="load-history" aria-label="加载更早记录" onClick={() => {
          const viewport = scrollRef.current; if (viewport) historyAnchorRef.current = { height: viewport.scrollHeight, top: viewport.scrollTop };
          chatFollowRef.current = false; setChatAtBottom(false); setHistoryStart(start => Math.max(0, start - HISTORY_WINDOW));
        }}>加载更早记录 · {historyStart} 条</button>}{activities.slice(historyStart).map(item => <ActivityCard item={item} onCheckOperation={checkOperation} onRestoreDeleted={restoreDeleted} restoreDisabled={running || terminalBusy || projectBusy || restoreBusy} key={item.id} />)}</div>}</div>
        {!chatAtBottom && <button className="jump-latest" onClick={() => { chatFollowRef.current = true; setChatAtBottom(true); const viewport = scrollRef.current; if (viewport) { viewport.scrollTop = viewport.scrollHeight; chatScrollTopRef.current = viewport.scrollTop; } }}><ArrowDown size={13} />回到最新</button>}
        <div className="composer-area"><div className={`composer ${running ? 'is-running' : ''}`}><textarea ref={inputRef} aria-label="消息" placeholder={workspace ? `向 Gemma 描述任务，工作目录：${basename(workspace)}…` : '描述你的任务，或先连接模型、选择项目…'} value={input} onChange={e => setInput(e.target.value)} onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(); } }} rows={3} />{attachments.length > 0 && <div className="composer-attachments">{attachments.map((image, i) => <span key={`${image.name}-${i}`}>{image.name}<button aria-label={`移除图片 ${image.name}`} onClick={() => setAttachments(old => old.filter((_, index) => index !== i))}><X size={12} /></button></span>)}</div>}<div className="composer-toolbar"><div className="composer-actions"><button onClick={selectWorkspace}><Plus size={16} /><span>项目上下文</span></button><button disabled={running} onClick={() => imageInputRef.current?.click()}><ImagePlus size={15} /><span>添加图片</span></button><input ref={imageInputRef} type="file" accept="image/png,image/jpeg" multiple hidden aria-label="选择图片" onChange={e => { const files = Array.from(e.currentTarget.files || []); e.currentTarget.value = ''; void addImages(files); }} /></div><div className="composer-meta"><span className={`composer-status ${status === '就绪' ? 'is-idle' : ''}`}>{running && <Loader2 size={11} className="spin" />}{status === '就绪' ? <><Code2 size={13} /> 编程与电脑操作</> : <><span role="status">{status}</span><QueueElapsed since={queueSince} /></>}</span>{running ? <button className="send-button stop" aria-label="停止" onClick={() => void stop()}><Square size={14} fill="currentColor" /></button> : <button className="send-button" aria-label="发送" disabled={(!input.trim() && attachments.length === 0) || terminalBusy || projectBusy} onClick={() => void send()}><ArrowUp size={19} /></button>}</div></div></div></div>
      </section>
      {panelOpen && <aside className="inspector"><div className="inspector-tabs">{[{ id: 'files', icon: <Folder size={14} />, label: '文件' }, { id: 'diff', icon: <FileDiff size={14} />, label: '差异' }, { id: 'desktop', icon: <Monitor size={14} />, label: '电脑' }].map(tab => <button className={panel === tab.id ? 'selected' : ''} key={tab.id} onClick={() => void inspect(tab.id)}>{tab.icon}{tab.label}</button>)}</div><div className="inspector-body">{panel === 'files' ? <><div className="inspector-heading"><span>{workspace ? workspaceName : '项目文件'}</span><IconButton label="刷新文件" onClick={() => void inspect('files')}><MoreHorizontal size={15} /></IconButton></div>{file ? <div className="file-editor"><div><button onClick={() => setFile(null)}>← 文件</button><span title={file.path}>{basename(file.path)}</span><button disabled={fileText === file.text} onClick={() => void saveFile()}>保存</button></div><textarea aria-label="文件编辑器" spellCheck={false} value={fileText} onChange={e => setFileText(e.target.value)} /></div> : entries.length ? <div className="file-list">{entries.map(entry => <button key={String(entry.path || entry.name)} onClick={() => entry.directory || entry.is_dir || entry.isDir || entry.kind === 'directory' ? void inspect('files', String(entry.path)) : void readFile(String(entry.path || entry.name))}>{entry.directory || entry.is_dir || entry.isDir || entry.kind === 'directory' ? <Folder size={14} /> : <FileCode2 size={14} />}<span>{String(entry.name || entry.path)}</span></button>)}</div> : <div className="inspector-empty"><FolderOpen size={28} strokeWidth={1.2} /><p>{workspace ? '浏览项目文件' : '打开你的项目'}</p><small>{workspace ? '读取当前执行设备上的目录' : '文件、终端与差异将在这里呈现'}</small><button onClick={() => workspace ? void inspect('files') : void selectWorkspace()}>{workspace ? '加载文件' : '选择文件夹'}<ArrowUpRight size={13} /></button></div>}</> : <><div className="inspector-heading"><span>{panel === 'diff' ? '工作区差异' : '当前设备观测'}</span><IconButton label="刷新" onClick={() => void inspect(panel)}><MoreHorizontal size={15} /></IconButton></div>{panel === 'desktop' && <div className="computer-controls"><select aria-label="电脑控制类型" value={computerKind} onChange={e => { setComputerKind(e.target.value); setInspection(''); setPreviewImage(''); }}><option value="desktop">原生桌面</option><option value="browser">浏览器</option></select><button onClick={() => void inspect('desktop')}>观测</button><button onClick={() => void capture()}>截图</button></div>}{previewImage && panel === 'desktop' && <img className="computer-preview" src={previewImage} alt="当前执行设备的真实截图" />}{inspection ? <pre className="inspection-content">{inspection}</pre> : <div className="inspector-empty">{panel === 'diff' ? <FileDiff size={28} /> : <Monitor size={28} />}<p>{panel === 'diff' ? '修改清晰可见' : '先观察，再操作'}</p><small>{panel === 'diff' ? '选择项目后查看真实 Git 差异' : '在工具设置中启用桌面控制'}</small><button onClick={() => panel === 'desktop' ? setSettings('tools') : void inspect('diff')}>{panel === 'desktop' ? '检查工具设置' : '查看差异'}<ArrowUpRight size={13} /></button></div>}</>}
      <div className="model-info"><div className="tiny-label">模型状态</div><div><Cpu size={15} /><strong>{modelId || '尚未配置模型'}</strong></div><p>{modelId ? `连接：${connection?.name}` : '配置模型后可在这里查看模型 ID'}</p>{modelId && <span>已配置 · 当前连接状态未知</span>}<button onClick={() => setSettings('models')}>{modelId ? '查看模型设置' : '配置模型'} <ArrowUpRight size={12} /></button></div></div></aside>}
      </div>
      {terminalOpen && <section className="terminal-panel"><div className="terminal-heading"><span><Terminal size={13} /> 终端 · {deviceName}</span><IconButton label="关闭终端" onClick={() => setTerminalOpen(false)}><X size={14} /></IconButton></div><pre ref={terminalOutputRef} onScroll={event => {
        const output = event.currentTarget;
        const nearBottom = output.scrollHeight - output.clientHeight - output.scrollTop <= 24;
        // A queued programmatic scroll can arrive after more text was appended.
        // Only a changed scroll position should count as scrolling away.
        if (nearBottom || output.scrollTop !== terminalScrollTopRef.current) terminalFollowRef.current = nearBottom;
        terminalScrollTopRef.current = output.scrollTop;
      }}>{terminal ?? '命令将在当前会话绑定的执行设备上运行。'}</pre><form onSubmit={e => { e.preventDefault(); void runCommand(); }}><span>❯</span><input aria-label="终端命令" placeholder={workspace ? '输入命令…' : '请先选择项目'} value={command} onChange={e => setCommand(e.target.value)} disabled={!workspace || terminalBusy || running || projectBusy} />{terminalBusy && <button type="button" className="terminal-stop" aria-label="停止终端命令" onClick={() => void api('interrupt', { sessionId: terminalOperation }).catch(notify)}><Square size={12} fill="currentColor" />停止</button>}</form></section>}
      {performanceOpen && <PerformancePanel usage={usage} metrics={metrics} queueSince={queueSince} queueWaitMs={queueWaitMs} status={status} compaction={compaction} onClose={() => setPerformanceOpen(false)} />}
      <footer className="statusbar"><div><button onClick={() => setTerminalOpen(!terminalOpen)}><Terminal size={13} />终端</button><span className="statusbar-divider" /><span><HardDrive size={12} />{deviceName}</span><span className="statusbar-connection"><span className={`status-dot ${modelId ? 'configured' : ''}`} />{modelId ? '已配置模型' : '模型未配置'}</span></div><div><button className="performance-toggle" aria-label="推理性能与上下文" aria-expanded={performanceOpen} onClick={() => setPerformanceOpen(open => !open)} title={usage.budget ? `输入预算 ${usage.budget.toLocaleString()}；实际加载上下文 ${usage.context.toLocaleString()}` : '查看上下文预算、真实用量与最近请求性能'}><span className="context-track"><i style={{ width: `${Math.min(100, usage.input / (usage.budget || usage.context) * 100)}%` }} /></span>{usage.estimated ? '约 ' : ''}{usage.input.toLocaleString()} / {usage.budget ? `${usage.budget.toLocaleString()} 输入` : `${(usage.context / 1024).toFixed(0)}K`}<ChevronDown size={10} /></button><span className="statusbar-divider" />{desktop && <UpdateControl update={update} />}<span>Gemma Harness</span></div></footer>
    </main>
    {updateInstalling && <div className="update-lock" role="status" aria-live="polite"><Loader2 size={18} className="spin" />正在安装更新，应用即将重启…</div>}
    {settings && <Settings initialTab={settings} config={config} update={update} onDirtyChange={recordSettingsDirty} onClose={() => setSettings(null)} onSave={(next, savedAppearanceOnly) => { const appearanceOnly = savedAppearanceOnly || withoutAppearance(configRef.current) === withoutAppearance(next); setConfig(next); if (appearanceOnly) { void refresh(); return; } const selected = next.connections.find(c => c.id === next.selectedConnectionId); metricBindingRef.current = metricKey(undefined, selected?.id, selected?.contextLength); resetPerformance(selected?.contextLength || 8192); setConfig(next); setCurrent(null); setActivities([]); setHistoryStart(0); chatFollowRef.current = true; setChatAtBottom(true); setFiles(null); void refresh(); }} onError={notify} />}
    {projectDialog && <ProjectDialog key={`${projectDialog.project.id}:${projectDialog.action}`} project={projectDialog.project} action={projectDialog.action} onClose={() => setProjectDialog(null)} onConfirm={manageProject} />}
    {toast && <div role="alert" className="toast"><Unplug size={16} /><span>{toast}</span><button aria-label="关闭提示" onClick={() => setToast('')}><X size={14} /></button></div>}
  </div>;
}
