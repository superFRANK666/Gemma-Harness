import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ChevronRight, Menu, Monitor, PanelRightClose, Unplug, Loader2, HardDrive, ChevronDown, Terminal, X } from 'lucide-react';
import { api, choosePath, desktop, pretty, stream, streamTool } from './bridge';
import { defaultConfig } from './types';
import type { Activity, Config, ImageAttachment, Json, Project, Session, Snapshot } from './types';
import { applyAppearance } from './appearance';
import ActivityCard from './components/ActivityCard';
import Composer from './components/Composer';
import InspectorPanel from './components/InspectorPanel';
import type { OpenFileState } from './components/InspectorPanel';
import { IconButton } from './components/Mark';
import PerformancePanel, { applyUsage, initialUsage, numberValue, readMetrics } from './components/PerformancePanel';
import type { CompactionInfo, ModelMetrics } from './components/PerformancePanel';
import { ProjectDialog } from './components/Projects';
import Settings from './components/Settings';
import Sidebar, { SessionRenameDialog } from './components/Sidebar';
import TerminalPanel from './components/TerminalPanel';
import UpdateControl, { useAppUpdates } from './components/UpdateControl';
import WelcomeView from './components/WelcomeView';
import {
  basename,
  buildActivitiesFromEvents,
  findLastToolIndex,
  HISTORY_WINDOW,
  LIVE_TRUNCATION_NOTICE,
  metricKey,
  modelRetryText,
  savedTokens,
  terminalResultNotices,
} from './utils/sessionEvents';

const withoutAppearance = (config: Config) => {
  const copy = { ...config };
  delete copy.appearance;
  return JSON.stringify(copy);
};

