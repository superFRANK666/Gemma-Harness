import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Check, ChevronDown } from 'lucide-react';
import type { KeyboardEvent } from 'react';

const presets = [2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144];
const formatLength = (value: number) => `${value % 1024 === 0 ? `${value / 1024}K · ` : ''}${value.toLocaleString('en-US')} token`;

export default function ContextLengthSelect({ value, disabled, onChange }: { value: number; disabled: boolean; onChange: (value: number) => void }) {
  const id = useId();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [position, setPosition] = useState({ top: 0, left: 0, width: 0 });
  const options = [...new Set([...presets, value])].sort((a, b) => a - b);

  function showOptions() {
    if (disabled) return;
    setActiveIndex(options.indexOf(value));
    setOpen(true);
  }
  function choose(next: number) {
    if (disabled) return;
    onChange(next);
    setOpen(false);
  }
  useEffect(() => { if (disabled) setOpen(false); }, [disabled]);
  useLayoutEffect(() => {
    if (!open || !triggerRef.current || !menuRef.current) return;
    const anchor = triggerRef.current.getBoundingClientRect();
    const height = menuRef.current.getBoundingClientRect().height;
    const top = anchor.bottom + 6 + height <= window.innerHeight - 8 ? anchor.bottom + 6 : Math.max(8, anchor.top - height - 6);
    setPosition({ top, left: anchor.left, width: anchor.width });
  }, [open, options.length]);
  useLayoutEffect(() => {
    if (open) listRef.current?.children[activeIndex]?.scrollIntoView({ block: 'nearest' });
  }, [open, activeIndex]);
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!triggerRef.current?.contains(event.target as Node) && !menuRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const scroll = (event: Event) => { if (!menuRef.current?.contains(event.target as Node)) setOpen(false); };
    const resize = () => setOpen(false);
    document.addEventListener('pointerdown', outside);
    document.addEventListener('scroll', scroll, true);
    window.addEventListener('resize', resize);
    return () => {
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('scroll', scroll, true);
      window.removeEventListener('resize', resize);
    };
  }, [open]);

  function keyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (!open) showOptions();
      else setActiveIndex(index => Math.max(0, Math.min(options.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1))));
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      if (open) choose(options[activeIndex]); else showOptions();
    } else if (open && (event.key === 'Home' || event.key === 'End')) {
      event.preventDefault();
      setActiveIndex(event.key === 'Home' ? 0 : options.length - 1);
    } else if (open && event.key === 'Escape') {
      event.preventDefault(); event.stopPropagation(); setOpen(false);
    } else if (event.key === 'Tab') setOpen(false);
  }

  return <>
    <button ref={triggerRef} type="button" className="context-length-trigger" role="combobox" aria-label="上下文长度" aria-haspopup="listbox" aria-expanded={open} aria-controls={open ? id : undefined} aria-activedescendant={open ? `${id}-option-${activeIndex}` : undefined} disabled={disabled} onClick={() => open ? setOpen(false) : showOptions()} onKeyDown={keyDown} onBlur={() => setOpen(false)}>
      <span>{formatLength(value)}</span><ChevronDown size={14} />
    </button>
    {open && !disabled && createPortal(<div ref={menuRef} className="context-length-menu" style={position}>
      <div ref={listRef} id={id} className="context-length-options" role="listbox" aria-label="上下文长度选项" onMouseDown={event => event.preventDefault()}>
        {options.map((option, index) => <div id={`${id}-option-${index}`} key={option} className="context-length-option" role="option" aria-selected={option === value} data-active={index === activeIndex || undefined} onClick={() => choose(option)}>
          <span>{formatLength(option)}</span>{option === value && <Check size={14} />}
        </div>)}
      </div>
    </div>, document.body)}
  </>;
}
