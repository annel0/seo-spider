import { useEffect, useRef, useState } from "react";
import type { TabColumn } from "../api";
import { Icon } from "./Icon";

const key = (tab: string) => `seo-spider.hidden-columns.${tab}`;

/// The columns a tab hides, as this browser remembers them.
export function hiddenColumns(tab: string): string[] {
  try {
    const kept = JSON.parse(window.localStorage.getItem(key(tab)) ?? "[]");
    return Array.isArray(kept) ? kept.filter((item) => typeof item === "string") : [];
  } catch {
    return [];
  }
}

export function keepHiddenColumns(tab: string, hidden: string[]) {
  try {
    window.localStorage.setItem(key(tab), JSON.stringify(hidden));
  } catch {
    // Private windows may refuse: the choice lasts this session only.
  }
}

/// Which of a tab's columns show; the address always does.
export function ColumnPicker({ columns, hidden, onChange }: { columns: TabColumn[]; hidden: string[]; onChange: (hidden: string[]) => void }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const away = (event: MouseEvent) => {
      if (box.current && !box.current.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [open]);

  const toggle = (column: string) =>
    onChange(hidden.includes(column) ? hidden.filter((item) => item !== column) : [...hidden, column]);
  const count = columns.filter((column) => hidden.includes(column.key)).length;

  return (
    <div className="column-picker" ref={box}>
      <button className={open ? "on" : undefined} onClick={() => setOpen((shown) => !shown)} title="Какие колонки показывать">
        <Icon name="table" /> Колонки
        {count > 0 && <span className="badge">−{count}</span>}
      </button>
      {open && (
        <div className="column-menu" role="menu">
          {columns
            .filter((column) => column.key !== "url")
            .map((column) => (
              <label key={column.key}>
                <input type="checkbox" checked={!hidden.includes(column.key)} onChange={() => toggle(column.key)} />
                {column.title}
              </label>
            ))}
          {count > 0 && (
            <button className="ghost" onClick={() => onChange([])}>
              Показать все
            </button>
          )}
        </div>
      )}
    </div>
  );
}
