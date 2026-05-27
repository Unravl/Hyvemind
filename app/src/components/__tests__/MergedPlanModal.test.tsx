import React from "react";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { act, render, screen, fireEvent, cleanup } from "@testing-library/react";
import { MergedPlanModal } from "../MergedPlanModal";

// react-markdown reaches into the Tauri shell when handling external links;
// stub the plugin so unit tests don't require a Tauri runtime.
vi.mock("@tauri-apps/plugin-shell", () => ({
  open: vi.fn(),
}));

const VIEW_STORAGE_KEY = "hivemind.mergedPlanView";

const SAMPLE_PLAN = `# Heading

Some prose.

- item one
- item two
`;

beforeEach(() => {
  localStorage.clear();
  // Reset clipboard between tests.
  Object.assign(navigator, {
    clipboard: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
});

describe("MergedPlanModal", () => {
  it("renders nothing when open=false", () => {
    const { container } = render(
      <MergedPlanModal
        open={false}
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders markdown in rendered mode by default", () => {
    render(
      <MergedPlanModal
        open
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    // Markdown component renders <h1> and <ul>.
    expect(screen.getByRole("heading", { level: 1, name: /heading/i })).toBeInTheDocument();
    expect(screen.getByRole("list")).toBeInTheDocument();
    // Source <pre> should NOT be rendered in rendered mode.
    expect(screen.queryByTestId("merged-plan-source")).toBeNull();
  });

  it("switches to Source mode and renders the plan text in a <pre>", () => {
    render(
      <MergedPlanModal
        open
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    const sourceBtn = screen.getByRole("radio", { name: /source/i });
    fireEvent.click(sourceBtn);
    const pre = screen.getByTestId("merged-plan-source");
    expect(pre.tagName).toBe("PRE");
    expect(pre.textContent).toBe(SAMPLE_PLAN);
  });

  it("copies the plan text to clipboard when Copy is clicked", () => {
    render(
      <MergedPlanModal
        open
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    const copyBtn = screen.getByRole("button", { name: /copy plan to clipboard/i });
    fireEvent.click(copyBtn);
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(SAMPLE_PLAN);
  });

  it("persists view-mode toggle across remounts via localStorage", () => {
    const { unmount } = render(
      <MergedPlanModal
        open
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    fireEvent.click(screen.getByRole("radio", { name: /source/i }));
    expect(localStorage.getItem(VIEW_STORAGE_KEY)).toBe(JSON.stringify("source"));
    unmount();
    cleanup();

    render(
      <MergedPlanModal
        open
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    // After remount it should open in source mode.
    expect(screen.getByTestId("merged-plan-source")).toBeInTheDocument();
    const sourceRadio = screen.getByRole("radio", { name: /source/i });
    expect(sourceRadio.getAttribute("aria-checked")).toBe("true");
  });

  it("falls back to rendered mode when localStorage value is corrupt", () => {
    // Manually corrupt the stored value with non-JSON garbage.
    localStorage.setItem(VIEW_STORAGE_KEY, "{not valid json");
    render(
      <MergedPlanModal
        open
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    expect(screen.queryByTestId("merged-plan-source")).toBeNull();
    const renderedRadio = screen.getByRole("radio", { name: /rendered/i });
    expect(renderedRadio.getAttribute("aria-checked")).toBe("true");
  });

  it("does not call onClose during a StrictMode double-mount cycle", async () => {
    const onClose = vi.fn();
    render(
      <React.StrictMode>
        <MergedPlanModal
          open
          onClose={onClose}
          planText={SAMPLE_PLAN}
          title="Merged plan — Round 1"
        />
      </React.StrictMode>,
    );
    // Belt: catch a hypothetical fully-synchronous deactivate path.
    expect(onClose).not.toHaveBeenCalled();
    // Braces: flush the StrictMode mount → effects → cleanup → remount
    // cycle. The pre-fix bug fired onDeactivate during the cleanup pass,
    // which happens AFTER render() returns. Without this flush the test
    // gives false confidence.
    await act(async () => {});
    expect(onClose).not.toHaveBeenCalled();
  });

  it("calls onClose when Escape is dispatched on document", () => {
    const onClose = vi.fn();
    render(
      <MergedPlanModal
        open
        onClose={onClose}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
      />,
    );
    // The Modal listener is registered in capture phase on document, so a
    // standard fireEvent.keyDown(document, ...) reaches it. No other
    // document-level Escape listener is registered by this test setup.
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("renders subtitle when provided", () => {
    render(
      <MergedPlanModal
        open
        onClose={() => {}}
        planText={SAMPLE_PLAN}
        title="Merged plan — Round 1"
        subtitle="Source: task-42"
      />,
    );
    expect(screen.getByText("Source: task-42")).toBeInTheDocument();
  });
});
