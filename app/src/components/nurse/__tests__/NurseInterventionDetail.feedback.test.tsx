import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

beforeEach(() => {
  (globalThis as { isTauri?: unknown }).isTauri = true;
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
});
afterEach(() => {
  delete (globalThis as { isTauri?: unknown }).isTauri;
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

vi.mock("../../../lib/ipc", async () => {
  const actual = await vi.importActual<typeof import("../../../lib/ipc")>(
    "../../../lib/ipc",
  );
  return {
    ...actual,
    recordNurseInterventionFeedback: vi.fn().mockResolvedValue(undefined),
    getNurseDecisionChain: vi
      .fn()
      .mockResolvedValue({
        decision_id: "dec-1",
        session_id: "sess-1",
        events: [],
      }),
    getNurseCapture: vi.fn().mockResolvedValue(""),
  };
});

import { NurseInterventionDetail } from "../NurseInterventionDetail";
import {
  recordNurseInterventionFeedback,
} from "../../../lib/ipc";
import type { NurseInterventionRecord } from "../../../lib/nurseTypes";

function baseRecord(
  overrides: Partial<NurseInterventionRecord> = {},
): NurseInterventionRecord {
  return {
    id: "int-1",
    session_id: "sess-1",
    timestamp: "2026-05-19T00:00:00.000Z",
    level: "steer",
    analysis: "Detected loop on tool xyz.",
    action_taken: {
      level: "steer",
      session_id: "sess-1",
      message: "switch strategy",
      timestamp: "2026-05-19T00:00:00.000Z",
    },
    outcome: null,
    tier: "llm",
    decision_id: "dec-1",
    ...overrides,
  };
}

describe("NurseInterventionDetail feedback", () => {
  beforeEach(() => {
    (recordNurseInterventionFeedback as unknown as Mock).mockClear();
  });

  it("fires record_nurse_intervention_feedback when 👍 is clicked", async () => {
    render(<NurseInterventionDetail record={baseRecord()} />);
    const up = screen.getByLabelText(/thumbs up/i);
    fireEvent.click(up);
    await waitFor(() =>
      expect(recordNurseInterventionFeedback).toHaveBeenCalledWith({
        intervention_id: "int-1",
        rating: "up",
      }),
    );
  });

  it("fires record_nurse_intervention_feedback when 👎 is clicked", async () => {
    render(<NurseInterventionDetail record={baseRecord({ id: "int-2" })} />);
    const down = screen.getByLabelText(/thumbs down/i);
    fireEvent.click(down);
    await waitFor(() =>
      expect(recordNurseInterventionFeedback).toHaveBeenCalledWith({
        intervention_id: "int-2",
        rating: "down",
      }),
    );
  });

  it("marks the chosen button as aria-pressed after a successful submit", async () => {
    render(<NurseInterventionDetail record={baseRecord()} />);
    const up = screen.getByLabelText(/thumbs up/i);
    fireEvent.click(up);
    await waitFor(() => {
      expect(up.getAttribute("aria-pressed")).toBe("true");
    });
  });
});
