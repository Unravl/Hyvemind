import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

let tauriEnv = false;
vi.mock("../../lib/tauri", () => ({ isTauri: () => tauriEnv }));

vi.mock("../../lib/ipc", () => ({
  refreshModels: vi.fn(),
  getProviders: vi.fn(),
  testProviderModels: vi.fn(),
}));

// audit 6.7 — ModelBrowser now reads the provider list from the
// shared ProvidersProvider. We mock it to mirror whatever the test
// stages into `ipc.getProviders.mockResolvedValue([...])` so the
// existing test bodies don't need wholesale rewrites.
const providersMockState = { providers: [] as any[] };
vi.mock("../../lib/ProvidersProvider", () => ({
  useProviders: () => ({
    providers: providersMockState.providers,
    configured: providersMockState.providers.filter((p: any) => p.configured),
    isLoading: false,
    error: null,
    refresh: vi.fn().mockResolvedValue(undefined),
  }),
  ProvidersProvider: ({ children }: { children: React.ReactNode }) => children,
}));

import { ModelBrowserModal } from "../ModelBrowser";

describe("ModelBrowserModal", () => {
  const onClose = vi.fn();
  const onSelect = vi.fn();

  /** audit 6.7 — stage providers into both the IPC mock (for any
   *  legacy callers still reading directly) and the
   *  ProvidersProvider mock (which is what ModelBrowser actually
   *  reads now). */
  async function stageProvidersList(list: any[]) {
    const ipc = await import("../../lib/ipc");
    (ipc.getProviders as any).mockResolvedValue(list);
    providersMockState.providers = list;
  }

  beforeEach(() => {
    vi.clearAllMocks();
    providersMockState.providers = [];
    try { localStorage.clear(); } catch { /* ignore */ }
  });

  it("renders nothing when open=false", () => {
    const { container } = render(
      <ModelBrowserModal open={false} onClose={onClose} onSelect={onSelect} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("renders the model browser when open=true", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    expect(screen.getByText("Choose model")).toBeInTheDocument();
  });

  it("renders provider filter buttons", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    // "Anthropic" appears capitalized in the provider sidebar
    const anthropicEls = screen.getAllByText("Anthropic");
    expect(anthropicEls.length).toBeGreaterThanOrEqual(1);
  });

  it("renders the search input", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    expect(
      screen.getByPlaceholderText(/Search models/),
    ).toBeInTheDocument();
  });

  it("renders model entries from mock data", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    // MODELS mock data contains these model IDs (filtered to first provider: anthropic)
    expect(screen.getByText("claude-opus-4.1")).toBeInTheDocument();
    expect(screen.getByText("claude-sonnet-4.5")).toBeInTheDocument();
  });

  it("has Add Model button", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    expect(screen.getByText("Add Model")).toBeInTheDocument();
  });

  it("has Cancel button", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    expect(screen.getByText("Cancel")).toBeInTheDocument();
  });

  it("filters models by search query", async () => {
    const user = userEvent.setup();
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    const search = screen.getByPlaceholderText(/Search models/);
    await user.type(search, "opus");
    // Should still show opus, but not unrelated models
    expect(screen.getByText("claude-opus-4.1")).toBeInTheDocument();
  });

  it("shows Thinking selector", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    expect(screen.getByText("Thinking")).toBeInTheDocument();
  });

  it("shows the available count", () => {
    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    expect(screen.getByText(/available/)).toBeInTheDocument();
  });

  /* ── initialModel tests ────────────────────────────────── */

  it("activates the matching provider tab when initialModel is provided", () => {
    const { container } = render(
      <ModelBrowserModal
        open={true}
        onClose={onClose}
        onSelect={onSelect}
        initialModel="anthropic/claude-sonnet-4.5"
      />,
    );
    // The "Anthropic" provider button should have active styling (text-honey-200 class)
    const anthropicBtn = screen.getByText("Anthropic");
    expect(anthropicBtn.className).toContain("text-honey-200");
  });

  it("auto-selects the matching model when initialModel is provided", () => {
    const { container } = render(
      <ModelBrowserModal
        open={true}
        onClose={onClose}
        onSelect={onSelect}
        initialModel="anthropic/claude-sonnet-4.5"
      />,
    );
    // The selected model row should have the checkmark SVG (path with d="M5 12l5 5 9-11")
    const modelBtns = container.querySelectorAll("button");
    // Find the button containing the model name
    const modelBtn = Array.from(modelBtns).find(
      (btn) => btn.textContent?.includes("claude-sonnet-4.5"),
    );
    expect(modelBtn).toBeTruthy();
    // It should contain the checkmark SVG path
    expect(modelBtn?.innerHTML).toContain('d="M5 12l5 5 9-11"');
  });

  it("starts on first provider with no selection when initialModel is not provided", () => {
    const { container } = render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    // First provider (Anthropic) button should have active styling
    const firstBtn = screen.getAllByText("Anthropic")[0];
    expect(firstBtn.className).toContain("text-honey-200");
    // No model button should contain the checkmark SVG
    const modelBtns = container.querySelectorAll("button");
    const anyCheckSelected = Array.from(modelBtns).some(
      (btn) =>
        btn.textContent?.includes("claude") &&
        btn.innerHTML.includes('d="M5 12l5 5 9-11"'),
    );
    expect(anyCheckSelected).toBe(false);
  });

  it("renders rich-column headers when a non-OpenRouter provider returns details", async () => {
    tauriEnv = true;
    const ipc = await import("../../lib/ipc");
    await stageProvidersList([
      {
        name: "groq",
        display_name: "Groq",
        provider_type: "OpenAI Compatible",
        endpoint: "https://api.groq.com/openai/v1",
        configured: true,
        model_count: 0,
        health: true,
      },
    ]);
    (ipc.refreshModels as any).mockResolvedValue([]);
    (ipc.testProviderModels as any).mockResolvedValue({
      ok: true,
      models: ["llama-3.3-70b-versatile"],
      details: [
        {
          id: "llama-3.3-70b-versatile",
          name: null,
          context_length: 131072,
          max_output: 32768,
          input_price: 0.59,
          output_price: 0.79,
        },
      ],
      error: null,
    });

    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );

    // Wait for the rich-column headers to appear (after fetch resolves)
    await waitFor(() => {
      expect(screen.getByText("Context")).toBeInTheDocument();
    });
    expect(screen.getByText("Output")).toBeInTheDocument();
    expect(screen.getByText("Input $/1M")).toBeInTheDocument();
    expect(screen.getByText("Output $/1M")).toBeInTheDocument();
    expect(screen.getByText("llama-3.3-70b-versatile")).toBeInTheDocument();
    tauriEnv = false;
  });

  /* ── Persisted-provider tests ─────────────────────── */

  it("dedupes duplicate ids returned by provider", async () => {
    // Regression: NVIDIA NIM's /v1/models returns duplicate `id` entries
    // for the same model. Backend dedupes these now; the frontend has a
    // defensive dedup as well. This test stages a provider response with
    // duplicates and asserts the modal renders one row per unique id and
    // the header count is accurate.
    tauriEnv = true;
    try {
      const ipc = await import("../../lib/ipc");
      await stageProvidersList([
        {
          name: "nvidia-nim",
          display_name: "NVIDIA NIM",
          provider_type: "OpenAI Compatible",
          endpoint: "https://integrate.api.nvidia.com/v1",
          configured: true,
          model_count: 0,
          health: true,
        },
      ]);
      (ipc.refreshModels as any).mockResolvedValue([]);
      (ipc.testProviderModels as any).mockResolvedValue({
        ok: true,
        models: ["m1", "m1", "m2"],
        details: [
          {
            id: "m1",
            name: null,
            context_length: null,
            max_output: null,
            input_price: null,
            output_price: null,
          },
          {
            id: "m1",
            name: null,
            context_length: null,
            max_output: null,
            input_price: null,
            output_price: null,
          },
          {
            id: "m2",
            name: null,
            context_length: null,
            max_output: null,
            input_price: null,
            output_price: null,
          },
        ],
        error: null,
      });

      render(
        <ModelBrowserModal
          open={true}
          onClose={onClose}
          onSelect={onSelect}
          initialProvider="nvidia-nim"
        />,
      );

      await waitFor(() => {
        expect(screen.getByText("m1")).toBeInTheDocument();
      });

      // Only one row per unique id.
      expect(screen.getAllByText("m1")).toHaveLength(1);
      expect(screen.getAllByText("m2")).toHaveLength(1);

      // Header count reflects deduped length, not raw response length.
      expect(screen.getByText("2 available")).toBeInTheDocument();
      expect(screen.queryByText("3 available")).not.toBeInTheDocument();
    } finally {
      tauriEnv = false;
    }
  });

  it("remembers the last clicked provider tab across modal opens", async () => {
    const user = userEvent.setup();

    // Open the modal with no initial provider / model.
    const { rerender } = render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );

    // Default is the first provider (Anthropic).
    expect(screen.getAllByText("Anthropic")[0].className).toContain("text-honey-200");

    // Click the OpenRouter tab.
    const openrouterBtn = screen.getByText("Openrouter");
    await user.click(openrouterBtn);
    expect(screen.getByText("Openrouter").className).toContain("text-honey-200");

    // Close the modal (open=false unmounts the contents).
    rerender(
      <ModelBrowserModal open={false} onClose={onClose} onSelect={onSelect} />,
    );

    // Reopen the modal — it should restore the OpenRouter tab from localStorage.
    rerender(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );
    await waitFor(() => {
      expect(screen.getByText("Openrouter").className).toContain("text-honey-200");
    });
    // And Anthropic (the previous default) is no longer the active tab.
    expect(screen.getAllByText("Anthropic")[0].className).not.toContain("text-honey-200");
  });

  it("prefers explicit initialProvider over the persisted value", () => {
    // Pre-seed a persisted tab choice.
    try { localStorage.setItem("hyvemind:model-browser-last-provider", "openrouter"); } catch { /* ignore */ }

    render(
      <ModelBrowserModal
        open={true}
        onClose={onClose}
        onSelect={onSelect}
        initialProvider="openai"
      />,
    );
    // initialProvider beats the persisted value.
    expect(screen.getByText("Openai").className).toContain("text-honey-200");
    expect(screen.getByText("Openrouter").className).not.toContain("text-honey-200");
  });

  it("falls back to first configured provider when persisted provider isn't configured", async () => {
    // Persisted value points at a provider that isn't in the configured list.
    try { localStorage.setItem("hyvemind:model-browser-last-provider", "glm"); } catch { /* ignore */ }

    tauriEnv = true;
    const ipc = await import("../../lib/ipc");
    await stageProvidersList([
      {
        name: "anthropic",
        display_name: "Anthropic",
        provider_type: "Anthropic",
        endpoint: "https://api.anthropic.com",
        configured: true,
        model_count: 0,
        health: true,
      },
      {
        name: "openai",
        display_name: "OpenAI",
        provider_type: "OpenAI",
        endpoint: "https://api.openai.com/v1",
        configured: true,
        model_count: 0,
        health: true,
      },
    ]);
    (ipc.refreshModels as any).mockResolvedValue([]);
    (ipc.testProviderModels as any).mockResolvedValue({
      ok: true,
      models: [],
      details: [],
      error: null,
    });

    render(
      <ModelBrowserModal open={true} onClose={onClose} onSelect={onSelect} />,
    );

    // Wait for getProviders() to resolve and the first configured provider tab to activate.
    await waitFor(() => {
      expect(screen.getByText("Anthropic").className).toContain("text-honey-200");
    });
    // "glm" is not in the configured list and should not appear at all.
    expect(screen.queryByText("Glm")).not.toBeInTheDocument();
    tauriEnv = false;
  });

  it("falls through initialProvider to persisted when initialProvider isn't configured", async () => {
    // Regression: New Swarm passes initialProvider="anthropic" but the user only has
    // "openai" and "openrouter" configured. The persisted tab ("openrouter") should win,
    // not silently fall through to configured[0] ("openai").
    try { localStorage.setItem("hyvemind:model-browser-last-provider", "openrouter"); } catch { /* ignore */ }

    tauriEnv = true;
    const ipc = await import("../../lib/ipc");
    await stageProvidersList([
      {
        name: "openai",
        display_name: "OpenAI",
        provider_type: "OpenAI",
        endpoint: "https://api.openai.com/v1",
        configured: true,
        model_count: 0,
        health: true,
      },
      {
        name: "openrouter",
        display_name: "OpenRouter",
        provider_type: "OpenRouter",
        endpoint: "https://openrouter.ai/api/v1",
        configured: true,
        model_count: 0,
        health: true,
      },
    ]);
    (ipc.refreshModels as any).mockResolvedValue([]);
    (ipc.testProviderModels as any).mockResolvedValue({
      ok: true,
      models: [],
      details: [],
      error: null,
    });

    render(
      <ModelBrowserModal
        open={true}
        onClose={onClose}
        onSelect={onSelect}
        initialProvider="anthropic"
      />,
    );

    // The persisted "openrouter" tab should win, since "anthropic" isn't configured.
    await waitFor(() => {
      expect(screen.getByText("Openrouter").className).toContain("text-honey-200");
    });
    // "Openai" (configured[0]) should NOT be the active tab.
    expect(screen.getByText("Openai").className).not.toContain("text-honey-200");
    tauriEnv = false;
  });

  it("uses a bare model id (no slash) with first provider tab", () => {
    const { container } = render(
      <ModelBrowserModal
        open={true}
        onClose={onClose}
        onSelect={onSelect}
        initialModel="claude-sonnet-4.5"
      />,
    );
    // Without a "/", the provider tab should fall back to first provider
    const firstBtn = screen.getAllByText("Anthropic")[0];
    expect(firstBtn.className).toContain("text-honey-200");
    // The bare model ID should still auto-select the right model
    const modelBtns = container.querySelectorAll("button");
    const modelBtn = Array.from(modelBtns).find(
      (btn) => btn.textContent?.includes("claude-sonnet-4.5"),
    );
    expect(modelBtn?.innerHTML).toContain('d="M5 12l5 5 9-11"');
  });
});
