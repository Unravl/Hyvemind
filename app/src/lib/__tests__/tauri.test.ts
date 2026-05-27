import { describe, it, expect, afterEach } from "vitest";
import { isTauri } from "../tauri";

describe("isTauri", () => {
  afterEach(() => {
    delete (window as any).__TAURI_INTERNALS__;
  });

  it("returns false when __TAURI_INTERNALS__ not on window", () => {
    expect(isTauri()).toBe(false);
  });

  it("returns true when __TAURI_INTERNALS__ is on window", () => {
    (window as any).__TAURI_INTERNALS__ = {};
    expect(isTauri()).toBe(true);
  });
});
