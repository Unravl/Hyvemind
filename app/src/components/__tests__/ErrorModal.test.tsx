import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, act } from "@testing-library/react";

// Mock @sentry/react so captureException is observable but inert.
vi.mock("@sentry/react", () => ({
  captureException: vi.fn(),
}));

import * as Sentry from "@sentry/react";
import { ErrorModalProvider } from "../ErrorModal";

const captureException = Sentry.captureException as unknown as ReturnType<typeof vi.fn>;

function renderProvider() {
  return render(
    <ErrorModalProvider>
      <div>app-children</div>
    </ErrorModalProvider>
  );
}

beforeEach(() => {
  captureException.mockClear();
});

afterEach(() => {
  // Ensure no leftover error state across tests
  document.body.innerHTML = "";
});

function dispatchWindowError(message: string, error?: Error, filename = "", lineno = 0) {
  const event = new Event("error") as ErrorEvent;
  // jsdom's ErrorEvent constructor is partial; assign fields directly.
  Object.defineProperty(event, "message", { value: message, configurable: true });
  Object.defineProperty(event, "error", { value: error ?? null, configurable: true });
  Object.defineProperty(event, "filename", { value: filename, configurable: true });
  Object.defineProperty(event, "lineno", { value: lineno, configurable: true });
  act(() => {
    window.dispatchEvent(event);
  });
}

function dispatchRejection(reason: unknown) {
  const event = new Event("unhandledrejection") as PromiseRejectionEvent;
  Object.defineProperty(event, "reason", { value: reason, configurable: true });
  act(() => {
    window.dispatchEvent(event);
  });
}

function modalShown(): boolean {
  return !!document.querySelector("[data-modal]");
}

describe("ErrorModal filtering", () => {
  it("shows modal for window.error with a Hyvemind source stack", () => {
    renderProvider();
    const err = new Error("Real bug");
    err.stack =
      "Error: Real bug\n    at TasksScreen (http://localhost:5173/src/screens/Tasks.tsx:42:13)";
    dispatchWindowError("Real bug", err);
    expect(modalShown()).toBe(true);
    expect(captureException).toHaveBeenCalledTimes(1);
    const tags = captureException.mock.calls[0][1]?.tags;
    expect(tags).toMatchObject({ source: "window.error", suppressed: "false" });
  });

  it("suppresses the reported el.dispatchEvent third-party noise", () => {
    renderProvider();
    const err = new Error("null is not an object (evaluating 'el.dispatchEvent')");
    err.stack =
      "@http://localhost:5173/:27:11\nglobal code@http://localhost:5173/:36:9";
    dispatchWindowError(
      "null is not an object (evaluating 'el.dispatchEvent')",
      err
    );
    expect(modalShown()).toBe(false);
    expect(captureException).toHaveBeenCalledTimes(1);
    const tags = captureException.mock.calls[0][1]?.tags;
    expect(tags).toMatchObject({ source: "window.error", suppressed: "filtered" });
  });

  it("shows modal for unhandledrejection with a Hyvemind source stack", () => {
    renderProvider();
    const err = new Error("Async failure");
    err.stack =
      "Error: Async failure\n    at fetchSwarms (http://localhost:5173/src/api/swarms.ts:88:21)";
    dispatchRejection(err);
    expect(modalShown()).toBe(true);
    expect(captureException).toHaveBeenCalledTimes(1);
    const tags = captureException.mock.calls[0][1]?.tags;
    expect(tags).toMatchObject({ source: "unhandledrejection", suppressed: "false" });
  });

  it("suppresses unhandledrejection with only bare-document frames", () => {
    renderProvider();
    const err = new Error("Some weird message");
    err.stack =
      "@http://localhost:5173/:27:11\nglobal code@http://localhost:5173/:36:9";
    dispatchRejection(err);
    expect(modalShown()).toBe(false);
    expect(captureException).toHaveBeenCalledTimes(1);
    const tags = captureException.mock.calls[0][1]?.tags;
    expect(tags).toMatchObject({
      source: "unhandledrejection",
      suppressed: "filtered",
    });
  });

  it("still suppresses console.error('Warning: ...') (existing behavior)", () => {
    renderProvider();
    act(() => {
      // eslint-disable-next-line no-console
      console.error("Warning: something React-y happened");
    });
    expect(modalShown()).toBe(false);
    expect(captureException).toHaveBeenCalledTimes(1);
    const tags = captureException.mock.calls[0][1]?.tags;
    expect(tags).toMatchObject({
      source: "console.error",
      suppressed: "filtered",
    });
  });
});
