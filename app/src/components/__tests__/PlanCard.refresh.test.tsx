import React from "react";
import { describe, it, expect, beforeEach, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";

// react-markdown reaches into the Tauri shell when handling external links;
// stub the plugin so unit tests don't require a Tauri runtime.
vi.mock("@tauri-apps/plugin-shell", () => ({
  open: vi.fn(),
}));

// Stub `renderMd` so this test focuses on the PlanCard footer states and
// doesn't pull the whole App.tsx graph in.
vi.mock("../../App", () => ({
  renderMd: (text: string) => <div data-testid="plan-body">{text}</div>,
}));

import { PlanCard } from "../PlanCard";

const PLAN = "## Plan\n- step 1\n- step 2";

beforeEach(() => {
  Object.assign(navigator, {
    clipboard: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
});

afterEach(() => {
  cleanup();
});

describe("PlanCard features-refresh states", () => {
  it("renders 'Refining features…' pulse and a disabled Launch button while pendingFeaturesRefresh is true", () => {
    render(
      <PlanCard
        planText={PLAN}
        onLaunchSwarm={() => {}}
        launchDisabledReason="Waiting for Queen to refine features…"
        pendingFeaturesRefresh={true}
        featuresRefreshFailed={false}
      />,
    );
    expect(screen.getByText(/Refining features/)).toBeTruthy();
    const launchBtn = screen.getByRole("button", { name: /Launch Swarm/i });
    expect((launchBtn as HTMLButtonElement).disabled).toBe(true);
  });

  it("renders the failure banner, an enabled Launch button, and the Re-emit button when featuresRefreshFailed is true", () => {
    const onReq = vi.fn();
    const onLaunch = vi.fn();
    render(
      <PlanCard
        planText={PLAN}
        onLaunchSwarm={onLaunch}
        launchDisabledReason={undefined}
        onRequestFeatures={onReq}
        pendingFeaturesRefresh={false}
        featuresRefreshFailed={true}
      />,
    );
    expect(
      screen.getByText(/Queen didn't emit refined features/),
    ).toBeTruthy();
    const launchBtn = screen.getByRole("button", { name: /Launch Swarm/i });
    expect((launchBtn as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByRole("button", { name: /Re-emit FEATURES/i })).toBeTruthy();
  });

  it("outer-footer-guard regression: footer + Re-emit button still render when all CTAs except onRequestFeatures are undefined", () => {
    const onReq = vi.fn();
    render(
      <PlanCard
        planText={PLAN}
        onImplement={undefined}
        onHivemindReview={undefined}
        onLaunchSwarm={undefined}
        onRequestFeatures={onReq}
        featuresRefreshFailed={true}
      />,
    );
    // Footer banner and Re-emit button are both rendered.
    expect(
      screen.getByText(/Queen didn't emit refined features/),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: /Re-emit FEATURES/i })).toBeTruthy();
  });

  it("memo-comparator regression: footer text + Re-emit button identity transitions are observed", () => {
    const onReq = vi.fn();
    const onLaunch = vi.fn();
    const { rerender } = render(
      <PlanCard
        planText={PLAN}
        onLaunchSwarm={onLaunch}
        onRequestFeatures={onReq}
        launchDisabledReason="Waiting for Queen…"
        pendingFeaturesRefresh={true}
        featuresRefreshFailed={false}
      />,
    );
    expect(screen.getByText(/Refining features/)).toBeTruthy();

    // Flip pendingFeaturesRefresh false / featuresRefreshFailed true.
    rerender(
      <PlanCard
        planText={PLAN}
        onLaunchSwarm={onLaunch}
        onRequestFeatures={onReq}
        launchDisabledReason={undefined}
        pendingFeaturesRefresh={false}
        featuresRefreshFailed={true}
      />,
    );
    expect(
      screen.getByText(/Queen didn't emit refined features/),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: /Re-emit FEATURES/i })).toBeTruthy();

    // Flip onRequestFeatures to undefined → Re-emit button must disappear.
    rerender(
      <PlanCard
        planText={PLAN}
        onLaunchSwarm={onLaunch}
        onRequestFeatures={undefined}
        launchDisabledReason={undefined}
        pendingFeaturesRefresh={false}
        featuresRefreshFailed={true}
      />,
    );
    expect(screen.queryByRole("button", { name: /Re-emit FEATURES/i })).toBeNull();
  });
});
