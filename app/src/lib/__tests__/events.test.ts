import { describe, it, expect, vi, beforeEach, type Mock } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

import { listen } from "@tauri-apps/api/event";
import { onChatEvent, onHivemindProgress, onSwarmEvent } from "../events";

beforeEach(() => {
  (listen as Mock).mockReset();
});

// ── onChatEvent ──

describe("onChatEvent", () => {
  it("registers listener on chat-event channel", async () => {
    const unlisten = vi.fn();
    (listen as Mock).mockResolvedValue(unlisten);
    const cb = vi.fn();
    await onChatEvent(cb);
    expect(listen).toHaveBeenCalledWith("chat-event", expect.any(Function));
  });

  it("passes event.payload to callback", async () => {
    let handler: any;
    (listen as Mock).mockImplementation((_channel: string, fn: any) => {
      handler = fn;
      return Promise.resolve(vi.fn());
    });
    const cb = vi.fn();
    await onChatEvent(cb);
    handler({ payload: { session_id: "s1", event_type: "chunk", content: "hi" } });
    expect(cb).toHaveBeenCalledWith({ session_id: "s1", event_type: "chunk", content: "hi" });
  });

  it("returns unlisten function", async () => {
    const unlisten = vi.fn();
    (listen as Mock).mockResolvedValue(unlisten);
    const result = await onChatEvent(vi.fn());
    expect(result).toBe(unlisten);
  });
});

// ── onHivemindProgress ──

describe("onHivemindProgress", () => {
  it("registers listener on hivemind-progress channel", async () => {
    const unlisten = vi.fn();
    (listen as Mock).mockResolvedValue(unlisten);
    const cb = vi.fn();
    await onHivemindProgress(cb);
    expect(listen).toHaveBeenCalledWith("hivemind-progress", expect.any(Function));
  });

  it("passes event.payload to callback", async () => {
    let handler: any;
    (listen as Mock).mockImplementation((_channel: string, fn: any) => {
      handler = fn;
      return Promise.resolve(vi.fn());
    });
    const cb = vi.fn();
    await onHivemindProgress(cb);
    const payload = { job_id: "j1", event_type: "round_complete", round: 2, model_id: "gpt-4", message: "done" };
    handler({ payload });
    expect(cb).toHaveBeenCalledWith(payload);
  });

  it("returns unlisten function", async () => {
    const unlisten = vi.fn();
    (listen as Mock).mockResolvedValue(unlisten);
    const result = await onHivemindProgress(vi.fn());
    expect(result).toBe(unlisten);
  });
});

// ── onSwarmEvent ──

describe("onSwarmEvent", () => {
  it("registers listener on swarm-event channel", async () => {
    const unlisten = vi.fn();
    (listen as Mock).mockResolvedValue(unlisten);
    const cb = vi.fn();
    await onSwarmEvent(cb);
    expect(listen).toHaveBeenCalledWith("swarm-event", expect.any(Function));
  });

  it("passes event.payload to callback", async () => {
    let handler: any;
    (listen as Mock).mockImplementation((_channel: string, fn: any) => {
      handler = fn;
      return Promise.resolve(vi.fn());
    });
    const cb = vi.fn();
    await onSwarmEvent(cb);
    const payload = { swarm_id: "sw1", event_type: "feature_started", feature_id: "f1", message: "starting" };
    handler({ payload });
    expect(cb).toHaveBeenCalledWith(payload);
  });

  it("returns unlisten function", async () => {
    const unlisten = vi.fn();
    (listen as Mock).mockResolvedValue(unlisten);
    const result = await onSwarmEvent(vi.fn());
    expect(result).toBe(unlisten);
  });
});
