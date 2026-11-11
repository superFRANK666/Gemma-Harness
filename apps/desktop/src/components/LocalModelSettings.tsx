import { ArrowDownToLine, Cpu, FolderOpen, Loader2, RefreshCw, Server, ShieldCheck } from 'lucide-react';
import type { EngineConfiguration, Json } from '../types';
import { pretty } from '../bridge';

type PathField = 'binaryPath' | 'modelPath' | 'projectorPath' | 'chatTemplatePath';
interface Props {
  config: EngineConfiguration; backend: string; busy: string; status: Json | null; result: Json | null;
  onChange: (value: Partial<EngineConfiguration>) => void; onBackendChange: (value: string) => void;
  onPath: (field: PathField) => void; onInstall: () => void; onStart: () => void; onStop: () => void;
  onInspect: () => void; onLog: () => void; onRefresh: () => void;
}
export default function LocalModelSettings({ config, backend, busy, status, result, onChange, onBackendChange, onPath, onInstall, onStart, onStop, onInspect, onLog, onRefresh }: Props) {
  const running = status?.running === true;
  const locked = !!busy || !status || running;
  return <section className="local-model-settings" aria-label="本地模型">
    <div className="local-model-status"><Server size={17} /><div><strong>{!status ? '正在读取引擎状态' : running ? '推理进程运行中' : '本地推理已停止'}</strong><small>{running ? '修改模型或加载参数前，请先停止推理。模型是否就绪可查看日志或运行诊断。' : '选择 GGUF 并启动推理，再使用下方的共用设置运行诊断。'}</small></div><button className="icon-button" aria-label="刷新引擎状态" onClick={onRefresh} disabled={!!busy}><RefreshCw size={14} /></button></div>
    <fieldset className="model-configuration" disabled={locked}>
      <EnginePath field="modelPath" label="模型 GGUF 文件" placeholder="选择你的量化模型文件" value={config.modelPath} onChange={onChange} onPath={onPath} />
      <EnginePath field="projectorPath" label="视觉 projector · mmproj · 可选" placeholder="图像输入需要与模型匹配的 projector" value={config.projectorPath} onChange={onChange} onPath={onPath} />
      <details className="sampling-settings"><summary>本地运行高级设置</summary>
        <div className="field-row"><label>计算后端<select value={backend} onChange={e => onBackendChange(e.target.value)}><option value="metal">Metal · Mac</option><option value="cuda">CUDA · NVIDIA</option><option value="vulkan">Vulkan · 兼容 GPU</option><option value="cpu">CPU</option></select></label><button className="secondary-button" onClick={onInstall}>{busy === 'install' ? <Loader2 className="spin" size={14} /> : <ArrowDownToLine size={14} />}{config.binaryPath ? '重新安装引擎' : '安装引擎'}</button></div>
        <EnginePath field="binaryPath" label="llama-server 可执行文件" placeholder="安装引擎后自动填写，或选择已有版本" value={config.binaryPath} onChange={onChange} onPath={onPath} />
        <EnginePath field="chatTemplatePath" label="自定义聊天模板 · 可选" placeholder="留空使用 GGUF 内嵌模板" value={config.chatTemplatePath || ''} onChange={onChange} onPath={onPath} />
        <div className="field-row"><label>GPU 层数<input type="number" min={-1} value={config.gpuLayers} onChange={e => onChange({ gpuLayers: Number(e.target.value) })} /></label><label>本机端口<input type="number" min={1024} max={65535} value={config.port} onChange={e => onChange({ port: Number(e.target.value) })} /></label></div>
        <small>安装固定运行时 b12211，并核对 SHA256。选择自定义模板后将覆盖内嵌模板；启动会记录模板摘要，兼容性需单独诊断。</small>
      </details>
    </fieldset>
    {!config.binaryPath && <p className="local-model-hint">首次使用请展开高级设置安装引擎，或选择已有的 llama-server。</p>}
    <div className="action-row"><button className="primary-button" disabled={!!busy || !status || running || !config.binaryPath || !config.modelPath} onClick={onStart}>{busy === 'start-engine' ? <Loader2 className="spin" size={14} /> : <Cpu size={14} />}启动推理</button><button className="secondary-button" disabled={!!busy || !running} onClick={onStop}>停止推理</button><button className="secondary-button" disabled={!!busy || !config.modelPath} onClick={onInspect}>检查模型元数据</button><button className="secondary-button" disabled={!!busy} onClick={onLog}>查看日志</button></div>
    {result && <details className="diagnostic-report"><summary><ShieldCheck size={14} />引擎操作结果</summary><pre>{pretty(result)}</pre></details>}
  </section>;
}
function EnginePath({ field, label, placeholder, value, onChange, onPath }: { field: PathField; label: string; placeholder: string; value: string; onChange: Props['onChange']; onPath: Props['onPath'] }) {
  return <label className="field">{label}<div className="input-with-button"><input value={value} placeholder={placeholder} onChange={e => onChange({ [field]: e.target.value })} /><button onClick={() => onPath(field)} aria-label={`选择${label}`}><FolderOpen size={16} /></button></div></label>;
}
