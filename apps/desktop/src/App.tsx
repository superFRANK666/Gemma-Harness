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
import PerformancePanel, { initialUsage } from './components/PerformancePanel';
import { ProjectDialog } from './components/Projects';
import Settings from './components/Settings';
import Sidebar, { SessionDialog, type SessionAction } from './components/Sidebar';
import TerminalPanel from './components/TerminalPanel';
import UpdateControl, { useAppUpdates } from './components/UpdateControl';
import WelcomeView from './components/WelcomeView';
import {
  applyConversationEvent,
  emptyActivities,
  emptyConversation,
  releaseConversationHistory,
} from './utils/conversation';
import type { Conversation } from './utils/conversation';
import {
  basename,
  buildActivitiesFromEvents,
  HISTORY_WINDOW,
  LIVE_TRUNCATION_NOTICE,
  terminalResultNotices,
} from './utils/sessionEvents';

const withoutAppearance = (config: Config) => {
  const copy = { ...config };
  delete copy.appearance;
  return JSON.stringify(copy);
};

type RunningTurn = {
  sessionId?: string;
  started: boolean;
  completed: boolean;
  stopRequested: boolean;
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
  const [sessionDialog, setSessionDialog] = useState<{
    session: Session;
    action: SessionAction;
  } | null>(null);
  const [conversations, setConversations] = useState(() => {
    const draft: string = crypto.randomUUID();
    return { selected: draft, draft, chats: new Map<string, Conversation>() };
  });
  const chatKey = conversations.selected;
  const chat = conversations.chats.get(chatKey) ?? emptyConversation;
  const { session: current, input, attachments, running, status, terminal, command } = chat;
  const { metrics, queueSince, queueWaitMs, compaction } = chat;
  const activities = chat.activities ?? emptyActivities;
  const runningChats = new Set<string>();
  for (const [key, view] of conversations.chats) {
    if (view.running) runningChats.add(key);
  }
  const anyRunning = runningChats.size > 0;
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
  const [performanceOpen, setPerformanceOpen] = useState(false);
  const [historyStart, setHistoryStart] = useState(0);
  const [chatAtBottom, setChatAtBottom] = useState(true);
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [terminalBusy, setTerminalBusy] = useState(false);
  const [updateInstalling, setUpdateInstalling] = useState(false);
  const [terminalOperation, setTerminalOperation] = useState('');
  const [filter, setFilter] = useState('');
  const [searchOpen, setSearchOpen] = useState(false);

  const shortcut = navigator.platform.toLowerCase().includes('mac') ? '⌘' : 'Ctrl';

  const scrollRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const inspectorBindingRef = useRef(0);
  const chatFollowRef = useRef(true);
  const chatScrollTopRef = useRef(0);
  const chatResetFollowRef = useRef(false);
  const historyAnchorRef = useRef<{ height: number; top: number } | null>(null);
  const turns = useRef(new Map<string, RunningTurn>());
  const bindingBusyRef = useRef(false);
  const navigationRevision = useRef(0);
  const sessionListRevision = useRef(0);
  const settingsDirtyRef = useRef(true);
  const recordSettingsDirty = useCallback((dirty: boolean) => {
    settingsDirtyRef.current = dirty;
  }, []);

  const hasActiveWork = useCallback(() => bindingBusyRef.current || turns.current.size > 0, []);

  const configRef = useRef(config);
  configRef.current = config;

  const connection = config.connections.find(
    c => c.id === (current?.connectionId || config.selectedConnectionId),
  );
  const modelId = connection?.model?.trim() || '';
  const workspace = current?.workspace || config.workspace;
  const deviceId = current?.deviceId || config.deviceId;
  const usage = chat.usage ?? initialUsage(connection?.contextLength || 8192);
  const visibleBinding = useRef({ workspace, deviceId, dirty: false });
  visibleBinding.current = { workspace, deviceId, dirty: !!file && fileText !== file.text };
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

  const updateConversation = useCallback((key: string, update: (view: Conversation) => Conversation) => {
    setConversations(state => {
      const stored = state.chats.get(key);
      if (!stored && key !== state.selected && key !== state.draft) return state;
      const previous = stored ?? emptyConversation;
      const next = update(previous);
      if (previous === next) return state;
      return { ...state, chats: new Map(state.chats).set(key, next) };
    });
  }, []);

  const selectConversation = useCallback((key: string, activityCount = 0, restored?: Partial<Conversation>) => {
    navigationRevision.current += 1;
    chatFollowRef.current = true;
    chatResetFollowRef.current = true;
    setChatAtBottom(true);
    historyAnchorRef.current = null;
    setHistoryStart(Math.max(0, activityCount - HISTORY_WINDOW));
    setConversations(state => {
      const chats = new Map(state.chats);
      if (state.selected !== key && state.selected !== state.draft) {
        releaseConversationHistory(chats, state.selected);
      }
      if (restored) chats.set(key, { ...(chats.get(key) ?? emptyConversation), ...restored });
      return { ...state, selected: key, chats };
    });
  }, []);

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
    setInspecting(false);
  }, [workspace, deviceId]);

  async function refresh(sessionsOnly = false) {
    if (!desktop) return;
    const revision = ++sessionListRevision.current;
    try {
      const snapshot = await api<Snapshot>('snapshot');
      if (revision === sessionListRevision.current) setSessions(snapshot.sessions);
      if (!sessionsOnly) {
        setConfig(snapshot.config);
        setProjects(snapshot.projects ?? null);
      }
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
  }, [activities, chatKey, historyStart]);

  const startNewChat = useCallback(() => {
    if (bindingBusyRef.current) {
      notify('请等待当前操作完成后再新建会话');
      return;
    }
    const latestConfig = configRef.current;
    const binding = visibleBinding.current;
    if (binding.dirty && (binding.workspace !== latestConfig.workspace || binding.deviceId !== latestConfig.deviceId)) {
      notify('请先保存编辑内容，再新建会话');
      return;
    }
    selectConversation(conversations.draft);
    inputRef.current?.focus();
  }, [conversations.draft, notify, selectConversation]);

  const toggleSearch = useCallback(() => {
    setSidebarOpen(true);
    setSearchOpen(open => {
      if (open) setFilter('');
      return !open;
    });
  }, []);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (settings || projectDialog || sessionDialog) return;
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
  }, [settings, projectDialog, sessionDialog, startNewChat, toggleSearch]);

  function applyProjectSelection(result: { config: Config; projects: Project[] }) {
    setConfig(result.config);
    setProjects(result.projects);
    selectConversation(conversations.draft);
    setFiles(null);
    setDiffResult(null);
  }

  async function selectWorkspace() {
    if (hasActiveWork()) {
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
    if (hasActiveWork()) {
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
    if (action === 'remove' && hasActiveWork()) {
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
      selectConversation(conversations.draft);
      setFiles(null);
      setDiffResult(null);
    }
    notify(
      action === 'rename'
        ? '项目名称已更新'
        : '项目已从列表移除，文件和历史会话已保留',
    );
  }

  async function openSession(session: Session) {
    if (bindingBusyRef.current) {
      notify('请等待当前操作完成后再切换会话');
      return;
    }
    const revision = ++navigationRevision.current;
    if (chatKey === session.id) return;
    const needsSave = () => {
      const binding = visibleBinding.current;
      return binding.dirty && (session.workspace !== binding.workspace || session.deviceId !== binding.deviceId);
    };
    if (needsSave()) {
      notify('请先保存编辑内容，再切换会话');
      return;
    }
    const cached = conversations.chats.get(session.id);
    if (cached?.activities) {
      selectConversation(session.id, cached.activities.length);
      return;
    }
    try {
      const result = await api<{ session?: Session; events: { kind: string; payload: Json }[] }>(
        'load_session',
        { sessionId: session.id },
      );
      if (revision !== navigationRevision.current || bindingBusyRef.current) return;
      if (needsSave()) {
        notify('请先保存编辑内容，再切换会话');
        return;
      }
      const selected = configRef.current.connections.find(c => c.id === session.connectionId);
      const restored = buildActivitiesFromEvents(result.events, selected?.contextLength || 8192);
      selectConversation(session.id, restored.list.length, {
        session: result.session ?? session,
        activities: restored.list,
        usage: restored.usage,
        metrics: restored.metrics,
        queueWaitMs: restored.waitMs,
        compaction: restored.compaction,
      });
    } catch (error) {
      if (revision === navigationRevision.current) notify(error);
    }
  }

  async function manageSession(title: string) {
    if (!sessionDialog) return;
    const { session, action } = sessionDialog;
    if (action === 'delete') {
      if (hasActiveWork()) throw new Error('请等待当前操作结束后再删除会话');
      const deletingCurrent = current?.id === session.id;
      if (
        deletingCurrent && file && fileText !== file.text &&
        (workspace !== config.workspace || deviceId !== config.deviceId)
      ) {
        throw new Error('请先保存编辑中的文件，再删除当前会话');
      }
      navigationRevision.current += 1;
      await api('delete_session', { sessionId: session.id });
      sessionListRevision.current += 1;
      setSessions(old => old.filter(s => s.id !== session.id));
      setConversations(state => {
        const chats = new Map(state.chats);
        chats.delete(session.id);
        return { ...state, chats };
      });
      if (deletingCurrent) startNewChat();
      notify('会话已删除');
      return;
    }
    await api('rename_session', { sessionId: session.id, title });
    sessionListRevision.current += 1;
    setSessions(old => old.map(s => (s.id === session.id ? { ...s, title } : s)));
    updateConversation(session.id, view => view.session ? { ...view, session: { ...view.session, title } } : view);
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
          updateConversation(chatKey, view => view.activities ? {
            ...view,
            activities: view.activities.map(item =>
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
          } : view),
        )
        .catch(notify);
    },
    [chatKey, current?.id, notify, updateConversation],
  );

  const restoreDeleted = useCallback(
    (callId: string) => {
      if (!current?.id || hasActiveWork()) return;
      bindingBusyRef.current = true;
      setRestoreBusy(true);
      void api('restore_deleted_file', { sessionId: current.id, callId })
        .then(result => {
          if (result.restored !== true) {
            throw new Error('没有收到恢复成功的结果，请检查文件状态');
          }
          setFiles(null);
          updateConversation(chatKey, view => ({
            ...view,
            activities: [...(view.activities ?? emptyActivities), {
              id: crypto.randomUUID(),
              role: 'status',
              text: `已恢复 ${String(result.path || '')}`,
            }],
          }));
          if (result.historyWarning) notify(result.historyWarning);
        })
        .catch(notify)
        .finally(() => {
          bindingBusyRef.current = false;
          setRestoreBusy(false);
        });
    },
    [chatKey, current?.id, hasActiveWork, notify, updateConversation],
  );

  async function addImages(chosenFiles: File[]) {
    if (!chosenFiles.length) return;
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
      if (!current && (workspace !== configRef.current.workspace || deviceId !== configRef.current.deviceId)) return;
      updateConversation(chatKey, view => {
        const all = [...view.attachments, ...added];
        if (view.running || all.length > 2 || all.reduce((n, f) => n + f.size, 0) > 8 * 1024 * 1024) return view;
        return { ...view, attachments: all };
      });
    } catch (error) {
      notify(error);
    }
  }

  async function interruptTurn(turn: RunningTurn) {
    if (!turn.sessionId) return;
    try {
      await api('interrupt', { sessionId: turn.sessionId });
    } catch (error) {
      turn.stopRequested = false;
      notify(error);
    }
  }

  async function send() {
    if ((!input.trim() && attachments.length === 0) || running || bindingBusyRef.current || turns.current.has(chatKey)) return;
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

    const turn: RunningTurn = {
      sessionId: current?.id, started: false, completed: false, stopRequested: false,
    };
    let key = chatKey;
    turns.current.set(key, turn);
    const nextDraft = current ? null : crypto.randomUUID();
    setConversations(state => {
      const previous = state.chats.get(chatKey) ?? emptyConversation;
      const view: Conversation = {
        ...previous,
        activities: previous.activities ?? emptyActivities,
        input: '',
        attachments: [],
        running: true,
        status: '正在连接',
        unverifiedTool: false,
        usage: initialUsage(connection.contextLength || 8192),
        metrics: null,
        queueSince: null,
        queueWaitMs: null,
        compaction: { count: 0 },
      };
      return {
        ...state,
        draft: nextDraft && state.draft === chatKey ? nextDraft : state.draft,
        chats: new Map(state.chats).set(chatKey, view),
      };
    });
    chatFollowRef.current = true;
    chatResetFollowRef.current = true;
    setChatAtBottom(true);
    historyAnchorRef.current = null;
    setHistoryStart(Math.max(0, activities.length - HISTORY_WINDOW));

    const pending: Json[] = [];
    const receivedDeltaTypes = new Set<string>();
    let answerLength = 0;
    let lastFlushAt = 0;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let settled = false;
    function flushEvents() {
      if (timer !== null) clearTimeout(timer);
      timer = null;
      if (pending.length === 0) return;
      lastFlushAt = performance.now();
      const events = pending.splice(0);
      updateConversation(key, view => events.reduce(applyConversationEvent, view));
    }

    function onEvent(event: Json) {
      if (settled) return;
      if (event.type === 'turn_completed') turn.completed = true;
      if (!turn.started) {
        turn.started = true;
        if (turn.stopRequested && !turn.completed) void interruptTurn(turn);
      }
      if (event.type === 'turn_started') return;
      if (event.type === 'context_usage') {
        receivedDeltaTypes.clear();
        answerLength = 0;
      }
      if (event.type === 'text_delta' || event.type === 'reasoning_delta' || event.type === 'tool_output') {
        const text = String(event.text || '');
        if (event.type === 'text_delta') answerLength += text.length;
        const last = pending.at(-1);
        if (last?.type === event.type && last.callId === event.callId && last.stream === event.stream) {
          last.text += text;
        } else {
          pending.push({ ...event, text });
        }
        // Show the first delta immediately. Limit full Markdown parses during long replies.
        const interval = answerLength >= 2048 ? 96 : 32;
        const remaining = lastFlushAt + interval - performance.now();
        if (!receivedDeltaTypes.has(event.type) || remaining <= 0) {
          receivedDeltaTypes.add(event.type);
          flushEvents();
        } else if (timer === null) {
          timer = setTimeout(flushEvents, remaining);
        }
        return;
      }
      pending.push(event);
      flushEvents();
      if (
        event.type === 'tool_completed' && String(event.name).startsWith('file_') &&
        visibleBinding.current.workspace === workspace && visibleBinding.current.deviceId === deviceId
      ) {
        setFiles(null);
        setDiffResult(null);
      }
    }

    try {
      let session = current;
      if (!session) {
        const created = await api<Session>('create_session', { workspace, deviceId, connectionId: connection.id });
        session = created.title === '新会话' && !turn.stopRequested
          ? { ...created, title: input.trim() ? Array.from(input).slice(0, 30).join('') : '图片消息' }
          : created;
        const saved = session;
        turns.current.delete(chatKey);
        key = saved.id;
        turn.sessionId = saved.id;
        turns.current.set(key, turn);
        setConversations(state => {
          const chats = new Map(state.chats);
          const previous = chats.get(chatKey) ?? emptyConversation;
          chats.delete(chatKey);
          chats.set(saved.id, { ...previous, session: saved });
          return { ...state, selected: state.selected === chatKey ? saved.id : state.selected, chats };
        });
        sessionListRevision.current += 1;
        setSessions(old => [saved, ...old.filter(item => item.id !== saved.id)]);
      }
      if (turn.stopRequested) {
        pending.push({ type: 'turn_completed', status: 'interrupted' });
        return;
      }
      const userMessage: Activity = {
        id: crypto.randomUUID(),
        role: 'user',
        text: input,
        images: attachments.length > 0 ? attachments.map(image => image.name) : undefined,
      };
      updateConversation(key, view => ({
        ...view, activities: [...(view.activities ?? emptyActivities), userMessage],
      }));
      await stream(session.id, input, onEvent, attachments.map(image => image.dataUrl));
      if (!turn.completed) pending.push({ type: 'turn_completed', status: 'completed' });
    } catch (error) {
      if (!turn.stopRequested) notify(error);
      if (!turn.completed) {
        if (!turn.stopRequested) pending.push({ type: 'error', message: error instanceof Error ? error.message : String(error) });
        pending.push({ type: 'turn_completed', status: turn.stopRequested ? 'interrupted' : 'failed' });
      }
    } finally {
      settled = true;
      flushEvents();
      turns.current.delete(key);
      const finishedKey = key;
      setConversations(state => {
        const previous = state.chats.get(finishedKey);
        if (!previous) return state;
        const chats = new Map(state.chats);
        chats.set(finishedKey, {
          ...previous,
          running: false,
          queueSince: null,
          activities: previous.activities?.map(item => item.running ? { ...item, running: false } : item) ?? null,
        });
        if (!previous.session && state.selected !== finishedKey) chats.delete(finishedKey);
        return { ...state, chats };
      });
      if (turn.sessionId) void refresh(true);
    }
  }

  async function stop() {
    const turn = turns.current.get(chatKey);
    if (!turn || turn.completed || turn.stopRequested) return;
    turn.stopRequested = true;
    updateConversation(chatKey, view => ({ ...view, status: '正在停止' }));
    if (turn.started) await interruptTurn(turn);
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
    if (!command.trim() || hasActiveWork()) return;
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
      let streamedOutput = false;
      let settled = false;
      const result = await streamTool(
        {
          operationId,
          sessionId: current?.id,
          name: 'terminal_run',
          args: { command: cmd, timeout_ms: 600000 },
        },
        event => {
          if (settled || event.type !== 'tool_output') return;
          let text = typeof event.text === 'string' ? event.text : '';
          if (event.truncated === true) {
            text = text.replace(LIVE_TRUNCATION_NOTICE, '');
            if (!truncated) {
              truncated = true;
              streamedOutput = true;
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
          if (text) {
            streamedOutput = true;
            setTerminal(t => (t ?? '') + text);
          }
        },
      );
      let fallback = '';
      if (!streamedOutput) {
        settled = true;
        const out = typeof result.stdout === 'string' ? result.stdout : '';
        const err = typeof result.stderr === 'string' ? result.stderr : '';
        fallback = out + err;
      }
      const notices = terminalResultNotices(result, truncated);
      if (fallback || notices.length) {
        setTerminal(t => {
          let next = (t ?? '') + fallback;
          if (notices.length) {
            next += `${!next || next.endsWith('\n') ? '' : '\n'}${notices.join('\n')}\n`;
          }
          return next;
        });
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
    if (hasActiveWork()) return '请先停止运行中的任务或终端命令，再安装更新。';
    if (file && fileText !== file.text) return '请先保存正在编辑的文件，再安装更新。';
    const unfinished = [...conversations.chats.values()].find(view => view.input || view.attachments.length || view.command);
    if (unfinished) {
      return `请先处理「${unfinished.session?.title || '新会话'}」中未提交的消息、图片或终端命令。`;
    }
    if (projectDialog || sessionDialog || (settings && !fromSettings)) {
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

  function setInput(input: string) {
    updateConversation(chatKey, view => ({ ...view, input }));
  }

  function setCommand(command: string) {
    updateConversation(chatKey, view => ({ ...view, command }));
  }

  function setTerminal(update: string | ((previous: string | null) => string)) {
    updateConversation(chatKey, view => ({
      ...view,
      terminal: typeof update === 'string' ? update : update(view.terminal),
    }));
  }

  return (
    <div className="app-shell">
      <Sidebar
        collapsed={!sidebarOpen}
        inert={updateInstalling || !!sessionDialog}
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
        busy={anyRunning || terminalBusy || projectBusy || restoreBusy}
        runningChats={runningChats}
        onNewChat={startNewChat}
        onToggleSearch={toggleSearch}
        onFilterChange={setFilter}
        onSelectWorkspace={() => void selectWorkspace()}
        onOpenProject={project => void openProject(project)}
        onManageProject={(project, action) => setProjectDialog({ project, action })}
        onOpenSession={session => void openSession(session)}
        onManageSession={(session, action) => setSessionDialog({ session, action })}
        onExportSession={session => void exportSession(session)}
        onOpenSettings={tab => setSettings(tab)}
      />

      <main className="main-area" inert={updateInstalling || !!sessionDialog || undefined}>
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
                        anyRunning || terminalBusy || projectBusy || restoreBusy
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
              projectBusy={projectBusy || restoreBusy}
              status={status}
              queueSince={queueSince}
              chatAtBottom={chatAtBottom}
              onInputChange={setInput}
              onAddImages={addImages}
              onRemoveAttachment={index =>
                updateConversation(chatKey, view => ({
                  ...view, attachments: view.attachments.filter((_, i) => i !== index),
                }))
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
            key={chatKey}
            terminal={terminal}
            command={command}
            deviceName={deviceName}
            workspace={workspace}
            terminalBusy={terminalBusy}
            running={anyRunning}
            projectBusy={projectBusy || restoreBusy}
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
            selectConversation(conversations.draft);
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

      {sessionDialog && (
        <SessionDialog
          key={sessionDialog.session.id}
          session={sessionDialog.session}
          action={sessionDialog.action}
          onClose={() => setSessionDialog(null)}
          onConfirm={manageSession}
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
