import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { NeuralWattUsagePill } from "../widgets/NeuralWattUsagePill";
import type { SnapshotEntry, SnapshotStatus, Tone } from "../types";

/** Build a SnapshotEntry for the neuralwatt_usage extension.
 *
 *  `mode` controls whether subscription plan data (kwh_included)
 *  is present in the metrics array. */
function entry(
  status: SnapshotStatus,
  headlineValue: number,
  tone: Tone = "ok",
  mode: "subscription" | "fallback" = "fallback",
): SnapshotEntry {
  const metrics =
    mode === "subscription"
      ? [
          {
            key: "kwh_used",
            label: "Used",
            display: `${headlineValue.toFixed(2)}%`,
            value: 0.0072,
            kind: "count" as const,
            tone: "ok" as const,
          },
          {
            key: "kwh_included",
            label: "Included",
            display: "0.0100 kWh",
            value: 0.01,
            kind: "count" as const,
            tone: "ok" as const,
          },
        ]
      : [];

  return {
    manifest: {
      id: "neuralwatt_usage:neuralwatt",
      type_id: "neuralwatt_usage",
      provider_id: "neuralwatt",
      display_name: "NeuralWatt Usage (neuralwatt)",
      description: "Displays NeuralWatt energy usage.",
      capabilities: ["usage", "billing"],
      requires_api_key: true,
      docs_url: null,
    },
    snapshot:
      status === "ok"
        ? {
            extension_id: "neuralwatt_usage:neuralwatt",
            provider_id: "neuralwatt",
            fetched_at: 0,
            headline: {
              key: mode === "subscription" ? "kwh_pct" : "energy_kwh",
              label: mode === "subscription" ? "Usage" : "Energy",
              display:
                mode === "subscription"
                  ? `${Math.round(headlineValue)}%`
                  : `${headlineValue.toFixed(4)} kWh`,
              value: headlineValue,
              kind: "percentage" as const,
              tone,
            },
            metrics,
            raw: null,
          }
        : null,
    last_error: null,
    last_fetched_at: 0,
    status,
    user_settings: { enabled: true, show_in_topbar: true, preferences: {} },
  };
}

describe("NeuralWattUsagePill", () => {
  describe("subscription mode (plan data available)", () => {
    it("renders the NW badge and used percentage with a progress bar", () => {
      render(
        <NeuralWattUsagePill entry={entry("ok", 72, "ok", "subscription")} />,
      );
      // NW badge
      expect(screen.getByText("NW")).toBeInTheDocument();
      // Percentage
      expect(screen.getByText("72%")).toBeInTheDocument();
    });

    it("applies green fill for usedPct < 60", () => {
      render(
        <NeuralWattUsagePill entry={entry("ok", 42, "ok", "subscription")} />,
      );
      const pctSpan = screen.getByText("42%");
      expect(pctSpan.style.color).toBe("rgb(110, 231, 183)");
    });

    it("applies yellow fill for usedPct ≥ 60", () => {
      render(
        <NeuralWattUsagePill entry={entry("ok", 65, "ok", "subscription")} />,
      );
      const pctSpan = screen.getByText("65%");
      expect(pctSpan.style.color).toBe("rgb(252, 211, 77)");
    });

    it("applies amber fill for usedPct ≥ 75", () => {
      render(
        <NeuralWattUsagePill entry={entry("ok", 80, "warn", "subscription")} />,
      );
      const pctSpan = screen.getByText("80%");
      expect(pctSpan.style.color).toBe("rgb(251, 146, 60)");
    });

    it("applies red fill for usedPct ≥ 90", () => {
      render(
        <NeuralWattUsagePill entry={entry("ok", 95, "crit", "subscription")} />,
      );
      const pctSpan = screen.getByText("95%");
      expect(pctSpan.style.color).toBe("rgb(252, 165, 165)");
    });

    it("clamps usedPct to 0–100", () => {
      render(
        <NeuralWattUsagePill
          entry={entry("ok", 150, "crit", "subscription")}
        />,
      );
      // Should clamp to 100%
      expect(screen.getByText("100%")).toBeInTheDocument();
    });
  });

  describe("fallback mode (no plan data)", () => {
    it("renders the NW badge and kWh display string", () => {
      render(<NeuralWattUsagePill entry={entry("ok", 0.1234, "ok")} />);
      expect(screen.getByText("NW")).toBeInTheDocument();
      expect(screen.getByText("0.1234 kWh")).toBeInTheDocument();
    });
  });

  describe("gating / edge cases", () => {
    it("renders nothing when status is loading", () => {
      const { container } = render(
        <NeuralWattUsagePill entry={entry("loading", 42)} />,
      );
      expect(container).toBeEmptyDOMElement();
    });

    it("renders nothing when status is unsupported", () => {
      const { container } = render(
        <NeuralWattUsagePill entry={entry("unsupported", 42)} />,
      );
      expect(container).toBeEmptyDOMElement();
    });

    it("renders nothing when status is disabled", () => {
      const { container } = render(
        <NeuralWattUsagePill entry={entry("disabled", 42)} />,
      );
      expect(container).toBeEmptyDOMElement();
    });

    it("renders nothing when headline is null", () => {
      const e = entry("ok", 42);
      if (e.snapshot) e.snapshot.headline = null;
      const { container } = render(<NeuralWattUsagePill entry={e} />);
      expect(container).toBeEmptyDOMElement();
    });
  });

  describe("tone classes on the pill container", () => {
    it("applies amber tone container for warn", () => {
      render(<NeuralWattUsagePill entry={entry("ok", 0.5, "warn")} />);
      const pill = screen.getByText("NW").parentElement;
      expect(pill?.className).toMatch(/amber/);
    });

    it("applies red tone container for crit", () => {
      render(<NeuralWattUsagePill entry={entry("ok", 0.5, "crit")} />);
      const pill = screen.getByText("NW").parentElement;
      expect(pill?.className).toMatch(/red/);
    });

    it("applies emerald tone container for ok (fallback)", () => {
      render(<NeuralWattUsagePill entry={entry("ok", 0.1234, "ok")} />);
      const pill = screen.getByText("NW").parentElement;
      expect(pill?.className).toMatch(/emerald/);
    });
  });
});
