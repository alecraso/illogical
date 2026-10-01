// Right-click menus: one open at a time, closed by Escape, a click
// elsewhere, or choosing an item.

import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";

export type MenuItem = { label: string; run: () => void; danger?: boolean; disabled?: boolean } | "separator";

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
        ) : (
          <button
            key={i}
            role="menuitem"
            class={item.danger ? "menu-item danger" : "menu-item"}
            disabled={item.disabled}
            onClick={() => {
              closeMenu();
              item.run();
            }}
          >
            {item.label}
          </button>
        ),
      )}
    </div>
  );
}
