import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { ArrowDownToLine, ArrowUpRight, Braces, Check, ChevronRight, Cpu, FolderOpen, Globe2, KeyRound, Loader2, Monitor, Palette, Plus, Radio, RefreshCw, Server, Settings2, ShieldCheck, Trash2, X } from 'lucide-react';
import type { Config, Connection, Json } from '../types';
import { api, choosePath, desktop, pretty } from '../bridge';
import { applyAppearance, normalizeAppearance } from '../appearance';
import AppearanceSettings from './AppearanceSettings';
import { VersionControl, type UpdateController } from './UpdateControl';

interface Props { initialTab: string; config: Config; onClose: () => void; onSave: (config: Config, appearanceOnly?: boolean) => void; onError: (error: unknown) => void; update: UpdateController; onDirtyChange: (dirty: boolean) => void }
const tabs = [{ id: 'models', label: '模型连接', icon: <Cpu size={17} /> }, { id: 'engine', label: '本地引擎', icon: <Server size={17} /> }, { id: 'devices', label: '执行设备', icon: <Monitor size={17} /> }, { id: 'tools', label: '工具与扩展', icon: <Braces size={17} /> }, { id: 'appearance', label: '外观', icon: <Palette size={17} /> }, { id: 'version', label: '版本与控制', icon: <RefreshCw size={17} /> }];
export default function Settings({ initialTab, config, onClose, onSave, onError, update, onDirtyChange }: Props) {
  const [tab, setTab] = useState(initialTab);
  const [draft, setDraft] = useState<Config>(structuredClone(config));
  const [selected, setSelected] = useState(config.selectedConnectionId);
  const [busy, setBusy] = useState('');
  const [report, setReport] = useState<Json | null>(null);
  const [models, setModels] = useState<Json[]>([]);
  const [invitation, setInvitation] = useState('');
  const [hostInvite, setHostInvite] = useState('');
  const [hostUrl, setHostUrl] = useState('wss://127.0.0.1:45455');
  const [hostStatus, setHostStatus] = useState<Json | null>(null);
  const [engine, setEngine] = useState({ binaryPath: '', modelPath: '', projectorPath: '', chatTemplatePath: '', contextLength: 8192, port: 1235, gpuLayers: 99, ...config.engineConfig });
  const initialEngineRef = useRef(JSON.stringify(engine));
  const [backend, setBackend] = useState(navigator.platform.toLowerCase().includes('mac') ? 'metal' : 'cpu');
  const [engineResult, setEngineResult] = useState<Json | null>(null);
  const [mcpName, setMcpName] = useState('');
  const [mcpCommand, setMcpCommand] = useState('');
  const [mcpArgs, setMcpArgs] = useState('[]');
  const savedConfigRef = useRef(config);
  useLayoutEffect(() => { savedConfigRef.current = config; }, [config]);
  useLayoutEffect(() => {
    onDirtyChange(JSON.stringify(draft) !== JSON.stringify(savedConfigRef.current)
      || JSON.stringify(engine) !== initialEngineRef.current || !!busy || !!invitation || !!hostInvite
      || hostUrl !== 'wss://127.0.0.1:45455' || !!mcpName || !!mcpCommand || mcpArgs !== '[]');
  }, [draft, engine, busy, invitation, hostInvite, hostUrl, mcpName, mcpCommand, mcpArgs, config, onDirtyChange]);
  useEffect(() => () => onDirtyChange(true), [onDirtyChange]);
  useLayoutEffect(() => applyAppearance(draft.appearance), [draft.appearance]);
  useLayoutEffect(() => () => { applyAppearance(savedConfigRef.current.appearance); }, []);
  const diagnosisOperationRef = useRef<string | null>(null);
  const closedRef = useRef(false);
  useEffect(() => {
    closedRef.current = false;
    return () => {
      closedRef.current = true;
      const operationId = diagnosisOperationRef.current; diagnosisOperationRef.current = null;
      if (operationId) void api('interrupt', { sessionId: operationId }).catch(() => {});
    };
  }, []);
  function closeSettings() {
    closedRef.current = true;
    const operationId = diagnosisOperationRef.current; diagnosisOperationRef.current = null;
    if (operationId) void api('interrupt', { sessionId: operationId }).catch(onError);
    applyAppearance(savedConfigRef.current.appearance);
    onClose();
  }
  const connection = draft.connections.find(c => c.id === selected) || draft.connections[0];
  function updateConnection(next: Partial<Connection>) { setDraft(old => ({ ...old, connections: old.connections.map(c => c.id === connection.id ? { ...c, ...next } : c) })); }
  async function task(name: string, job: () => Promise<void>) { setBusy(name); try { await job(); } catch (error) { onError(error); } finally { setBusy(''); } }
  function acceptSaved(saved: Config, appearanceOnly = false) {
    savedConfigRef.current = saved;
    onSave(saved, appearanceOnly);
    if (!closedRef.current) setDraft(saved);
  }
  async function saveConfiguration() {
    const saved = await api<Config>('save_settings', { config: draft });
    acceptSaved(saved); return saved;
  }
  async function save() { await task('save', async () => {
    const { appearance: _draftAppearance, ...draftSettings } = draft;
    const { appearance: _savedAppearance, ...savedSettings } = savedConfigRef.current;
    const appearanceChanged = JSON.stringify(normalizeAppearance(draft.appearance)) !== JSON.stringify(normalizeAppearance(savedConfigRef.current.appearance));
    const onlyAppearance = JSON.stringify(draftSettings) === JSON.stringify(savedSettings) && (appearanceChanged || tab === 'appearance');
    const saved = onlyAppearance ? await api<Config>('save_appearance', { appearance: normalizeAppearance(draft.appearance) }) : await api<Config>('save_settings', { config: draft });
    acceptSaved(saved, onlyAppearance);
    if (!closedRef.current) closeSettings();
  }); }
  async function discover() {
    await task('discover', async () => {
      const result = await api('discover', { connection }); setReport(result);
      const modelEnvelope = result.models as Json | undefined;
      const nativeEnvelope = result.native as Json | undefined;
      const nativeData = nativeEnvelope?.data as Json | undefined;
      const nativeModels = Array.isArray(nativeData?.models) ? nativeData.models as Json[] : [];
      const candidate = Array.isArray(result.models) ? result.models : modelEnvelope?.data || result.data;
      const openAiModels = Array.isArray(candidate) ? candidate as Json[] : [];
      const items = openAiModels.map(model => ({ ...model, ...nativeModels.find(n => n.key === model.id || (Array.isArray(n.loaded_instances) && n.loaded_instances.some((v: Json) => v.id === model.id))) }));
      for (const native of nativeModels) if (!items.some(m => m.id === native.key || m.key === native.key)) items.push({ ...native, id: native.key });
      setModels(items);
      if (items.length === 1 && !connection.model?.trim()) selectModel(items[0]);
    });
  }
  function selectModel(model: Json) {
    const loaded = Array.isArray(model.loadedInstances) ? model.loadedInstances : Array.isArray(model.loaded_instances) ? model.loaded_instances : [];
    const instance = loaded.find((item: Json) => item.id === model.id) || loaded[0];
    const context = instance?.config?.context_length || model.loadedContextLength || model.contextLength;
    updateConnection({ model: String(instance?.id || model.id || model.key || model.model || ''), modelFamily: 'auto', thinkingMode: 'auto', temperature: null, topP: null, topK: null, minP: null, seed: null, ...(typeof context === 'number' ? { contextLength: context, ...(connection.maxOutputTokens && connection.maxOutputTokens > context / 2 ? { maxOutputTokens: null } : {}) } : {}) });
  }
  async function path(field: 'binaryPath' | 'modelPath' | 'projectorPath' | 'chatTemplatePath') { try { const p = await choosePath(false); if (p) setEngine(e => ({ ...e, [field]: p })); } catch (error) { onError(error); } }
  async function startHost() { await task('host', async () => {
    await saveConfiguration();
    const result = await api('host_start', { advertisedUrl: hostUrl, workspace: draft.workspace }); setHostInvite(String(result.invitation)); setHostStatus(result.status as Json);
  }); }
  return <div className="modal-backdrop" onMouseDown={e => { if (e.target === e.currentTarget && !busy) closeSettings(); }}><section className="settings-modal" role="dialog" aria-modal="true" aria-label="设置与连接">
    <aside className="settings-sidebar"><div className="settings-brand"><Settings2 size={18} />设置与连接</div><nav>{tabs.map(t => <button className={tab === t.id ? 'selected' : ''} key={t.id} onClick={() => { setTab(t.id); setReport(null); }}>{t.icon}{t.label}<ChevronRight size={13} /></button>)}</nav><div className="settings-local"><ShieldCheck size={18} /><strong>你的模型，你的设备</strong><span>API 密钥与配对凭据<br />保存在操作系统凭据库</span></div></aside>
    <div className="settings-main"><header><div><div className="tiny-label">WORKSPACE PREFERENCES</div><h2>{tabs.find(t => t.id === tab)?.label}</h2></div><button className="icon-button" aria-label="关闭设置" onClick={closeSettings}><X size={19} /></button></header><div className="settings-content">
      {tab === 'version' && <VersionControl update={update} />}
      {tab === 'appearance' && <AppearanceSettings value={normalizeAppearance(draft.appearance)} onChange={appearance => setDraft(current => ({ ...current, appearance }))} />}
      {tab === 'models' && <><p className="settings-intro">连接 LM Studio、本机引擎或远程推理服务。推理位置与工具执行位置独立选择。</p><div className="connection-tabs">{draft.connections.map(c => <button className={connection.id === c.id ? 'selected' : ''} key={c.id} onClick={() => { setSelected(c.id); setModels([]); setReport(null); }}><span className="status-dot" />{c.name}</button>)}<button aria-label="添加连接" onClick={() => { const id = crypto.randomUUID(); setDraft(d => ({ ...d, connections: [...d.connections, { id, name: '新连接', baseUrl: 'http://127.0.0.1:1235', protocol: 'chat_completions', model: '', contextLength: 8192 }] })); setSelected(id); }}><Plus size={14} /></button></div>
      <div className="field-row"><label>连接名称<input value={connection.name} onChange={e => updateConnection({ name: e.target.value })} /></label><label>接口协议<select value={connection.protocol} onChange={e => updateConnection({ protocol: e.target.value, ...(e.target.value === 'responses' ? { topK: null, minP: null, seed: null } : {}) })}><option value="lmstudio">LM Studio</option><option value="chat_completions">Chat Completions</option><option value="responses">Responses</option></select></label></div>
      <label className="field">服务地址<input aria-label="服务地址" placeholder="http://192.168.1.10:1234" value={connection.baseUrl} onChange={e => updateConnection({ baseUrl: e.target.value })} /><small>支持 http(s) 地址，可填写根地址或 /v1。</small></label>
      <label className="field">API 密钥 <span className="optional">可选</span><div className="input-icon"><KeyRound size={15} /><input type="password" autoComplete="off" placeholder="留空则保留已保存的密钥" value={connection.apiKey || ''} onChange={e => updateConnection({ apiKey: e.target.value })} /></div></label>
      <div className="field-row model-row"><label>模型 ID<input aria-label="模型 ID" placeholder="连接服务后发现模型，或手动填写" value={connection.model} onChange={e => updateConnection({ model: e.target.value, modelFamily: 'auto', thinkingMode: 'auto', temperature: null, topP: null, topK: null, minP: null, seed: null })} /></label><button className="secondary-button" disabled={!!busy} onClick={() => void discover()}>{busy === 'discover' ? <Loader2 className="spin" size={14} /> : <Radio size={14} />}发现模型</button></div>
      {models.length > 0 && <div className="discovered-models">{models.map((model, i) => <button key={i} onClick={() => selectModel(model)}><Cpu size={14} /><span>{String(model.displayName || model.display_name || model.id || model.key)}</span><ArrowUpRight size={12} /></button>)}</div>}
      <div className="field-row"><label>实际加载上下文<input type="number" min={2048} max={262144} value={connection.contextLength} onChange={e => updateConnection({ contextLength: Math.max(2048, Number(e.target.value)) })} /><small>应与推理服务的加载配置一致，不是模型理论上限。</small></label><label className="check-field"><input type="radio" checked={draft.selectedConnectionId === connection.id} onChange={() => setDraft(d => ({ ...d, selectedConnectionId: connection.id }))} />作为默认连接</label></div>
      <div className="field-row"><label>模型系列<select aria-label="模型系列" value={connection.modelFamily || 'auto'} onChange={e => updateConnection({ modelFamily: e.target.value, thinkingMode: 'auto' })}><option value="auto">自动识别（未认证）</option><option value="gemma4">Gemma 4</option><option value="gemma3">Gemma 3</option><option value="gemma3n">Gemma 3n</option><option value="functiongemma">FunctionGemma</option><option value="other">其他模型（含 CodeGemma）</option></select></label><label>推理后端<select aria-label="推理后端" value={connection.backend || 'auto'} onChange={e => updateConnection({ backend: e.target.value, thinkingMode: 'auto', ...(e.target.value !== 'llama_cpp' ? { topK: null, minP: null } : {}) })}><option value="auto">自动识别</option><option value="llama_cpp">llama.cpp</option><option value="lmstudio">LM Studio</option><option value="other">其他兼容服务</option></select></label></div>
      {(connection.modelFamily === 'functiongemma' || (!connection.modelFamily || connection.modelFamily === 'auto') && /function[-_]?gemma/i.test(connection.model)) && <p className="settings-intro">FunctionGemma 专门用于工具选择；复杂参数、多步任务和最终答案需要实测，不应视为通用编程模型。</p>}
      <div className="field-row"><label>思考模式<select aria-label="思考模式" value={connection.thinkingMode || 'auto'} onChange={e => updateConnection({ thinkingMode: e.target.value as Connection['thinkingMode'] })}><option value="auto">由后端控制</option><option value="on">开启（Gemma 4 / llama.cpp）</option><option value="off">关闭（Gemma 4 / llama.cpp）</option></select><small>显式开关仅用于 Gemma 4 的 llama.cpp Chat Completions；其他后端请选自动。</small></label><label>单次最大输出 token<input aria-label="单次最大输出 token" type="number" min={256} max={Math.floor(connection.contextLength / 2)} placeholder="自动" value={connection.maxOutputTokens ?? ''} onChange={e => updateConnection({ maxOutputTokens: e.target.value ? Number(e.target.value) : null })} /><small>包含后端生成的思考内容；上限为已加载上下文的一半。</small></label></div>
      <details className="sampling-settings"><summary>采样参数 · 可选</summary><p className="settings-intro">留空使用服务默认值。显示的是请求值，实际应用情况取决于后端。</p>
        <div className="field-row"><label>Temperature<input aria-label="Temperature" type="number" min={0} max={2} step="0.05" placeholder="服务默认" value={connection.temperature ?? ''} onChange={e => updateConnection({ temperature: e.target.value === '' ? null : Number(e.target.value) })} /></label><label>Top P<input aria-label="Top P" type="number" min={0.01} max={1} step="0.01" placeholder="服务默认" value={connection.topP ?? ''} onChange={e => updateConnection({ topP: e.target.value === '' ? null : Number(e.target.value) })} /></label></div>
        <div className="field-row"><label>Top K<input aria-label="Top K" type="number" min={0} max={65536} placeholder="服务默认" disabled={connection.backend !== 'llama_cpp' || connection.protocol === 'responses'} value={connection.topK ?? ''} onChange={e => updateConnection({ topK: e.target.value === '' ? null : Number(e.target.value) })} /></label><label>Min P<input aria-label="Min P" type="number" min={0} max={1} step="0.01" placeholder="服务默认" disabled={connection.backend !== 'llama_cpp' || connection.protocol === 'responses'} value={connection.minP ?? ''} onChange={e => updateConnection({ minP: e.target.value === '' ? null : Number(e.target.value) })} /></label></div>
        <label className="field">Seed<input aria-label="Seed" type="number" min={0} max={4294967294} placeholder="服务默认" disabled={connection.protocol === 'responses'} value={connection.seed ?? ''} onChange={e => updateConnection({ seed: e.target.value === '' ? null : Number(e.target.value) })} /></label>
        <div className="action-row"><button className="secondary-button" disabled={connection.backend !== 'llama_cpp' || connection.protocol === 'responses'} onClick={() => updateConnection({ temperature: 1, topP: 0.95, topK: 64, minP: 0 })}>使用 Gemma 4 对照采样</button><button className="secondary-button" disabled={connection.backend !== 'llama_cpp' || connection.protocol === 'responses'} onClick={() => updateConnection({ temperature: 0, topP: 1, topK: 1, minP: 0 })}>贪心采样对照</button><button className="secondary-button" onClick={() => updateConnection({ temperature: null, topP: null, topK: null, minP: null, seed: null })}>恢复服务默认</button></div>
        <small>Gemma 4 对照的 Temperature、Top P、Top K 采用 Google 建议值，另将 Min P 设为 0；贪心采样仅供对比，不保证工具调用。Top K / Min P 仅用于 llama.cpp Chat Completions；不同量化或衍生权重需要实测。</small></details>
      <div className="profile-notice"><Cpu size={19} /><div><strong>预设适配目标：Gemma 4 26B A4B · Heretic I1</strong><p>这是适配参考，不代表已连接该模型。适配认证关联具体量化、模板和后端版本；诊断会实际测试函数回灌及图像输入。</p></div></div><div className="action-row"><button className="secondary-button" disabled={!!busy || !connection.model?.trim()} onClick={() => void task('diagnose', async () => {
        const saved = await saveConfiguration();
        if (closedRef.current) return;
        setDraft(saved);
        const savedConnection = saved.connections.find(c => c.id === connection.id);
        if (!savedConnection) throw new Error('保存后的模型连接不存在');
        const operationId = crypto.randomUUID(); diagnosisOperationRef.current = operationId;
        try {
          const result = await api('diagnose', { connection: savedConnection, operationId });
          if (diagnosisOperationRef.current === operationId && !closedRef.current) setReport(result);
        } catch (error) { if (diagnosisOperationRef.current === operationId && !closedRef.current) throw error; }
        finally { if (diagnosisOperationRef.current === operationId) diagnosisOperationRef.current = null; }
      })}>{busy === 'diagnose' ? <Loader2 size={14} className="spin" /> : <ShieldCheck size={15} />}保存并运行诊断</button>{draft.connections.length > 1 && <button className="text-danger" onClick={() => { setDraft(d => ({ ...d, connections: d.connections.filter(c => c.id !== connection.id), selectedConnectionId: d.selectedConnectionId === connection.id ? d.connections.find(c => c.id !== connection.id)!.id : d.selectedConnectionId })); setSelected(draft.connections.find(c => c.id !== connection.id)!.id); }}><Trash2 size={13} />删除连接</button>}</div>{report && <Report value={report} />}</>}
      {tab === 'engine' && <><p className="settings-intro">由 Harness 管理独立的 llama.cpp 进程。选择本地 GGUF 和视觉 projector，模型权重不会自动替换或上传。</p><div className="profile-notice"><Server size={20} /><div><strong>固定运行时 · b12211</strong><p>从官方发布地址安装，核对清单中的 SHA256 后执行。模型兼容性需要单独运行诊断。</p></div></div><div className="field-row"><label>计算后端<select value={backend} onChange={e => setBackend(e.target.value)}><option value="metal">Metal · Apple Silicon / Mac</option><option value="cuda">CUDA · NVIDIA</option><option value="vulkan">Vulkan · 兼容 GPU</option><option value="cpu">CPU</option></select></label><button className="secondary-button" disabled={!!busy} onClick={() => void task('install', async () => { const result = await api('engine_install', { backend }); setEngine(e => ({ ...e, binaryPath: String(result.binaryPath) })); setEngineResult(result); })}>{busy === 'install' ? <Loader2 className="spin" size={14} /> : <ArrowDownToLine size={14} />}安装引擎</button></div>
      {([{ key: 'binaryPath', label: 'llama-server 可执行文件', placeholder: '安装引擎后自动填写，或选择已有版本' }, { key: 'modelPath', label: '模型 GGUF 文件', placeholder: '选择你的量化模型文件' }, { key: 'projectorPath', label: '视觉 projector · mmproj', placeholder: 'Computer Use 图像输入所需，可选' }, { key: 'chatTemplatePath', label: '自定义聊天模板 · 可选', placeholder: '留空使用 GGUF 内嵌模板；选择与模型匹配的 Jinja 文件' }] as const).map(f => <label className="field" key={f.key}>{f.label}<div className="input-with-button"><input value={engine[f.key]} placeholder={f.placeholder} onChange={e => setEngine(v => ({ ...v, [f.key]: e.target.value }))} /><button onClick={() => void path(f.key)} aria-label={`选择${f.label}`}><FolderOpen size={16} /></button></div>{f.key === 'chatTemplatePath' && <small>仅在明确选择后覆盖；启动会记录模板摘要，修改后需重新启动并运行诊断。</small>}</label>)}
      <div className="field-row"><label>上下文<input type="number" min={2048} value={engine.contextLength} onChange={e => setEngine(v => ({ ...v, contextLength: Number(e.target.value) }))} /></label><label>GPU 层数<input type="number" value={engine.gpuLayers} onChange={e => setEngine(v => ({ ...v, gpuLayers: Number(e.target.value) }))} /></label><label>本机端口<input type="number" min={1024} max={65535} value={engine.port} onChange={e => setEngine(v => ({ ...v, port: Number(e.target.value) }))} /></label></div><div className="action-row"><button className="secondary-button" disabled={!!busy || !engine.modelPath} onClick={() => void task('inspect-gguf', async () => setEngineResult(await api('inspect_gguf', { path: engine.modelPath })))}>检查模型元数据</button><button className="primary-button" disabled={!!busy || !engine.binaryPath || !engine.modelPath} onClick={() => void task('start-engine', async () => {
        const result = await api('engine_start', { config: engine }); setEngineResult(result);
        const id = 'managed-llama'; const previous = draft.connections.find(v => v.id === id); const c: Connection = { id, name: '内置 llama.cpp', baseUrl: `http://127.0.0.1:${engine.port}`, protocol: 'chat_completions', model: 'gemma-local', contextLength: engine.contextLength, backend: 'llama_cpp', modelFamily: 'auto', thinkingMode: 'auto', maxOutputTokens: previous?.maxOutputTokens && previous.maxOutputTokens <= engine.contextLength / 2 ? previous.maxOutputTokens : null };
        setDraft(d => ({ ...d, connections: [...d.connections.filter(v => v.id !== id), c], selectedConnectionId: id }));
      })}>{busy === 'start-engine' ? <Loader2 className="spin" size={14} /> : <Cpu size={14} />}启动推理</button><button className="secondary-button" disabled={!!busy} onClick={() => void task('engine-stop', async () => setEngineResult(await api('engine_stop')))}>停止</button><button className="secondary-button" onClick={() => void task('engine-log', async () => setEngineResult(await api('engine_log')))}>查看日志</button></div>{engineResult && <Report value={engineResult} />}</>}
      {tab === 'devices' && <><p className="settings-intro">模型可以在另一台电脑运行；文件、终端与桌面始终由这里选择的设备执行。</p><label className="field">执行设备<select value={draft.deviceId} onChange={e => setDraft(d => ({ ...d, deviceId: e.target.value, workspace: '' }))}><option value="local">本机</option>{draft.devices.map(d => <option key={d.id} value={d.id}>{d.name} · {d.url}</option>)}</select></label><label className="field">执行设备上的工作目录<div className="input-with-button"><input placeholder={draft.deviceId === 'local' ? '选择本机项目目录' : '例如 C:\\Projects\\app 或 /home/user/project'} value={draft.workspace} onChange={e => setDraft(d => ({ ...d, workspace: e.target.value }))} /><button disabled={draft.deviceId !== 'local'} aria-label="选择工作目录" onClick={() => void task('folder', async () => { const path = await choosePath(true); if (path) setDraft(d => ({ ...d, workspace: path })); })}><FolderOpen size={16} /></button></div><small>远端目录由执行设备解释。运行中的会话固定绑定原设备与目录。</small></label>
      {draft.deviceId !== 'local' && <button className="secondary-button" disabled={!!busy} onClick={() => void task('shared-workspace', async () => { const caps = await api('device_capabilities', { deviceId: draft.deviceId }); setDraft(d => ({ ...d, workspace: String(caps.workspace || '') })); setReport(caps); })}>读取设备共享目录</button>}<div className="settings-divider" /><h3><Monitor size={16} />配对远端电脑</h3><p className="settings-intro">在另一台设备安装 Harness，进入执行设备模式，复制其一次性配对串。</p><label className="field">一次性配对串<textarea rows={3} placeholder="粘贴远端设备生成的配对串…" value={invitation} onChange={e => setInvitation(e.target.value)} /></label><button className="secondary-button" disabled={!!busy || !invitation} onClick={() => void task('pair', async () => { await api('pair_device', { invitation }); const snapshot = await api<{ config: Config }>('snapshot'); setDraft(snapshot.config); setInvitation(''); })}>{busy === 'pair' ? <Loader2 className="spin" size={14} /> : <ShieldCheck size={14} />}校验证书并配对</button>
      {draft.devices.map(d => <div className="device-row" key={d.id}><Monitor size={16} /><div><strong>{d.name}</strong><small>{d.url}</small></div><button className="icon-button" aria-label={`移除 ${d.name}`} onClick={() => void task('remove', async () => { await api('remove_device', { deviceId: d.id }); setDraft(v => ({ ...v, devices: v.devices.filter(x => x.id !== d.id), deviceId: v.deviceId === d.id ? 'local' : v.deviceId })); })}><Trash2 size={14} /></button></div>)}
      <div className="settings-divider" /><h3><Radio size={16} />将本机作为执行设备</h3><p className="settings-intro">在已登录的图形会话中运行。开启后，已配对主控可以操作所选工作目录、运行命令和使用桌面工具。</p><label className="field">其他设备可访问的地址<input value={hostUrl} onChange={e => setHostUrl(e.target.value)} /><small>将 127.0.0.1 改为本机局域网 / VPN 地址，端口 45455。无需公网中继。</small></label><div className="action-row"><button className="secondary-button" disabled={!!busy || !draft.workspace} onClick={() => void startHost()}><Radio size={14} />启动与生成配对串</button><button className="secondary-button" onClick={() => void task('host-status', async () => setHostStatus(await api('host_status')))}>状态</button><button className="text-danger" onClick={() => void task('host-stop', async () => { setHostStatus(await api('host_stop')); setHostInvite(''); })}>停止服务</button></div>{hostInvite && <div className="invitation"><label>五分钟有效 · 仅可使用一次</label><textarea readOnly value={hostInvite} /><button onClick={() => void navigator.clipboard.writeText(hostInvite).catch(onError)}>复制配对串</button></div>}{hostStatus && <><Report value={hostStatus} /><div className="action-row"><button className="text-danger" onClick={() => void task('cancel-host', async () => { await api('host_cancel_all'); setHostStatus(await api('host_status')); })}>停止设备上的全部任务</button></div>{Array.isArray(hostStatus.pairings) && (hostStatus.pairings as Json[]).filter(p => !p.revokedAt).map(pair => <div className="device-row" key={String(pair.id)}><Monitor size={14} /><div><strong>已授权主控</strong><small>{String(pair.id)}</small></div><button className="text-danger" onClick={() => void task('revoke-host', async () => { await api('host_revoke', { deviceId: pair.id }); setHostStatus(await api('host_status')); })}>撤销并停止</button></div>)}</>}</>}
      {tab === 'tools' && <><p className="settings-intro">编程工具在选定设备执行。桌面操作还需要操作系统授权；启用功能并不表示权限或模型视觉已经通过验收。</p>{([{ id: 'terminal', icon: <Braces size={19} />, title: '终端与命令', detail: '从工作目录启动系统 Shell，保存输出与退出状态。' }, { id: 'browser', icon: <Globe2 size={19} />, title: '浏览器控制', detail: 'Playwright 独立浏览器，通过当前元素引用观察与操作。' }, { id: 'desktop', icon: <Monitor size={19} />, title: '原生桌面控制', detail: '无障碍树、截图与键鼠；仅向当前选定设备发送动作。' }] as const).map(t => <div className="permission-row" key={t.id}><span>{t.icon}</span><div><strong>{t.title}</strong><p>{t.detail}</p></div><button role="switch" aria-checked={draft.toolPermissions[t.id]} aria-label={t.title} className={`toggle ${draft.toolPermissions[t.id] ? 'on' : ''}`} onClick={() => setDraft(d => ({ ...d, toolPermissions: { ...d.toolPermissions, [t.id]: !d.toolPermissions[t.id] } }))}><i /></button></div>)}<div className="action-row"><button className="secondary-button" disabled={!!busy || !draft.workspace} onClick={() => void task('capabilities', async () => { await saveConfiguration(); setReport(await api('capabilities')); })}><ShieldCheck size={14} />检查设备能力</button></div>{report && <Report value={report} />}<div className="action-row"><button className="secondary-button" disabled={!!busy || !draft.workspace || draft.deviceId !== 'local'} onClick={() => void task('browser-install', async () => { await saveConfiguration(); setReport(await api('browser_setup')); })}>{busy === 'browser-install' ? <Loader2 className="spin" size={14} /> : <ArrowDownToLine size={14} />}安装浏览器组件</button><small className="setup-hint">首次下载 Chromium；远端需在设备本机安装。</small></div>
      <div className="settings-divider" /><h3><Braces size={16} />MCP 扩展 · stdio</h3><p className="settings-intro">MCP 服务按需发现；无法转换的工具会单独标记，不影响其他工具和普通对话。</p>{draft.mcpServers.map(server => <div className="device-row" key={server.id}><Braces size={16} /><div><strong>{server.name}</strong><small>{server.command} {server.args.join(' ')}</small></div><button className={`toggle ${server.enabled ? 'on' : ''}`} role="switch" aria-label={server.name} aria-checked={server.enabled} onClick={() => setDraft(d => ({ ...d, mcpServers: d.mcpServers.map(s => s.id === server.id ? { ...s, enabled: !s.enabled } : s) }))}><i /></button><button className="icon-button" aria-label={`删除 ${server.name}`} onClick={() => setDraft(d => ({ ...d, mcpServers: d.mcpServers.filter(s => s.id !== server.id) }))}><Trash2 size={14} /></button></div>)}<div className="field-row"><label>名称<input value={mcpName} onChange={e => setMcpName(e.target.value)} placeholder="例如 项目知识库" /></label><label>可执行文件<input value={mcpCommand} onChange={e => setMcpCommand(e.target.value)} placeholder="可执行文件绝对路径" /></label></div><label className="field">参数 · JSON 数组<input value={mcpArgs} onChange={e => setMcpArgs(e.target.value)} placeholder={'["server.js"]'} /></label><button className="secondary-button" disabled={!mcpName || !mcpCommand} onClick={() => { try { const args: unknown = JSON.parse(mcpArgs); if (!Array.isArray(args) || args.some(x => typeof x !== 'string')) throw new Error('参数必须是字符串数组'); setDraft(d => ({ ...d, mcpServers: [...d.mcpServers, { id: crypto.randomUUID(), name: mcpName, command: mcpCommand, args, enabled: true }] })); setMcpName(''); setMcpCommand(''); setMcpArgs('[]'); } catch (error) { onError(error); } }}><Plus size={14} />添加扩展</button></>}
    </div><footer><span><ShieldCheck size={12} />{desktop ? '设置仅保存在本机' : '浏览器预览 · 系统功能需桌面应用'}</span><button className="secondary-button" onClick={closeSettings}>取消</button><button className="primary-button" disabled={!!busy} onClick={() => void save()}>{busy === 'save' ? <Loader2 size={14} className="spin" /> : <Check size={14} />}保存设置</button></footer></div>
  </section></div>;
}
function Report({ value }: { value: Json }) { return <details className="diagnostic-report" open><summary><ShieldCheck size={14} />实际诊断结果 <span>{String(value.status || '')}</span></summary><pre>{pretty(value)}</pre></details>; }
