import type { DashboardStats, ModelUsageSummary, ProviderUsageSummary, CostSummary, ActivityEntry } from "../lib/types";

// Mock data shared across screens

export interface Hivemind {
  id: string;
  name: string;
  runs: number;
  rounds: string[][];
  desc: string;
}

export interface Swarm {
  id: string;
  name: string;
  status: "running" | "paused" | "completed" | "failed" | "planning";
  duration: string;
  cost: string;
  features: [number, number];
  milestone: string;
  queen: string;
  worker: string;
  scout: string;
  guard?: string;
  hivemind: string;
  cwd: string;
  /** Optional human-readable error from the backend. Surfaced in the card
   *  when a swarm was reconciled to Failed/Cancelled on restart so the
   *  user can see why the status changed. */
  error?: string;
}

export interface Model {
  id: string;
  provider: string;
  ctx: string;
  out: string;
  tags: string[];
  price: string;
  type: string;
  /** Numeric companion to `ctx` (context-window length, tokens). */
  ctxNum?: number;
  /** Numeric companion to `out` (max output tokens). */
  outNum?: number;
  /** Per-1M input price (USD), when known. */
  inputPrice?: number;
  /** Per-1M output price (USD), when known. */
  outputPrice?: number;
}

export interface ProgressEntry {
  t: string;
  msg: string;
  detail: string;
  kind: string;
}

export interface Feature {
  id: number;
  name: string;
  status: "done" | "active" | "pending";
}

export interface TerminalLine {
  kind: "meta" | "sep" | "tool" | "agent";
  text?: string;
  detail?: string;
  cur?: boolean;
}

export type AutoCommitOverride = "inherit" | "on" | "off";

export interface Project {
  id: string;
  name: string;
  org: string;
  cwd: string;
  branch: string;
  dirty: number;
  lang: string;
  activeSwarms: number;
  chats: number;
  lastTouched: string;
  autoCommitOverride?: AutoCommitOverride; // undefined === "inherit"
}

export const HIVEMINDS: Hivemind[] = [
  {
    id: "enhance",
    name: "enhance",
    runs: 7,
    rounds: [
      ["claude-opus-4.1", "gpt-5-codex", "gemini-2.5-pro"],
      ["deepseek-v3.2", "glm-4.6"],
    ],
    desc: "Multi-perspective code review with synthesis pass.",
  },
  {
    id: "security-audit",
    name: "security-audit",
    runs: 12,
    rounds: [
      ["claude-opus-4.1", "gpt-5-codex"],
      ["kimi-k2", "deepseek-v3.2", "glm-4.6"],
      ["claude-sonnet-4.5"],
    ],
    desc: "Three-round audit with synthesis judge.",
  },
  {
    id: "fast-review",
    name: "fast-review",
    runs: 23,
    rounds: [["claude-haiku-4.5", "gemini-2.5-flash", "deepseek-v3.2"]],
    desc: "Single-round fast triage for small diffs.",
  },
  {
    id: "arch-council",
    name: "arch-council",
    runs: 4,
    rounds: [
      ["claude-opus-4.1", "gpt-5-codex", "gemini-2.5-pro", "kimi-k2"],
      ["claude-opus-4.1"],
    ],
    desc: "Heavy hitter panel for architectural decisions.",
  },
  {
    id: "edge-hunter",
    name: "edge-hunter",
    runs: 9,
    rounds: [
      ["o4-mini-high", "deepseek-v3.2"],
      ["claude-opus-4.1"],
    ],
    desc: "Reasoning-heavy edge case discovery.",
  },
  {
    id: "docs-pass",
    name: "docs-pass",
    runs: 2,
    rounds: [["claude-haiku-4.5", "gpt-4.1-mini"]],
    desc: "Lightweight docs/comment review.",
  },
];

