import { afterEach, describe, expect, it } from "vitest";
import { isMac } from "../platform";

const originalUserAgent = Object.getOwnPropertyDescriptor(
  Navigator.prototype,
  "userAgent",
);
const originalPlatform = Object.getOwnPropertyDescriptor(
  Navigator.prototype,
  "platform",
);
const originalUserAgentData = (navigator as any).userAgentData;

function setNavigator({
  userAgent,
  platform,
  userAgentData,
}: {
  userAgent?: string;
  platform?: string;
  userAgentData?: { platform?: string } | undefined;
}) {
  if (userAgent !== undefined) {
    Object.defineProperty(navigator, "userAgent", {
      get: () => userAgent,
      configurable: true,
    });
  }
  if (platform !== undefined) {
    Object.defineProperty(navigator, "platform", {
      get: () => platform,
      configurable: true,
    });
  }
  Object.defineProperty(navigator, "userAgentData", {
    value: userAgentData,
    configurable: true,
    writable: true,
  });
}

function restoreNavigator() {
  if (originalUserAgent) {
    Object.defineProperty(Navigator.prototype, "userAgent", originalUserAgent);
  }
  if (originalPlatform) {
    Object.defineProperty(Navigator.prototype, "platform", originalPlatform);
  }
  Object.defineProperty(navigator, "userAgentData", {
    value: originalUserAgentData,
    configurable: true,
    writable: true,
  });
}

describe("isMac", () => {
  afterEach(() => {
    restoreNavigator();
  });

  it("returns true for a macOS user agent / platform", () => {
    setNavigator({
      userAgent:
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Safari/605.1.15",
      platform: "MacIntel",
      userAgentData: undefined,
    });

    expect(isMac()).toBe(true);
  });

  it("returns true when userAgentData reports macOS", () => {
    setNavigator({
      userAgent: "irrelevant",
      platform: "irrelevant",
      userAgentData: { platform: "macOS" },
    });

    expect(isMac()).toBe(true);
  });

  it("returns false for a Windows user agent / platform", () => {
    setNavigator({
      userAgent:
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
      platform: "Win32",
      userAgentData: { platform: "Windows" },
    });

    expect(isMac()).toBe(false);
  });

  it("returns false for a Linux user agent / platform", () => {
    setNavigator({
      userAgent:
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
      platform: "Linux x86_64",
      userAgentData: { platform: "Linux" },
    });

    expect(isMac()).toBe(false);
  });
});
