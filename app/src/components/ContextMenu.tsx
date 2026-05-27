import React, {
  createContext,
  useContext,
  useState,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
} from "react";
import ReactDOM from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { I } from "./icons";
import { confirmDialog } from "../lib/confirm";

/* ── Types ─────────────────────────────────────────────────── */

interface ContextMenuItem {
  id: string;
  label: string;
  icon?: React.ReactNode;
  shortcut?: string;
  disabled?: boolean;
  danger?: boolean;
  separator?: boolean;
  action: () => void;
}

export interface TaskContextActions {
  onDelete: (id: string) => void;
  onMarkDone: (id: string) => void;
  onRename: (id: string) => void;
}

interface ContextMenuProviderProps {
  children: React.ReactNode;
  onQuickTask?: (text: string) => void;
  onToggleInspector?: () => void;
  inspectorOn?: boolean;
}

interface MenuState {
  visible: boolean;
  x: number;
  y: number;
  items: ContextMenuItem[];
  focusedIndex: number;
  measured: boolean;
}

const INITIAL: MenuState = {
  visible: false,
  x: 0,
  y: 0,
  items: [],
  focusedIndex: 0,
  measured: false,
};

/* ── Context (no-op default) ───────────────────────────────── */

const ContextMenuCtx = createContext<{
  close: () => void;
  registerTaskActions: (actions: TaskContextActions | null) => void;
}>({ close: () => {}, registerTaskActions: () => {} });
export const useContextMenu = () => useContext(ContextMenuCtx);

/* ── Helpers ───────────────────────────────────────────────── */

function dedupe(items: ContextMenuItem[]): ContextMenuItem[] {
  const seen = new Set<string>();
  return items.filter((it) => {
    if (it.separator) return true; // separators always pass
    if (seen.has(it.id)) return false;
    seen.add(it.id);
    return true;
  });
}

/** Remove leading/trailing separators and collapse consecutive separators. */
function cleanSeparators(items: ContextMenuItem[]): ContextMenuItem[] {
  const out: ContextMenuItem[] = [];
  for (const it of items) {
    if (it.separator) {
      if (out.length === 0) continue; // skip leading
      if (out[out.length - 1]?.separator) continue; // collapse consecutive
    }
    out.push(it);
  }
  // strip trailing separator
  while (out.length > 0 && out[out.length - 1]?.separator) out.pop();
  return out;
}

function sep(): ContextMenuItem {
  return { id: `sep-${Math.random()}`, label: "", separator: true, action: () => {} };
}

/* ── Provider ──────────────────────────────────────────────── */