export default function App() {
  const [config, setConfig] = useState<Config>(structuredClone(defaultConfig));
  const [sessions, setSessions] = useState<Session[]>([]);
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [projectBusy, setProjectBusy] = useState(false);
  const [restoreBusy, setRestoreBusy] = useState(false);
  const [projectDialog, setProjectDialog] = useState<{
    project: Project;
    action: 'rename' | 'remove';
  } | null>(null);
  const [renamingSession, setRenamingSession] = useState<Session | null>(null);
  const [current, setCurrent] = useState<Session | null>(null);
  const [activities, setActivities] = useState<Activity[]>([]);
  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<ImageAttachment[]>([]);
  const [running, setRunning] = useState(false);
  const [settings, setSettings] = useState<string | null>(null);
  const [toast, setToast] = useState('');
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [panel, setPanel] = useState('files');
  const [panelOpen, setPanelOpen] = useState(true);
  const [currentDir, setCurrentDir] = useState('.');
  const [files, setFiles] = useState<Json | null>(null);
  const [file, setFile] = useState<OpenFileState | null>(null);
  const [fileText, setFileText] = useState('');
  const [diffResult, setDiffResult] = useState<Json | null>(null);
  const [inspection, setInspection] = useState('');
  const [inspecting, setInspecting] = useState(false);
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

  const scrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const unverifiedToolRef = useRef(false);
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
  const recordSettingsDirty = useCallback((dirty: boolean) => {
    settingsDirtyRef.current = dirty;
  }, []);

  bindingBusyRef.current =
    running || terminalBusy || projectBusy || restoreBusy || updateInstalling;

  const configRef = useRef(config);
  configRef.current = config;

  const connection = config.connections.find(
    c => c.id === (current?.connectionId || config.selectedConnectionId),
  );
  const modelId = connection?.model?.trim() || '';
  const workspace = current?.workspace || config.workspace;
  const deviceId = current?.deviceId || config.deviceId;
  const deviceName =
    deviceId === 'local'
      ? '本机'
      : config.devices.find(d => d.id === deviceId)?.name || '远端';
  const workspaceName =
    projects?.find(p => p.workspace === workspace && p.deviceId === deviceId)?.name ||
    basename(workspace);

  useLayoutEffect(() => {
    if (!settings) return applyAppearance(config.appearance);
  }, [config.appearance, settings]);

  const notify = useCallback(
    (error: unknown) => setToast(error instanceof Error ? error.message : String(error)),
    [],
  );

  const performanceBinding = metricKey(
    current?.id,
    connection?.id,
    connection?.contextLength,
  );

  function resetPerformance(context = connection?.contextLength || 8192) {
    setUsage(initialUsage(context));
    setMetrics(null);
    setQueueSince(null);
    setQueueWaitMs(null);
    setCompaction({ count: 0 });
  }

  useLayoutEffect(() => {
    if (metricBindingRef.current === performanceBinding) return;
    metricBindingRef.current = performanceBinding;
    resetPerformance();
  }, [performanceBinding]);

  useLayoutEffect(() => {
    inspectorBindingRef.current += 1;
    setCurrentDir('.');
    setFiles(null);
    setFile(null);
    setFileText('');
    setDiffResult(null);
    setInspection('');
    setPreviewImage('');
    setPanel('files');
    setAttachments([]);
  }, [workspace, deviceId]);

  async function refresh() {
    if (!desktop) return;
    try {
      const snapshot = await api<Snapshot>('snapshot');
      setConfig(snapshot.config);
      setSessions(snapshot.sessions);
      setProjects(snapshot.projects ?? null);
    } catch (error) {
      notify(error);
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(''), 7000);
    return () => clearTimeout(timer);
  }, [toast]);

  useLayoutEffect(() => {
    const output = scrollRef.current;
    if (!output) return;
    if (chatResetFollowRef.current) {
      chatResetFollowRef.current = false;
      chatFollowRef.current = true;
      output.scrollTop = output.scrollHeight;
      chatScrollTopRef.current = output.scrollTop;
      return;
    }
    const anchor = historyAnchorRef.current;
    if (anchor) {
      output.scrollTop = anchor.top + output.scrollHeight - anchor.height;
      chatScrollTopRef.current = output.scrollTop;
      historyAnchorRef.current = null;
      return;
    }
    if (!chatFollowRef.current) return;
    const frame = requestAnimationFrame(() => {
      if (chatFollowRef.current && scrollRef.current === output) {
        output.scrollTop = output.scrollHeight;
        chatScrollTopRef.current = output.scrollTop;
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [activities, historyStart]);

  useEffect(
    () => () => {
      if (pendingTimerRef.current !== null) clearTimeout(pendingTimerRef.current);
      pendingEventsRef.current = [];
    },
    [],
  );

  const startNewChat = useCallback(() => {
    if (bindingBusyRef.current) {
      notify('请先停止当前任务或终端命令');
      return;
    }
    if (pendingTimerRef.current !== null) clearTimeout(pendingTimerRef.current);
    pendingTimerRef.current = null;
    pendingEventsRef.current = [];
    const latestConfig = configRef.current;
    const selected = latestConfig.connections.find(
      c => c.id === latestConfig.selectedConnectionId,
    );
    metricBindingRef.current = metricKey(
      undefined,
      selected?.id,
      selected?.contextLength,
    );
    resetPerformance(selected?.contextLength || 8192);
    chatFollowRef.current = true;
    chatResetFollowRef.current = true;
    setChatAtBottom(true);
    historyAnchorRef.current = null;
    setCurrent(null);
    setActivities([]);
    setHistoryStart(0);
    setStatus('就绪');
    inputRef.current?.focus();
  }, [notify]);

  const toggleSearch = useCallback(() => {
    setSidebarOpen(true);
    setSearchOpen(open => {
      if (open) setFilter('');
      return !open;
    });
  }, []);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (settings || projectDialog || renamingSession) return;
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key === 'n') {
        event.preventDefault();
        startNewChat();
      } else if (key === 'k') {
        event.preventDefault();
        toggleSearch();
      } else if (key === 'b') {
        event.preventDefault();
        setSidebarOpen(v => !v);
      } else if (event.key === ',') {
        event.preventDefault();
        setSettings('models');
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [settings, projectDialog, renamingSession, startNewChat, toggleSearch]);

  function applyProjectSelection(result: { config: Config; projects: Project[] }) {
    setConfig(result.config);
    setProjects(result.projects);
    setCurrent(null);
    setActivities([]);
    setHistoryStart(0);
    setFiles(null);
    setDiffResult(null);
    setStatus('就绪');
    chatFollowRef.current = true;
    chatResetFollowRef.current = true;
    setChatAtBottom(true);
  }

  async function selectWorkspace() {
    if (bindingBusyRef.current) {
      notify('请先停止当前任务或终端命令，再切换项目');
      return;
    }
    if (file && fileText !== file.text) {
      notify('请先保存编辑内容，再切换项目');
      return;
    }
    bindingBusyRef.current = true;
    setProjectBusy(true);
    try {
      let selected: string | null;
      if (configRef.current.deviceId === 'local') selected = await choosePath(true);
      else {
        setSettings('devices');
        return;
      }
      if (selected) {
        const result = await api<{ config: Config; projects: Project[] }>(
          'open_project',
          { workspace: selected, deviceId: 'local' },
        );
        applyProjectSelection(result);
      }
    } catch (error) {
      notify(error);
    } finally {
      bindingBusyRef.current = false;
      setProjectBusy(false);
    }
  }

  async function openProject(project: Project) {
    if (bindingBusyRef.current) {
      notify('请先停止当前任务或终端命令，再切换项目');
      return;
    }
    if (
      file &&
      fileText !== file.text &&
      (project.workspace !== workspace || project.deviceId !== deviceId)
    ) {
      notify('请先保存编辑内容，再切换项目');
      return;
    }
    bindingBusyRef.current = true;
    setProjectBusy(true);
    try {
      applyProjectSelection(
        await api<{ config: Config; projects: Project[] }>('open_project', {
          projectId: project.id,
        }),
      );
    } catch (error) {
      notify(error);
    } finally {
      bindingBusyRef.current = false;
      setProjectBusy(false);
    }
  }

  async function manageProject(name: string) {
    if (!projectDialog) return;
    const { project, action } = projectDialog;
    if (action === 'remove' && bindingBusyRef.current) {
      throw new Error('请先停止当前任务或终端命令，再移除项目');
    }
    if (
      action === 'remove' &&
      project.workspace === workspace &&
      project.deviceId === deviceId &&
      file &&
      fileText !== file.text
    ) {
      throw new Error('请先保存编辑内容，再移除项目');
    }
    const result = await api<{ config: Config; projects: Project[] }>(
      action === 'rename' ? 'rename_project' : 'remove_project',
      { projectId: project.id, ...(action === 'rename' ? { name } : {}) },
    );
    setConfig(result.config);
    setProjects(result.projects);
    if (
      action === 'remove' &&
      project.workspace === workspace &&
      project.deviceId === deviceId
    ) {
      setCurrent(null);
      setActivities([]);
      setHistoryStart(0);
      setFiles(null);
      setDiffResult(null);
      setStatus('就绪');
    }
    notify(
      action === 'rename'
        ? '项目名称已更新'
        : '项目已从列表移除，文件和历史会话已保留',
    );
  }

  async function openSession(session: Session) {
    if (bindingBusyRef.current) {
      notify('请先停止当前任务或终端命令');
      return;
    }
    const switchingBinding =
      session.workspace !== workspace || session.deviceId !== deviceId;
    if (file && fileText !== file.text && switchingBinding) {
      notify('请先保存编辑内容，再切换会话');
      return;
    }
    bindingBusyRef.current = true;
    setProjectBusy(true);
    try {
      const result = await api<{ events: { kind: string; payload: Json }[] }>(
        'load_session',
        { sessionId: session.id },
      );
      const selected = config.connections.find(c => c.id === session.connectionId);
      const restored = buildActivitiesFromEvents(
        result.events,
        selected?.contextLength || 8192,
      );
      metricBindingRef.current = metricKey(
        session.id,
        selected?.id,
        selected?.contextLength,
      );
      setUsage(restored.usage);
      setMetrics(restored.metrics);
      setQueueSince(null);
      setQueueWaitMs(restored.waitMs);
      setCompaction(restored.compaction);
      chatFollowRef.current = true;
      chatResetFollowRef.current = true;
      setChatAtBottom(true);
      historyAnchorRef.current = null;
      setHistoryStart(Math.max(0, restored.list.length - HISTORY_WINDOW));
      setActivities(restored.list);
      setCurrent(session);
      if (switchingBinding) {
        setFile(null);
        setFiles(null);
        setDiffResult(null);
      }
      setStatus('就绪');
    } catch (error) {
      notify(error);
    } finally {
      bindingBusyRef.current = false;
      setProjectBusy(false);
    }
  }

  async function renameSession(session: Session, title: string) {
    await api('rename_session', { sessionId: session.id, title });
    setSessions(old => old.map(s => (s.id === session.id ? { ...s, title } : s)));
    setCurrent(curr => (curr?.id === session.id ? { ...curr, title } : curr));
    notify('会话标题已更新');
  }

  async function exportSession(session: Session) {
    try {
      const data = await api<{ session: Session; events: Json[] }>('export_session', {
        sessionId: session.id,
      });
      const blob = new Blob([JSON.stringify(data, null, 2)], {
        type: 'application/json',
      });
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = `harness-session-${session.id.slice(0, 8)}.json`;
      link.click();
      URL.revokeObjectURL(url);
      notify('会话记录已导出');
    } catch (error) {
      notify(error);
    }
  }

  const checkOperation = useCallback(
    (itemId: string, operationId: string) => {
      void api('remote_operation_status', { sessionId: current?.id, operationId })
        .then(result =>
          setActivities(old =>
            old.map(item =>
              item.id === itemId
                ? {
                    ...item,
                    result: {
                      ...result,
                      indeterminate:
                        result.state === 'indeterminate' ||
                        result.state === 'dispatching' ||
                        result.state === 'accepted',
                    },
                  }
                : item,
            ),
          ),
        )
        .catch(notify);
    },
    [current?.id, notify],
  );

  const restoreDeleted = useCallback(
    (callId: string) => {
      if (!current?.id || bindingBusyRef.current) return;
      bindingBusyRef.current = true;
      setRestoreBusy(true);
      void api('restore_deleted_file', { sessionId: current.id, callId })
        .then(result => {
          if (result.restored !== true) {
            throw new Error('没有收到恢复成功的结果，请检查文件状态');
          }
          setFiles(null);
          setActivities(old => [
            ...old,
            {
              id: crypto.randomUUID(),
              role: 'status',
              text: `已恢复 ${String(result.path || '')}`,
            },
          ]);
          if (result.historyWarning) notify(result.historyWarning);
        })
        .catch(notify)
        .finally(() => {
          bindingBusyRef.current = false;
          setRestoreBusy(false);
        });
    },
    [current?.id, notify],
  );

  function applyEvent(event: Json) {
    const type = String(event.type);
    if (type === 'text_delta' || type === 'reasoning_delta') {
      setActivities(old => {
        const list = [...old];
        let last = list.at(-1);
        if (!last || last.role !== 'assistant' || !last.running) {
          last = {
            id: crypto.randomUUID(),
            role: 'assistant',
            text: '',
            running: true,
          };
          list.push(last);
        }
        list[list.length - 1] =
          type === 'text_delta'
            ? { ...last, text: last.text + String(event.text) }
            : { ...last, reasoning: (last.reasoning || '') + String(event.text) };
        return list;
      });
      setStatus(type === 'reasoning_delta' ? '思考中' : '生成中');
    } else if (type === 'assistant_completed') {
      setActivities(old => {
        const hasRunning = old.some(item => item.role === 'assistant' && item.running);
        const finalText = typeof event.text === 'string' ? event.text : '';
        if (hasRunning) {
          return old.map(item =>
            item.role === 'assistant' && item.running
              ? { ...item, running: false, text: item.text || finalText }
              : item,
          );
        }
        return finalText
          ? [...old, { id: crypto.randomUUID(), role: 'assistant', text: finalText }]
          : old;
      });
    } else if (type === 'tool_started') {
      setStatus(`正在执行 ${String(event.name)}`);
      setActivities(old => [
        ...old,
        {
          id: crypto.randomUUID(),
          callId: String(event.callId),
          role: 'tool',
          name: String(event.name),
          text: '',
          args: String(event.arguments),
          deviceId: String(event.deviceId),
          running: true,
        },
      ]);
    } else if (type === 'tool_output') {
      setActivities(old => {
        const idx = findLastToolIndex(old, event.callId);
        if (idx === -1) return old;
        const next = [...old];
        next[idx] = { ...next[idx], text: next[idx].text + String(event.text || '') };
        return next;
      });
      setTerminal(old => (old ?? '') + String(event.text || ''));
    } else if (type === 'tool_completed') {
      setActivities(old => {
        const idx = findLastToolIndex(old, event.callId);
        if (idx === -1) return old;
        const next = [...old];
        next[idx] = {
          ...next[idx],
          running: false,
          result: event.result,
          durationMs: Number(event.durationMs),
        };
        return next;
      });
      if (String(event.name).startsWith('file_')) {
        setFiles(null);
        setDiffResult(null);
      }
      if (event.name === 'terminal_run') {
        setTerminal(old => (old ?? '') + '\n[命令结束，详细结果保存在工具卡片]\n');
      }
    } else if (type === 'context_usage' || type === 'usage') {
      setUsage(u => applyUsage(u, event));
      if (type === 'context_usage') setStatus('准备模型响应');
    } else if (type === 'inference_queued') {
      setQueueSince(Date.now());
      setQueueWaitMs(null);
      setStatus('排队等待推理资源');
    } else if (type === 'inference_acquired') {
      setQueueSince(null);
      setQueueWaitMs(numberValue(event.waitMs) ?? null);
      setStatus('准备模型响应');
    } else if (type === 'model_metrics') {
      const measured = readMetrics(event);
      setMetrics(measured);
      setQueueWaitMs(old => measured.queueWaitMs ?? old);
    } else if (type === 'compaction_started') {
      setStatus('正在压缩上下文');
    } else if (type === 'tool_batch_rejected') {
      setStatus('正在纠正工具参数');
      setActivities(old => [
        ...old,
        {
          id: crypto.randomUUID(),
          role: 'status',
          text: `工具调用未执行：${String(event.message || '')}`,
        },
      ]);
    } else if (type === 'model_retry') {
      const empty = event.reason === 'empty_final_after_tool';
      setStatus(empty ? '正在整理工具结果' : '正在增加思考预算并重试');
      setActivities(old => [
        ...old.map(item =>
          item.role === 'assistant' && item.running
            ? { ...item, running: false }
            : item,
        ),
        { id: crypto.randomUUID(), role: 'status', text: modelRetryText(event) },
      ]);
    } else if (type === 'capability_warning') {
      if (event.capability === 'tool_not_run') unverifiedToolRef.current = true;
      setActivities(old => [
        ...old,
        { id: crypto.randomUUID(), role: 'status', text: String(event.message || '') },
      ]);
    } else if (type === 'compaction_completed') {
      setStatus('上下文已压缩');
      const saved = savedTokens(event);
      setCompaction(old => ({
        count: old.count + 1,
        method: String(event.method || event.reason || ''),
        savedTokens: saved,
      }));
      setActivities(old => [
        ...old,
        {
          id: crypto.randomUUID(),
          role: 'status',
          text: `已整理上下文${
            saved !== undefined ? ` · 节省约 ${saved.toLocaleString()} token` : ''
          } · 完整历史保留在本地`,
        },
      ]);
    } else if (type === 'provider_error') {
      setStatus('推理服务异常');
      setActivities(old => [
        ...old,
        {
          id: crypto.randomUUID(),
          role: 'status',
          text: `推理异常：${String(event.message || '')}`,
        },
      ]);
    } else if (type === 'loop_guard') {
      setStatus('触发重复失败保护');
    } else if (type === 'error') {
      notify(event.message);
      setActivities(old => [
        ...old,
        { id: crypto.randomUUID(), role: 'status', text: String(event.message) },
      ]);
    } else if (type === 'status') {
      setStatus(String(event.text));
    } else if (type === 'turn_completed') {
      setRunning(false);
      setQueueSince(null);
      setStatus(
        event.status === 'completed'
          ? unverifiedToolRef.current
            ? '需要核对'
            : '回复已结束'
          : event.status === 'interrupted'
            ? '已停止'
            : '执行失败',
      );
    }
  }

  function flushStreamEvents() {
    if (pendingTimerRef.current !== null) clearTimeout(pendingTimerRef.current);
    pendingTimerRef.current = null;
    const events = pendingEventsRef.current;
    pendingEventsRef.current = [];
    for (const event of events) applyEvent(event);
  }

  function onEvent(event: Json) {
    if (
      event.type === 'text_delta' ||
      event.type === 'reasoning_delta' ||
      event.type === 'tool_output'
    ) {
      const last = pendingEventsRef.current.at(-1);
      if (
        last?.type === event.type &&
        last.callId === event.callId &&
        last.stream === event.stream
      ) {
        last.text = String(last.text || '') + String(event.text || '');
      } else {
        pendingEventsRef.current.push({ ...event });
      }
      if (pendingTimerRef.current === null) {
        pendingTimerRef.current = setTimeout(flushStreamEvents, 32);
      }
      return;
    }
    flushStreamEvents();
    applyEvent(event);
  }

  async function addImages(chosenFiles: File[]) {
    if (!chosenFiles.length) return;
    const binding = inspectorBindingRef.current;
    if (
      attachments.length + chosenFiles.length > 2 ||
      attachments.reduce((n, f) => n + f.size, 0) +
        chosenFiles.reduce((n, f) => n + f.size, 0) >
        8 * 1024 * 1024 ||
      chosenFiles.some(f => !['image/png', 'image/jpeg'].includes(f.type))
    ) {
      notify('最多选择两张 PNG/JPEG 图片，总大小不超过 8 MiB。');
      return;
    }
    try {
      const added = await Promise.all(
        chosenFiles.map(
          item =>
            new Promise<ImageAttachment>((resolve, reject) => {
              const reader = new FileReader();
              reader.onload = () =>
                typeof reader.result === 'string'
                  ? resolve({ name: item.name, size: item.size, dataUrl: reader.result })
                  : reject(new Error('无法读取图片'));
              reader.onerror = () => reject(new Error('无法读取图片'));
              reader.readAsDataURL(item);
            }),
        ),
      );
      if (binding !== inspectorBindingRef.current) return;
      setAttachments(old =>
        old.length + added.length <= 2 &&
        old.reduce((n, f) => n + f.size, 0) + added.reduce((n, f) => n + f.size, 0) <=
          8 * 1024 * 1024
          ? [...old, ...added]
          : old,
      );
    } catch (error) {
      notify(error);
    }
  }

  async function send() {
    if ((!input.trim() && attachments.length === 0) || bindingBusyRef.current) return;
    if (!desktop) {
      notify('当前为界面预览。请启动 Gemma Harness 桌面应用来连接模型与执行工具。');
      return;
    }
    if (!workspace) {
      notify('请先选择项目目录');
      return;
    }
    if (!connection || !modelId) {
      setSettings('models');
      notify('请先选择推理服务中的模型');
      return;
    }
    bindingBusyRef.current = true;
    const text = input;
    const images = attachments;
    unverifiedToolRef.current = false;
    resetPerformance();
    chatFollowRef.current = true;
    chatResetFollowRef.current = true;
    setChatAtBottom(true);
    historyAnchorRef.current = null;
    setHistoryStart(Math.max(0, activities.length - HISTORY_WINDOW));
    setInput('');
    setAttachments([]);
    setRunning(true);
    setStatus('正在连接');
    try {
      let session = current;
      if (!session) {
        session = await api<Session>('create_session');
        metricBindingRef.current = metricKey(
          session.id,
          connection.id,
          connection.contextLength,
        );
        const optimisticTitle =
          session.title === '新会话'
            ? text.trim()
              ? text.trim().slice(0, 30)
              : '图片消息'
            : session.title;
        const created = { ...session, title: optimisticTitle };
        setCurrent(created);
        setSessions(old => (old.some(s => s.id === created.id) ? old : [created, ...old]));
        session = created;
      }
      setActivities(old => [
        ...old,
        {
          id: crypto.randomUUID(),
          role: 'user',
          text,
          images: images.length > 0 ? images.map(img => img.name) : undefined,
        },
      ]);
      await stream(
        session.id,
        text,
        onEvent,
        images.map(image => image.dataUrl),
      );
    } catch (error) {
      notify(error);
      setStatus('执行失败');
    } finally {
      bindingBusyRef.current = false;
      flushStreamEvents();
      setRunning(false);
      setQueueSince(null);
      setActivities(old =>
        old.map(item => (item.running ? { ...item, running: false } : item)),
      );
      void refresh();
    }
  }

  async function stop() {
    if (!current) return;
    setStatus('正在停止');
    try {
      await api('interrupt', { sessionId: current.id });
    } catch (error) {
      notify(error);
    }
  }

  async function inspect(kind: string, path?: string) {
    const binding = inspectorBindingRef.current;
    const targetPath = kind === 'files' ? path ?? '.' : undefined;
    setPanel(kind);
    setPanelOpen(true);
    if (kind === 'files' && targetPath !== undefined) setCurrentDir(targetPath);
    if (!workspace || !desktop) return;
    setInspecting(true);
    try {
      const result = await api('execute_tool', {
        sessionId: current?.id,
        name:
          kind === 'diff'
            ? 'git_diff'
            : kind === 'desktop'
              ? `${computerKind}_observe`
              : 'file_list',
        args: kind === 'files' ? { path: targetPath || '.', limit: 150 } : {},
      });
      if (binding !== inspectorBindingRef.current) return;
      if (kind === 'files') setFiles(result);
      else if (kind === 'diff') setDiffResult(result);
      else setInspection(pretty(result));
    } catch (error) {
      if (binding === inspectorBindingRef.current) notify(error);
    } finally {
      if (binding === inspectorBindingRef.current) setInspecting(false);
    }
  }

  async function capture() {
    const binding = inspectorBindingRef.current;
    setInspecting(true);
    try {
      const result = await api('execute_tool', {
        sessionId: current?.id,
        name: `${computerKind}_screenshot`,
        args: {},
      });
      if (binding !== inspectorBindingRef.current) return;
      const parts = Array.isArray(result.content) ? (result.content as Json[]) : [];
      const image = parts.find(p => p.type === 'image' && typeof p.data === 'string');
      if (image) setPreviewImage(`data:${String(image.mimeType)};base64,${String(image.data)}`);
      setInspection(
        pretty({
          ...result,
          content: parts.map(p =>
            p.type === 'image' ? { ...p, data: '[图像已展示]' } : p,
          ),
        }),
      );
    } catch (error) {
      if (binding === inspectorBindingRef.current) notify(error);
    } finally {
      if (binding === inspectorBindingRef.current) setInspecting(false);
    }
  }

  async function readFile(path: string) {
    const binding = inspectorBindingRef.current;
    try {
      const result = await api('execute_tool', {
        sessionId: current?.id,
        name: 'file_read',
        args: { path, raw: true },
      });
      if (binding !== inspectorBindingRef.current) return;
      const text = String(result.content ?? result.text ?? '');
      const hash = String(result.sha256 ?? '');
      setFile({ path, text, hash });
      setFileText(text);
    } catch (error) {
      if (binding === inspectorBindingRef.current) notify(error);
    }
  }

  async function saveFile() {
    if (!file) return;
    const binding = inspectorBindingRef.current;
    try {
      await api('execute_tool', {
        sessionId: current?.id,
        name: 'file_write',
        args: { path: file.path, content: fileText, expected_sha256: file.hash },
      });
      if (binding !== inspectorBindingRef.current) return;
      await readFile(file.path);
      if (binding === inspectorBindingRef.current) {
        setDiffResult(null);
        notify('文件已保存');
      }
    } catch (error) {
      notify(error);
    }
  }

  async function runCommand() {
    if (!command.trim() || bindingBusyRef.current) return;
    if (command.trim() === 'clear') {
      setCommand('');
      setTerminal('');
      return;
    }
    bindingBusyRef.current = true;
    const operationId = crypto.randomUUID();
    setTerminalOperation(operationId);
    setTerminalBusy(true);
    const cmd = command;
    setCommand('');
    setTerminal(t => (t ?? '') + `\n$ ${cmd}\n`);
    try {
      let truncated = false;
      const result = await streamTool(
        {
          operationId,
          sessionId: current?.id,
          name: 'terminal_run',
          args: { command: cmd, timeout_ms: 600000 },
        },
        event => {
          if (event.type !== 'tool_output') return;
          let text = typeof event.text === 'string' ? event.text : '';
          if (event.truncated === true) {
            text = text.replace(LIVE_TRUNCATION_NOTICE, '');
            if (!truncated) {
              truncated = true;
              setTerminal(t => {
                const output = (t ?? '') + text;
                return (
                  output +
                  `${output.endsWith('\n') ? '' : '\n'}[实时输出已截断；输出文件每个流最多保留 16 MiB。]\n`
                );
              });
              return;
            }
          }
          if (text) setTerminal(t => (t ?? '') + text);
        },
      );
      const notices = terminalResultNotices(result, truncated);
      if (notices.length) {
        setTerminal(
          t => (t ?? '') + `${!t || t.endsWith('\n') ? '' : '\n'}${notices.join('\n')}\n`,
        );
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setTerminal(
        t => (t ?? '') + `${!t || t.endsWith('\n') ? '' : '\n'}[终端错误] ${message}\n`,
      );
    } finally {
      bindingBusyRef.current = false;
      setTerminalBusy(false);
      setTerminalOperation('');
    }
  }

  function beginUpdateInstall(fromSettings: boolean): string | null {
    if (bindingBusyRef.current) return '请先停止当前任务或终端命令，再安装更新。';
    if (file && fileText !== file.text) return '请先保存正在编辑的文件，再安装更新。';
    if (input || attachments.length || command) {
      return '请先发送或清空未提交的消息、图片和终端命令。';
    }
    if (projectDialog || renamingSession || (settings && !fromSettings)) {
      return '请先关闭设置或项目操作，再安装更新。';
    }
    if (settings && settingsDirtyRef.current) {
      return '请先保存或取消未完成的设置，再安装更新。';
    }
    if (settings && fromSettings) setSettings(null);
    bindingBusyRef.current = true;
    setUpdateInstalling(true);
    return null;
  }

  function endUpdateInstall() {
    bindingBusyRef.current = false;
    setUpdateInstalling(false);
  }

  const update = useAppUpdates(beginUpdateInstall, endUpdateInstall);
  const contextLabel = `${Math.round(usage.context / 1024)}K`;

  return (
    <div className="app-shell">
      <Sidebar
        collapsed={!sidebarOpen}
        inert={updateInstalling}
        projects={projects}
        sessions={sessions}
        current={current}
        workspace={workspace}
        workspaceName={workspaceName}
        deviceId={deviceId}
        deviceName={deviceName}
        shortcut={shortcut}
        filter={filter}
        searchOpen={searchOpen}
        busy={running || terminalBusy || projectBusy}
        onNewChat={startNewChat}
        onToggleSearch={toggleSearch}
        onFilterChange={setFilter}
        onSelectWorkspace={() => void selectWorkspace()}
        onOpenProject={project => void openProject(project)}
        onManageProject={(project, action) => setProjectDialog({ project, action })}
        onOpenSession={session => void openSession(session)}
        onRequestRenameSession={session => setRenamingSession(session)}
        onExportSession={session => void exportSession(session)}
        onOpenSettings={tab => setSettings(tab)}
      />

      <main className="main-area" inert={updateInstalling || undefined}>
        <header className="topbar" data-tauri-drag-region>
          <div className="breadcrumb">
            <IconButton
              label={sidebarOpen ? '收起侧边栏' : '展开侧边栏'}
              active={!sidebarOpen}
              onClick={() => setSidebarOpen(v => !v)}
            >
              <Menu size={16} />
            </IconButton>
            <span title={workspace || undefined}>
              {workspace ? workspaceName : '工作台'}
            </span>
            <ChevronRight size={13} />
            <strong>{current?.title || '新会话'}</strong>
          </div>
          <div className="topbar-actions">
            <IconButton
              label={panelOpen ? '隐藏详情' : '显示详情'}
              onClick={() => setPanelOpen(!panelOpen)}
              active={panelOpen}
            >
              <PanelRightClose size={17} />
            </IconButton>
          </div>
        </header>

        {!desktop && (
          <div className="preview-banner">
            <Monitor size={13} /> 桌面界面预览{' '}
            <span>模型与系统工具在桌面应用中连接</span>
          </div>
        )}

        <div className="work-area">
          <section className="conversation">
            <div
              className="conversation-scroll"
              ref={scrollRef}
              onScroll={event => {
                const viewport = event.currentTarget;
                const nearBottom =
                  viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop <= 64;
                if (nearBottom || viewport.scrollTop !== chatScrollTopRef.current) {
                  chatFollowRef.current = nearBottom;
                  setChatAtBottom(nearBottom);
                }
                chatScrollTopRef.current = viewport.scrollTop;
              }}
            >
              {activities.length === 0 ? (
                <WelcomeView
                  modelConfigured={Boolean(modelId)}
                  workspaceConfigured={Boolean(workspace)}
                  onOpenModels={() => setSettings('models')}
                  onSelectWorkspace={() => void selectWorkspace()}
                  onChoosePrompt={prompt => {
                    setInput(prompt);
                    inputRef.current?.focus();
                  }}
                />
              ) : (
                <div className="messages">
                  {historyStart > 0 && (
                    <button
                      type="button"
                      className="load-history"
                      aria-label="加载更早记录"
                      onClick={() => {
                        const viewport = scrollRef.current;
                        if (viewport) {
                          historyAnchorRef.current = {
                            height: viewport.scrollHeight,
                            top: viewport.scrollTop,
                          };
                        }
                        chatFollowRef.current = false;
                        setChatAtBottom(false);
                        setHistoryStart(start => Math.max(0, start - HISTORY_WINDOW));
                      }}
                    >
                      加载更早记录 · {historyStart} 条
                    </button>
                  )}
                  {activities.slice(historyStart).map(item => (
                    <ActivityCard
                      item={item}
                      onCheckOperation={checkOperation}
                      onRestoreDeleted={restoreDeleted}
                      restoreDisabled={
                        running || terminalBusy || projectBusy || restoreBusy
                      }
                      key={item.id}
                    />
                  ))}
                </div>
              )}
            </div>

            <Composer
              inputRef={inputRef}
              input={input}
              attachments={attachments}
              workspace={workspace}
              running={running}
              terminalBusy={terminalBusy}
              projectBusy={projectBusy}
              status={status}
              queueSince={queueSince}
              chatAtBottom={chatAtBottom}
              onInputChange={setInput}
              onAddImages={addImages}
              onRemoveAttachment={index =>
                setAttachments(old => old.filter((_, i) => i !== index))
              }
              onSelectWorkspace={() => void selectWorkspace()}
              onSend={send}
              onStop={stop}
              onJumpLatest={() => {
                chatFollowRef.current = true;
                setChatAtBottom(true);
                const viewport = scrollRef.current;
                if (viewport) {
                  viewport.scrollTop = viewport.scrollHeight;
                  chatScrollTopRef.current = viewport.scrollTop;
                }
              }}
            />
          </section>

          {panelOpen && (
            <InspectorPanel
              panel={panel}
              workspace={workspace}
              workspaceName={workspaceName}
              currentDir={currentDir}
              files={files}
              file={file}
              fileText={fileText}
              diffResult={diffResult}
              inspection={inspection}
              inspecting={inspecting}
              computerKind={computerKind}
              previewImage={previewImage}
              modelId={modelId}
              connection={connection}
              onInspect={inspect}
              onSelectWorkspace={() => void selectWorkspace()}
              onReadFile={readFile}
              onFileTextChange={setFileText}
              onSaveFile={saveFile}
              onCloseFile={() => setFile(null)}
              onComputerKindChange={kind => {
                setComputerKind(kind);
                setInspection('');
                setPreviewImage('');
              }}
              onCapture={capture}
              onOpenSettings={tab => setSettings(tab)}
            />
          )}
        </div>

        {terminalOpen && (
          <TerminalPanel
            terminal={terminal}
            command={command}
            deviceName={deviceName}
            workspace={workspace}
            terminalBusy={terminalBusy}
            running={running}
            projectBusy={projectBusy}
            onCommandChange={setCommand}
            onRunCommand={runCommand}
            onStopCommand={() =>
              void api('interrupt', { sessionId: terminalOperation }).catch(notify)
            }
            onClearTerminal={() => setTerminal('')}
            onClose={() => setTerminalOpen(false)}
          />
        )}

        {performanceOpen && (
          <PerformancePanel
            usage={usage}
            metrics={metrics}
            queueSince={queueSince}
            queueWaitMs={queueWaitMs}
            status={status}
            compaction={compaction}
            onClose={() => setPerformanceOpen(false)}
          />
        )}

        <footer className="statusbar">
          <div>
            <button
              type="button"
              className={terminalOpen ? 'is-active' : ''}
              onClick={() => setTerminalOpen(!terminalOpen)}
            >
              <Terminal size={13} />
              终端
            </button>
            <span className="statusbar-divider" />
            <span>
              <HardDrive size={12} />
              {deviceName}
            </span>
            <span className="statusbar-connection">
              <span className={`status-dot ${modelId ? 'configured' : ''}`} />
              {modelId ? '已配置模型' : '模型未配置'}
            </span>
          </div>
          <div>
            <button
              type="button"
              className="performance-toggle"
              aria-label="推理性能与上下文"
              aria-expanded={performanceOpen}
              onClick={() => setPerformanceOpen(open => !open)}
              title={
                usage.budget
                  ? `输入预算 ${usage.budget.toLocaleString()}；实际加载上下文 ${usage.context.toLocaleString()}`
                  : '查看上下文预算、真实用量与最近请求性能'
              }
            >
              <span className="context-track">
                <i
                  style={{
                    width: `${Math.min(100, (usage.input / (usage.budget || usage.context)) * 100)}%`,
                  }}
                />
              </span>
              {usage.estimated ? '约 ' : ''}
              {usage.input.toLocaleString()} /{' '}
              {usage.budget
                ? `${usage.budget.toLocaleString()} 输入`
                : contextLabel}
              <ChevronDown size={10} />
            </button>
            <span className="statusbar-divider" />
            {desktop && <UpdateControl update={update} />}
            <span>Gemma Harness</span>
          </div>
        </footer>
      </main>

      {updateInstalling && (
        <div className="update-lock" role="status" aria-live="polite">
          <Loader2 size={18} className="spin" />
          正在安装更新，应用即将重启…
        </div>
      )}

      {settings && (
        <Settings
          initialTab={settings}
          config={config}
          update={update}
          onDirtyChange={recordSettingsDirty}
          onClose={() => setSettings(null)}
          onSave={(next, savedAppearanceOnly) => {
            const appearanceOnly =
              savedAppearanceOnly ||
              withoutAppearance(configRef.current) === withoutAppearance(next);
            setConfig(next);
            if (appearanceOnly) {
              void refresh();
              return;
            }
            const selected = next.connections.find(
              c => c.id === next.selectedConnectionId,
            );
            metricBindingRef.current = metricKey(
              undefined,
              selected?.id,
              selected?.contextLength,
            );
            resetPerformance(selected?.contextLength || 8192);
            setCurrent(null);
            setActivities([]);
            setHistoryStart(0);
            chatFollowRef.current = true;
            setChatAtBottom(true);
            setFiles(null);
            setDiffResult(null);
            void refresh();
          }}
          onError={notify}
        />
      )}

      {projectDialog && (
        <ProjectDialog
          key={`${projectDialog.project.id}:${projectDialog.action}`}
          project={projectDialog.project}
          action={projectDialog.action}
          onClose={() => setProjectDialog(null)}
          onConfirm={manageProject}
        />
      )}

      {renamingSession && (
        <SessionRenameDialog
          key={renamingSession.id}
          session={renamingSession}
          onClose={() => setRenamingSession(null)}
          onConfirm={title => renameSession(renamingSession, title)}
        />
      )}

      {toast && (
        <div role="alert" className="toast">
          <Unplug size={16} />
          <span>{toast}</span>
          <button
            type="button"
            aria-label="关闭提示"
            onClick={() => setToast('')}
          >
            <X size={14} />
          </button>
        </div>
      )}
    </div>
  );
}
