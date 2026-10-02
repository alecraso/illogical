// Right-click menus: one open at a time, closed by Escape, a click
// elsewhere, or choosing an item.

import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";

export type MenuItem =
  | { label: string; run: () => void; danger?: boolean; disabled?: boolean; checked?: boolean }
  | { header: string }
  | "separator";

interface OpenMenu {
  x: number;
  y: number;
  items: MenuItem[];
}

let current: OpenMenu | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

export function openMenu(e: { clientX: number; clientY: number; preventDefault(): void }, items: MenuItem[]) {
  e.preventDefault();
  current = { x: e.clientX, y: e.clientY, items };
  changed();
}

export function closeMenu() {
  if (current) {
    current = null;
    changed();
  }
}

export function MenuLayer() {
  const [, setTick] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    const key = (e: KeyboardEvent) => e.key === "Escape" && closeMenu();
    const down = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) closeMenu();
    };
    window.addEventListener("keydown", key, true);
    window.addEventListener("pointerdown", down, true);
    window.addEventListener("blur", closeMenu);
    return () => {
      listeners.delete(fn);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("pointerdown", down, true);
      window.removeEventListener("blur", closeMenu);
    };
  }, []);

  // Keep the menu on screen.
  useLayoutEffect(() => {
    if (!current || !ref.current) return setPos(null);
    const r = ref.current.getBoundingClientRect();
    setPos({
      left: Math.max(4, Math.min(current.x, window.innerWidth - r.width - 4)),
      top: Math.max(4, Math.min(current.y, window.innerHeight - r.height - 4)),
    });
  }, [current]);

  if (!current) return null;
  return (
    <div
      ref={ref}
      class="menu"
      role="menu"
      style={{
        left: `${pos?.left ?? current.x}px`,
        top: `${pos?.top ?? current.y}px`,
        visibility: pos ? "visible" : "hidden",
      }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {current.items.map((item, i) =>
        item === "separator" ? (
          <div key={i} class="menu-sep" />
        ) : "header" in item ? (
          <div key={i} class="menu-header">
            {item.header}
          </div>
        ) : (
          <button
            key={i}
            role={item.checked === undefined ? "menuitem" : "menuitemradio"}
            aria-checked={item.checked}
            class={item.danger ? "menu-item danger" : "menu-item"}
            disabled={item.disabled}
            onClick={() => {
              closeMenu();
              item.run();
            }}
          >
            {item.checked !== undefined && <span class="menu-check">{item.checked ? "●" : ""}</span>}
            {item.label}
          </button>
        ),
      )}
    </div>
  );
}

// ---------------------------------------------------------------- prompt

interface OpenPrompt {
  title: string;
  value: string;
  placeholder?: string;
  done: (value: string | null) => void;
}

let prompt: OpenPrompt | null = null;
const promptListeners = new Set<() => void>();

/** Ask for one line of text (no browser dialogs: they block everything). */
export function askText(title: string, value: string, placeholder?: string): Promise<string | null> {
  return new Promise((resolve) => {
    prompt = { title, value, placeholder, done: resolve };
    promptListeners.forEach((fn) => fn());
  });
}

export function PromptLayer() {
  const [, setTick] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    promptListeners.add(fn);
    return () => {
      promptListeners.delete(fn);
    };
  }, []);
  useLayoutEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, [prompt]);
  if (!prompt) return null;
  const p = prompt;
  const finish = (v: string | null) => {
    prompt = null;
    promptListeners.forEach((fn) => fn());
    p.done(v);
  };
  return (
    <div
      class="prompt-backdrop"
      role="dialog"
      aria-label={p.title}
      onPointerDown={(e) => e.target === e.currentTarget && finish(null)}
    >
      <form
        class="prompt"
        aria-label={p.title}
        onSubmit={(e) => {
          e.preventDefault();
          finish(input.current?.value ?? "");
        }}
      >
        <label>
          {p.title}
          <input
            ref={input}
            value={p.value}
            // Kept as typed: the page re-renders as panes change.
            onInput={(e) => (p.value = (e.target as HTMLInputElement).value)}
            placeholder={p.placeholder}
            onKeyDown={(e) => e.key === "Escape" && finish(null)}
          />
        </label>
        <div class="prompt-buttons">
          <button type="button" onClick={() => finish(null)}>
            Cancel
          </button>
          <button type="submit" class="primary">
            OK
          </button>
        </div>
      </form>
    </div>
  );
}
