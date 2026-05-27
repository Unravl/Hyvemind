import React, { useEffect } from "react";
import { Modal, Kbd } from "./atoms";
import { isMac } from "../lib/platform";

interface ShortcutCheatsheetProps {
  open: boolean;
  onClose: () => void;
}

function GroupTitle({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="text-xs font-semibold text-muted uppercase tracking-wider mb-3 mt-5 first:mt-0">
      {children}
    </h3>
  );
}

export function ShortcutCheatsheet({ open, onClose }: ShortcutCheatsheetProps) {
  const mac = isMac();
  const mod = mac ? "⌘" : "Ctrl";
  const shift = mac ? "⇧" : "Shift";

  const groups = [
    {
      name: "Navigation",
      shortcuts: [
        { keys: [mod, "1"], desc: "Dashboard" },
        { keys: [mod, "2"], desc: "Tasks" },
        { keys: [mod, "3"], desc: "Swarms" },
        { keys: [mod, "4"], desc: "Hiveminds" },
        { keys: [mod, "5"], desc: "Tests" },
        { keys: [mod, "6"], desc: "Settings" },
      ],
    },
    {
      name: "Actions",
      shortcuts: [
        { keys: [mod, shift, "T"], desc: "Quick Task" },
        { keys: ["?"], desc: "Keyboard shortcuts" },
        { keys: ["Esc"], desc: "Close modal / dialog" },
      ],
    },
    {
      name: "Display",
      shortcuts: [
        { keys: [mod, "+"], desc: "Zoom in" },
        { keys: [mod, "−"], desc: "Zoom out" },
        { keys: [mod, "0"], desc: "Reset zoom" },
      ],
    },
  ];

  // Close on Escape
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  return (
    <Modal open={open} onClose={onClose} title="Keyboard Shortcuts">
      {groups.map((group) => (
        <div key={group.name}>
          <GroupTitle>{group.name}</GroupTitle>
          <div className="space-y-2">
            {group.shortcuts.map((s, i) => (
              <div
                key={i}
                className="flex items-center justify-between gap-4"
              >
                <span className="text-[13px] text-slate-300">
                  {s.desc}
                </span>
                <span className="flex items-center gap-1 shrink-0">
                  {s.keys.map((k, j) => (
                    <Kbd key={j}>{k}</Kbd>
                  ))}
                </span>
              </div>
            ))}
          </div>
        </div>
      ))}
    </Modal>
  );
}
