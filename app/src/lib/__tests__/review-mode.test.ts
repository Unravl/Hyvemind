import { describe, it, expect } from "vitest";
import { detectContextSteer } from "../taskRuntime";

describe("detectContextSteer", () => {
  it("returns isContextSteer=true when flow is context phase with valid SID", () => {
    const flow = { phase: "context", contextSid: "sid-abc" } as any;
    expect(detectContextSteer(flow)).toEqual({
      isContextSteer: true,
      contextSid: "sid-abc",
    });
  });

  it("returns isContextSteer=false when flow is null", () => {
    expect(detectContextSteer(null)).toEqual({
      isContextSteer: false,
      contextSid: null,
    });
  });

  it("returns isContextSteer=false when flow.phase is not context", () => {
    const flow = { phase: "merge", contextSid: "sid-abc" } as any;
    expect(detectContextSteer(flow)).toEqual({
      isContextSteer: false,
      contextSid: null,
    });
  });

  it("returns isContextSteer=false when contextSid is null", () => {
    const flow = { phase: "context", contextSid: null } as any;
    expect(detectContextSteer(flow)).toEqual({
      isContextSteer: false,
      contextSid: null,
    });
  });

  it("returns isContextSteer=false when contextSid is undefined", () => {
    const flow = { phase: "context" } as any;
    expect(detectContextSteer(flow)).toEqual({
      isContextSteer: false,
      contextSid: null,
    });
  });
});
