import { describe, it, expect } from "vitest";
import { pathForCompare, isUnderApprovedRoot } from "../ProjectPicker";

describe("pathForCompare", () => {
  it("strips trailing separators (both kinds)", () => {
    expect(pathForCompare("/foo/bar/")).toBe("/foo/bar");
    expect(pathForCompare("/foo/bar//")).toBe("/foo/bar");
    expect(pathForCompare("C:\\foo\\bar\\")).toBe("c:/foo/bar");
    expect(pathForCompare("C:\\foo\\bar\\\\")).toBe("c:/foo/bar");
  });

  it("normalizes backslashes to forward slashes", () => {
    expect(pathForCompare("C:\\Users\\Elysia\\Desktop\\Test")).toBe(
      "c:/users/elysia/desktop/test",
    );
  });

  it("lowercases Windows drive-letter paths only", () => {
    // Drive letter path: case-insensitive (Windows is).
    expect(pathForCompare("C:\\Users\\Foo")).toBe(pathForCompare("c:/users/foo"));
    // POSIX path: case-sensitive (Linux can have both).
    expect(pathForCompare("/Foo/Bar")).not.toBe(pathForCompare("/foo/bar"));
    expect(pathForCompare("/Foo/Bar")).toBe("/Foo/Bar");
  });

  it("leaves a non-drive-letter mixed-case path unchanged", () => {
    expect(pathForCompare("/Users/Elysia/Code")).toBe("/Users/Elysia/Code");
  });
});

describe("isUnderApprovedRoot", () => {
  it("accepts an exact Windows path that differs only in slash kind", () => {
    // Persisted allowlist (from `dunce::canonicalize`) uses backslashes;
    // user's picked path uses forward slashes — the bug that caused the
    // approval modal to loop on Windows.
    expect(
      isUnderApprovedRoot(
        "C:/Users/Elysia/Desktop/Test",
        "C:\\Users\\Elysia\\Desktop\\Test",
      ),
    ).toBe(true);
  });

  it("accepts a descendant directory on Windows", () => {
    expect(
      isUnderApprovedRoot(
        "C:\\Users\\Elysia\\Desktop\\Test\\sub",
        "C:\\Users\\Elysia\\Desktop\\Test",
      ),
    ).toBe(true);
  });

  it("rejects a sibling whose name shares a prefix with the approved root", () => {
    // Regression: a substring `startsWith` would wrongly accept this.
    expect(
      isUnderApprovedRoot(
        "C:\\Users\\Elysia\\Desktop\\Test-other",
        "C:\\Users\\Elysia\\Desktop\\Test",
      ),
    ).toBe(false);
  });

  it("treats Windows paths as case-insensitive", () => {
    expect(
      isUnderApprovedRoot("c:\\users\\elysia\\test", "C:\\Users\\Elysia\\Test"),
    ).toBe(true);
  });

  it("treats POSIX paths as case-sensitive", () => {
    expect(isUnderApprovedRoot("/home/elysia/Test", "/home/elysia/test")).toBe(false);
  });

  it("accepts POSIX exact match and descendant", () => {
    expect(isUnderApprovedRoot("/tmp", "/tmp")).toBe(true);
    expect(isUnderApprovedRoot("/tmp/sub", "/tmp")).toBe(true);
    expect(isUnderApprovedRoot("/tmp-other", "/tmp")).toBe(false);
  });
});