export function ContextMenuProvider({
  children,
  onQuickTask,
  onToggleInspector,
  inspectorOn,
}: ContextMenuProviderProps) {
  const [menu, setMenu] = useState<MenuState>(INITIAL);
  const portalRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const previousFocusRef = useRef<Element | null>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);

  const taskActionsRef = useRef<TaskContextActions | null>(null);

  const registerTaskActions = useCallback((actions: TaskContextActions | null) => {
    taskActionsRef.current = actions;
  }, []);

  const close = useCallback(() => {
    setMenu(INITIAL);
    // Restore focus
    const prev = previousFocusRef.current;
    if (prev && typeof (prev as HTMLElement).focus === "function") {
      requestAnimationFrame(() => (prev as HTMLElement).focus());
    }
    previousFocusRef.current = null;
  }, []);

  /* ── Build menu items from click target ─────────────────── */

  const buildItems = useCallback(
    (e: MouseEvent): ContextMenuItem[] => {
      const target = e.target as HTMLElement;
      const items: ContextMenuItem[] = [];

      // Walk composedPath / ancestors
      let el: HTMLElement | null = target;

      let foundInput = false;
      let foundSelection = false;
      let foundCodeBlock = false;
      let foundLink = false;
      let foundMessage = false;
      let foundTaskItem = false;
      let foundReview = false;

      // Check input/textarea first
      if (
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement
      ) {
        foundInput = true;
        const elem = target;
        const isEditable = !elem.disabled && !elem.readOnly;
        const hasSelection =
          elem.selectionStart != null &&
          elem.selectionEnd != null &&
          elem.selectionStart !== elem.selectionEnd;
        const selectedText = hasSelection
          ? elem.value.substring(elem.selectionStart!, elem.selectionEnd!)
          : "";

        items.push({
          id: "cut",
          label: "Cut",
          icon: I.scissors({ size: 14, className: "text-muted" }),
          shortcut: "⌘X",
          disabled: !isEditable || !hasSelection,
          action: () => {
            try {
              elem.focus();
              document.execCommand("cut");
            } catch (err) {
              console.warn("Context menu cut failed:", err);
            }
          },
        });

        items.push({
          id: "copy",
          label: "Copy",
          icon: I.copy({ size: 14, className: "text-muted" }),
          shortcut: "⌘C",
          disabled: !hasSelection,
          action: () => {
            if (selectedText) {
              navigator.clipboard.writeText(selectedText).catch(() => {});
            }
          },
        });

        items.push({
          id: "paste",
          label: "Paste",
          icon: I.paste({ size: 14, className: "text-muted" }),
          shortcut: "⌘V",
          disabled: !isEditable,
          action: () => {
            try {
              navigator.clipboard
                .readText()
                .then((text) => {
                  elem.focus();
                  // Insert at cursor position
                  const start = elem.selectionStart ?? 0;
                  const end = elem.selectionEnd ?? 0;
                  const before = elem.value.substring(0, start);
                  const after = elem.value.substring(end);
                  const nativeInputValueSetter = Object.getOwnPropertyDescriptor(
                    window.HTMLInputElement.prototype, 'value'
                  )?.set || Object.getOwnPropertyDescriptor(
                    window.HTMLTextAreaElement.prototype, 'value'
                  )?.set;
                  if (nativeInputValueSetter) {
                    nativeInputValueSetter.call(elem, before + text + after);
                    elem.dispatchEvent(new Event('input', { bubbles: true }));
                  } else {
                    elem.value = before + text + after;
                  }
                  elem.selectionStart = elem.selectionEnd = start + text.length;
                })
                .catch(() => {
                  try {
                    elem.focus();
                    document.execCommand("paste");
                  } catch {
                    console.warn(
                      "Context menu paste failed: clipboard API and execCommand both unavailable"
                    );
                  }
                });
            } catch {
              console.warn(
                "Context menu paste failed: clipboard API and execCommand both unavailable"
              );
            }
          },
        });

        items.push({
          id: "select-all",
          label: "Select All",
          icon: I.selectAll({ size: 14, className: "text-muted" }),
          shortcut: "⌘A",
          action: () => {
            elem.focus();
            elem.select();
          },
        });

        if (hasSelection && selectedText && onQuickTask) {
          items.push(sep());
          items.push({
            id: "send-to-quick-task",
            label: "Send to Quick Task",
            icon: I.rocket({ size: 14, className: "text-honey-400" }),
            action: () => onQuickTask(selectedText),
          });
        }
      }

      // Check text selection (outside input/textarea)
      // Skip selection items inside task cards — those get task-specific actions instead.
      const insideTaskCard = !!target.closest("[data-ctx-task-id]");
      if (!foundInput && !insideTaskCard) {
        const selection = window.getSelection()?.toString() || "";
        if (selection.length > 0) {
          foundSelection = true;
          items.push({
            id: "copy",
            label: "Copy",
            icon: I.copy({ size: 14, className: "text-muted" }),
            shortcut: "⌘C",
            action: () => {
              navigator.clipboard.writeText(selection).catch(() => {});
            },
          });
          if (onQuickTask) {
            items.push({
              id: "send-to-quick-task",
              label: "Send to Quick Task",
              icon: I.rocket({ size: 14, className: "text-honey-400" }),
              action: () => onQuickTask(selection),
            });
          }
        }
      }

      // Walk up ancestors for contextual items
      el = target;
      while (el) {
        // Code block
        if (
          !foundCodeBlock &&
          (el.tagName === "PRE" || el.tagName === "CODE")
        ) {
          foundCodeBlock = true;
          const codeEl = el;
          items.push({
            id: "copy-code",
            label: "Copy Code",
            icon: I.terminal({ size: 14, className: "text-muted" }),
            action: () => {
              const text = codeEl.textContent || "";
              navigator.clipboard.writeText(text).catch(() => {});
            },
          });
        }

        // Link
        if (!foundLink && el.tagName === "A") {
          foundLink = true;
          const href = (el as HTMLAnchorElement).href;
          items.push({
            id: "copy-link",
            label: "Copy Link",
            icon: I.copy({ size: 14, className: "text-muted" }),
            action: () => {
              navigator.clipboard.writeText(href).catch(() => {});
            },
          });
        }

        // Message bubble
        if (!foundMessage && el.hasAttribute("data-ctx-role")) {
          foundMessage = true;
          const msgEl = el;
          items.push({
            id: "copy-message",
            label: "Copy Message",
            icon: I.copy({ size: 14, className: "text-muted" }),
            action: () => {
              const bubble = msgEl.querySelector("[data-ctx-bubble]");
              const text = bubble
                ? bubble.textContent || ""
                : msgEl.textContent || "";
              navigator.clipboard.writeText(text.trim()).catch(() => {});
            },
          });

          if (onQuickTask) {
            items.push({
              id: "send-to-quick-task",
              label: "Send to Quick Task",
              icon: I.rocket({ size: 14, className: "text-honey-400" }),
              action: () => {
                const bubble = msgEl.querySelector("[data-ctx-bubble]");
                const text = bubble
                  ? bubble.textContent || ""
                  : msgEl.textContent || "";
                onQuickTask(text.trim());
              },
            });
          }
        }

        // Review sidebar item (hivemind review history)
        if (!foundReview && el.hasAttribute("data-ctx-review-id")) {
          foundReview = true;
          const reviewId = el.getAttribute("data-ctx-review-id")!;

          items.push(sep());

          items.push({
            id: "delete-review",
            label: "Delete Review",
            icon: I.trash({ size: 14, className: "text-red-400" }),
            danger: true,
            action: () => {
              void (async () => {
                const ok = await confirmDialog(`Delete review #${reviewId}? This cannot be undone.`, {
                  title: "Delete review",
                  okLabel: "Delete",
                  cancelLabel: "Cancel",
                  kind: "warning",
                });
                if (!ok) return;
                const { deleteReview } = await import("../lib/ipc");
                try {
                  await deleteReview(reviewId);
                  window.dispatchEvent(new CustomEvent("review-deleted", { detail: { reviewId } }));
                } catch (err) {
                  console.error("Failed to delete review:", err);
                }
              })();
            },
          });
        }

        // Task sidebar item
        if (!foundTaskItem && el.hasAttribute("data-ctx-task-id")) {
          foundTaskItem = true;
          const taskId = el.getAttribute("data-ctx-task-id")!;
          const taskPhase = el.getAttribute("data-ctx-task-phase") || "";
          const isDone = taskPhase === "implement-done";
          const actions = taskActionsRef.current;

          items.push({
            id: "rename-task",
            label: "Rename",
            icon: I.edit({ size: 14, className: "text-muted" }),
            disabled: !actions,
            action: () => actions?.onRename(taskId),
          });

          if (!isDone) {
            items.push({
              id: "mark-done",
              label: "Mark as Done",
              icon: I.check({ size: 14, className: "text-emerald-400" }),
              disabled: !actions,
              action: () => actions?.onMarkDone(taskId),
            });
          }

          items.push(sep());

          items.push({
            id: "delete-task",
            label: "Delete Task",
            icon: I.trash({ size: 14, className: "text-red-400" }),
            danger: true,
            disabled: !actions,
            action: () => actions?.onDelete(taskId),
          });
        }

        el = el.parentElement;
      }

      // ── Fallback / always-shown items ──────────────────────

      items.push(sep());

      if (onQuickTask) {
        items.push({
          id: "quick-task",
          label: "Quick Task",
          icon: I.rocket({ size: 14, className: "text-honey-400" }),
          shortcut: "⌘⇧T",
          action: () => onQuickTask(""),
        });
      }

      if (import.meta.env.DEV && onToggleInspector) {
        items.push(sep());
        items.push({
          id: "inspect-element",
          label: "Inspect Element",
          icon: I.crosshair({ size: 14, className: "text-muted" }),
          action: () => onToggleInspector(),
        });
        items.push({
          id: "developer-tools",
          label: "Developer Tools",
          icon: I.terminal({ size: 14, className: "text-muted" }),
          shortcut: "⌘⌥I",
          action: () => {
            invoke("plugin:webview|internal_toggle_devtools").catch((err: unknown) =>
              console.warn("Failed to toggle devtools:", err)
            );
          },
        });
      }

      return cleanSeparators(dedupe(items));
    },
    [onQuickTask, onToggleInspector]
  );

  /* ── Global contextmenu listener ────────────────────────── */

  useEffect(() => {
    const handler = (e: MouseEvent) => {
      const target = e.target as HTMLElement;

      // 1. data-ctx-ignore opt-out
      if (target.closest?.("[data-ctx-ignore]")) return;

      // 2. Inspector active
      if (inspectorOn) return;

      // 3. Modal guard
      const path = e.composedPath();
      for (const node of path) {
        if (node instanceof HTMLElement && node.hasAttribute("data-modal")) {
          return;
        }
      }

      // 4. Self-click guard
      if (portalRef.current?.contains(target)) {
        e.preventDefault();
        return;
      }

      // Build items
      const items = buildItems(e);
      if (items.length === 0) return;

      e.preventDefault();

      // Save focus for restoration
      previousFocusRef.current = document.activeElement;

      setMenu({
        visible: true,
        x: e.clientX,
        y: e.clientY,
        items,
        focusedIndex: 0,
        measured: false,
      });
    };

    window.addEventListener("contextmenu", handler, true);
    return () => window.removeEventListener("contextmenu", handler, true);
  }, [inspectorOn, buildItems]);

  /* ── Dismiss listeners (attached only when menu is open) ── */

  useEffect(() => {
    if (!menu.visible) return;

    const onMouseDown = (e: MouseEvent) => {
      if (portalRef.current?.contains(e.target as Node)) return;
      close();
    };
    const onBlur = () => close();
    const onScroll = (e: Event) => {
      // Ignore scroll inside the menu itself
      if (portalRef.current?.contains(e.target as Node)) return;
      close();
    };

    document.addEventListener("mousedown", onMouseDown);
    window.addEventListener("blur", onBlur);
    window.addEventListener("scroll", onScroll, { capture: true, passive: true });

    return () => {
      document.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("scroll", onScroll, true);
    };
  }, [menu.visible, close]);

  /* ── Viewport clamping after render ─────────────────────── */

  useLayoutEffect(() => {
    if (!menu.visible || menu.measured) return;
    const el = menuRef.current;
    if (!el) return;

    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let { x, y } = menu;

    if (x + w > window.innerWidth) x = Math.max(0, x - w);
    if (y + h > window.innerHeight) y = Math.max(0, y - h);

    setMenu((prev) => ({ ...prev, x, y, measured: true }));
  }, [menu]);

  /* ── Focus first item when menu opens and is measured ────── */

  useEffect(() => {
    if (menu.visible && menu.measured) {
      // Focus the first non-disabled, non-separator item
      const firstIdx = menu.items.findIndex((it) => !it.separator && !it.disabled);
      if (firstIdx >= 0) {
        itemRefs.current[firstIdx]?.focus();
        setMenu((prev) => ({ ...prev, focusedIndex: firstIdx }));
      }
    }
  }, [menu.visible, menu.measured]);

  /* ── Keyboard navigation ────────────────────────────────── */

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      const { items, focusedIndex } = menu;

      const navigate = (dir: 1 | -1) => {
        let idx = focusedIndex;
        for (let i = 0; i < items.length; i++) {
          idx = (idx + dir + items.length) % items.length;
          if (!items[idx].separator && !items[idx].disabled) break;
        }
        itemRefs.current[idx]?.focus();
        setMenu((prev) => ({ ...prev, focusedIndex: idx }));
      };

      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          navigate(1);
          break;
        case "ArrowUp":
          e.preventDefault();
          navigate(-1);
          break;
        case "Enter":
        case " ":
          e.preventDefault();
          if (items[focusedIndex] && !items[focusedIndex].disabled && !items[focusedIndex].separator) {
            items[focusedIndex].action();
            close();
          }
          break;
        case "Escape":
          e.preventDefault();
          close();
          break;
      }
    },
    [menu, close]
  );

  /* ── Render ─────────────────────────────────────────────── */

  const ctxValue = React.useMemo(() => ({ close, registerTaskActions }), [close, registerTaskActions]);

  return (
    <ContextMenuCtx.Provider value={ctxValue}>
      {children}
      {menu.visible &&
        ReactDOM.createPortal(
          <div
            ref={portalRef}
            className="fixed inset-0 z-[70]"
            style={{ pointerEvents: "none" }}
          >
            <div
              ref={menuRef}
              role="menu"
              tabIndex={-1}
              onKeyDown={handleKeyDown}
              onMouseDown={(e) => e.preventDefault()}
              className="absolute bg-ink-800 border border-line rounded-xl shadow-2xl py-1.5 min-w-[180px] max-h-[min(60vh,400px)] overflow-y-auto"
              style={{
                left: menu.x,
                top: menu.y,
                visibility: menu.measured ? "visible" : "hidden",
                pointerEvents: "auto",
                animation: menu.measured ? "fadeIn 100ms ease-out" : "none",
              }}
            >
              {menu.items.map((item, i) => {
                if (item.separator) {
                  return (
                    <div
                      key={item.id}
                      className="border-t border-line my-1"
                      role="separator"
                    />
                  );
                }
                return (
                  <button
                    key={item.id}
                    ref={(el) => { itemRefs.current[i] = el; }}
                    role="menuitem"
                    tabIndex={-1}
                    disabled={item.disabled}
                    onClick={() => {
                      if (!item.disabled) {
                        item.action();
                        close();
                      }
                    }}
                    onMouseEnter={() => {
                      if (!item.disabled) {
                        itemRefs.current[i]?.focus();
                        setMenu((prev) => ({ ...prev, focusedIndex: i }));
                      }
                    }}
                    className={`w-full flex items-center gap-2 px-3 py-1.5 text-[13px] text-left rounded-md mx-0 focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 transition-colors ${
                      item.disabled
                        ? "opacity-40 pointer-events-none text-slate-200"
                        : item.danger
                          ? "text-red-400 hover:bg-red-500/10 focus:bg-red-500/10"
                          : "text-slate-200 hover:bg-ink-700/60 focus:bg-ink-700/60"
                    }`}
                  >
                    {item.icon && <span className="shrink-0 w-[14px]">{item.icon}</span>}
                    <span className="flex-1">{item.label}</span>
                    {item.shortcut && (
                      <span className="text-dim text-[11px] font-mono ml-auto pl-4">
                        {item.shortcut}
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          </div>,
          document.body
        )}
    </ContextMenuCtx.Provider>
  );
}
