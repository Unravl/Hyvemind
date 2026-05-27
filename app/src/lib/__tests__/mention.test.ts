import { describe, it, expect } from "vitest";
import { detectMention } from "../mention";

describe("detectMention", () => {
  it("returns null when cursor is at position 0", () => {
    expect(detectMention("@foo", 0)).toBeNull();
  });

  it("detects `@foo` at start of input", () => {
    const r = detectMention("@foo", 4);
    expect(r).toEqual({ startIdx: 0, tokenEnd: 4 });
  });

  it("detects bare `@` (empty query)", () => {
    const r = detectMention("@", 1);
    expect(r).toEqual({ startIdx: 0, tokenEnd: 1 });
  });

  it("detects `@world` after whitespace", () => {
    const r = detectMention("hello @world", 12);
    expect(r).toEqual({ startIdx: 6, tokenEnd: 12 });
  });

  it("rejects email-like pattern `user@host` (no whitespace before @)", () => {
    expect(detectMention("user@host", 9)).toBeNull();
  });

  it("returns span for mid-token cursor (ArrowLeft)", () => {
    // value = "@foo bar", cursor at index 2 ("@f|oo bar")
    const r = detectMention("@foo bar", 2);
    expect(r).toEqual({ startIdx: 0, tokenEnd: 4 });
    // Caller derives query as value.slice(startIdx+1, cursor) = "f"
  });

  it("multiple @ — picks most recent valid one", () => {
    // "@first @second", cursor at end
    const r = detectMention("@first @second", 14);
    expect(r).toEqual({ startIdx: 7, tokenEnd: 14 });
  });

  it("whitespace after @ closes the mention (cursor past space)", () => {
    // "@ " — cursor at index 2 (after the space)
    expect(detectMention("@ ", 2)).toBeNull();
  });

  it("`@@foo` — inner @ not preceded by whitespace, no mention", () => {
    expect(detectMention("@@foo", 5)).toBeNull();
  });

  it("boundary: cursor at tokenEnd returns span", () => {
    const r = detectMention("@foo bar", 4);
    expect(r).toEqual({ startIdx: 0, tokenEnd: 4 });
  });

  it("boundary: cursor past the space returns null", () => {
    expect(detectMention("@foo bar", 5)).toBeNull();
  });

  it("handles tab as whitespace boundary", () => {
    const r = detectMention("a\t@x", 4);
    expect(r).toEqual({ startIdx: 2, tokenEnd: 4 });
  });

  it("handles newline as whitespace boundary", () => {
    const r = detectMention("line1\n@foo", 10);
    expect(r).toEqual({ startIdx: 6, tokenEnd: 10 });
  });
});
