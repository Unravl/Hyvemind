import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { NurseSessionCard } from "../NurseSessionCard";
import type { DerivedSession } from "../../../hooks/useNurseSessions";
import type { MonitoredSessionSnapshot } from "../../../lib/nurseTypes";

function baseSession(
  overrides: Partial<MonitoredSessionSnapshot> = {},
): MonitoredSessionSnapshot {
  return {
    session_id: "sess-1234-abcd",
    last_activity_ms: Date.now() - 5_000,
    event_count: 12,
    is_alive: true,
    is_busy: false,
    status: "healthy",
    stall_detected_at: null,
    intervention_count: 0,
    last_check_at: new Date().toISOString(),
    ...overrides,
  };
}

function derived(
  s: MonitoredSessionSnapshot,
  tier: DerivedSession["tier"],
): DerivedSession {
  return { session: s, tier, age_ms: Date.now() - s.last_activity_ms };
}

describe("NurseSessionCard tier color derivation", () => {
  it("renders the quiet border when no active signals are present", () => {
    const s = baseSession();
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    const card = screen.getByTestId("nurse-session-card");
    expect(card.dataset.tier).toBe("quiet");
    expect(card.className).toContain("border-emerald-500/30");
  });

  it("renders the warning border when the highest signal is warn", () => {
    const s = baseSession({
      highest_severity: "warn",
      active_signals: [
        {
          detector: "loop",
          dedup_key: "loop:exact:abc",
          severity: "warn",
          description: "exact-repeat loop",
          raised_at: new Date().toISOString(),
        },
      ],
    });
    render(
      <NurseSessionCard
        derived={derived(s, "warning")}
        onOpenDetail={vi.fn()}
      />,
    );
    const card = screen.getByTestId("nurse-session-card");
    expect(card.dataset.tier).toBe("warning");
    expect(card.className).toContain("border-amber-400/40");
  });

  it("renders the critical border + pulse-red when highest is critical", () => {
    const s = baseSession({
      highest_severity: "critical",
      active_signals: [
        {
          detector: "tool_failure",
          dedup_key: "tool_stuck:bash:xyz",
          severity: "critical",
          description: "bash failure repeating",
          raised_at: new Date().toISOString(),
        },
      ],
    });
    render(
      <NurseSessionCard
        derived={derived(s, "critical")}
        onOpenDetail={vi.fn()}
      />,
    );
    const card = screen.getByTestId("nurse-session-card");
    expect(card.dataset.tier).toBe("critical");
    expect(card.className).toContain("border-red-500");
    expect(card.className).toContain("pulse-red");
  });

  it("derives tier from highest active signal severity (mixed severities)", () => {
    // Owner card receives the highest of [warn, stalled] — stalled wins.
    const s = baseSession({
      highest_severity: "stalled",
      active_signals: [
        {
          detector: "loop",
          dedup_key: "loop:exact:abc",
          severity: "warn",
          description: "warn-level",
          raised_at: new Date().toISOString(),
        },
        {
          detector: "stall",
          dedup_key: "stall:1",
          severity: "stalled",
          description: "stall-level",
          raised_at: new Date().toISOString(),
        },
      ],
    });
    render(
      <NurseSessionCard
        derived={derived(s, "stalled")}
        onOpenDetail={vi.fn()}
      />,
    );
    const card = screen.getByTestId("nurse-session-card");
    expect(card.dataset.tier).toBe("stalled");
    expect(card.className).toContain("border-red-400/50");
  });

  it("fires onOpenDetail with the session id when clicked", () => {
    const onOpen = vi.fn();
    const s = baseSession();
    render(<NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={onOpen} />);
    screen.getByTestId("nurse-session-card").click();
    expect(onOpen).toHaveBeenCalledWith(s.session_id);
  });
});

describe("NurseSessionCard owner label rendering", () => {
  it("renders task owner using task_id", () => {
    const s = baseSession({
      owner: { kind: "task", task_id: "task-abcd1234-efgh" },
    });
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    expect(screen.getByText(/Task task-abc/)).toBeInTheDocument();
  });

  it("renders review owner using job_id without crashing", () => {
    const s = baseSession({
      owner: { kind: "review", job_id: "hmr-abcd1234-efgh" },
    });
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    expect(screen.getByText(/Review hmr-abcd/)).toBeInTheDocument();
  });

  it("renders merge owner using job_id and round (no swarm_id)", () => {
    const s = baseSession({
      owner: { kind: "merge", job_id: "hmr-deadbeef-xyz", round: 2 },
    });
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    expect(screen.getByText(/Merge hmr-dead.*r2/)).toBeInTheDocument();
  });

  it("renders merge owner with optional swarm_id present", () => {
    const s = baseSession({
      owner: {
        kind: "merge",
        job_id: "hmr-deadbeef-xyz",
        round: 3,
        swarm_id: "swm-12345678",
      },
    });
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    expect(screen.getByText(/Merge hmr-dead.*r3/)).toBeInTheDocument();
  });

  it("renders swarm owner with role (no feature_id)", () => {
    const s = baseSession({
      owner: { kind: "swarm", swarm_id: "swm-12345678", role: "worker" },
    });
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    expect(screen.getByText(/Swarm swm-1234.*worker/)).toBeInTheDocument();
  });

  it("renders swarm owner with role and feature_id", () => {
    const s = baseSession({
      owner: {
        kind: "swarm",
        swarm_id: "swm-12345678",
        role: "guard",
        feature_id: "feat-001",
      },
    });
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    expect(
      screen.getByText(/Swarm swm-1234.*guard.*feat-001/),
    ).toBeInTheDocument();
  });

  it("renders unknown owner as Unknown", () => {
    const s = baseSession({ owner: { kind: "unknown" } });
    render(
      <NurseSessionCard derived={derived(s, "quiet")} onOpenDetail={vi.fn()} />,
    );
    expect(screen.getByText("Unknown")).toBeInTheDocument();
  });
});
