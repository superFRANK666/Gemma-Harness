import type { AppearanceConfig } from './types';

export const DEFAULT_APPEARANCE: AppearanceConfig = {
  theme: 'dark', accent: 'sage', fontFamily: 'system', fontSize: 13,
  codeFontFamily: 'system', codeFontSize: 12,
};

const uiFonts = {
  system: '-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif',
  sans: 'Arial, "Helvetica Neue", "Noto Sans CJK SC", "PingFang SC", "Microsoft YaHei", sans-serif',
  serif: 'Georgia, "Times New Roman", "Songti SC", "Noto Serif CJK SC", SimSun, serif',
};
const codeFonts = {
  system: 'ui-monospace, "SFMono-Regular", Consolas, "Liberation Mono", monospace',
  mono: 'Menlo, Monaco, "Cascadia Mono", "DejaVu Sans Mono", "Courier New", monospace',
};
function option<T extends string>(value: unknown, options: readonly T[], fallback: T): T {
  return options.includes(value as T) ? value as T : fallback;
}
function size(value: unknown, min: number, max: number, fallback: number) {
  return typeof value === 'number' && Number.isFinite(value) ? Math.min(max, Math.max(min, Math.round(value))) : fallback;
}
export function normalizeAppearance(value?: unknown): AppearanceConfig {
  const input = value && typeof value === 'object' ? value as Record<string, unknown> : {};
  return {
    theme: option(input.theme, ['dark', 'light', 'system'], DEFAULT_APPEARANCE.theme),
    accent: option(input.accent, ['sage', 'blue', 'violet', 'amber'], DEFAULT_APPEARANCE.accent),
    fontFamily: option(input.fontFamily, ['system', 'sans', 'serif'], DEFAULT_APPEARANCE.fontFamily),
    fontSize: size(input.fontSize, 11, 18, DEFAULT_APPEARANCE.fontSize),
    codeFontFamily: option(input.codeFontFamily, ['system', 'mono'], DEFAULT_APPEARANCE.codeFontFamily),
    codeFontSize: size(input.codeFontSize, 10, 18, DEFAULT_APPEARANCE.codeFontSize),
  };
}

// The workbench and Settings share one theme owner. An old effect's cleanup can
// remove only its own listener, so closing a preview cannot detach a newer one.
let activeCleanup: (() => void) | undefined;
export function applyAppearance(value?: unknown): () => void {
  activeCleanup?.();
  const appearance = normalizeAppearance(value);
  const root = document.documentElement;
  const media = window.matchMedia('(prefers-color-scheme: dark)');
  const updateTheme = () => { root.dataset.theme = appearance.theme === 'system' ? (media.matches ? 'dark' : 'light') : appearance.theme; };
  root.dataset.themePreference = appearance.theme;
  root.dataset.accent = appearance.accent;
  root.style.setProperty('--font-ui', uiFonts[appearance.fontFamily]);
  root.style.setProperty('--font-code', codeFonts[appearance.codeFontFamily]);
  root.style.setProperty('--ui-font-size', `${appearance.fontSize}px`);
  root.style.setProperty('--ui-font-scale', String(appearance.fontSize / DEFAULT_APPEARANCE.fontSize));
  root.style.setProperty('--code-font-size', `${appearance.codeFontSize}px`);
  updateTheme();
  if (appearance.theme === 'system') media.addEventListener('change', updateTheme);
  const dispose = () => { media.removeEventListener('change', updateTheme); };
  activeCleanup = dispose;
  return () => { dispose(); if (activeCleanup === dispose) activeCleanup = undefined; };
}
