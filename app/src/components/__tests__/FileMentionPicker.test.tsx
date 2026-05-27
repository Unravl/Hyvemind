import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("../../lib/ipc", () => ({
  listProjectFiles: vi.fn(),
}));

import { FileMentionPicker } from "../FileMentionPicker";
import * as ipc from "../../lib/ipc";

const mockList = ipc.listProjectFiles as unknown as ReturnType<typeof vi.fn>;

describe("FileMentionPicker", () => {
  beforeEach(() => {
    vi.resetAllMocks();
  });

  it("returns null when open=false (no DOM)", () => {
    const { container } = render(
      <FileMentionPicker
        open={false}
        query=""
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={vi.fn()}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("renders matches from listProjectFiles and notifies onItemsChange", async () => {
    mockList.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
      { path: "src/bar.ts", basename: "bar.ts", score: 500 },
    ]);
    const onItemsChange = vi.fn();
    render(
      <FileMentionPicker
        open={true}
        query="f"
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={onItemsChange}
      />,
    );
    await waitFor(() => {
      expect(onItemsChange).toHaveBeenCalledWith(
        expect.arrayContaining([expect.objectContaining({ path: "src/foo.ts" })]),
      );
    });
    // Row is rendered as an `option` with the file path
    const rows = await screen.findAllByRole("option");
    expect(rows.length).toBeGreaterThanOrEqual(1);
  });

  it("renders error state when IPC fails", async () => {
    mockList.mockRejectedValue(new Error("boom"));
    render(
      <FileMentionPicker
        open={true}
        query="x"
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={vi.fn()}
      />,
    );
    await waitFor(() => {
      expect(screen.getByText(/Failed to load files/i)).toBeInTheDocument();
    });
  });

  it("renders 'No matches' when results are empty", async () => {
    mockList.mockResolvedValue([]);
    render(
      <FileMentionPicker
        open={true}
        query="zzz"
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={vi.fn()}
      />,
    );
    await waitFor(() => {
      expect(screen.getByText(/No matches/i)).toBeInTheDocument();
    });
  });

  it("calls onPick when a row is mouseDowned", async () => {
    const user = userEvent.setup();
    mockList.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    const onPick = vi.fn();
    render(
      <FileMentionPicker
        open={true}
        query="foo"
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={onPick}
        onItemsChange={vi.fn()}
      />,
    );
    const row = await screen.findByRole("option");
    await user.click(row);
    expect(onPick).toHaveBeenCalledWith("src/foo.ts");
  });

  it("defaults to dropUp (bottom-full positioning) when prop omitted", async () => {
    mockList.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(
      <FileMentionPicker
        open={true}
        query="foo"
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={vi.fn()}
      />,
    );
    const listbox = await screen.findByRole("listbox");
    expect(listbox.className).toContain("bottom-full");
    expect(listbox.className).not.toContain("top-full");
  });

  it("uses bottom-full positioning when dropUp={true}", async () => {
    mockList.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(
      <FileMentionPicker
        open={true}
        query="foo"
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={vi.fn()}
        dropUp={true}
      />,
    );
    const listbox = await screen.findByRole("listbox");
    expect(listbox.className).toContain("bottom-full");
    expect(listbox.className).not.toContain("top-full");
  });

  it("uses top-full positioning when dropUp={false}", async () => {
    mockList.mockResolvedValue([
      { path: "src/foo.ts", basename: "foo.ts", score: 800 },
    ]);
    render(
      <FileMentionPicker
        open={true}
        query="foo"
        projectPath="/tmp/proj"
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={vi.fn()}
        dropUp={false}
      />,
    );
    const listbox = await screen.findByRole("listbox");
    expect(listbox.className).toContain("top-full");
    expect(listbox.className).not.toContain("bottom-full");
  });

  it("does not fetch when projectPath is null", async () => {
    render(
      <FileMentionPicker
        open={true}
        query="foo"
        projectPath={null}
        selectedIndex={0}
        onSetSelection={vi.fn()}
        onPick={vi.fn()}
        onItemsChange={vi.fn()}
      />,
    );
    // Give the debounce a chance — it should still not be called.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 200));
    });
    expect(mockList).not.toHaveBeenCalled();
  });
});
