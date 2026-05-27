import React, { useEffect, useState } from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  render,
  screen,
  fireEvent,
  cleanup,
  act,
} from "@testing-library/react";

// ── Mocks ─────────────────────────────────────────────────────
// QuickTaskDialog reaches into the runtime; mock it the same way the
// existing QuickTaskDialog.test.tsx does so we can render it standalone.
vi.mock("../../lib/tauri", () => ({ isTauri: () => false }));

vi.mock("../../lib/ipc", () => ({
  listProjectFiles: vi.fn().mockResolvedValue([]),
  setDefaultModel: vi.fn().mockResolvedValue(undefined),
}));

const mockCreateTask = vi.fn((_opts: any) => "task-1");

vi.mock("../../lib/taskRuntime", () => ({
  useTaskRuntime: () => ({
    defaultModel: "anthropic/claude-sonnet-4",
    defaultHivemind: null,
    hivemindOptions: [],
    createTask: mockCreateTask,
  }),
}));

const mockProject = {
  id: "proj",
  name: "proj",
  org: "hyvemind",
  cwd: "/tmp/proj",
  branch: "main",
  dirty: 0,
  lang: "rust",
  activeSwarms: 0,
  chats: 0,
  lastTouched: "",
};

vi.mock("../ProjectPicker", () => ({
  useProject: () => ({
    project: mockProject,
    setProject: vi.fn(),
    projects: [mockProject],
    addProject: vi.fn(),
    removeProject: vi.fn(),
    updateProject: vi.fn(),
  }),
  LANG_DOT: {},
}));

vi.mock("../TaskConfigChip", () => ({
  TaskConfigChip: () => null,
}));

import { QuickTaskDialog } from "../QuickTaskDialog";

/**
 * Test harness that mirrors the global `C` keydown listener from
 * `App.tsx`. Keep this effect in sync with the corresponding `useEffect`
 * in `app/src/App.tsx`. The harness exists because fully mounting
 * `<App />` requires a deep provider tree (Settings, Providers, Nurse,
 * Tasks, Extensions, …) which is too brittle for a focused unit test.
 */
function Harness({ inspectorOn = false }: { inspectorOn?: boolean } = {}) {
  const [quickOpen, setQuickOpen] = useState(false);

  // ── BEGIN copy from App.tsx — Quick Task `C` shortcut ─────────
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (quickOpen) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.isComposing || e.key === "Process") return;
      if (e.repeat) return;
      if (e.key !== "c" && e.key !== "C") return;

      const el = document.activeElement;
      if (el) {
        const tag = el.tagName.toLowerCase();
        if (tag === "input" || tag === "textarea" || tag === "select") return;
        if ((el as HTMLElement).isContentEditable) return;
      }

      if (document.querySelector('[role="dialog"][aria-modal="true"]')) return;
      if (inspectorOn) return;

      e.preventDefault();
      setQuickOpen(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [quickOpen, inspectorOn]);
  // ── END copy from App.tsx ─────────────────────────────────────

  return (
    <>
      <input data-testid="text-input" />
      <QuickTaskDialog
        open={quickOpen}
        onClose={() => setQuickOpen(false)}
      />
    </>
  );
}

beforeEach(() => {
  // Stub URL APIs touched by QuickTaskDialog's paste path.
  (URL as any).createObjectURL = vi.fn(() => "blob:fake");
  (URL as any).revokeObjectURL = vi.fn();
  mockCreateTask.mockClear();
});

afterEach(() => {
  cleanup();
});

describe("Quick Task global `C` shortcut", () => {
  it("plain `c` keydown opens the Quick Task dialog", async () => {
    render(<Harness />);
    expect(screen.queryByPlaceholderText(/what do you want to build/i)).toBeNull();

    act(() => {
      fireEvent.keyDown(window, { key: "c" });
    });

    expect(
      await screen.findByPlaceholderText(/what do you want to build/i),
    ).toBeInTheDocument();
  });

  it("plain `C` (capital, Caps Lock) keydown opens the Quick Task dialog", async () => {
    render(<Harness />);
    act(() => {
      fireEvent.keyDown(window, { key: "C" });
    });
    expect(
      await screen.findByPlaceholderText(/what do you want to build/i),
    ).toBeInTheDocument();
  });

  it("Shift+C opens the Quick Task dialog (Linear parity)", async () => {
    render(<Harness />);
    act(() => {
      fireEvent.keyDown(window, { key: "C", shiftKey: true });
    });
    expect(
      await screen.findByPlaceholderText(/what do you want to build/i),
    ).toBeInTheDocument();
  });

  it("`c` keydown while an <input> is focused does NOT open the dialog", () => {
    render(<Harness />);
    const input = screen.getByTestId("text-input") as HTMLInputElement;
    input.focus();
    expect(document.activeElement).toBe(input);

    act(() => {
      fireEvent.keyDown(input, { key: "c" });
    });

    expect(
      screen.queryByPlaceholderText(/what do you want to build/i),
    ).toBeNull();
  });

  it("Meta+c keydown does NOT open the dialog (preserves native copy)", () => {
    render(<Harness />);
    act(() => {
      fireEvent.keyDown(window, { key: "c", metaKey: true });
    });
    expect(
      screen.queryByPlaceholderText(/what do you want to build/i),
    ).toBeNull();
  });
});