export const SWARMS: Swarm[] = [
  {
    id: "sw-1",
    name: "auth-refactor",
    status: "running",
    duration: "2h 14m",
    cost: "$8.42",
    features: [19, 27],
    milestone: "M3/4",
    queen: "opus-4.1",
    worker: "deepseek-v3.2",
    scout: "sonnet-4.5",
    hivemind: "enhance",
    cwd: "~/code/hyvemind/apps/auth-service",
  },
  {
    id: "sw-2",
    name: "payments-v2",
    status: "planning",
    duration: "00:04:21",
    cost: "$0.18",
    features: [0, 12],
    milestone: "M0/3",
    queen: "opus-4.1",
    worker: "glm-4.6",
    scout: "sonnet-4.5",
    hivemind: "arch-council",
    cwd: "~/code/atlas/services/payments",
  },
  {
    id: "sw-3",
    name: "mobile-onboarding",
    status: "paused",
    duration: "6h 02m",
    cost: "$22.10",
    features: [8, 14],
    milestone: "M2/3",
    queen: "opus-4.1",
    worker: "deepseek-v3.2",
    scout: "haiku-4.5",
    hivemind: "fast-review",
    cwd: "~/code/atlas/mobile",
  },
  {
    id: "sw-4",
    name: "cli-rewrite",
    status: "completed",
    duration: "14h 38m",
    cost: "$54.20",
    features: [42, 42],
    milestone: "M5/5",
    queen: "opus-4.1",
    worker: "sonnet-4.5",
    scout: "haiku-4.5",
    hivemind: "enhance",
    cwd: "~/code/hyvemind/cli",
  },
  {
    id: "sw-5",
    name: "webhooks-retry",
    status: "failed",
    duration: "47m",
    cost: "$3.04",
    features: [4, 9],
    milestone: "M1/2",
    queen: "opus-4.1",
    worker: "deepseek-v3.2",
    scout: "sonnet-4.5",
    hivemind: "security-audit",
    cwd: "~/code/atlas/services/webhooks",
  },
];

export const MODELS: Model[] = [
  { id: "claude-opus-4.1", provider: "anthropic", ctx: "200k", out: "32k", tags: ["reasoning"], price: "$15.00 / $75.00", type: "text\u2192text" },
  { id: "claude-sonnet-4.5", provider: "anthropic", ctx: "200k", out: "64k", tags: ["reasoning"], price: "$3.00 / $15.00", type: "text\u2192text" },
  { id: "claude-haiku-4.5", provider: "anthropic", ctx: "200k", out: "8k", tags: [], price: "$1.00 / $5.00", type: "text\u2192text" },
  { id: "gpt-5-codex", provider: "openai", ctx: "256k", out: "32k", tags: ["reasoning"], price: "$10.00 / $40.00", type: "text\u2192text" },
  { id: "o4-mini-high", provider: "openai", ctx: "128k", out: "16k", tags: ["reasoning"], price: "$3.00 / $12.00", type: "text\u2192text" },
  { id: "gpt-4.1-mini", provider: "openai", ctx: "128k", out: "8k", tags: [], price: "$0.40 / $1.60", type: "text\u2192text" },
  { id: "gemini-2.5-pro", provider: "openrouter", ctx: "1M", out: "64k", tags: ["reasoning"], price: "$1.25 / $5.00", type: "text\u2192text" },
  { id: "gemini-2.5-flash", provider: "openrouter", ctx: "1M", out: "8k", tags: [], price: "$0.10 / $0.30", type: "text\u2192text" },
  { id: "deepseek-v3.2", provider: "deepseek", ctx: "128k", out: "8k", tags: ["reasoning"], price: "$0.27 / $1.10", type: "text\u2192text" },
  { id: "deepseek-coder", provider: "deepseek", ctx: "128k", out: "8k", tags: [], price: "$0.14 / $0.28", type: "text\u2192text" },
  { id: "glm-4.6", provider: "glm", ctx: "128k", out: "4k", tags: [], price: "$0.50 / $1.50", type: "text\u2192text" },
  { id: "kimi-k2", provider: "openrouter", ctx: "256k", out: "8k", tags: ["reasoning"], price: "$0.55 / $2.20", type: "text\u2192text" },
  { id: "qwen3-coder", provider: "openrouter", ctx: "128k", out: "8k", tags: [], price: "$0.30 / $0.90", type: "text\u2192text" },
  { id: "llama-3.3-70b", provider: "ollama", ctx: "128k", out: "4k", tags: [], price: "local", type: "text\u2192text" },
  { id: "qwen3-32b", provider: "ollama", ctx: "128k", out: "4k", tags: [], price: "local", type: "text\u2192text" },
  { id: "mistral-large-2", provider: "mistral", ctx: "128k", out: "8k", tags: [], price: "$2.00 / $6.00", type: "text\u2192text" },
];

