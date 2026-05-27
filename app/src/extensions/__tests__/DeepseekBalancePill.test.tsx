import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { DeepseekBalancePill } from "../widgets/DeepseekBalancePill";
import type { SnapshotEntry, SnapshotStatus, Tone } from "../types";

function entry(
  status: SnapshotStatus,
  value: number,
  tone: Tone = "ok",
): SnapshotEntry {
  return {
    manifest: {
      id: "deepseek_balance:deepseek",
      type_id: "deepseek_balance",
      provider_id: "deepseek",
      display_name: "DeepSeek Balance (deepseek)",
      description: "Displays remaining DeepSeek account credit.",
      capabilities: ["usage", "billing"],
      requires_api_key: true,
      docs_url: null,
    },
    snapshot:
      status === "ok"
        ? {
            extension_id: "deepseek_balance:deepseek",
            provider_id: "deepseek",
            fetched_at: 0,
            headline: {
              key: "balance",
              label: "Balance",
              display: `$${value.toFixed(2)}`,
              value,
              kind: "currency",
              tone,
            },
            metrics: [],
            raw: null,
          }
        : null,
    last_error: null,
    last_fetched_at: 0,
    status,
    user_settings: { enabled: true, show_in_topbar: true, preferences: {} },
  };
}

describe("DeepseekBalancePill", () => {
  it("renders the balance and DS badge on the ok path", () => {
    render(<DeepseekBalancePill entry={entry("ok", 12.34, "ok")} />);
    expect(screen.getByText("$12.34")).toBeInTheDocument();
    // DS badge
    expect(screen.getByText("DS")).toBeInTheDocument();
  });

  it("renders 'Depleted' when balance is zero", () => {
    render(<DeepseekBalancePill entry={entry("ok", 0, "crit")} />);
    expect(screen.getByText("Depleted")).toBeInTheDocument();
  });

  it("renders 'Depleted' when balance is negative", () => {
    render(<DeepseekBalancePill entry={entry("ok", -0.5, "crit")} />);
    expect(screen.getByText("Depleted")).toBeInTheDocument();
  });

  it("applies amber tone class for warn", () => {
    render(<DeepseekBalancePill entry={entry("ok", 2.5, "warn")} />);
    const badge = screen.getByText("DS");
    const pill = badge.parentElement;
    expect(pill?.className).toMatch(/amber/);
  });

  it("applies red tone class for crit", () => {
    render(<DeepseekBalancePill entry={entry("ok", 0.5, "crit")} />);
    const badge = screen.getByText("DS");
    const pill = badge.parentElement;
    expect(pill?.className).toMatch(/red/);
  });

  it("renders emerald for ok tone", () => {
    render(<DeepseekBalancePill entry={entry("ok", 15.0, "ok")} />);
    const badge = screen.getByText("DS");
    const pill = badge.parentElement;
    expect(pill?.className).toMatch(/emerald/);
  });

  it("renders nothing when status is loading", () => {
    const { container } = render(
      <DeepseekBalancePill entry={entry("loading", 5)} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing when status is error", () => {
    const { container } = render(
      <DeepseekBalancePill entry={entry("error", 5)} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing when headline is missing", () => {
    const e = entry("ok", 5);
    if (e.snapshot) e.snapshot.headline = null;
    const { container } = render(<DeepseekBalancePill entry={e} />);
    expect(container).toBeEmptyDOMElement();
  });
});
