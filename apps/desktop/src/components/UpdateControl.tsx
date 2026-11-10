import { useCallback, useEffect, useRef, useState } from 'react';
import { getVersion } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { ArrowDownToLine, Loader2, RefreshCw, X } from 'lucide-react';
import { desktop } from '../bridge';

const CHECK_INTERVAL = 6 * 60 * 60 * 1000;
type Phase = 'idle' | 'checking' | 'current' | 'available' | 'downloading' | 'downloaded' | 'installing' | 'error' | 'installed';

/** One controller is shared by the background check, status bar, and settings. */
export function useAppUpdates(beginInstall: (fromSettings: boolean) => string | null, endInstall: () => void) {
  const [open, setOpen] = useState(false);
  const [phase, setPhase] = useState<Phase>('idle');
  const [version, setVersion] = useState('');
  const [message, setMessage] = useState('');
  const [progress, setProgress] = useState<number | null>(null);
  const updateRef = useRef<Update | null>(null);
  const downloadedRef = useRef(false);
  const checkingRef = useRef(false);
  const showCheckRef = useRef(false);
  const workingRef = useRef(false);
  const mountedRef = useRef(false);
  const callbacksRef = useRef({ beginInstall, endInstall });
  callbacksRef.current = { beginInstall, endInstall };

  const checkForUpdate = useCallback(async (manual: boolean) => {
    if (!desktop || workingRef.current || (!manual && updateRef.current)) return;
    if (manual) { showCheckRef.current = true; setOpen(true); setPhase('checking'); setMessage(''); }
    if (checkingRef.current) return;
    checkingRef.current = true;
    try {
      const update = await check({ timeout: 15000 });
      if (!mountedRef.current) { if (update) await update.close().catch(() => {}); return; }
      const previous = updateRef.current;
      updateRef.current = update;
      downloadedRef.current = false;
      if (previous) void previous.close().catch(() => {});
      if (update) {
        setVersion(update.version); setPhase('available'); setMessage(''); setOpen(true);
      } else {
        setVersion(''); setPhase('current'); setMessage('');
      }
    } catch (error) {
      if (mountedRef.current && showCheckRef.current) { setPhase('error'); setMessage(error instanceof Error ? error.message : String(error)); }
    } finally { checkingRef.current = false; showCheckRef.current = false; }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    if (!desktop) return () => { mountedRef.current = false; };
    const initial = setTimeout(() => void checkForUpdate(false), 3000);
    const interval = setInterval(() => void checkForUpdate(false), CHECK_INTERVAL);
    return () => {
      mountedRef.current = false;
      clearTimeout(initial); clearInterval(interval);
      const update = updateRef.current; updateRef.current = null;
      if (update && !workingRef.current) void update.close().catch(() => {});
    };
  }, [checkForUpdate]);

  async function installDownloaded(update: Update, fromSettings: boolean) {
    const reason = callbacksRef.current.beginInstall(fromSettings);
    if (reason) { setPhase('downloaded'); setMessage(reason); setOpen(true); return; }
    let installed = false;
    let prepared = false;
    try {
      setPhase('installing'); setMessage('');
      await invoke('prepare_app_update');
      prepared = true;
      await update.install();
      installed = true;
      await relaunch();
      if (mountedRef.current) { setPhase('installed'); setMessage('更新已安装，请退出并重新打开应用。'); }
    } catch (error) {
      let cleanupError = false;
      if (prepared && !installed) {
        try { await invoke('cancel_app_update'); } catch { cleanupError = true; }
      }
      if (mountedRef.current) {
        setPhase(installed ? 'installed' : 'downloaded');
        setMessage(installed ? '更新已安装，但未能自动重启，请手动重新打开应用。' : `安装失败：${error instanceof Error ? error.message : String(error)}${cleanupError ? '；请重新打开应用后重试。' : prepared ? '；本地推理服务可能需要重新启动。' : ''}`);
      }
    } finally { callbacksRef.current.endInstall(); }
  }

  async function downloadAndInstall(fromSettings: boolean) {
    const update = updateRef.current;
    if (!update || workingRef.current) return;
    workingRef.current = true; setMessage('');
    try {
      if (!downloadedRef.current) {
        setPhase('downloading'); setProgress(null);
        let downloaded = 0; let total: number | undefined;
        await update.download(event => {
          if (event.event === 'Started') total = event.data.contentLength;
          else if (event.event === 'Progress') { downloaded += event.data.chunkLength; if (total && mountedRef.current) setProgress(Math.min(100, Math.round(downloaded / total * 100))); }
        }, { timeout: 600000 });
        downloadedRef.current = true;
      }
      if (mountedRef.current) await installDownloaded(update, fromSettings);
    } catch (error) {
      if (mountedRef.current) { setPhase('available'); setMessage(`下载失败：${error instanceof Error ? error.message : String(error)}`); }
    } finally { workingRef.current = false; }
  }
  return { open, setOpen, phase, version, message, progress, checkForUpdate, downloadAndInstall };
}
export type UpdateController = ReturnType<typeof useAppUpdates>;

