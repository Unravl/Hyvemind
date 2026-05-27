import { describe, it, expect, beforeEach } from "vitest";
import { workspaceColor, workspaceLabel, workspaceKey, WORKSPACE_COLORS } from "../categories";

describe("workspaceColor", () => {
  it("returns a valid workspace style for any name", () => {
    const style = workspaceColor("payments");
    expect(WORKSPACE_COLORS).toContainEqual(style);
    expect(style.dot).toBeTruthy();
    expect(style.pill).toBeTruthy();
    expect(style.border).toBeTruthy();
  });

  it("is deterministic — same name always returns same color", () => {
    const a = workspaceColor("auth-service");
    const b = workspaceColor("auth-service");
    expect(a).toBe(b);
  });

  it("is case-insensitive deterministic", () => {
    const lower = workspaceColor("backend");
    const upper = workspaceColor("BACKEND");
    expect(lower).toBe(upper);
  });

  it("handles empty string without crashing", () => {
    const style = workspaceColor("");
    expect(WORKSPACE_COLORS).toContainEqual(style);
  });
});

describe("workspaceLabel", () => {
  it("returns 'No workspace' for empty or undefined", () => {
    expect(workspaceLabel("")).toBe("No workspace");
    expect(workspaceLabel(undefined)).toBe("No workspace");
  });

  it("returns the raw project name for non-path strings", () => {
    expect(workspaceLabel("payments")).toBe("payments");
    expect(workspaceLabel("auth-service")).toBe("auth-service");
  });

  it("extracts last segment from a path", () => {
    expect(workspaceLabel("/Users/moh/Desktop/Work/Hyvemind")).toBe("Hyvemind");
    expect(workspaceLabel("/home/user/projects/foo")).toBe("foo");
  });

  it("handles Windows-style paths", () => {
    expect(workspaceLabel("C:\\Users\\dev\\project")).toBe("project");
  });

  it("handles trailing slashes", () => {
    // filter(Boolean) removes empty segments from trailing slash
    expect(workspaceLabel("/Users/moh/project/")).toBe("project");
  });
});

describe("workspaceKey", () => {
  it("returns project field when present", () => {
    expect(workspaceKey({ project: "payments" })).toBe("payments");
  });

  it("falls back to projectPath when project is empty", () => {
    expect(workspaceKey({ project: "", projectPath: "/path/to/proj" })).toBe("/path/to/proj");
  });

  it("returns empty string when neither is set", () => {
    expect(workspaceKey({})).toBe("");
    expect(workspaceKey({ project: "" })).toBe("");
  });

  it("prefers project over projectPath", () => {
    expect(workspaceKey({ project: "foo", projectPath: "/bar" })).toBe("foo");
  });
});