export const PROVIDERS = ["anthropic", "openai", "openrouter", "deepseek", "glm", "ollama", "mistral"];

export const PROGRESS_LOG: ProgressEntry[] = [
  { t: "just now", msg: "Worker #2 \u25b8 Read", detail: "src/auth/session.rs", kind: "tool" },
  { t: "12s ago", msg: "Worker #2 \u25b8 Edit", detail: "src/auth/session.rs +18 \u22124", kind: "tool" },
  { t: "38s ago", msg: "Hivemind R2 complete", detail: "0 issues \u00b7 synthesis pass clean", kind: "review-ok" },
  { t: "1m ago", msg: "Hivemind R1 complete", detail: "2 issues \u00b7 forwarded to R2", kind: "review" },
  { t: "2m ago", msg: "Scout finished spec", detail: "feature: rotate-refresh-tokens", kind: "scout" },
  { t: "4m ago", msg: "Scout started", detail: "feature: rotate-refresh-tokens", kind: "scout" },
  { t: "5m ago", msg: "Worker #1 \u25b8 feature complete", detail: "jwt-clock-skew-tolerance", kind: "done" },
  { t: "8m ago", msg: "Hivemind R1 complete", detail: "1 issue \u00b7 auto-resolved", kind: "review-ok" },
  { t: "11m ago", msg: "Worker #1 started", detail: "jwt-clock-skew-tolerance", kind: "worker" },
  { t: "13m ago", msg: "Queen approved plan", detail: "M3 \u00b7 8 features queued", kind: "queen" },
];

export const FEATURES: Feature[] = [
  { id: 1, name: "jwt-clock-skew-tolerance", status: "done" },
  { id: 2, name: "argon2id-rehash-on-login", status: "done" },
  { id: 3, name: "session-store-redis-migrate", status: "done" },
  { id: 4, name: "csrf-double-submit-cookie", status: "done" },
  { id: 5, name: "oauth-state-binding", status: "done" },
  { id: 6, name: "pkce-fallback-public-clients", status: "done" },
  { id: 7, name: "refresh-token-rotation", status: "done" },
  { id: 8, name: "device-fingerprint-binding", status: "done" },
  { id: 9, name: "rate-limit-token-bucket", status: "done" },
  { id: 10, name: "login-throttle-fail2ban", status: "done" },
  { id: 11, name: "audit-log-append-only", status: "done" },
  { id: 12, name: "session-revocation-broadcast", status: "done" },
  { id: 13, name: "webauthn-registration", status: "done" },
  { id: 14, name: "webauthn-assertion-verify", status: "done" },
  { id: 15, name: "magic-link-issuer", status: "done" },
  { id: 16, name: "magic-link-redeem", status: "done" },
  { id: 17, name: "totp-secret-rotation", status: "done" },
  { id: 18, name: "recovery-code-hashing", status: "done" },
  { id: 19, name: "rotate-refresh-tokens", status: "active" },
  { id: 20, name: "session-fingerprint-cache", status: "pending" },
  { id: 21, name: "oauth-pkce-storage", status: "pending" },
  { id: 22, name: "admin-impersonation-trail", status: "pending" },
  { id: 23, name: "sso-saml-assertion-replay", status: "pending" },
  { id: 24, name: "tenant-key-derivation", status: "pending" },
  { id: 25, name: "lockout-policy-windowed", status: "pending" },
  { id: 26, name: "audit-export-redaction", status: "pending" },
  { id: 27, name: "health-probe-auth-bypass", status: "pending" },
];

