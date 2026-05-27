import { describe, it, expect, vi, beforeEach } from "vitest";
import React from "react";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

vi.mock("../../lib/tauri", () => ({ isTauri: () => false }));

// Mock ModelBrowserModal so tests can drive its `onSelect` callback directly.
vi.mock("../ModelBrowser", () => ({
  ModelBrowserModal: ({
    open,
    onSelect,
  }: {
    open: boolean;
    onSelect?: (m: any, opts: any) => void;
  }) => {
    if (!open) return null;
    return (
      <div data-testid="mock-model-browser">
        <button
          data-testid="mock-select-32k"
          onClick={() =>
            onSelect?.(
              {
                id: "claude-sonnet-test",
                provider: "anthropic",
                ctx: "200k",
                out: "32k",
                tags: [],
                price: "",
                type: "",
                outNum: 32_000,
                ctxNum: 200_000,
              },
              { thinking: "high" },
            )
          }
        >
          select 32k
        </button>
        <button
          data-testid="mock-select-huge"
          onClick={() =>
            onSelect?.(
              {
                id: "big-model",
                provider: "anthropic",
                ctx: "1M",
                out: "1M",
                tags: [],
                price: "",
                type: "",
                outNum: 1_000_000,
                ctxNum: 1_000_000,
              },
              { thinking: "high" },
            )
          }
        >
          select huge
        </button>
        <button
          data-testid="mock-select-no-outnum"
          onClick={() =>
            onSelect?.(
              {
                id: "plain-model",
                provider: "openai",
                ctx: "",
                out: "",
                tags: [],
                price: "",
                type: "",
              },
              { thinking: "high" },
            )
          }
        >
          select plain
        </button>
      </div>
    );
  },
}));

import { HivemindEditModal } from "../HivemindEdit";