function UpdateStatus({ update, fromSettings }: { update: UpdateController; fromSettings: boolean }) {
  const { phase, version, message, progress } = update;
  return <>
    {phase === 'idle' && <p>可手动检查更新，应用也会定期检查。</p>}
    {phase === 'checking' && <p role="status"><Loader2 size={13} className="spin" /> 正在检查更新…</p>}
    {phase === 'current' && <p role="status">当前已是最新版本。</p>}
    {(phase === 'available' || phase === 'downloading' || phase === 'downloaded' || phase === 'installing') && <><p>发现新版本 <strong>{version}</strong></p><small>安装时应用会重启；请先保存编辑内容并结束正在运行的任务。</small></>}
    {phase === 'downloading' && <p role="status"><Loader2 size={13} className="spin" /> 正在下载{progress === null ? '…' : ` · ${progress}%`}</p>}
    {phase === 'installing' && <p role="status"><Loader2 size={13} className="spin" /> 正在安装并重启…</p>}
    {message && <p className="update-message" role="status">{message}</p>}
    {(phase === 'available' || phase === 'downloaded') && <button className="primary-button" type="button" onClick={() => void update.downloadAndInstall(fromSettings)}><ArrowDownToLine size={14} />{phase === 'available' ? '下载并安装' : '安装并重启'}</button>}
  </>;
}

export default function UpdateControl({ update }: { update: UpdateController }) {
  const { open, setOpen, phase } = update;
  return <div className="update-anchor">
    <button type="button" className="update-trigger" aria-expanded={open} onClick={() => { if (phase === 'idle' || phase === 'current' || phase === 'error') void update.checkForUpdate(true); else setOpen(value => !value); }}>
      <RefreshCw size={12} />{phase === 'available' || phase === 'downloaded' ? '发现更新' : phase === 'downloading' ? '正在下载更新' : '检查更新'}
    </button>
    {open && <section className="update-panel" aria-label="应用更新" role="region">
      <header><strong>应用更新</strong><button className="icon-button" type="button" aria-label="关闭更新提示" disabled={phase === 'installing'} onClick={() => setOpen(false)}><X size={14} /></button></header>
      <UpdateStatus update={update} fromSettings={false} />
      {(phase === 'error' || phase === 'current') && <button className="secondary-button" type="button" onClick={() => void update.checkForUpdate(true)}>重新检查</button>}
    </section>}
  </div>;
}

export function VersionControl({ update }: { update: UpdateController }) {
  const [currentVersion, setCurrentVersion] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    if (desktop) void getVersion().then(value => { if (active) setCurrentVersion(value); }).catch(() => { if (active) setCurrentVersion('无法读取'); });
    return () => { active = false; };
  }, []);
  return <div className="version-control">
    <section className="version-card" aria-label="当前版本">
      <h3>当前版本</h3><p className="version-number">{desktop ? (currentVersion === '无法读取' ? currentVersion : currentVersion ? `v${currentVersion}` : '正在读取…') : '桌面应用中可查看'}</p>
      <small>这里显示的是当前运行的应用版本。</small>
    </section>
    <section className="version-card" aria-label="检查应用更新">
      <h3>检查更新</h3>
      {desktop ? <><UpdateStatus update={update} fromSettings />
        <button className="secondary-button" type="button" disabled={update.phase === 'checking' || update.phase === 'downloading' || update.phase === 'installing'} onClick={() => void update.checkForUpdate(true)}><RefreshCw size={14} />检查更新</button>
      </> : <p>请在桌面应用中检查更新。</p>}
    </section>
  </div>;
}