export const TERMINAL_STREAM: TerminalLine[] = [
  { kind: "meta", text: "\u23f5 Worker #2 \u2014 feature: rotate-refresh-tokens" },
  { kind: "meta", text: "   model: deepseek-v3.2 \u00b7 thinking: high \u00b7 run: 0:38" },
  { kind: "sep" },
  { kind: "tool", text: "Read", detail: "src/auth/session.rs (412 lines)" },
  { kind: "tool", text: "Read", detail: "src/auth/refresh.rs (88 lines)" },
  { kind: "tool", text: "Grep", detail: 'pattern="rotate_refresh" in src/' },
  { kind: "agent", text: "I see refresh tokens are issued in `mint_refresh()` but never rotated on use. I'll add rotation, family tracking, and theft detection." },
  { kind: "tool", text: "Edit", detail: "src/auth/refresh.rs +42 \u22126" },
  { kind: "agent", text: "Adding family-id linkage so a stolen rotation invalidates the whole chain. Writing tests next." },
  { kind: "tool", text: "Write", detail: "src/auth/refresh_test.rs (new, 124 lines)" },
  { kind: "tool", text: "Bash", detail: "cargo test refresh_ -- --nocapture" },
  { kind: "agent", text: "6 of 7 tests pass. The reuse-after-rotation case is failing because the family invalidation lookup hits the cold cache. Fixing." },
  { kind: "tool", text: "Edit", detail: "src/auth/session.rs +18 \u22124", cur: true },
];

export const MOCK_DASHBOARD_STATS: DashboardStats = {
  active_tasks: 3,
  running_swarms: 2,
  paused_swarms: 1,
  total_reviews: 46,
  cost_today: 8.42,
};

const modelUsageData: ModelUsageSummary[] = [
  { model_id: "claude-sonnet-4.5", provider: "anthropic", input_tokens: 6_120_000, output_tokens: 1_840_000, total_tokens: 7_960_000, calls: 142, cost: 46.20 },
  { model_id: "deepseek-v3.2", provider: "deepseek", input_tokens: 4_800_000, output_tokens: 980_000, total_tokens: 5_780_000, calls: 98, cost: 2.37 },
  { model_id: "claude-opus-4.1", provider: "anthropic", input_tokens: 3_200_000, output_tokens: 640_000, total_tokens: 3_840_000, calls: 34, cost: 96.00 },
  { model_id: "gpt-5-codex", provider: "openai", input_tokens: 2_900_000, output_tokens: 720_000, total_tokens: 3_620_000, calls: 56, cost: 57.80 },
  { model_id: "gemini-2.5-pro", provider: "openrouter", input_tokens: 2_100_000, output_tokens: 510_000, total_tokens: 2_610_000, calls: 41, cost: 5.18 },
  { model_id: "claude-haiku-4.5", provider: "anthropic", input_tokens: 1_800_000, output_tokens: 420_000, total_tokens: 2_220_000, calls: 87, cost: 3.90 },
  { model_id: "kimi-k2", provider: "openrouter", input_tokens: 1_400_000, output_tokens: 350_000, total_tokens: 1_750_000, calls: 29, cost: 1.54 },
  { model_id: "glm-4.6", provider: "glm", input_tokens: 980_000, output_tokens: 240_000, total_tokens: 1_220_000, calls: 22, cost: 0.85 },
  { model_id: "o4-mini-high", provider: "openai", input_tokens: 720_000, output_tokens: 180_000, total_tokens: 900_000, calls: 18, cost: 4.32 },
  { model_id: "deepseek-coder", provider: "deepseek", input_tokens: 540_000, output_tokens: 130_000, total_tokens: 670_000, calls: 15, cost: 0.11 },
];

export const MOCK_MODEL_USAGE: Record<string, ModelUsageSummary[]> = {
  all: modelUsageData,
  month: modelUsageData.map(m => ({ ...m, input_tokens: Math.round(m.input_tokens * 0.4), output_tokens: Math.round(m.output_tokens * 0.4), total_tokens: Math.round(m.total_tokens * 0.4), calls: Math.round(m.calls * 0.4), cost: +(m.cost * 0.4).toFixed(2) })),
  week: modelUsageData.map(m => ({ ...m, input_tokens: Math.round(m.input_tokens * 0.15), output_tokens: Math.round(m.output_tokens * 0.15), total_tokens: Math.round(m.total_tokens * 0.15), calls: Math.round(m.calls * 0.15), cost: +(m.cost * 0.15).toFixed(2) })),
  day: modelUsageData.map(m => ({ ...m, input_tokens: Math.round(m.input_tokens * 0.03), output_tokens: Math.round(m.output_tokens * 0.03), total_tokens: Math.round(m.total_tokens * 0.03), calls: Math.round(m.calls * 0.03), cost: +(m.cost * 0.03).toFixed(2) })),
};

