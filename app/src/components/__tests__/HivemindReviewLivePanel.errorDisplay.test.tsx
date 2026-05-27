import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { HivemindReviewLivePanel } from "../HivemindReviewLivePanel";
import type { ReviewState } from "../../lib/hivemindReducer";

// The panel pulls in MergedPlanModal → IPC. Stub the lazy plan loader.
vi.mock("../../lib/mergedPlanLoader", () => ({
  loadMergedPlan: vi.fn(async () => null),
}));

function makeStateWithFailedModel(
  errorMessage: string | undefined,
  modelId = "anthropic/claude-sonnet-4",
): ReviewState {
  return {
    jobId: "job-err-1",
    status: "running",
    phase: "round",
    rounds: {
      1: {
        round: 1,
        models: {
          [`${modelId}#0`]: {
            instanceKey: `${modelId}#0`,
            modelId,
            status: "failed",
            outputPreview: "",
            ...(errorMessage !== undefined ? { errorMessage } : {}),
          },
        },
        modelOrder: [`${modelId}#0`],
      },
    },
    roundOrder: [1],
    merges: {},
    mergeOrder: [],
    startedAt: Date.now() - 1000,
  };
}

const LONG_ERROR =
  "Anthropic API error 401 Unauthorized: { \"type\":\"error\", \"error\": { \"type\":\"authentication_error\", \"message\":\"invalid x-api-key\" } } " +
  "[".padEnd(360, "x") +
  "]";

describe("HivemindReviewLivePanel — failed-model error display", () => {
  beforeEach(() => {
    // Stub Clipboard API for the Copy error button test.
    Object.assign(navigator, {
      clipboard: { writeText: vi.fn(async () => undefined) },
    });
  });

  it("renders the full error verbatim and not truncated", () => {
    expect(LONG_ERROR.length).toBeGreaterThan(400);
    const state = makeStateWithFailedModel(LONG_ERROR);
    render(<HivemindReviewLivePanel state={state} />);

    // The error text is rendered in full inside the region.
    const region = screen.getByRole("region", { name: /error detail/i });
    expect(region).toBeInTheDocument();
    expect(region.textContent).toBe(LONG_ERROR);
    // The compact pill label "Failed" sits in the metric slot.
    expect(screen.getByText("Failed")).toBeInTheDocument();
  });

  it("region has aria-label referencing the reviewer label and aria-live=off", () => {
    const state = makeStateWithFailedModel(LONG_ERROR, "openai/gpt-4o");
    render(<HivemindReviewLivePanel state={state} />);
    const region = screen.getByRole("region", { name: /error detail/i });
    expect(region.getAttribute("aria-label")).toMatch(/openai\/gpt-4o/);
    expect(region.getAttribute("aria-live")).toBe("off");
  });

  it("clicks Copy error button → calls navigator.clipboard.writeText with full error", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    const state = makeStateWithFailedModel(LONG_ERROR);
    render(<HivemindReviewLivePanel state={state} />);
    const btn = screen.getByRole("button", { name: /copy error message/i });
    fireEvent.click(btn);
    // The handler awaits; flush the microtask queue.
    await Promise.resolve();
    expect(writeText).toHaveBeenCalledWith(LONG_ERROR);
  });

  it("falls back to document.execCommand('copy') when clipboard.writeText rejects", async () => {
    const writeText = vi.fn(async () => {
      throw new Error("blocked");
    });
    Object.assign(navigator, { clipboard: { writeText } });
    const execSpy = vi.fn(() => true);
    // jsdom doesn't implement execCommand by default; install a stub before spying.
    (document as unknown as { execCommand: typeof execSpy }).execCommand = execSpy;

    const state = makeStateWithFailedModel(LONG_ERROR);
    render(<HivemindReviewLivePanel state={state} />);
    const btn = screen.getByRole("button", { name: /copy error message/i });
    fireEvent.click(btn);
    await Promise.resolve();
    await Promise.resolve();
    expect(writeText).toHaveBeenCalled();
    expect(execSpy).toHaveBeenCalledWith("copy");
  });

  it("shows 'Model call failed' fallback when errorMessage is undefined", () => {
    const state = makeStateWithFailedModel(undefined);
    render(<HivemindReviewLivePanel state={state} />);
    expect(screen.getByText("Model call failed")).toBeInTheDocument();
    // No region — there's nothing to render in the detail block.
    expect(
      screen.queryByRole("region", { name: /error detail/i }),
    ).not.toBeInTheDocument();
  });
});
