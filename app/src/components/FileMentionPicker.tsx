import React, { useEffect, useRef, useState } from "react";
import { I } from "./icons";
import * as ipc from "../lib/ipc";
import type { ProjectFileEntry } from "../lib/ipc";

interface FileMentionPickerProps {
  open: boolean;
  /** Text after the `@` up to the cursor. May be empty. */
  query: string;
  /** Active task's project directory. Picker is unmounted when null. */
  projectPath: string | null;
  /** Selected index, owned by parent so Enter/Arrow keys can drive it. */
  selectedIndex: number;
  onSetSelection: (n: number) => void;
  /** Called when a row is picked (by click or by Enter on the parent). */
  onPick: (path: string) => void;
  /** Fires whenever the items list changes (load complete / cleared). */
  onItemsChange: (items: ProjectFileEntry[]) => void;
  /** When true (default), picker opens above the trigger. When false, opens below. */
  dropUp?: boolean;
}

/** Highlight occurrences of `q` (case-insensitive) within `text`. */
function highlightMatch(text: string, q: string): React.ReactNode {
  if (!q) return text;
  const lower = text.toLowerCase();
  const qLower = q.toLowerCase();
  const idx = lower.indexOf(qLower);
  if (idx < 0) return text;
  return (
    <>
      {text.slice(0, idx)}
      <span className="text-honey-400">{text.slice(idx, idx + q.length)}</span>
      {text.slice(idx + q.length)}
    </>
  );
}

export function FileMentionPicker(props: FileMentionPickerProps) {
  const {
    open,
    query,
    projectPath,
    selectedIndex,
    onSetSelection,
    onPick,
    onItemsChange,
    dropUp,
  } = props;

  const [items, setItems] = useState<ProjectFileEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestSeqRef = useRef(0);
  const onItemsChangeRef = useRef(onItemsChange);
  onItemsChangeRef.current = onItemsChange;

  // Debounced fetch with latest-call-wins via requestSeqRef.
  useEffect(() => {
    if (!open || !projectPath) {
      // Closed / no project: clear state and notify parent.
      setItems([]);
      setError(null);
      setLoading(false);
      onItemsChangeRef.current([]);
      // Invalidate any in-flight responses on close.
      requestSeqRef.current++;
      return;
    }

    const seq = ++requestSeqRef.current;
    const timer = setTimeout(async () => {
      setLoading(true);
      try {
        const results = await ipc.listProjectFiles(projectPath, query, 50);
        if (requestSeqRef.current !== seq) return;
        setItems(results);
        setError(null);
        onItemsChangeRef.current(results);
      } catch (e) {
        if (requestSeqRef.current !== seq) return;
        setError(String(e));
        setItems([]);
        onItemsChangeRef.current([]);
      } finally {
        if (requestSeqRef.current === seq) setLoading(false);
      }
    }, 150);

    return () => {
      clearTimeout(timer);
      // Invalidate any in-flight IPC response that arrives after cleanup.
      requestSeqRef.current++;
    };
  }, [open, projectPath, query]);

  // Clamp selectedIndex when items change.
  useEffect(() => {
    if (items.length === 0) {
      if (selectedIndex !== 0) onSetSelection(0);
      return;
    }
    const clamped = Math.max(0, Math.min(selectedIndex, items.length - 1));
    if (clamped !== selectedIndex) onSetSelection(clamped);
  }, [items.length, selectedIndex, onSetSelection]);

  if (!open) return null;

  return (
    <div
      role="listbox"
      aria-label="File mention picker"
      // Same focus-theft prevention pattern as ContextMenu.tsx — clicking
      // inside the picker must not blur the textarea.
      onMouseDown={(e) => e.preventDefault()}
      className={`absolute ${dropUp !== false ? "bottom-full mb-1" : "top-full mt-1"} left-0 right-0 max-w-[420px] bg-ink-800 border border-line rounded-lg shadow-2xl overflow-hidden z-50`}
      style={{ maxHeight: "18rem" }}
    >
      <div className="max-h-72 overflow-y-auto">
        {error ? (
          <div className="px-3 py-2 text-[12px] text-red-400/90">
            Failed to load files
          </div>
        ) : loading && items.length === 0 ? (
          <div className="px-3 py-2 text-[12px] text-dim">Searching…</div>
        ) : items.length === 0 ? (
          <div className="px-3 py-2 text-[12px] text-dim">No matches</div>
        ) : (
          items.map((it, i) => {
            const isSel = i === selectedIndex;
            const dir = it.path.endsWith(it.basename)
              ? it.path.slice(0, it.path.length - it.basename.length).replace(/\/$/, "")
              : it.path;
            return (
              <button
                key={it.path}
                type="button"
                role="option"
                aria-selected={isSel}
                data-mention-row={i}
                onMouseEnter={() => onSetSelection(i)}
                onMouseDown={(e) => {
                  // Prevent textarea blur; pick on mousedown.
                  e.preventDefault();
                  onPick(it.path);
                }}
                className={`w-full text-left flex items-center gap-2 px-2.5 py-1.5 text-[12px] ${
                  isSel ? "bg-honey-500/15 text-white" : "text-white/85 hover:bg-ink-700/60"
                }`}
              >
                {I.doc({ size: 12, className: "text-honey-400/80 shrink-0" })}
                <span className="font-mono truncate">
                  {highlightMatch(it.basename, query)}
                </span>
                {dir && (
                  <span className="font-mono text-dim text-[11px] truncate min-w-0">
                    {highlightMatch(dir, query)}
                  </span>
                )}
              </button>
            );
          })
        )}
      </div>
    </div>
  );
}

export default FileMentionPicker;