describe("HivemindEditModal", () => {
  const onClose = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("renders nothing when open=false", () => {
    const { container } = render(
      <HivemindEditModal
        open={false}
        onClose={onClose}
        hivemind={null}
        creating={false}
      />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("renders the modal when open=true in create mode", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("Create Hivemind")).toBeInTheDocument();
    expect(screen.getByText("New review team")).toBeInTheDocument();
  });

  it("shows rounds editor with Round 1", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("Round 1")).toBeInTheDocument();
  });

  it("has Add model button", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("Add model")).toBeInTheDocument();
  });

  it("has Add round button", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("Add round")).toBeInTheDocument();
  });

  it("has Save Hivemind button", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("Save Hivemind")).toBeInTheDocument();
  });

  it("has Cancel button", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("Cancel")).toBeInTheDocument();
  });

  it("calls onClose when Save Hivemind is clicked", async () => {
    const user = userEvent.setup();
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    const saveBtn = screen.getByText("Save Hivemind");
    await user.click(saveBtn);
    expect(onClose).toHaveBeenCalled();
  });

  it("shows Team name and Description fields", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("Team name")).toBeInTheDocument();
    expect(screen.getByText("Description")).toBeInTheDocument();
  });

  it("renders in edit mode with existing hivemind data", () => {
    const hm = {
      id: "enhance",
      name: "enhance",
      description: "General code review",
      runs: 42,
      rounds_config: JSON.stringify([
        {
          timeout: 300,
          models: [
            { id: "claude-opus-4.1", provider: "anthropic", thinking: "high", max_tokens: 16384 },
            { id: "gpt-5-codex", provider: "openai", thinking: "high", max_tokens: 16384 },
          ],
        },
      ]),
      inherit_orchestrator: true,
      orchestrator_model: null,
      orchestrator_provider: null,
      orchestrator_thinking: "high",
      orchestrator_context_window: null,
      orchestrator_max_output: null,
      created_at: "2026-05-09T00:00:00Z",
      updated_at: "2026-05-09T00:00:00Z",
    };
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={hm}
        creating={false}
      />,
    );
    expect(screen.getByText("Edit Hivemind")).toBeInTheDocument();
  });

  it("shows how rounds work hint", () => {
    render(
      <HivemindEditModal
        open={true}
        onClose={onClose}
        hivemind={null}
        creating={true}
      />,
    );
    expect(screen.getByText("How rounds work")).toBeInTheDocument();
  });

  it("shows a Clone button in the round header", () => {
    render(
      <HivemindEditModal open={true} onClose={onClose} hivemind={null} creating={true} />,
    );
    expect(screen.getByText("Clone")).toBeInTheDocument();
  });

  it("clones a round when Clone is clicked", async () => {
    const user = userEvent.setup();
    render(
      <HivemindEditModal open={true} onClose={onClose} hivemind={null} creating={true} />,
    );
    // Initially 1 round
    expect(screen.getByText("Round 1")).toBeInTheDocument();
    expect(screen.queryByText("Round 2")).not.toBeInTheDocument();

    // Click Clone
    await user.click(screen.getByText("Clone"));

    // Now 2 rounds
    expect(screen.getByText("Round 1")).toBeInTheDocument();
    expect(screen.getByText("Round 2")).toBeInTheDocument();
  });

  it("cloned round preserves model configuration", async () => {
    const user = userEvent.setup();
    const hm = {
      id: "test",
      name: "test",
      description: "",
      runs: 0,
      rounds_config: JSON.stringify([{
        timeout: 300,
        models: [
          { id: "claude-opus-4.1", provider: "anthropic", thinking: "high", max_tokens: 16384 },
          { id: "gpt-5-codex", provider: "openai", thinking: "medium", max_tokens: 8192 },
        ],
      }]),
      inherit_orchestrator: true,
      orchestrator_model: null,
      orchestrator_provider: null,
      orchestrator_thinking: "high",
      orchestrator_context_window: null,
      orchestrator_max_output: null,
      created_at: "2026-05-09T00:00:00Z",
      updated_at: "2026-05-09T00:00:00Z",
    };
    render(
      <HivemindEditModal open={true} onClose={onClose} hivemind={hm} creating={false} />,
    );

    await user.click(screen.getAllByText("Clone")[0]);

    // Should now show Round 2 with 2 models (same as Round 1)
    expect(screen.getByText("Round 2")).toBeInTheDocument();
    // Footer counter should reflect 4 models · 2 rounds
    expect(screen.getByText(/4 models · 2 rounds/)).toBeInTheDocument();
    // Verify actual model IDs are rendered in both rounds (each model ID button text appears twice)
    const opusButtons = screen.getAllByText("claude-opus-4.1");
    expect(opusButtons).toHaveLength(2);
    const codexButtons = screen.getAllByText("gpt-5-codex");
    expect(codexButtons).toHaveLength(2);
  });

  it("auto-fills Max tokens from the model's outNum on select", async () => {
    const user = userEvent.setup();
    render(
      <HivemindEditModal open={true} onClose={onClose} hivemind={null} creating={true} />,
    );
    // Open the model browser for the existing slot in Round 1
    await user.click(screen.getByText(/Click to choose model.../));
    // The mocked ModelBrowser is rendered; click its "select 32k" button
    await user.click(screen.getByTestId("mock-select-32k"));
    const maxTokensInput = screen.getByLabelText(/Max output tokens/) as HTMLInputElement;
    expect(maxTokensInput.value).toBe("32000");
  });

  it("caps absurdly large model max output when auto-filling", async () => {
    const user = userEvent.setup();
    render(
      <HivemindEditModal open={true} onClose={onClose} hivemind={null} creating={true} />,
    );
    await user.click(screen.getByText(/Click to choose model.../));
    await user.click(screen.getByTestId("mock-select-huge"));
    const maxTokensInput = screen.getByLabelText(/Max output tokens/) as HTMLInputElement;
    // Capped at 200_000 to avoid silly default request sizes.
    expect(Number(maxTokensInput.value)).toBeLessThanOrEqual(200_000);
  });

  it("does not change Max tokens when the model has no outNum", async () => {
    const user = userEvent.setup();
    render(
      <HivemindEditModal open={true} onClose={onClose} hivemind={null} creating={true} />,
    );
    const before = (screen.getByLabelText(/Max output tokens/) as HTMLInputElement).value;
    await user.click(screen.getByText(/Click to choose model.../));
    await user.click(screen.getByTestId("mock-select-no-outnum"));
    const after = (screen.getByLabelText(/Max output tokens/) as HTMLInputElement).value;
    expect(after).toBe(before);
  });

  it("editing a cloned round does not affect the original", async () => {
    const user = userEvent.setup();
    const hm = {
      id: "test",
      name: "test",
      description: "",
      runs: 0,
      rounds_config: JSON.stringify([{
        timeout: 300,
        models: [
          { id: "claude-opus-4.1", provider: "anthropic", thinking: "high", max_tokens: 16384 },
        ],
      }]),
      inherit_orchestrator: true,
      orchestrator_model: null,
      orchestrator_provider: null,
      orchestrator_thinking: "high",
      orchestrator_context_window: null,
      orchestrator_max_output: null,
      created_at: "2026-05-09T00:00:00Z",
      updated_at: "2026-05-09T00:00:00Z",
    };
    render(
      <HivemindEditModal open={true} onClose={onClose} hivemind={hm} creating={false} />,
    );

    // Clone the round
    await user.click(screen.getAllByText("Clone")[0]);
    expect(screen.getByText("Round 2")).toBeInTheDocument();

    // Remove the model from Round 2 using data-testid scoping
    const round2 = within(screen.getByTestId("round-1"));
    await user.click(round2.getByTestId("remove-model"));

    // Round 1 should still show its model
    const round1 = within(screen.getByTestId("round-0"));
    expect(round1.getByText("claude-opus-4.1")).toBeInTheDocument();
    // Round 2 should have 0 models
    expect(round2.queryByText("claude-opus-4.1")).not.toBeInTheDocument();
    // Footer should reflect 1 models · 2 rounds
    expect(screen.getByText(/1 models · 2 rounds/)).toBeInTheDocument();
  });
});
