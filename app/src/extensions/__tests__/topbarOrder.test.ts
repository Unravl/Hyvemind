import { describe, it, expect, beforeEach, vi } from "vitest";
import { loadOrder, saveOrder, applyOrder } from "../topbarOrder";
import type { SnapshotEntry } from "../types";

const STORAGE_KEY = "hyvemind:extension-topbar-order";

function entry(id: string): SnapshotEntry {
  return {
    manifest: {
      id,
      type_id: "mock",
      provider_id: id,
      display_name: id,
      description: "",
      capabilities: ["usage"],
      requires_api_key: false,
      docs_url: null,
    },
    snapshot: null,
    last_error: null,
    last_fetched_at: 0,
    status: "ok",
    user_settings: { enabled: true, show_in_topbar: true, preferences: {} },
  };
}

describe("applyOrder", () => {
  it("reorders matching IDs to follow the saved order", () => {
    const visible = [entry("a"), entry("b"), entry("c")];
    const out = applyOrder(visible, ["c", "a", "b"]);
    expect(out.map((e) => e.manifest.id)).toEqual(["c", "a", "b"]);
  });

  it("appends unknown IDs alphabetically after the saved ones", () => {
    const visible = [entry("zeta"), entry("alpha"), entry("mike"), entry("bravo")];
    // Saved knows only "mike". The rest are unknown and should sort
    // alphabetically after it.
    const out = applyOrder(visible, ["mike"]);
    expect(out.map((e) => e.manifest.id)).toEqual([
      "mike",
      "alpha",
      "bravo",
      "zeta",
    ]);
  });

  it("filters out saved IDs that are no longer visible", () => {
    const visible = [entry("a"), entry("c")];
    const out = applyOrder(visible, ["a", "b", "c"]);
    expect(out.map((e) => e.manifest.id)).toEqual(["a", "c"]);
  });

  it("handles empty saved order — sorts everything alphabetically", () => {
    const visible = [entry("zeta"), entry("alpha"), entry("mike")];
    const out = applyOrder(visible, []);
    expect(out.map((e) => e.manifest.id)).toEqual(["alpha", "mike", "zeta"]);
  });

  it("handles empty visible — returns empty", () => {
    expect(applyOrder([], ["a", "b"])).toEqual([]);
  });

  it("dedupes duplicate IDs in the saved order", () => {
    const visible = [entry("a"), entry("b")];
    const out = applyOrder(visible, ["a", "a", "b"]);
    expect(out.map((e) => e.manifest.id)).toEqual(["a", "b"]);
  });
});

describe("loadOrder / saveOrder", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("loadOrder returns [] when no entry exists", () => {
    expect(loadOrder()).toEqual([]);
  });

  it("loadOrder returns [] on malformed JSON", () => {
    localStorage.setItem(STORAGE_KEY, "{not json");
    expect(loadOrder()).toEqual([]);
  });

  it("loadOrder returns [] on non-array JSON", () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ foo: 1 }));
    expect(loadOrder()).toEqual([]);
  });

  it("loadOrder returns [] when array contains non-strings", () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(["a", 5, "b"]));
    expect(loadOrder()).toEqual([]);
  });

  it("saveOrder + loadOrder round-trips", () => {
    saveOrder(["x", "y", "z"]);
    expect(loadOrder()).toEqual(["x", "y", "z"]);
  });

  it("saveOrder swallows storage errors without throwing", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    // Swap localStorage wholesale — spying on the host object's setItem is
    // unreliable when Node's experimental native Storage is in play.
    const original = globalThis.localStorage;
    const throwingStorage: Storage = {
      length: 0,
      clear: () => {},
      getItem: () => null,
      key: () => null,
      removeItem: () => {},
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
    };
    Object.defineProperty(globalThis, "localStorage", {
      value: throwingStorage,
      writable: true,
      configurable: true,
    });
    try {
      expect(() => saveOrder(["a"])).not.toThrow();
      expect(warn).toHaveBeenCalled();
    } finally {
      Object.defineProperty(globalThis, "localStorage", {
        value: original,
        writable: true,
        configurable: true,
      });
      warn.mockRestore();
    }
  });
});
