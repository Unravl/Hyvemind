import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { Markdown } from "../Markdown";

describe("Markdown XSS / unsafe protocol handling", () => {
  it("strips javascript: hrefs from links (react-markdown defaultUrlTransform)", () => {
    const { container } = render(<Markdown text="[xss](javascript:alert(1))" />);
    const anchor = container.querySelector("a");
    // react-markdown v10's defaultUrlTransform replaces unsafe URLs with an empty string,
    // so the rendered anchor should either be missing or have no/empty href starting with javascript:.
    if (anchor) {
      const href = anchor.getAttribute("href") ?? "";
      expect(href.toLowerCase().startsWith("javascript:")).toBe(false);
    }
  });

  it("strips javascript: src from images", () => {
    const { container } = render(<Markdown text="![x](javascript:alert(1))" />);
    const img = container.querySelector("img");
    if (img) {
      const src = img.getAttribute("src") ?? "";
      expect(src.toLowerCase().startsWith("javascript:")).toBe(false);
    }
  });

  it("preserves safe https links", () => {
    const { container } = render(<Markdown text="[hi](https://example.com)" />);
    const anchor = container.querySelector("a");
    expect(anchor).not.toBeNull();
    expect(anchor?.getAttribute("href")).toBe("https://example.com");
  });
});
