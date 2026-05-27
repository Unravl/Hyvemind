import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { NurseTunableField } from "../NurseTunableField";
import type { TunableDef } from "../../../lib/nurseTypes";

/**
 * Regression guard: every tunable field MUST render
 *  (a) the right input primitive for its kind
 *  (b) the unit, default, direction copy, AND description
 *
 * A bare i32 slider without unit / description is the exact bug this
 * test exists to prevent.
 */
describe("NurseTunableField rendering by kind", () => {
  it("renders numeric_range as a slider + always shows unit, default, direction, description", () => {
    const def: TunableDef = {
      name: "stall_threshold_secs",
      kind: "numeric_range",
      unit: "seconds",
      direction: "higher_less_sensitive",
      default: 300,
      safe_range: { min: 60, max: 1800, step: 30 },
      description: "Time without activity before stall is declared.",
    };
    render(<NurseTunableField def={def} value={300} onChange={vi.fn()} />);

    const field = screen.getByTestId("nurse-tunable-field");
    expect(field.dataset.kind).toBe("numeric_range");

    // The slider is a real <input type="range"> inside this field.
    const slider = field.querySelector('input[type="range"]') as HTMLInputElement;
    expect(slider).not.toBeNull();
    expect(slider.min).toBe("60");
    expect(slider.max).toBe("1800");

    // Unit, default, direction, description ALL must render.
    expect(field.textContent).toMatch(/seconds/);
    expect(field.textContent).toMatch(/default: 300/);
    expect(
      screen.getByTestId("nurse-tunable-direction").textContent,
    ).toMatch(/Higher = less sensitive/i);
    expect(
      screen.getByTestId("nurse-tunable-description").textContent,
    ).toMatch(/stall is declared/i);
  });

  it("renders stepper as a number input with explicit unit + description", () => {
    const def: TunableDef = {
      name: "max_attempts",
      kind: "stepper",
      unit: "attempts",
      direction: "higher_more_sensitive",
      default: 3,
      safe_range: { min: 0, max: 10 },
      description: "How many fix attempts before giving up.",
    };
    render(<NurseTunableField def={def} value={3} onChange={vi.fn()} />);
    const field = screen.getByTestId("nurse-tunable-field");
    expect(field.dataset.kind).toBe("stepper");
    const numInput = field.querySelector('input[type="number"]') as HTMLInputElement;
    expect(numInput).not.toBeNull();
    expect(field.textContent).toMatch(/attempts/);
    expect(
      screen.getByTestId("nurse-tunable-description").textContent,
    ).toMatch(/fix attempts/i);
  });

  it("renders enum as a select with the provided choices", () => {
    const def: TunableDef = {
      name: "match_mode",
      kind: "enum",
      unit: "",
      direction: "neutral",
      default: "exact",
      safe_range: {
        choices: [
          { value: "exact", label: "Exact" },
          { value: "fuzzy", label: "Fuzzy" },
        ],
      },
      description: "How identical two tool calls must be to count as a loop.",
    };
    render(
      <NurseTunableField def={def} value={"exact"} onChange={vi.fn()} />,
    );
    const field = screen.getByTestId("nurse-tunable-field");
    expect(field.dataset.kind).toBe("enum");
    const select = field.querySelector("select") as HTMLSelectElement;
    expect(select).not.toBeNull();
    expect(select.options.length).toBe(2);
    // Description renders.
    expect(
      screen.getByTestId("nurse-tunable-description").textContent,
    ).toMatch(/count as a loop/i);
  });

  it("renders toggle as a checkbox", () => {
    const def: TunableDef = {
      name: "scrub_ansi",
      kind: "toggle",
      unit: "",
      direction: "neutral",
      default: true,
      safe_range: null,
      description: "Strip ANSI escapes from tool output before classification.",
    };
    render(<NurseTunableField def={def} value={true} onChange={vi.fn()} />);
    const field = screen.getByTestId("nurse-tunable-field");
    expect(field.dataset.kind).toBe("toggle");
    const cb = field.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(cb).not.toBeNull();
    expect(cb.checked).toBe(true);
  });

  it("renders text as a text input with unit + description", () => {
    const def: TunableDef = {
      name: "label_prefix",
      kind: "text",
      unit: "",
      direction: "neutral",
      default: "auto-nurse",
      safe_range: null,
      description: "Prefix attached to every intervention message.",
    };
    render(
      <NurseTunableField
        def={def}
        value={"auto-nurse"}
        onChange={vi.fn()}
      />,
    );
    const field = screen.getByTestId("nurse-tunable-field");
    expect(field.dataset.kind).toBe("text");
    const text = field.querySelector('input[type="text"]') as HTMLInputElement;
    expect(text).not.toBeNull();
    expect(text.value).toBe("auto-nurse");
    expect(
      screen.getByTestId("nurse-tunable-description").textContent,
    ).toMatch(/intervention message/i);
  });

  it("hides the direction line when direction is 'neutral'", () => {
    const def: TunableDef = {
      name: "label",
      kind: "text",
      unit: "",
      direction: "neutral",
      default: "x",
      safe_range: null,
      description: "test description",
    };
    render(<NurseTunableField def={def} value={"x"} onChange={vi.fn()} />);
    expect(screen.queryByTestId("nurse-tunable-direction")).toBeNull();
  });
});