export const MOCK_PROVIDER_USAGE: Record<string, ProviderUsageSummary[]> = {
  all: [
    { provider: "anthropic", input_tokens: 11_120_000, output_tokens: 2_900_000, total_tokens: 14_020_000, calls: 263, cost: 146.10 },
    { provider: "openai", input_tokens: 3_620_000, output_tokens: 900_000, total_tokens: 4_520_000, calls: 74, cost: 62.12 },
    { provider: "deepseek", input_tokens: 5_340_000, output_tokens: 1_110_000, total_tokens: 6_450_000, calls: 113, cost: 2.48 },
    { provider: "openrouter", input_tokens: 3_500_000, output_tokens: 860_000, total_tokens: 4_360_000, calls: 70, cost: 6.72 },
    { provider: "glm", input_tokens: 980_000, output_tokens: 240_000, total_tokens: 1_220_000, calls: 22, cost: 0.85 },
    { provider: "ollama", input_tokens: 320_000, output_tokens: 80_000, total_tokens: 400_000, calls: 12, cost: 0 },
  ],
  month: [
    { provider: "anthropic", input_tokens: 4_448_000, output_tokens: 1_160_000, total_tokens: 5_608_000, calls: 105, cost: 58.44 },
    { provider: "openai", input_tokens: 1_448_000, output_tokens: 360_000, total_tokens: 1_808_000, calls: 30, cost: 24.85 },
    { provider: "deepseek", input_tokens: 2_136_000, output_tokens: 444_000, total_tokens: 2_580_000, calls: 45, cost: 0.99 },
    { provider: "openrouter", input_tokens: 1_400_000, output_tokens: 344_000, total_tokens: 1_744_000, calls: 28, cost: 2.69 },
    { provider: "glm", input_tokens: 392_000, output_tokens: 96_000, total_tokens: 488_000, calls: 9, cost: 0.34 },
    { provider: "ollama", input_tokens: 128_000, output_tokens: 32_000, total_tokens: 160_000, calls: 5, cost: 0 },
  ],
  week: [
    { provider: "anthropic", input_tokens: 1_668_000, output_tokens: 435_000, total_tokens: 2_103_000, calls: 39, cost: 21.92 },
    { provider: "openai", input_tokens: 543_000, output_tokens: 135_000, total_tokens: 678_000, calls: 11, cost: 9.32 },
    { provider: "deepseek", input_tokens: 801_000, output_tokens: 167_000, total_tokens: 968_000, calls: 17, cost: 0.37 },
    { provider: "openrouter", input_tokens: 525_000, output_tokens: 129_000, total_tokens: 654_000, calls: 11, cost: 1.01 },
    { provider: "glm", input_tokens: 147_000, output_tokens: 36_000, total_tokens: 183_000, calls: 3, cost: 0.13 },
    { provider: "ollama", input_tokens: 48_000, output_tokens: 12_000, total_tokens: 60_000, calls: 2, cost: 0 },
  ],
  day: [
    { provider: "anthropic", input_tokens: 334_000, output_tokens: 87_000, total_tokens: 421_000, calls: 8, cost: 4.38 },
    { provider: "openai", input_tokens: 109_000, output_tokens: 27_000, total_tokens: 136_000, calls: 2, cost: 1.86 },
    { provider: "deepseek", input_tokens: 160_000, output_tokens: 33_000, total_tokens: 193_000, calls: 3, cost: 0.07 },
    { provider: "openrouter", input_tokens: 105_000, output_tokens: 26_000, total_tokens: 131_000, calls: 2, cost: 0.20 },
    { provider: "glm", input_tokens: 0, output_tokens: 0, total_tokens: 0, calls: 0, cost: 0 },
    { provider: "ollama", input_tokens: 0, output_tokens: 0, total_tokens: 0, calls: 0, cost: 0 },
  ],
};

export const MOCK_COST_SUMMARY: CostSummary = {
  today: 8.42,
  week: 34.18,
  month: 87.31,
  all_time: 218.27,
};

