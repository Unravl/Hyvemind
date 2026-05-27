import React from "react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  render,
  screen,
  waitFor,
  act,
  cleanup,
  fireEvent,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";

// ── Mocks ─────────────────────────────────────────────────────
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
let currentProject: typeof mockProject | null = mockProject;

vi.mock("../ProjectPicker", () => ({
  useProject: () => ({
    project: currentProject,
    setProject: vi.fn(),
    projects: currentProject ? [currentProject] : [],
    addProject: vi.fn(),
    removeProject: vi.fn(),
    updateProject: vi.fn(),
  }),
  LANG_DOT: {},
}));

// Avoid pulling the real config chip (which loads model registry etc.)
vi.mock("../TaskConfigChip", () => ({
  TaskConfigChip: () => null,
}));

import { QuickTaskDialog } from "../QuickTaskDialog";
import * as ipc from "../../lib/ipc";

const mockListProjectFiles = ipc.listProjectFiles as unknown as ReturnType<
  typeof vi.fn
>;

// ── Helpers ────────────────────────────────────────────────────
// jsdom doesn't support `new ClipboardEvent` cleanly, and DataTransfer's
// `items.add` is iffy. Instead we build a DataTransferItem-like shape and
// hand it to React via fireEvent.paste's eventInit.
function makeClipboardItems(files: File[]) {
  return files.map((file) => ({
    type: file.type,
    kind: "file" as const,
    getAsFile: () => file,
    getAsString: (_cb: (data: string) => void) => {},
  }));
}

async function pasteImageInto(textarea: HTMLElement) {
  const blob = new Blob(["fakepng"], { type: "image/png" });
  const file = new File([blob], "screenshot.png", { type: "image/png" });
  const items = makeClipboardItems([file]);
  await act(async () => {
    fireEvent.paste(textarea, {
      clipboardData: {
        items,
        files: [file],
        types: ["Files"],
        getData: () => "",
      },
    });
    // Let FileReader microtask resolve.
    await new Promise((r) => setTimeout(r, 50));
  });
}

// jsdom doesn't ship full crypto.randomUUID in older versions; ensure present.
if (!(globalThis as any).crypto) (globalThis as any).crypto = {};
if (!(globalThis as any).crypto.randomUUID) {
  let n = 0;
  (globalThis as any).crypto.randomUUID = () => `uuid-${++n}`;
}

// Stub URL.createObjectURL / revokeObjectURL
const createObjUrlSpy = vi.fn(() => "blob:fake");
const revokeObjUrlSpy = vi.fn();
beforeEach(() => {
  (URL as any).createObjectURL = createObjUrlSpy;
  (URL as any).revokeObjectURL = revokeObjUrlSpy;
  createObjUrlSpy.mockClear();
  revokeObjUrlSpy.mockClear();
  mockCreateTask.mockClear();
  mockListProjectFiles.mockClear();
  mockListProjectFiles.mockResolvedValue([]);
  currentProject = mockProject;
});

afterEach(() => {
  cleanup();
});

