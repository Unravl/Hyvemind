import "@testing-library/jest-dom";

// Polyfill Element.scrollIntoView for jsdom (not implemented in jsdom)
Element.prototype.scrollIntoView = function () {};

// Node 25 ships an experimental native `localStorage` that shadows jsdom's
// Storage implementation — when run without `--localstorage-file`, the native
// is a no-op plain object that lacks `getItem`/`setItem`. Install a small
// in-memory shim so component code (and tests) that relies on Web Storage
// behaves correctly in the test environment.
if (typeof localStorage === "undefined" || typeof (localStorage as any).getItem !== "function") {
  class MemoryStorage {
    private store = new Map<string, string>();
    get length() { return this.store.size; }
    clear() { this.store.clear(); }
    getItem(key: string) { return this.store.has(key) ? this.store.get(key)! : null; }
    setItem(key: string, value: string) { this.store.set(key, String(value)); }
    removeItem(key: string) { this.store.delete(key); }
    key(i: number) { return Array.from(this.store.keys())[i] ?? null; }
  }
  const ls = new MemoryStorage();
  // Override both globalThis and window.localStorage so direct/indirect access agrees.
  Object.defineProperty(globalThis, "localStorage", { value: ls, writable: true, configurable: true });
  if (typeof window !== "undefined") {
    Object.defineProperty(window, "localStorage", { value: ls, writable: true, configurable: true });
  }
}
if (typeof sessionStorage === "undefined" || typeof (sessionStorage as any).getItem !== "function") {
  class MemoryStorage {
    private store = new Map<string, string>();
    get length() { return this.store.size; }
    clear() { this.store.clear(); }
    getItem(key: string) { return this.store.has(key) ? this.store.get(key)! : null; }
    setItem(key: string, value: string) { this.store.set(key, String(value)); }
    removeItem(key: string) { this.store.delete(key); }
    key(i: number) { return Array.from(this.store.keys())[i] ?? null; }
  }
  const ss = new MemoryStorage();
  Object.defineProperty(globalThis, "sessionStorage", { value: ss, writable: true, configurable: true });
  if (typeof window !== "undefined") {
    Object.defineProperty(window, "sessionStorage", { value: ss, writable: true, configurable: true });
  }
}

// Polyfill ResizeObserver for jsdom (used by SwarmControl and other screens)
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class ResizeObserver {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as any;
}