export const MOCK_RECENT_ACTIVITY: ActivityEntry[] = [
  { id: "a1", timestamp: "2026-05-07T14:32:00Z", source: "chat", source_id: "sess-1", model_id: "claude-sonnet-4.5", provider: "anthropic", input_tokens: 12400, output_tokens: 3200, cost: 0.085 },
  { id: "a2", timestamp: "2026-05-07T14:28:00Z", source: "hivemind", source_id: "job-1", model_id: "deepseek-v3.2", provider: "deepseek", input_tokens: 8900, output_tokens: 2100, cost: 0.005 },
  { id: "a3", timestamp: "2026-05-07T14:25:00Z", source: "hivemind", source_id: "job-1", model_id: "claude-opus-4.1", provider: "anthropic", input_tokens: 9200, output_tokens: 4100, cost: 0.445 },
  { id: "a4", timestamp: "2026-05-07T14:20:00Z", source: "swarm", source_id: "sw-1", model_id: "deepseek-v3.2", provider: "deepseek", input_tokens: 15000, output_tokens: 4800, cost: 0.009 },
  { id: "a5", timestamp: "2026-05-07T14:15:00Z", source: "chat", source_id: "sess-2", model_id: "claude-sonnet-4.5", provider: "anthropic", input_tokens: 6800, output_tokens: 1900, cost: 0.049 },
  { id: "a6", timestamp: "2026-05-07T14:10:00Z", source: "hivemind", source_id: "job-2", model_id: "gemini-2.5-pro", provider: "openrouter", input_tokens: 11200, output_tokens: 3400, cost: 0.031 },
  { id: "a7", timestamp: "2026-05-07T14:05:00Z", source: "swarm", source_id: "sw-1", model_id: "claude-haiku-4.5", provider: "anthropic", input_tokens: 4200, output_tokens: 980, cost: 0.008 },
  { id: "a8", timestamp: "2026-05-07T13:58:00Z", source: "chat", source_id: "sess-1", model_id: "claude-sonnet-4.5", provider: "anthropic", input_tokens: 18200, output_tokens: 5100, cost: 0.131 },
  { id: "a9", timestamp: "2026-05-07T13:50:00Z", source: "hivemind", source_id: "job-3", model_id: "gpt-5-codex", provider: "openai", input_tokens: 7800, output_tokens: 2600, cost: 0.182 },
  { id: "a10", timestamp: "2026-05-07T13:42:00Z", source: "swarm", source_id: "sw-2", model_id: "glm-4.6", provider: "glm", input_tokens: 5400, output_tokens: 1200, cost: 0.005 },
];

export const PROJECTS: Project[] = [
  {
    id: "auth-service",
    name: "auth-service",
    org: "hyvemind",
    cwd: "~/code/hyvemind/apps/auth-service",
    branch: "main",
    dirty: 3,
    lang: "rust",
    activeSwarms: 1,
    chats: 4,
    lastTouched: "2m ago",
  },
  {
    id: "payments",
    name: "payments",
    org: "atlas",
    cwd: "~/code/atlas/services/payments",
    branch: "feat/v2-rewrite",
    dirty: 0,
    lang: "go",
    activeSwarms: 1,
    chats: 2,
    lastTouched: "5m ago",
  },
  {
    id: "mobile",
    name: "mobile",
    org: "atlas",
    cwd: "~/code/atlas/mobile",
    branch: "main",
    dirty: 12,
    lang: "ts",
    activeSwarms: 1,
    chats: 1,
    lastTouched: "38m ago",
  },
  {
    id: "cli",
    name: "cli",
    org: "hyvemind",
    cwd: "~/code/hyvemind/cli",
    branch: "main",
    dirty: 0,
    lang: "rust",
    activeSwarms: 0,
    chats: 0,
    lastTouched: "yesterday",
  },
  {
    id: "webhooks",
    name: "webhooks",
    org: "atlas",
    cwd: "~/code/atlas/services/webhooks",
    branch: "fix/retry-backoff",
    dirty: 1,
    lang: "go",
    activeSwarms: 0,
    chats: 1,
    lastTouched: "3h ago",
  },
];