describe("QuickTaskDialog", () => {
  it("pasting an image adds a thumbnail; remove button removes it", async () => {
    const user = userEvent.setup();
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);

    const textarea = await screen.findByPlaceholderText(
      /what do you want to build/i,
    );
    await pasteImageInto(textarea);

    const img = await screen.findByAltText("Pending");
    expect(img).toBeInTheDocument();
    expect(createObjUrlSpy).toHaveBeenCalled();

    // Remove
    const removeBtn = img.parentElement!.querySelector("button");
    expect(removeBtn).toBeTruthy();
    await user.click(removeBtn!);
    expect(screen.queryByAltText("Pending")).toBeNull();
    expect(revokeObjUrlSpy).toHaveBeenCalled();
  });

  it("Create button enabled when only images (no text) are pending", async () => {
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = await screen.findByPlaceholderText(
      /what do you want to build/i,
    );
    const createBtn = screen.getByRole("button", { name: /create/i });
    expect(createBtn).toBeDisabled();

    await pasteImageInto(textarea);
    await screen.findByAltText("Pending");

    expect(createBtn).not.toBeDisabled();
  });

  it("typing @fo opens the mention picker", async () => {
    const user = userEvent.setup();
    mockListProjectFiles.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = await screen.findByPlaceholderText(
      /what do you want to build/i,
    );
    await user.click(textarea);
    await user.type(textarea, "@fo");
    const picker = await screen.findByRole("listbox", {
      name: /file mention picker/i,
    });
    expect(picker).toBeInTheDocument();
  });

  it("Enter on a picker item adds the file to the chip strip and removes the @token", async () => {
    const user = userEvent.setup();
    mockListProjectFiles.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await user.click(textarea);
    await user.type(textarea, "hi @fo");
    // Wait for the picker listbox AND its option row to render.
    const picker = await screen.findByRole("listbox", {
      name: /file mention picker/i,
    });
    await waitFor(() => {
      expect(picker.querySelector('[role="option"]')).toBeTruthy();
    });
    await user.keyboard("{Enter}");
    // chip strip
    const chips = await screen.findByTestId("attached-files");
    expect(chips.textContent).toContain("src/foo.ts");
    // @fo removed from textarea
    expect(textarea.value).not.toContain("@fo");
  });

  it("Cmd+Enter with picker open submits (does not pick a file)", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    mockListProjectFiles.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(<QuickTaskDialog open={true} onClose={onClose} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await user.click(textarea);
    await user.type(textarea, "hi @fo");
    const picker = await screen.findByRole("listbox", {
      name: /file mention picker/i,
    });
    await waitFor(() => {
      expect(picker.querySelector('[role="option"]')).toBeTruthy();
    });

    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(mockCreateTask).toHaveBeenCalledTimes(1);
    // file NOT attached (Cmd+Enter is submit, not pick)
    expect(screen.queryByTestId("attached-files")).toBeNull();
  });

  it("submitting with attached files appends [Attached files] to the prompt", async () => {
    const user = userEvent.setup();
    mockListProjectFiles.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await user.click(textarea);
    await user.type(textarea, "do thing @fo");
    const picker2 = await screen.findByRole("listbox", {
      name: /file mention picker/i,
    });
    await waitFor(() => {
      expect(picker2.querySelector('[role="option"]')).toBeTruthy();
    });
    await user.keyboard("{Enter}");

    const createBtn = screen.getByRole("button", { name: /create/i });
    await user.click(createBtn);

    expect(mockCreateTask).toHaveBeenCalledTimes(1);
    const args = mockCreateTask.mock.calls[0][0];
    expect(args.prompt).toContain("[Attached files]");
    expect(args.prompt).toContain("- src/foo.ts");
    // Title should NOT contain the Attached files block.
    expect(args.title).toBeTruthy();
    expect(args.title).not.toContain("[Attached files]");
  });

  it("submitting with images passes `images` to createTask and does NOT revoke URLs", async () => {
    const user = userEvent.setup();
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await pasteImageInto(textarea);
    await screen.findByAltText("Pending");

    revokeObjUrlSpy.mockClear();
    const createBtn = screen.getByRole("button", { name: /create/i });
    await user.click(createBtn);

    expect(mockCreateTask).toHaveBeenCalledTimes(1);
    const args = mockCreateTask.mock.calls[0][0];
    expect(Array.isArray(args.images)).toBe(true);
    expect(args.images.length).toBe(1);
    expect(args.title).toBe("Image task");

    // URL.revokeObjectURL should NOT have been called on submit path.
    expect(revokeObjUrlSpy).not.toHaveBeenCalled();
  });

  it("closing the dialog clears pending images and revokes object URLs", async () => {
    const onClose = vi.fn();
    function Wrapper() {
      const [open, setOpen] = React.useState(true);
      return (
        <>
          <button data-testid="close" onClick={() => setOpen(false)}>
            close
          </button>
          <QuickTaskDialog
            open={open}
            onClose={() => {
              onClose();
              setOpen(false);
            }}
          />
        </>
      );
    }
    const user = userEvent.setup();
    render(<Wrapper />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await pasteImageInto(textarea);
    await screen.findByAltText("Pending");

    revokeObjUrlSpy.mockClear();
    await user.click(screen.getByTestId("close"));
    // After close transition, revoke should have been called.
    await waitFor(() => {
      expect(revokeObjUrlSpy).toHaveBeenCalled();
    });
  });

  it("pressing Escape with mention picker open keeps dialog open", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    mockListProjectFiles.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(<QuickTaskDialog open={true} onClose={onClose} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await user.click(textarea);
    await user.type(textarea, "@fo");
    const pickerEsc = await screen.findByRole("listbox", {
      name: /file mention picker/i,
    });
    await waitFor(() => {
      expect(pickerEsc.querySelector('[role="option"]')).toBeTruthy();
    });

    await user.keyboard("{Escape}");
    // picker should be closed but dialog still open (onClose not called)
    expect(onClose).not.toHaveBeenCalled();
  });

  it("switching projects clears attached files", async () => {
    const user = userEvent.setup();
    mockListProjectFiles.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    const { rerender } = render(
      <QuickTaskDialog open={true} onClose={vi.fn()} />,
    );
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await user.click(textarea);
    await user.type(textarea, "@fo");
    const pickerSw = await screen.findByRole("listbox", {
      name: /file mention picker/i,
    });
    await waitFor(() => {
      expect(pickerSw.querySelector('[role="option"]')).toBeTruthy();
    });
    await user.keyboard("{Enter}");
    await screen.findByTestId("attached-files");

    // Swap project
    currentProject = { ...mockProject, id: "other", cwd: "/tmp/other" };
    rerender(<QuickTaskDialog open={true} onClose={vi.fn()} />);

    await waitFor(() => {
      expect(screen.queryByTestId("attached-files")).toBeNull();
    });
  });

  it("rapid double Cmd+Enter only calls createTask once", async () => {
    const user = userEvent.setup();
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await user.click(textarea);
    await user.type(textarea, "hello world");

    // Fire two Cmd+Enter events back-to-back without any re-render between
    // them. The submittedRef guard should prevent the second call.
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    await user.keyboard("{Meta>}{Enter}{/Meta}");

    expect(mockCreateTask).toHaveBeenCalledTimes(1);
  });

  it("Create button enabled when only attached files are present", async () => {
    const user = userEvent.setup();
    mockListProjectFiles.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    const createBtn = screen.getByRole("button", { name: /create/i });
    expect(createBtn).toBeDisabled();

    await user.click(textarea);
    await user.type(textarea, "@fo");
    const picker = await screen.findByRole("listbox", {
      name: /file mention picker/i,
    });
    await waitFor(() => {
      expect(picker.querySelector('[role="option"]')).toBeTruthy();
    });
    await user.keyboard("{Enter}");
    await screen.findByTestId("attached-files");
    expect(textarea.value).toBe(""); // file was just `@fo`
    expect(createBtn).not.toBeDisabled();
  });

  it("Enter with picker open and zero items closes picker, no submit, no newline", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    mockListProjectFiles.mockResolvedValue([]); // zero items
    render(<QuickTaskDialog open={true} onClose={onClose} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await user.click(textarea);
    await user.type(textarea, "hi @zzz");
    // Picker should open even with zero items.
    await screen.findByRole("listbox", { name: /file mention picker/i });

    const valueBefore = textarea.value;
    await user.keyboard("{Enter}");

    // Picker closed
    expect(
      screen.queryByRole("listbox", { name: /file mention picker/i }),
    ).toBeNull();
    // No newline inserted (value unchanged)
    expect(textarea.value).toBe(valueBefore);
    // No submit
    expect(mockCreateTask).not.toHaveBeenCalled();
    // Dialog still open
    expect(onClose).not.toHaveBeenCalled();
  });

  it("Escape with no picker open closes the dialog", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<QuickTaskDialog open={true} onClose={onClose} />);
    await screen.findByPlaceholderText(/what do you want to build/i);
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("submitting with only images (no text) sends finalText='(image attachment)'", async () => {
    const user = userEvent.setup();
    render(<QuickTaskDialog open={true} onClose={vi.fn()} />);
    const textarea = (await screen.findByPlaceholderText(
      /what do you want to build/i,
    )) as HTMLTextAreaElement;
    await pasteImageInto(textarea);
    await screen.findByAltText("Pending");

    const createBtn = screen.getByRole("button", { name: /create/i });
    await user.click(createBtn);

    expect(mockCreateTask).toHaveBeenCalledTimes(1);
    const args = mockCreateTask.mock.calls[0][0];
    expect(args.prompt).toBe("(image attachment)");
  });

  it("FileReader.onload after dialog closes revokes URL and does NOT add image", async () => {
    // Replace FileReader with a controllable shim that defers onload.
    let deferredFire: (() => void) | null = null;
    const realFR = (globalThis as any).FileReader;
    class DeferredFR {
      onload: ((e: any) => void) | null = null;
      onerror: ((e: any) => void) | null = null;
      result: string | null = null;
      readAsDataURL(_file: Blob) {
        this.result = "data:image/png;base64,AAAA";
        deferredFire = () => this.onload?.({ target: this } as any);
      }
    }
    (globalThis as any).FileReader = DeferredFR as any;

    try {
      function Wrapper() {
        const [open, setOpen] = React.useState(true);
        return (
          <>
            <button data-testid="close" onClick={() => setOpen(false)}>
              close
            </button>
            <QuickTaskDialog open={open} onClose={() => setOpen(false)} />
          </>
        );
      }
      const user = userEvent.setup();
      render(<Wrapper />);
      const textarea = (await screen.findByPlaceholderText(
        /what do you want to build/i,
      )) as HTMLTextAreaElement;
      // Start a paste — FileReader will not fire onload until we say so.
      const blob = new Blob(["fakepng"], { type: "image/png" });
      const file = new File([blob], "shot.png", { type: "image/png" });
      await act(async () => {
        fireEvent.paste(textarea, {
          clipboardData: {
            items: makeClipboardItems([file]),
            files: [file],
            types: ["Files"],
            getData: () => "",
          },
        });
      });
      expect(deferredFire).not.toBeNull();

      // Close the dialog BEFORE the FileReader fires.
      revokeObjUrlSpy.mockClear();
      await user.click(screen.getByTestId("close"));

      // Now fire the deferred FileReader.onload.
      await act(async () => {
        deferredFire!();
      });

      // No thumbnail rendered (dialog was closed; ref guard hit).
      expect(screen.queryByAltText("Pending")).toBeNull();
      // The previewUrl we created should have been revoked.
      expect(revokeObjUrlSpy).toHaveBeenCalled();
    } finally {
      (globalThis as any).FileReader = realFR;
    }
  });
});
