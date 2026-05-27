import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { NurseBudgetEditor } from "../NurseBudgetEditor";
import type { BudgetConfig } from "../../../lib/nurseTypes";

function makeBudget(overrides: Partial<BudgetConfig> = {}): BudgetConfig {
  return {
    initial_cap: 6,
    decay_per_hour: 3,
    max_cap: 12,
    per_detector_cap: 3,
    per_key_cooldown_secs: 120,
    ...overrides,
  };
}

describe("NurseBudgetEditor validation", () => {
  it("renders the five hand-built fields, each with unit + helper", () => {
    render(
      <NurseBudgetEditor budget={makeBudget()} onChange={vi.fn()} />,
    );
    const fields = screen.getAllByTestId("nurse-budget-field");
    expect(fields).toHaveLength(5);
    // Every field has an explicit unit string and a one-line helper.
    for (const f of fields) {
      expect(f.textContent).toMatch(/seconds|interventions|hour/i);
    }
  });

  it("rejects max_cap < initial_cap and surfaces an inline error", () => {
    const onChange = vi.fn();
    render(
      <NurseBudgetEditor budget={makeBudget({ max_cap: 12 })} onChange={onChange} />,
    );
    // Find the "Max cap" input.
    const maxField = screen
      .getAllByTestId("nurse-budget-field")
      .find((el) => el.dataset.field === "max_cap")!;
    const maxInput = maxField.querySelector(
      'input[type="number"]',
    ) as HTMLInputElement;

    // Knock max_cap below initial_cap (initial is 6 — set max to 4).
    fireEvent.change(maxInput, { target: { value: "4" } });

    // Inline error must appear; onChange must NOT be invoked.
    const err = maxField.querySelector('[data-testid="nurse-budget-error"]');
    expect(err).not.toBeNull();
    expect(err?.textContent).toMatch(/initial cap/i);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("rejects per_detector_cap > initial_cap and surfaces an inline error", () => {
    const onChange = vi.fn();
    render(
      <NurseBudgetEditor budget={makeBudget()} onChange={onChange} />,
    );
    const detField = screen
      .getAllByTestId("nurse-budget-field")
      .find((el) => el.dataset.field === "per_detector_cap")!;
    const input = detField.querySelector(
      'input[type="number"]',
    ) as HTMLInputElement;

    // initial_cap is 6 — set per-detector to 9 (invalid).
    fireEvent.change(input, { target: { value: "9" } });

    const err = detField.querySelector('[data-testid="nurse-budget-error"]');
    expect(err).not.toBeNull();
    expect(err?.textContent).toMatch(/initial cap/i);
    expect(onChange).not.toHaveBeenCalled();
  });

  it("accepts valid edits and propagates onChange", () => {
    const onChange = vi.fn();
    render(
      <NurseBudgetEditor budget={makeBudget()} onChange={onChange} />,
    );
    const decayField = screen
      .getAllByTestId("nurse-budget-field")
      .find((el) => el.dataset.field === "decay_per_hour")!;
    const input = decayField.querySelector(
      'input[type="number"]',
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "5" } });

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange.mock.calls[0][0]).toMatchObject({ decay_per_hour: 5 });
  });

  it("rejects negative cooldown", () => {
    const onChange = vi.fn();
    render(
      <NurseBudgetEditor budget={makeBudget()} onChange={onChange} />,
    );
    const cooldownField = screen
      .getAllByTestId("nurse-budget-field")
      .find((el) => el.dataset.field === "per_key_cooldown_secs")!;
    const input = cooldownField.querySelector(
      'input[type="number"]',
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "-5" } });
    const err = cooldownField.querySelector('[data-testid="nurse-budget-error"]');
    expect(err).not.toBeNull();
    expect(onChange).not.toHaveBeenCalled();
  });
});
