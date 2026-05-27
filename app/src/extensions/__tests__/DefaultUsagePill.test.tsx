import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { DefaultUsagePill } from "../widgets/DefaultUsagePill";
import type { SnapshotEntry, SnapshotStatus, Tone } from "../types";

function entry(status: SnapshotStatus, tone: Tone = "ok"): SnapshotEntry {
  return {
    manifest: {
      id: "mock:foo",
      type_id: "mock",
      provider_id: "foo",
      display_name: "Mock",
      description: "test",
      capabilities: ["usage"],
      requires_api_key: false,
      docs_url: null,
    },
    snapshot:
      status === "ok"
        ? {
            extension_id: "mock:foo",
            provider_id: "foo",
            fetched_at: 0,
            headline: {
              key: "remaining",
              label: "Remaining",
              display: "$5.42",
              value: 5.42,
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

describe("DefaultUsagePill", () => {
  it("renders the headline display string when status is ok", () => {
    render(<DefaultUsagePill entry={entry("ok", "ok")} />);
    expect(screen.getByText("$5.42")).toBeInTheDocument();
    // Provider id rendered alongside.
    expect(screen.getByText("foo")).toBeInTheDocument();
  });

  it("renders nothing when status is loading", () => {
    const { container } = render(<DefaultUsagePill entry={entry("loading")} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing when status is unsupported", () => {
    const { container } = render(
      <DefaultUsagePill entry={entry("unsupported")} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing when status is disabled", () => {
    const { container } = render(
      <DefaultUsagePill entry={entry("disabled")} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing when headline is missing", () => {
    const e = entry("ok");
    if (e.snapshot) e.snapshot.headline = null;
    const { container } = render(<DefaultUsagePill entry={e} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("applies tone classes for warn", () => {
    render(<DefaultUsagePill entry={entry("ok", "warn")} />);
    const pill = screen.getByText("$5.42").parentElement;
    expect(pill?.className).toMatch(/amber/);
  });

  it("applies tone classes for crit", () => {
    render(<DefaultUsagePill entry={entry("ok", "crit")} />);
    const pill = screen.getByText("$5.42").parentElement;
    expect(pill?.className).toMatch(/red/);
  });
});
