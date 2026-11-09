import { Check, Monitor, Moon, RotateCcw, Sun } from 'lucide-react';
import type { AppearanceConfig } from '../types';
import { DEFAULT_APPEARANCE } from '../appearance';

export default function AppearanceSettings({ value, onChange }: { value: AppearanceConfig; onChange: (value: AppearanceConfig) => void }) {
  function update(next: Partial<AppearanceConfig>) { onChange({ ...value, ...next }); }
  return <div className="appearance-settings">
    <p className="settings-intro">调整整个工作台的配色与阅读字体。更改即时预览，保存后在此设备生效；取消会恢复已保存的外观。</p>
    <fieldset className="appearance-section"><legend>主题</legend><div className="theme-options">
      {([{ id: 'light', label: '浅色', icon: <Sun size={17} /> }, { id: 'dark', label: '深色', icon: <Moon size={17} /> }, { id: 'system', label: '跟随系统', icon: <Monitor size={17} /> }] as const).map(theme => <button key={theme.id} type="button" aria-pressed={value.theme === theme.id} onClick={() => update({ theme: theme.id })}><span className={`theme-thumbnail theme-thumbnail-${theme.id}`}><i /><b /><em /></span><span>{theme.icon}{theme.label}{value.theme === theme.id && <Check size={13} />}</span></button>)}
    </div></fieldset>
    <fieldset className="appearance-section"><legend>强调色</legend><div className="accent-options">
      {([{ id: 'sage', label: '鼠尾草绿' }, { id: 'blue', label: '海蓝' }, { id: 'violet', label: '紫罗兰' }, { id: 'amber', label: '琥珀' }] as const).map(accent => <button key={accent.id} type="button" aria-pressed={value.accent === accent.id} onClick={() => update({ accent: accent.id })}><i className={`accent-swatch accent-swatch-${accent.id}`}>{value.accent === accent.id && <Check size={13} />}</i>{accent.label}</button>)}
    </div></fieldset>
    <div className="appearance-section"><h3>字体与字号</h3><div className="field-row"><label>界面字体<select aria-label="界面字体" value={value.fontFamily} onChange={e => update({ fontFamily: e.target.value as AppearanceConfig['fontFamily'] })}><option value="system">系统默认</option><option value="sans">无衬线</option><option value="serif">衬线</option></select></label><label>界面字号<select aria-label="界面字号" value={value.fontSize} onChange={e => update({ fontSize: Number(e.target.value) })}>{Array.from({ length: 8 }, (_, i) => i + 11).map(size => <option key={size} value={size}>{size} px</option>)}</select></label></div>
    <div className="field-row"><label>代码字体<select aria-label="代码字体" value={value.codeFontFamily} onChange={e => update({ codeFontFamily: e.target.value as AppearanceConfig['codeFontFamily'] })}><option value="system">系统等宽</option><option value="mono">经典等宽</option></select></label><label>代码字号<select aria-label="代码字号" value={value.codeFontSize} onChange={e => update({ codeFontSize: Number(e.target.value) })}>{Array.from({ length: 9 }, (_, i) => i + 10).map(size => <option key={size} value={size}>{size} px</option>)}</select></label></div><p className="appearance-hint">使用设备已有字体，无需下载。代码字体同时用于代码块、文件编辑器与终端。</p></div>
    <section className="appearance-preview" aria-label="外观预览"><div><span className="status-dot configured" />阅读预览 <small>Gemma Harness</small></div><p>让思考与实现自然衔接。<span> Build with your local model.</span></p><pre><code>{'const message = "Hello, Gemma";\nconsole.log(message);'}</code></pre><a href="#appearance-preview" onClick={e => e.preventDefault()}>强调色与链接</a></section>
    <button className="secondary-button appearance-reset" type="button" onClick={() => onChange({ ...DEFAULT_APPEARANCE })}><RotateCcw size={13} />恢复默认外观</button>
  </div>;
}
