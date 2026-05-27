import { describe, it, expect, vi } from "vitest";
import React, { createRef } from "react";
import { render, screen, fireEvent } from "@testing-library/react";

// Mock the runtime hook so we don't need a provider, and stub the file
// mention picker (it pulls in IPC that doesn't exist in jsdom).
vi.mock("../../lib/taskRuntime", () => ({
  useTaskRuntime: () => ({
    getDraft: () => "",
    setDraft: vi.fn(),
  }),
  useTaskRuntimeState: () => ({
    tasks: {},
    streamingTaskIds: {},
    updateTask: vi.fn(),
  }),
}));
vi.mock("../../components/FileMentionPicker", () => ({
  FileMentionPicker: () => null,
}));
vi.mock("../../components/NurseTestDropdown", () => ({
  NurseTestDropdown: () => null,
}));

import { Composer } from "../Tasks";

const baseProps = {
  activeId: "test-1",
  streaming: true,
  headerModel: "test-model",
  autoMode: "off" as const,
  hasHivemind: false,
  pendingImagesCount: 0,
  attachedFilesCount: 0,
  projectPath: null,
  onAddAttachedFile: vi.fn(),
  messagesRef: { current: [] } as React.MutableRefObject<any[]>,
  onSend: vi.fn(),
  onStop: vi.fn(),
  onSetAutoMode: vi.fn(),
  onPaste: vi.fn(),
};

describe("Composer steerableWhileStreaming", () => {
  it("textarea is not disabled when steerableWhileStreaming=true and streaming=true", () => {
    const ref = createRef<any>();
    render(<Composer ref={ref} {...baseProps} steerableWhileStreaming />);
    const textarea = screen.getByRole("textbox");
    expect(textarea).not.toBeDisabled();
  });

  it("textarea is NOT disabled when streaming=true and questionsPending=false", () => {
    const ref = createRef<any>();
    render(<Composer ref={ref} {...baseProps} steerableWhileStreaming={false} />);
    const textarea = screen.getByRole("textbox");
    expect(textarea).not.toBeDisabled();
  });

  it("Enter calls onSend when steerableWhileStreaming=true and streaming=true", () => {
    const onSend = vi.fn();
    const ref = createRef<any>();
    render(
      <Composer
        ref={ref}
        {...baseProps}
        steerableWhileStreaming
        onSend={onSend}
      />,
    );
    const textarea = screen.getByRole("textbox");
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("Enter calls onSend when streaming=true and questionsPending=false", () => {
    const onSend = vi.fn();
    const ref = createRef<any>();
    render(
      <Composer
        ref={ref}
        {...baseProps}
        steerableWhileStreaming={false}
        onSend={onSend}
      />,
    );
    const textarea = screen.getByRole("textbox");
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("displays steer placeholder when steerableWhileStreaming && streaming", () => {
    const ref = createRef<any>();
    render(<Composer ref={ref} {...baseProps} steerableWhileStreaming />);
    expect(screen.getByPlaceholderText(/Steer the context gatherer/)).toBeTruthy();
  });

  it("displays normal streaming placeholder when steerableWhileStreaming=false", () => {
    const ref = createRef<any>();
    render(<Composer ref={ref} {...baseProps} steerableWhileStreaming={false} />);
    expect(screen.getByPlaceholderText(/type to steer/i)).toBeTruthy();
  });

  it("textarea IS disabled when questionsPending=true (streaming=true)", () => {
    const ref = createRef<any>();
    render(<Composer ref={ref} {...baseProps} questionsPending />);
    const textarea = screen.getByRole("textbox");
    expect(textarea).toBeDisabled();
  });

  it("Enter does NOT call onSend when questionsPending=true", () => {
    const onSend = vi.fn();
    const ref = createRef<any>();
    render(
      <Composer
        ref={ref}
        {...baseProps}
        questionsPending
        onSend={onSend}
      />,
    );
    const textarea = screen.getByRole("textbox");
    fireEvent.keyDown(textarea, { key: "Enter", shiftKey: false });
    expect(onSend).not.toHaveBeenCalled();
  });
});
