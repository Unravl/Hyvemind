import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { NurseHeader } from "../NurseHeader";
import { NurseProvider } from "../../../lib/NurseProvider";
import type {
  NurseHealth,
  NurseServiceConfigSnapshot,
  NurseStats,
  ProviderHealthSnapshot,
} from "../../../lib/nurseTypes";

function baseConfig(): NurseServiceConfigSnapshot {
  return {
    enabled: true,
    stall_threshold_secs: 300,
    nurse_model: "anthropic/claude-haiku-4.5",
    max_interventions: 3,
    tick_interval_secs: 60,
    nurse_provider: null,
  };
}

function baseHealth(): NurseHealth {
  return {
    last_tick_at: Date.now() - 1000,
    last_successful_tick_at: Date.now() - 1000,
    consecutive_failed_ticks: 0,
    consecutive_bad_parse_ticks: 0,
    consecutive_skipped_ticks: 0,
    degraded: false,
  };
}

function baseStats(): NurseStats {
  return {
    monitored_count: 1,
    stall_count: 0,
    intervention_count: 0,
    last_check_at: new Date().toISOString(),
    is_running: true,
  };
}

const providers: ProviderHealthSnapshot[] = [
  {
    provider_id: "anthropic",
    display_name: "Anthropic",
    breaker_state: "closed",
  },
  {
    provider_id: "openrouter",
    display_name: "OpenRouter",
    breaker_state: "half_open",
  },
  {
    provider_id: "deepseek",
    display_name: "DeepSeek",
    breaker_state: "open",
    retry_at: "2026-05-19T12:00:00.000Z",
  },
];

describe("NurseHeader provider pill cluster", () => {
  it("renders one pill per provider with breaker-state colored dots", () => {
    const { container } = render(
      <NurseProvider>
        <NurseHeader
          config={baseConfig()}
          health={baseHealth()}
          stats={baseStats()}
          providers={providers}
          onOpenModelBrowser={vi.fn()}
          onChangeConfig={vi.fn()}
        />
      </NurseProvider>,
    );

    // Provider names render.
    expect(screen.getByText("Anthropic")).toBeInTheDocument();
    expect(screen.getByText("OpenRouter")).toBeInTheDocument();
    expect(screen.getByText("DeepSeek")).toBeInTheDocument();

    // Each pill has a single colored dot. Map provider → expected
    // Tailwind color by checking the pills' DOM.
    const pills = container.querySelectorAll(
      'span[title^="Anthropic"], span[title^="OpenRouter"], span[title^="DeepSeek"]',
    );
    expect(pills.length).toBe(3);

    // Verify the breaker-color contract: closed → emerald, half_open → amber, open → red.
    const dotColor = (label: string): string => {
      const pill = Array.from(pills).find((p) =>
        p.getAttribute("title")?.startsWith(label),
      );
      const dot = pill?.querySelector("span");
      return dot?.className ?? "";
    };
    expect(dotColor("Anthropic")).toContain("bg-emerald-400");
    expect(dotColor("OpenRouter")).toContain("bg-amber-400");
    expect(dotColor("DeepSeek")).toContain("bg-red-400");
  });

  it("renders the retry_at timestamp in the open-state pill's title", () => {
    const { container } = render(
      <NurseProvider>
        <NurseHeader
          config={baseConfig()}
          health={baseHealth()}
          stats={baseStats()}
          providers={providers}
          onOpenModelBrowser={vi.fn()}
          onChangeConfig={vi.fn()}
        />
      </NurseProvider>,
    );
    const deepseekPill = container.querySelector(
      'span[title^="DeepSeek"]',
    ) as HTMLSpanElement;
    expect(deepseekPill).not.toBeNull();
    expect(deepseekPill.getAttribute("title") ?? "").toMatch(/Open until 2026/);
  });

  it("hides the provider cluster when no providers are supplied", () => {
    render(
      <NurseProvider>
        <NurseHeader
          config={baseConfig()}
          health={baseHealth()}
          stats={baseStats()}
          onOpenModelBrowser={vi.fn()}
          onChangeConfig={vi.fn()}
        />
      </NurseProvider>,
    );
    expect(screen.queryByText("Providers")).toBeNull();
  });
});
