import { ArrowUpRight, Check, Code2, Globe2, ShieldCheck } from 'lucide-react';
import { Mark } from './Mark';

const starterCards = [
  {
    icon: <Code2 size={19} />,
    title: '理解与修改代码',
    sub: '检查项目、编辑文件、运行测试',
    prompt: '请先检查这个项目的结构和说明，概括当前架构，再给出最值得改进的地方。',
  },
  {
    icon: <Globe2 size={19} />,
    title: '操作浏览器与桌面',
    sub: '观察界面、执行动作、验证结果',
    prompt: '请检查当前设备的 Computer Use 能力和权限，先观察状态，暂时不要执行点击。',
  },
];

export default function WelcomeView({
  modelConfigured,
  workspaceConfigured,
  onOpenModels,
  onSelectWorkspace,
  onChoosePrompt,
}: {
  modelConfigured: boolean;
  workspaceConfigured: boolean;
  onOpenModels: () => void;
  onSelectWorkspace: () => void;
  onChoosePrompt: (prompt: string) => void;
}) {
  return (
    <div className="welcome">
      <div className="welcome-emblem">
        <Mark />
      </div>
      <div className="eyebrow">YOUR MODEL. YOUR WORKSPACE.</div>
      <h1>
        让本地模型，
        <br />
        <span>真正开始工作。</span>
      </h1>
      <p className="welcome-description">
        为 Gemma 构建的编程与电脑操作工作台。
        <br />
        连接你的模型，把想法交给工具。
      </p>
      <div className="setup-path">
        <button
          type="button"
          className={modelConfigured ? 'done' : ''}
          onClick={onOpenModels}
        >
          <span>{modelConfigured ? <Check size={12} /> : '1'}</span>
          配置模型
          <ArrowUpRight size={14} />
        </button>
        <i aria-hidden="true" />
        <button
          type="button"
          className={workspaceConfigured ? 'done' : ''}
          onClick={onSelectWorkspace}
        >
          <span>{workspaceConfigured ? <Check size={12} /> : '2'}</span>
          选择项目
          <ArrowUpRight size={14} />
        </button>
      </div>
      <div className="starter-grid">
        {starterCards.map(card => (
          <button
            type="button"
            className="starter-card"
            key={card.title}
            onClick={() => onChoosePrompt(card.prompt)}
          >
            <span className="starter-icon">{card.icon}</span>
            <strong>{card.title}</strong>
            <small>{card.sub}</small>
            <ArrowUpRight size={15} />
          </button>
        ))}
      </div>
      <div className="local-note">
        <ShieldCheck size={13} /> 本地保存会话 · 自主选择推理与执行位置
      </div>
    </div>
  );
}
