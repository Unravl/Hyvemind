import React, { useState, useEffect, useCallback, useRef } from "react";
import { GoFn, usePiGate } from "../App";
import { I } from "../components/icons";
import { Btn, Pill, Input, Select } from "../components/atoms";
import { useNurseStatus } from "../hooks/useNurseStatus";
import { isTauri } from "../lib/tauri";
import { confirmDialog } from "../lib/confirm";
import * as ipc from "../lib/ipc";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { useTaskRuntime, MERGE_TIMEOUT_MIN_KEY, MERGE_TIMEOUT_DEFAULT_MIN, CHAT_CHECK_IN_SECS_KEY, CHAT_CHECK_IN_SECS_DEFAULT, CHAT_CHECK_IN_SECS_MIN, CHAT_CHECK_IN_SECS_MAX, EXTENSION_POLL_INTERVAL_KEY, EXTENSION_POLL_INTERVAL_DEFAULT, EXTENSION_POLL_INTERVAL_MIN, EXTENSION_POLL_INTERVAL_MAX } from "../lib/taskRuntime";
import { useSettings } from "../lib/SettingsProvider";
import { useProviders } from "../lib/ProvidersProvider";
import { onPiUpdateProgress, safeUnlisten } from "../lib/events";
import { AVAILABLE_SOUNDS, updateCompletionSoundConfig, playCompletionSound } from "../lib/sounds";
import { zoomIn, zoomOut, zoomReset, formatZoomPercent } from "../lib/zoom";
import type { SettingsResponse, ProviderInfo, TestModelsResult, TestChatResult, TestPiResult, PiStatusResponse, SubscriptionAuthResponse, SystemPromptInfo } from "../lib/types";
import { ModelBrowserModal } from "./ModelBrowser";
import { useErrorModal } from "../components/ErrorModal";
import { FRONTEND_PROMPT_CATALOG } from "../lib/promptCatalog";
import { useProject, projectFromPath } from "../components/ProjectPicker";
import { useExtensions } from "../extensions/useExtensions";

/* ── Local data ───────────────────────────────────────────── */

interface ProviderKey {
  id: string;
  name: string;
  type: string;
  endpoint: string;
  key: string;
  set: boolean;
}

const providerInfoToKey = (p: ProviderInfo): ProviderKey => ({
  id: p.name,
  name: p.display_name || p.name,
  type: p.provider_type || "OpenAI Compatible",
  endpoint: p.endpoint ?? "",
  key: p.configured ? "••••••••" : "",
  set: p.configured,
});

/* ── Helper components ────────────────────────────────────── */

function SettingsSection({
  title,
  subtitle,
  children,
}: {
  title: React.ReactNode;
  subtitle?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mb-7">
      <div className="flex items-baseline gap-3 mb-3">
        {typeof title === "string" ? (
          <h2 className="text-[15px] font-semibold text-white">{title}</h2>
        ) : (
          title
        )}
        {subtitle && <span className="text-[12px] text-dim">{subtitle}</span>}
      </div>
      {children}
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="block text-[12px] text-muted font-medium mb-1.5">{label}</label>
      {children}
    </div>
  );
}

/* ── Test status types ────────────────────────────────────── */

type TestState = "idle" | "testing" | "pass" | "fail";

interface ProviderTestState {
  models: TestState;
  modelsCount: number;
  modelsError?: string;
  modelsList: string[];
  chat: TestState;
  chatModel?: string;
  chatPreview?: string;
  chatError?: string;
  pi: TestState;
  piModel?: string;
  piPreview?: string;
  piError?: string;
}

/* ── Tab type ─────────────────────────────────────────────── */

type SettingsTab = "general" | "defaults" | "prompts" | "extensions" | "other";

/* ── Prompts tab ──────────────────────────────────────────── */

type PromptCategory = "Tasks" | "Hivemind" | "Bee Agents" | "Other";

// Controls the display order of sections on the Prompts page.
// Tasks first (primary UX), then Hivemind, then Bee Agents, then Other.
const PROMPT_CATEGORY_ORDER: PromptCategory[] = [
  "Tasks",
  "Hivemind",
  "Bee Agents",
  "Other",
] as const;

function PromptCard({ entry }: { entry: SystemPromptInfo }) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);

  const onCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(entry.body);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard may be unavailable in some webviews — fail silently
    }
  };

  return (
    <div className="rounded-lg border border-line bg-ink-850 overflow-hidden">
      <button
        onClick={() => setExpanded((v) => !v)}
        className="w-full text-left px-4 py-3 hover:bg-ink-700/30 transition-colors"
      >
        <div className="flex items-start gap-3">
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 mb-1">
              <span className="text-[13px] font-semibold text-white">{entry.name}</span>
              <span className={`text-dim transition-transform ${expanded ? "rotate-180" : ""}`}>
                {I.chevD({ size: 11 })}
              </span>
            </div>
            <div className="text-[12px] text-muted leading-relaxed">{entry.description}</div>
          </div>
          <Pill tone="mono" className="shrink-0">{entry.source}</Pill>
        </div>
      </button>
      {expanded && (
        <div className="border-t border-line bg-ink-900/40">
          <div className="flex justify-end px-3 pt-2">
            <Btn kind="ghost" size="sm" onClick={onCopy}>
              {copied ? "Copied" : "Copy"}
            </Btn>
          </div>
          <pre className="px-4 pb-3 pt-1 text-[12px] font-mono text-slate-300 whitespace-pre-wrap break-words leading-relaxed max-h-96 overflow-auto">
            {entry.body}
          </pre>
        </div>
      )}
    </div>
  );
}

function CustomPromptEditorModal({
  open,
  initial,
  onClose,
  onSave,
}: {
  open: boolean;
  initial: ipc.CustomPrompt | null;
  onClose: () => void;
  onSave: (id: string | null, name: string, body: string) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [body, setBody] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setName(initial?.name ?? "");
    setBody(initial?.body ?? "");
    setError(null);
  }, [open, initial]);

  if (!open) return null;

  const trimmedName = name.trim();
  const trimmedBody = body.trim();
  const canSave =
    !saving &&
    trimmedName.length > 0 &&
    trimmedName.length <= 100 &&
    trimmedBody.length > 0 &&
    body.length <= 32 * 1024;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
      <div className="w-[640px] max-w-[90vw] max-h-[85vh] rounded-lg border border-line bg-ink-900 flex flex-col">
        <div className="px-5 py-3 border-b border-line flex items-center justify-between">
          <h3 className="text-[14px] font-semibold text-white">
            {initial ? "Edit custom prompt" : "New custom prompt"}
          </h3>
          <button onClick={onClose} aria-label="Close" className="text-dim hover:text-white">
            {I.x({ size: 16 })}
          </button>
        </div>
        <div className="px-5 py-4 space-y-4 overflow-auto flex-1">
          <div>
            <label className="block text-[12px] text-muted font-medium mb-1.5">
              Name
            </label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. QA Specialist"
              maxLength={100}
              autoFocus
            />
            <div className="mt-1 text-[11px] text-dim">
              {trimmedName.length}/100
            </div>
          </div>
          <div>
            <label className="block text-[12px] text-muted font-medium mb-1.5">
              Prompt body
            </label>
            <textarea
              value={body}
              onChange={(e) => setBody(e.target.value)}
              placeholder="You are a specialist in QA testing…"
              className="w-full min-h-[240px] rounded-md bg-ink-850 border border-line px-3 py-2 text-[13px] font-mono text-white placeholder:text-dim focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 focus:border-honey-500/60"
            />
            <div className="mt-1 text-[11px] text-dim">
              {body.length.toLocaleString()} / {(32 * 1024).toLocaleString()} bytes — appended to the reviewer's system prompt
            </div>
          </div>
          {error && (
            <div className="px-3 py-2 rounded-md bg-red-900/30 border border-red-700/50 text-[12px] text-red-300">
              {error}
            </div>
          )}
        </div>
        <div className="px-5 py-3 border-t border-line flex items-center justify-end gap-2">
          <Btn kind="ghost" onClick={onClose} disabled={saving}>
            Cancel
          </Btn>
          <Btn
            kind="primary"
            disabled={!canSave}
            onClick={async () => {
              setSaving(true);
              setError(null);
              try {
                await onSave(initial?.id ?? null, trimmedName, body);
                onClose();
              } catch (e) {
                setError(String(e));
              } finally {
                setSaving(false);
              }
            }}
          >
            {saving ? "Saving…" : "Save"}
          </Btn>
        </div>
      </div>
    </div>
  );
}

function CustomPromptsSection({
  prompts,
  reload,
}: {
  prompts: ipc.CustomPrompt[];
  reload: () => void;
}) {
  const [editing, setEditing] = useState<ipc.CustomPrompt | null>(null);
  const [editorOpen, setEditorOpen] = useState(false);
  const [feedback, setFeedback] = useState<{ msg: string; ok: boolean } | null>(null);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  useEffect(() => {
    if (!feedback) return;
    const t = setTimeout(() => setFeedback(null), 3000);
    return () => clearTimeout(t);
  }, [feedback]);

  const onSave = useCallback(
    async (id: string | null, name: string, body: string) => {
      await ipc.saveCustomPrompt(id, name, body);
      reload();
      setFeedback({ ok: true, msg: id ? "Updated" : "Created" });
    },
    [reload]
  );

  const onDelete = useCallback(
    async (p: ipc.CustomPrompt) => {
      const ok = await confirmDialog(
        `"${p.name}" will be removed. Any Hivemind models still pointing at it will silently fall back to no custom prompt.`,
        {
          title: "Delete custom prompt?",
          okLabel: "Delete",
          kind: "warning",
        }
      );
      if (!ok) return;
      try {
        await ipc.deleteCustomPrompt(p.id);
        reload();
        setFeedback({ ok: true, msg: "Deleted" });
      } catch (e) {
        setFeedback({ ok: false, msg: String(e) });
      }
    },
    [reload]
  );

  return (
    <SettingsSection
      title="Your Custom Prompts"
      subtitle="Appended to the end of a Hivemind reviewer's system prompt"
    >
      <div className="mb-3 flex items-center justify-between">
        <div className="text-[12px] text-muted">
          {prompts.length === 0
            ? "No custom prompts yet."
            : `${prompts.length} prompt${prompts.length === 1 ? "" : "s"}`}
        </div>
        <div className="flex items-center gap-2">
          {feedback && (
            <span className={`text-[12px] ${feedback.ok ? "text-emerald-400" : "text-red-400"}`}>
              {feedback.msg}
            </span>
          )}
          <Btn
            kind="primary"
            size="sm"
            icon={I.plus({ size: 13 })}
            onClick={() => {
              setEditing(null);
              setEditorOpen(true);
            }}
          >
            New custom prompt
          </Btn>
        </div>
      </div>
      <div className="flex flex-col gap-2">
        {prompts.map((p) => {
          const isExpanded = expandedId === p.id;
          return (
            <div
              key={p.id}
              className="rounded-lg border border-line bg-ink-850 overflow-hidden"
            >
              <div className="flex items-center gap-2 px-4 py-3">
                <button
                  onClick={() => setExpandedId(isExpanded ? null : p.id)}
                  className="flex-1 min-w-0 text-left hover:opacity-80"
                >
                  <div className="flex items-center gap-2 mb-0.5">
                    <span className="text-[13px] font-semibold text-white truncate">
                      {p.name}
                    </span>
                    <span className={`text-dim transition-transform ${isExpanded ? "rotate-180" : ""}`}>
                      {I.chevD({ size: 11 })}
                    </span>
                  </div>
                  <div className="text-[12px] text-muted truncate">
                    {p.body.split("\n")[0].slice(0, 120)}
                  </div>
                </button>
                <Btn
                  kind="ghost"
                  size="sm"
                  onClick={() => {
                    setEditing(p);
                    setEditorOpen(true);
                  }}
                >
                  Edit
                </Btn>
                <Btn kind="danger" size="sm" onClick={() => onDelete(p)}>
                  Delete
                </Btn>
              </div>
              {isExpanded && (
                <pre className="px-4 pb-3 pt-0 text-[12px] font-mono text-slate-300 whitespace-pre-wrap break-words leading-relaxed max-h-96 overflow-auto border-t border-line bg-ink-900/40">
                  {p.body}
                </pre>
              )}
            </div>
          );
        })}
      </div>
      <CustomPromptEditorModal
        open={editorOpen}
        initial={editing}
        onClose={() => setEditorOpen(false)}
        onSave={onSave}
      />
    </SettingsSection>
  );
}

function PromptsTab() {
  const [backendPrompts, setBackendPrompts] = useState<SystemPromptInfo[] | null>(null);
  const [customPrompts, setCustomPrompts] = useState<ipc.CustomPrompt[]>([]);
  const [error, setError] = useState<string | null>(null);

  const reloadCustom = useCallback(() => {
    if (!isTauri()) {
      setCustomPrompts([]);
      return;
    }
    ipc.listCustomPrompts()
      .then(setCustomPrompts)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    if (!isTauri()) {
      setBackendPrompts([]);
      return;
    }
    let cancelled = false;
    ipc.getSystemPrompts()
      .then((list) => { if (!cancelled) setBackendPrompts(list); })
      .catch((e) => { if (!cancelled) setError(String(e)); });
    ipc.listCustomPrompts()
      .then((list) => { if (!cancelled) setCustomPrompts(list); })
      .catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, []);

  if (error) {
    return (
      <div className="px-3 py-2 rounded-md bg-red-900/30 border border-red-700/50 text-[13px] text-red-300">
        Failed to load prompts: {error}
      </div>
    );
  }

  if (backendPrompts === null) {
    return (
      <div className="text-[13px] text-honey-300 flex items-center gap-2">
        <span className="inline-block w-3 h-3 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
        Loading prompts…
      </div>
    );
  }

  const all: SystemPromptInfo[] = [...backendPrompts, ...FRONTEND_PROMPT_CATALOG];
  const grouped = new Map<string, SystemPromptInfo[]>();
  for (const cat of PROMPT_CATEGORY_ORDER) grouped.set(cat, []);
  for (const entry of all) {
    const cat: PromptCategory = PROMPT_CATEGORY_ORDER.includes(entry.category as PromptCategory)
      ? (entry.category as PromptCategory)
      : "Other";
    if (!PROMPT_CATEGORY_ORDER.includes(entry.category as PromptCategory)) {
      console.warn(
        `[Settings/Prompts] Unknown prompt category "${entry.category}" — remapping to "Other". ` +
          `Add it to PROMPT_CATEGORY_ORDER in Settings.tsx to give it its own section.`
      );
    }
    grouped.get(cat)!.push(entry);
  }

  return (
    <>
      <CustomPromptsSection prompts={customPrompts} reload={reloadCustom} />
      <div className="mb-6 text-[13px] text-muted leading-relaxed">
        Every system prompt and prompt template Hyvemind ships with an LLM agent. Read-only — these
        are baked into the binary at build time and cannot be edited from the UI.
      </div>
      {Array.from(grouped.entries())
        .filter(([, entries]) => entries.length > 0)
        .map(([category, entries]) => (
          <SettingsSection
            key={category}
            title={category}
            subtitle={`${entries.length} prompt${entries.length === 1 ? "" : "s"}`}
          >
            <div className="flex flex-col gap-2">
              {entries.map((entry) => (
                <PromptCard key={entry.id} entry={entry} />
              ))}
            </div>
          </SettingsSection>
        ))}
    </>
  );
}

/* ── SettingsScreen ───────────────────────────────────────── */

const TEST_ERRORS = [
  {
    title: "Pi session crashed unexpectedly",
    detail: `pi process crashed (exit_code=1): thread 'tokio-runtime-worker' panicked at 'called \`Result::unwrap()\` on an \`Err\` value: Os { code: 2, kind: NotFound, message: "No such file or directory" }'
note: run with \`RUST_BACKTRACE=1\` environment variable to display a backtrace
stack backtrace:
   0: std::panicking::begin_panic_handler
   1: core::panicking::panic_fmt
   2: core::result::unwrap_failed
   3: hyvemind::pi::session::PiSession::send_prompt
   4: hyvemind::commands::chat::send_message::{{closure}}`,
  },
  {
    title: "Hivemind review round timed out",
    detail: `ReviewEngine: round 1 timed out after 120s
  - anthropic/claude-opus-4.1: completed (8.2s)
  - openai/gpt-5-codex: pending (no response after 120s)
  - google/gemini-2.5-pro: error at 45s: "rate limit exceeded"
Circuit breaker tripped for provider 'openai' (3 consecutive failures)`,
  },
  {
    title: "Worker feature implementation failed",
    detail: `SwarmWorker[feat-0012]: implementation failed after 3 fix attempts
Feature: "Add tenant-scoped idempotency keys"
Milestone: M1 -- Foundations

Attempt 1: cargo check failed — E0308 mismatched types (expected &str, found String)
Attempt 2: cargo check failed — E0599 no method named 'tenant_id' found for struct 'IdempotencyKey'
Attempt 3: cargo test failed — test tenant_idempotency::test_duplicate_key panicked at 'assertion failed: result.is_err()'

Guard verdict: FAIL — max fix attempts exhausted, feature marked as failed.`,
  },
  {
    title: "SQLite migration conflict",
    detail: `sqlx::migrate: migration checksum mismatch for 0001_hivemind.sql
  expected: 8f3a2b1c9d4e5f6a7b8c9d0e1f2a3b4c
  found:    a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6

The migration file has been modified after it was applied.
Database path: ~/.hyvemind/hivemind/reviews.db
This usually means the schema was edited manually. Run with --force to reapply.`,
  },
  {
    title: "Nurse detected stalled swarm",
    detail: `NurseAgent: swarm 'payments-rewrite' stalled for 8m32s
  - Last activity: Worker[feat-0007] wrote 0 bytes in 8m32s
  - Event count: 1,247 (unchanged since last check)
  - Memory: 412MB RSS (within limits)

Intervention: escalating to RESTART (attempt 2/3)
Previous intervention (5m ago): NUDGE — no effect on event_count.
If next restart fails, swarm will be paused and user notified.`,
  },
];

/* ── Nurse Settings Section ───────────────────────────────── */

function formatRelativeMs(ms: number | null): string {
  if (!ms) return "never";
  const delta = Date.now() - ms;
  if (delta < 0) return "in the future";
  if (delta < 5_000) return "just now";
  if (delta < 60_000) return `${Math.floor(delta / 1000)}s ago`;
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  return `${Math.floor(delta / 3_600_000)}h ago`;
}

function NurseSettingsSection({ go }: { go: GoFn }) {
  const { status, refresh } = useNurseStatus();
  const { config, stats } = status;
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<{ msg: string; ok: boolean } | null>(null);
  const swarmsOnly = config.swarms_only ?? false;

  const toggleEnabled = async () => {
    setSaving(true);
    try {
      await ipc.setNurseConfig({ enabled: !config.enabled });
      await refresh();
      setFeedback({ msg: "Saved", ok: true });
    } catch (e) {
      setFeedback({ msg: String(e), ok: false });
    } finally {
      setSaving(false);
      setTimeout(() => setFeedback(null), 2000);
    }
  };

  const toggleSwarmsOnly = async () => {
    setSaving(true);
    try {
      await ipc.setNurseConfig({ swarms_only: !swarmsOnly });
      await refresh();
      setFeedback({ msg: "Saved", ok: true });
    } catch (e) {
      setFeedback({ msg: String(e), ok: false });
    } finally {
      setSaving(false);
      setTimeout(() => setFeedback(null), 2000);
    }
  };

  return (
    <SettingsSection
      title="Nurse"
      subtitle="Long-running session supervisor — full tuning lives on the Nurse screen"
    >
      <div className="rounded-lg border border-line bg-ink-850 p-4 space-y-3">
        <div className="flex items-center justify-between">
          <div>
            <div className="text-[13px] text-white font-medium">Enable Nurse</div>
            <div className="text-[11px] text-muted">
              Monitor all Pi sessions for stalls. Currently
              {" "}{stats.monitored_count} session{stats.monitored_count === 1 ? "" : "s"}.
            </div>
          </div>
          <button
            onClick={toggleEnabled}
            disabled={saving}
            className={`relative w-10 h-5 rounded-full transition-colors ${
              config.enabled ? "bg-emerald-500" : "bg-ink-700"
            }`}
            aria-pressed={config.enabled}
            aria-label="Enable Nurse"
          >
            <span
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
                config.enabled ? "left-[22px]" : "left-0.5"
              }`}
            />
          </button>
        </div>

        <div className="flex items-center justify-between">
          <div>
            <div className="text-[13px] text-white font-medium">Swarms only</div>
            <div className="text-[11px] text-muted">
              Only intervene on long-running swarm agents. Tasks and Hiveminds
              are still monitored but won't be steered, restarted, or
              cancelled.
            </div>
          </div>
          <button
            onClick={toggleSwarmsOnly}
            disabled={saving || !config.enabled}
            className={`relative w-10 h-5 rounded-full transition-colors ${
              swarmsOnly ? "bg-emerald-500" : "bg-ink-700"
            } ${!config.enabled ? "opacity-50 cursor-not-allowed" : ""}`}
            aria-pressed={swarmsOnly}
            aria-label="Swarms only"
          >
            <span
              className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
                swarmsOnly ? "left-[22px]" : "left-0.5"
              }`}
            />
          </button>
        </div>

        <button
          onClick={() => go("nurse")}
          className="flex items-center gap-2 text-[12px] text-honey-300 hover:text-honey-200 transition"
        >
          Open Nurse {I.chevR({ size: 11 })}
        </button>

        {feedback && (
          <div className={`text-[11.5px] ${feedback.ok ? "text-emerald-300" : "text-red-300"}`}>
            {feedback.msg}
          </div>
        )}
      </div>
    </SettingsSection>
  );
}


function ProviderExtensionsTab() {
  const { snapshots, isLoading, refresh, updateSettings } = useExtensions();
  const [busyId, setBusyId] = useState<string | null>(null);
  const [errorById, setErrorById] = useState<Record<string, string>>({});

  const handleRefresh = async (id: string) => {
    setBusyId(id);
    setErrorById((p) => ({ ...p, [id]: "" }));
    try {
      await refresh(id);
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setErrorById((p) => ({ ...p, [id]: msg }));
    } finally {
      setBusyId(null);
    }
  };

  return (
    <SettingsSection
      title="Provider Extensions"
      subtitle="Per-provider usage / status surfaces."
    >
      {isLoading && (
        <div className="text-[13px] text-honey-300 flex items-center gap-2">
          <span className="inline-block w-3 h-3 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
          Loading extensions…
        </div>
      )}
      {!isLoading && snapshots.length === 0 && (
        <div className="text-[13px] text-dim">
          No provider extensions registered. Configure a supported provider in the General tab
          (e.g. OpenRouter) to see usage data here.
        </div>
      )}
      <div className="flex flex-col gap-2">
        {snapshots.map((entry) => {
          const m = entry.manifest;
          const lastErr = errorById[m.id] || entry.last_error || "";
          const fetchedLabel = entry.last_fetched_at
            ? new Date(entry.last_fetched_at * 1000).toLocaleTimeString()
            : "never";
          const statusTone: Record<string, string> = {
            ok: "text-emerald-300 border-emerald-500/40 bg-emerald-500/10",
            error: "text-red-300 border-red-500/40 bg-red-500/10",
            loading: "text-honey-300 border-honey-500/40 bg-honey-500/10",
            unsupported: "text-dim border-line bg-ink-850",
            disabled: "text-muted border-line bg-ink-850",
          };
          return (
            <div
              key={m.id}
              className="rounded-lg border border-line bg-ink-850 p-3 flex items-start gap-3"
            >
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2 mb-1">
                  <span className="text-[13px] font-semibold text-white truncate">
                    {m.display_name}
                  </span>
                  <span
                    className={`px-1.5 py-0.5 rounded text-[10.5px] font-medium border ${
                      statusTone[entry.status] ?? statusTone.disabled
                    }`}
                  >
                    {entry.status}
                  </span>
                  {m.capabilities.map((cap) => (
                    <Pill key={cap} tone="neutral">
                      {cap}
                    </Pill>
                  ))}
                </div>
                <div className="text-[11.5px] text-dim mb-1">
                  Provider: <span className="font-mono">{m.provider_id}</span>
                  {" · "}
                  Last fetched: {fetchedLabel}
                </div>
                <div className="text-[12px] text-muted">{m.description}</div>
                {lastErr && (
                  <div className="text-[12px] text-red-300 mt-1.5 font-mono">
                    {lastErr}
                  </div>
                )}
              </div>
              <div className="flex flex-col gap-2 items-end shrink-0">
                <label className="flex items-center gap-1.5 text-[12px] text-muted cursor-pointer">
                  <input
                    type="checkbox"
                    checked={entry.user_settings.enabled}
                    onChange={(e) =>
                      updateSettings(m.id, { enabled: e.target.checked })
                    }
                  />
                  Enabled
                </label>
                <label className="flex items-center gap-1.5 text-[12px] text-muted cursor-pointer">
                  <input
                    type="checkbox"
                    checked={entry.user_settings.show_in_topbar}
                    onChange={(e) =>
                      updateSettings(m.id, { show_in_topbar: e.target.checked })
                    }
                  />
                  Show in topbar
                </label>
                {m.type_id === "crof_usage" && (
                  <label className="flex items-center gap-1.5 text-[12px] text-muted cursor-pointer">
                    <select
                      className="bg-ink-850 border border-line rounded px-1.5 py-0.5 text-[11px] text-white"
                      value={entry.user_settings.preferences?.["display_mode"] ?? "percentage"}
                      onChange={(e) =>
                        updateSettings(m.id, {
                          preferences: { ...entry.user_settings.preferences, display_mode: e.target.value },
                        })
                      }
                    >
                      <option value="percentage">% (88%)</option>
                      <option value="ratio">Ratio (2200 / 2500)</option>
                    </select>
                  </label>
                )}
                <Btn
                  kind="outline"
                  size="sm"
                  onClick={() => handleRefresh(m.id)}
                  disabled={
                    busyId === m.id ||
                    entry.status === "unsupported" ||
                    entry.status === "disabled"
                  }
                >
                  {busyId === m.id ? "Refreshing…" : "Refresh"}
                </Btn>
              </div>
            </div>
          );
        })}
      </div>
    </SettingsSection>
  );
}

export function SettingsScreen({ go, zoomLevel, onZoomChange }: { go: GoFn; zoomLevel?: number; onZoomChange?: (level: number) => void }) {
  const { showError } = useErrorModal();
  const { hivemindOptions } = useTaskRuntime();
  const { addProject } = useProject();
  // Live nurse status — used to hydrate the batch-interval input from the
  // resolved (user-override | env-var-default) value reported by the engine.
  const { status: nurseStatus } = useNurseStatus();
  // audit 6.7 — keep the local settings/providers copies for the
  // editor's mutation surface, but propagate writes back into the
  // shared SettingsProvider / ProvidersProvider so other screens stay
  // in sync without their own per-screen IPC calls.
  const { patchSettings: patchSharedSettings, refresh: refreshSharedSettings } = useSettings();
  const { refresh: refreshSharedProviders } = useProviders();
  const [activeTab, setActiveTab] = useState<SettingsTab>("general");
  const [settings, setSettings] = useState<SettingsResponse | null>(null);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Inline key editing state
  const [editingProvider, setEditingProvider] = useState<string | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [nameInput, setNameInput] = useState("");
  const [endpointInput, setEndpointInput] = useState("");
  const [saving, setSaving] = useState(false);
  const [feedback, setFeedback] = useState<{ provider: string; msg: string; ok: boolean } | null>(null);
  const [savingAdvanced, setSavingAdvanced] = useState(false);
  const [advancedFeedback, setAdvancedFeedback] = useState<{ msg: string; ok: boolean } | null>(null);

  // Test state per provider
  const [testStates, setTestStates] = useState<Record<string, ProviderTestState>>({});
  const [testingAll, setTestingAll] = useState(false);

  const PI_INSTALL_CMD = "curl -fsSL https://pi.dev/install.sh | sh";

  // Pi status
  const [piStatus, setPiStatus] = useState<PiStatusResponse | null>(null);
  const [piLoading, setPiLoading] = useState(false);
  const [piUpdating, setPiUpdating] = useState(false);
  const [piOpening, setPiOpening] = useState(false);
  const [piUpdateLog, setPiUpdateLog] = useState<string[]>([]);
  const [installCmdCopied, setInstallCmdCopied] = useState(false);
  const logEndRef = useRef<HTMLDivElement>(null);
  const { refresh: refreshAppPi } = usePiGate();

  // Subscription auth
  const [subAuth, setSubAuth] = useState<SubscriptionAuthResponse | null>(null);
  const [subInfoProvider, setSubInfoProvider] = useState<string | null>(null);

  const loadData = useCallback(async () => {
    if (!isTauri()) return;
    setLoading(true);
    try {
      // audit 6.7 — Settings owns the read/write surface and keeps a
      // local copy for the editor's mutation flow. The fetched
      // SettingsResponse is also pushed into the shared SettingsProvider
      // via `patchSharedSettings`/`refreshSharedSettings` so other
      // screens (Tasks, Tests, NewSwarm, ProjectPicker) reflect the
      // change without their own per-screen IPC calls.
      const [s, p, sa] = await Promise.all([ipc.getSettings(), ipc.getProviders(), ipc.checkSubscriptionAuth()]);
      setSettings(s);
      setProviders(p);
      setSubAuth(sa);
      patchSharedSettings(s);
      void refreshSharedProviders();
      setError(null);
    } catch (e) {
      console.error("Failed to load settings:", e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [patchSharedSettings, refreshSharedProviders]);

  useEffect(() => { loadData(); }, [loadData]);

  // Load Pi status on mount
  const loadPiStatus = useCallback(async () => {
    if (!isTauri()) return;
    setPiLoading(true);
    try {
      const status = await ipc.getPiStatus();
      setPiStatus(status);
    } catch (e) {
      setPiStatus({ installed: false, binary_path: null, resolved_path: null, binary_name: null, version: null, latest_version: null, is_outdated: false, install_method: "unknown", error: String(e) });
    } finally {
      setPiLoading(false);
    }
  }, []);

  useEffect(() => { loadPiStatus(); }, [loadPiStatus]);

  // Listen for Pi update progress events
  useEffect(() => {
    if (!isTauri()) return;
    let unlisten: (() => void) | null = null;
    let mounted = true;
    onPiUpdateProgress((e) => {
      if (!mounted) return;
      setPiUpdateLog((prev) => [...prev, `[${e.event_type}] ${e.message}`]);
      if (e.event_type === "completed" || e.event_type === "failed") {
        setPiUpdating(false);
        if (e.event_type === "completed") loadPiStatus();
      }
    }).then((fn) => {
      if (mounted) unlisten = fn;
      else safeUnlisten(fn);
    });
    return () => { mounted = false; safeUnlisten(unlisten); };
  }, [loadPiStatus]);



  // Auto-scroll update log
  useEffect(() => {
    logEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [piUpdateLog]);

  const handleUpdatePi = async () => {
    setPiUpdating(true);
    setPiUpdateLog([]);
    try {
      await ipc.updatePi();
    } catch (e) {
      console.error("Pi update failed:", e);
      setPiUpdateLog((prev) => [...prev, `[error] ${String(e)}`]);
      setPiUpdating(false);
    }
  };

  const handleOpenPi = async () => {
    setPiOpening(true);
    try {
      await ipc.openPiTerminal();
    } catch (e) {
      console.error("Open Pi failed:", e);
      showError("Could not open Pi in a terminal", ipc.formatIpcError(e));
    } finally {
      setPiOpening(false);
    }
  };

  const handleCopyInstallCmd = () => {
    navigator.clipboard.writeText(PI_INSTALL_CMD).then(() => {
      setInstallCmdCopied(true);
      setTimeout(() => setInstallCmdCopied(false), 2000);
    }).catch(() => {});
  };

  // Clear feedback after 3 seconds
  useEffect(() => {
    if (!feedback) return;
    const t = setTimeout(() => setFeedback(null), 3000);
    return () => clearTimeout(t);
  }, [feedback]);

  useEffect(() => {
    if (!advancedFeedback?.ok) return;
    const t = setTimeout(() => setAdvancedFeedback(null), 3000);
    return () => clearTimeout(t);
  }, [advancedFeedback]);

  const handleSaveProvider = async (providerId: string) => {
    const hasKeyChange = keyInput.trim().length > 0;
    const currentProvider = providers.find((p) => p.name === providerId);
    const nameChanged = nameInput.trim() !== (currentProvider?.display_name ?? "");
    const endpointChanged = endpointInput.trim() !== (currentProvider?.endpoint ?? "");
    const hasMetaChange = nameChanged || endpointChanged;
    if (!hasKeyChange && !hasMetaChange) return;
    setSaving(true);
    try {
      // Save name / endpoint if changed
      if (hasMetaChange) {
        const newName = nameInput.trim() || currentProvider?.display_name || providerId;
        const newEndpoint = endpointInput.trim() || currentProvider?.endpoint || undefined;
        await ipc.addProvider(providerId, newName, currentProvider?.provider_type, newEndpoint);
      }
      // Save API key if provided
      if (hasKeyChange) {
        await ipc.saveApiKey(providerId, keyInput.trim());
      }
      setEditingProvider(null);
      setKeyInput("");
      setNameInput("");
      setEndpointInput("");
      setFeedback({ provider: providerId, msg: "Saved", ok: true });
      const p = await ipc.getProviders();
      setProviders(p);
      // audit 6.7 — refresh the shared cache so other screens
      // (Hiveminds, ModelBrowser, Chat) pick up the new state.
      void refreshSharedProviders();
    } catch (e) {
      console.error("Failed to save API key:", e);
      setFeedback({ provider: providerId, msg: String(e), ok: false });
    } finally {
      setSaving(false);
    }
  };

  const handleDeleteKey = async (providerId: string, displayName: string) => {
    const ok = await confirmDialog(`Delete API key for ${displayName}?`, {
      title: "Delete API key",
      okLabel: "Delete",
      cancelLabel: "Cancel",
      kind: "warning",
    });
    if (!ok) return;
    try {
      await ipc.deleteApiKey(providerId);
      setFeedback({ provider: providerId, msg: "Key deleted", ok: true });
      const p = await ipc.getProviders();
      setProviders(p);
      // audit 6.7 — refresh the shared cache so other screens reflect
      // the deletion immediately.
      void refreshSharedProviders();
    } catch (e) {
      console.error("Failed to delete API key:", e);
      setFeedback({ provider: providerId, msg: String(e), ok: false });
    }
  };

  // ── Runtime settings local editing state ──
  const [localSettings, setLocalSettings] = useState<{
    default_model: string;
    default_model_provider: string;
    concurrency_cap: string;
    max_pi_processes: string;
  } | null>(null);

  const [showModelBrowser, setShowModelBrowser] = useState(false);
  const [defaultProjectPath, setDefaultProjectPath] = useState("");
  const [defaultHivemind, setDefaultHivemind] = useState("");
  const [autoModeDefault, setAutoModeDefault] = useState<"off" | "review" | "full">(() => {
    try {
      const raw = localStorage.getItem("hyvemind:auto-mode-default");
      if (raw === "full" || raw === "review" || raw === "off") return raw;
      if (raw === "true") return "full";
      return "off";
    } catch {
      return "off";
    }
  });
  const [autoCommitDefault, setAutoCommitDefault] = useState(false);
  const [autoCommitConventional, setAutoCommitConventional] = useState(false);
  const [crashReporting, setCrashReporting] = useState(true);
  const [mergeTimeoutMin, setMergeTimeoutMin] = useState<string>(() => {
    try {
      const raw = localStorage.getItem(MERGE_TIMEOUT_MIN_KEY);
      const parsed = raw == null ? NaN : Number(raw);
      if (Number.isFinite(parsed) && parsed >= 1) return String(Math.floor(parsed));
    } catch { /* ignore */ }
    return String(MERGE_TIMEOUT_DEFAULT_MIN);
  });
  const [chatCheckInSecs, setChatCheckInSecs] = useState<string>(() => {
    try {
      const raw = localStorage.getItem(CHAT_CHECK_IN_SECS_KEY);
      const parsed = raw == null ? NaN : Number(raw);
      if (Number.isFinite(parsed) && parsed >= CHAT_CHECK_IN_SECS_MIN && parsed <= CHAT_CHECK_IN_SECS_MAX) {
        return String(Math.floor(parsed));
      }
    } catch { /* ignore */ }
    return String(CHAT_CHECK_IN_SECS_DEFAULT);
  });
  // Hydrated from the live Nurse status snapshot (which exposes the
  // effective interval — resolved from user override → env-var default).
  const [batchIntervalSecs, setBatchIntervalSecs] = useState<string>("120");
  // Sync the input from the engine snapshot whenever it changes (initial
  // load, after a successful `set_nurse_config` write). Skip while the user
  // is actively editing — `batchIntervalDirty` flips for two seconds after
  // each keystroke so the engine value doesn't clobber an in-progress edit.
  const [batchIntervalDirty, setBatchIntervalDirty] = useState(false);
  useEffect(() => {
    if (batchIntervalDirty) return;
    const live = nurseStatus.batch?.interval_secs;
    if (typeof live === "number" && live > 0) {
      setBatchIntervalSecs(String(live));
    }
  }, [nurseStatus.batch?.interval_secs, batchIntervalDirty]);
  const [soundEnabled, setSoundEnabled] = useState(false);
  const [selectedSound, setSelectedSound] = useState("chime");
  // Phase 5A: global daily spending cap. Empty string means "unlimited".
  const [dailyBudgetUsd, setDailyBudgetUsd] = useState<string>("");
  const [savingDailyBudget, setSavingDailyBudget] = useState(false);
  const [dailyBudgetFeedback, setDailyBudgetFeedback] = useState<
    { ok: boolean; msg: string } | null
  >(null);

  // Sync local editing state when backend settings load
  useEffect(() => {
    if (settings && !localSettings) {
      const model = settings.default_model ?? "";
      setLocalSettings({
        default_model: model,
        default_model_provider: "",
        concurrency_cap: String(settings.concurrency_cap),
        max_pi_processes: String(settings.max_pi_processes),
      });
      if (settings.default_project_path) {
        setDefaultProjectPath(settings.default_project_path);
        // Ensure it's registered in the projects list
        addProject(projectFromPath(settings.default_project_path));
      }
      if (settings.default_hivemind) {
        // Validate the stored default still exists in the hivemind catalog
        const exists = hivemindOptions.some((h) => h.id === settings.default_hivemind);
        if (exists || hivemindOptions.length === 0) {
          setDefaultHivemind(settings.default_hivemind);
        } else {
          // Stale default — clear it
          setDefaultHivemind("");
          if (isTauri()) ipc.setDefaultHivemind("").catch(() => {});
        }
      } else {
        setDefaultHivemind("");
      }
      if (settings.auto_commit_tasks !== undefined) {
        setAutoCommitDefault(settings.auto_commit_tasks);
      }
      if (settings.auto_commit_conventional !== undefined) {
        setAutoCommitConventional(settings.auto_commit_conventional);
      }
      if (settings.crash_reporting_enabled !== undefined) {
        setCrashReporting(settings.crash_reporting_enabled);
      }
      if (typeof settings.chat_check_in_secs === "number") {
        // Reconcile localStorage with the canonical backend value on load
        // so the watchdog reads the same number on the next session start.
        const backendVal = Math.max(
          CHAT_CHECK_IN_SECS_MIN,
          Math.min(CHAT_CHECK_IN_SECS_MAX, Math.floor(settings.chat_check_in_secs)),
        );
        setChatCheckInSecs(String(backendVal));
        try {
          localStorage.setItem(CHAT_CHECK_IN_SECS_KEY, String(backendVal));
        } catch { /* ignore */ }
      }
      // Phase 5A: hydrate the daily-budget field from the backend.
      if (typeof settings.daily_budget_usd === "number") {
        setDailyBudgetUsd(String(settings.daily_budget_usd));
      } else {
        setDailyBudgetUsd("");
      }
      // Sync completion sound config from backend
      if (settings.task_completion_sound_enabled !== undefined) {
        setSoundEnabled(settings.task_completion_sound_enabled);
        setSelectedSound(settings.task_completion_sound ?? "chime");
        updateCompletionSoundConfig(settings.task_completion_sound_enabled, settings.task_completion_sound ?? "chime");
      }
    }
  }, [settings, localSettings]);

  const updateLocalSetting = (key: keyof NonNullable<typeof localSettings>, value: string) => {
    setLocalSettings((prev) => prev ? { ...prev, [key]: value } : prev);
    if (key === "concurrency_cap" || key === "max_pi_processes") {
      setAdvancedFeedback(null);
    }
  };

  const parsePositiveInteger = (value: string, label: string): number => {
    const trimmed = value.trim();
    if (!/^\d+$/.test(trimmed)) {
      throw new Error(`${label} must be a whole number greater than or equal to 1.`);
    }
    const parsed = Number(trimmed);
    if (!Number.isSafeInteger(parsed) || parsed < 1) {
      throw new Error(`${label} must be a whole number greater than or equal to 1.`);
    }
    return parsed;
  };

  const handleSaveRuntimeSettings = async () => {
    if (!localSettings) return;

    let concurrencyCap: number;
    let maxPiProcesses: number;
    try {
      concurrencyCap = parsePositiveInteger(localSettings.concurrency_cap, "Concurrency cap");
      maxPiProcesses = parsePositiveInteger(localSettings.max_pi_processes, "Max Pi processes");
    } catch (e) {
      setAdvancedFeedback({ msg: String(e instanceof Error ? e.message : e), ok: false });
      return;
    }

    setSavingAdvanced(true);
    setAdvancedFeedback(null);
    try {
      const updated = await ipc.setRuntimeSettings(concurrencyCap, maxPiProcesses);
      setSettings(updated);
      // audit 6.7 — patch the shared cache so other screens see the
      // new concurrency_cap / max_pi_processes without re-fetching.
      patchSharedSettings(updated);
      setLocalSettings((prev) => prev ? {
        ...prev,
        default_model: updated.default_model ?? "",
        concurrency_cap: String(updated.concurrency_cap),
        max_pi_processes: String(updated.max_pi_processes),
      } : prev);
      setAdvancedFeedback({ msg: "Advanced settings saved", ok: true });
    } catch (e) {
      console.error("Failed to save advanced settings:", e);
      setAdvancedFeedback({ msg: String(e), ok: false });
    } finally {
      setSavingAdvanced(false);
    }
  };

  // Phase 5A: persist the global daily spending cap. Empty input is
  // treated as "unlimited" (sent as `null`). Negative / non-finite
  // inputs are rejected client-side before the IPC call.
  const handleSaveDailyBudget = async () => {
    setDailyBudgetFeedback(null);
    const trimmed = dailyBudgetUsd.trim();
    let payload: number | null;
    if (trimmed === "") {
      payload = null;
    } else {
      const n = Number(trimmed);
      if (!Number.isFinite(n) || n < 0) {
        setDailyBudgetFeedback({
          ok: false,
          msg: "Daily budget must be a non-negative number or empty for unlimited.",
        });
        return;
      }
      payload = n;
    }
    if (!isTauri()) {
      setDailyBudgetFeedback({ ok: true, msg: "Mock mode — not persisted" });
      return;
    }
    setSavingDailyBudget(true);
    try {
      const updated = await ipc.setDailyBudget(payload);
      setSettings(updated);
      // audit 6.7 — keep the shared cache in sync.
      patchSharedSettings(updated);
      setDailyBudgetFeedback({
        ok: true,
        msg: payload == null ? "Daily cap cleared" : "Daily cap saved",
      });
    } catch (e) {
      setDailyBudgetFeedback({
        ok: false,
        msg: String(e instanceof Error ? e.message : e),
      });
    } finally {
      setSavingDailyBudget(false);
    }
  };

  // ── Custom provider state ──
  const [showAddProvider, setShowAddProvider] = useState(false);
  const [newProviderName, setNewProviderName] = useState("");
  const [newProviderEndpoint, setNewProviderEndpoint] = useState("");
  const [customProviders, setCustomProviders] = useState<ProviderKey[]>([]);

  const handleAddCustomProvider = async () => {
    const name = newProviderName.trim();
    if (!name) return;
    const id = name.toLowerCase().replace(/\s+/g, "-");
    if (isTauri()) {
      try {
        await ipc.addProvider(id, name, "OpenAI Compatible", newProviderEndpoint.trim() || undefined);
        const p = await ipc.getProviders();
        setProviders(p);
        // audit 6.7 — refresh the shared cache.
        void refreshSharedProviders();
      } catch (e) {
        console.error("Failed to add provider:", e);
        setError(String(e));
      }
    } else {
      setCustomProviders((prev) => [...prev, { id, name, type: "OpenAI Compatible", endpoint: newProviderEndpoint.trim(), key: "", set: false }]);
    }
    setNewProviderName("");
    setNewProviderEndpoint("");
    setShowAddProvider(false);
  };

  // ── Completion sound handlers ──
  const handleCompletionSoundToggle = async (newEnabled: boolean) => {
    const prevEnabled = soundEnabled;
    const prevSound = selectedSound;
    setSoundEnabled(newEnabled);
    if (isTauri()) {
      try {
        const result = await ipc.setTaskCompletionSound(newEnabled, selectedSound);
        updateCompletionSoundConfig(result.task_completion_sound_enabled, result.task_completion_sound);
      } catch (e) {
        setSoundEnabled(prevEnabled);
        console.error("Failed to save completion sound settings:", e);
      }
    }
  };

  const handleCompletionSoundChange = async (newSound: string) => {
    const prevSound = selectedSound;
    const prevEnabled = soundEnabled;
    setSelectedSound(newSound);
    if (isTauri()) {
      try {
        const result = await ipc.setTaskCompletionSound(soundEnabled, newSound);
        updateCompletionSoundConfig(result.task_completion_sound_enabled, result.task_completion_sound);
      } catch (e) {
        setSelectedSound(prevSound);
        console.error("Failed to save completion sound settings:", e);
      }
    }
  };

  // ── Test functions ──
  const updateTestState = (id: string, update: Partial<ProviderTestState>) => {
    setTestStates((prev) => ({
      ...prev,
      [id]: { ...{ models: "idle", modelsCount: 0, modelsList: [], chat: "idle", pi: "idle" }, ...prev[id], ...update },
    }));
  };

  const testOneProvider = async (providerId: string) => {
    const providerInfo = providers.find((p) => p.name === providerId);
    const isSub = providerInfo?.provider_type === "Subscription";

    if (isSub) {
      // Subscription providers: skip HTTP models/chat tests, only run Pi test
      const defaultModel = providerId === "chatgpt" ? "gpt-5.5" : "claude-sonnet-4-20250514";
      updateTestState(providerId, { models: "idle", chat: "idle", pi: "testing", piModel: defaultModel, modelsError: undefined, chatError: undefined, piError: undefined, modelsList: [] });
      try {
        const piResult: TestPiResult = await ipc.testProviderPi(providerId, defaultModel);
        if (piResult.ok) {
          updateTestState(providerId, { pi: "pass", piPreview: piResult.reply_preview ?? undefined });
        } else {
          updateTestState(providerId, { pi: "fail", piError: piResult.error ?? "unknown error" });
        }
      } catch (e) {
        updateTestState(providerId, { pi: "fail", piError: String(e) });
      }
      return;
    }

    // Step 1: test models
    updateTestState(providerId, { models: "testing", chat: "idle", pi: "idle", modelsList: [], modelsError: undefined, chatError: undefined, piError: undefined });
    let testModel: string | null = null;
    try {
      const result: TestModelsResult = await ipc.testProviderModels(providerId);
      if (result.ok) {
        updateTestState(providerId, { models: "pass", modelsCount: result.models.length, modelsList: result.models });
        // Step 2: test chat with a random model
        if (result.models.length > 0) {
          const randomModel = result.models[Math.floor(Math.random() * result.models.length)];
          testModel = randomModel;
          updateTestState(providerId, { chat: "testing", chatModel: randomModel });
          try {
            const chatResult: TestChatResult = await ipc.testProviderChat(providerId, randomModel);
            if (chatResult.ok) {
              updateTestState(providerId, { chat: "pass", chatPreview: chatResult.reply_preview ?? undefined });
            } else {
              updateTestState(providerId, { chat: "fail", chatError: chatResult.error ?? "unknown error" });
            }
          } catch (e) {
            updateTestState(providerId, { chat: "fail", chatError: String(e) });
          }
        }
      } else {
        updateTestState(providerId, { models: "fail", modelsError: result.error ?? "unknown error" });
      }
    } catch (e) {
      updateTestState(providerId, { models: "fail", modelsError: String(e) });
    }

    // Step 3: test Pi RPC registration (uses the same model)
    if (testModel) {
      updateTestState(providerId, { pi: "testing", piModel: testModel });
      try {
        const piResult: TestPiResult = await ipc.testProviderPi(providerId, testModel);
        if (piResult.ok) {
          updateTestState(providerId, { pi: "pass", piPreview: piResult.reply_preview ?? undefined });
        } else {
          updateTestState(providerId, { pi: "fail", piError: piResult.error ?? "unknown error" });
        }
      } catch (e) {
        updateTestState(providerId, { pi: "fail", piError: String(e) });
      }
    }
  };

  const handleTestAll = async () => {
    if (!isTauri()) return;
    setTestingAll(true);
    const configured = providers.filter((p) => p.configured);
    await Promise.allSettled(configured.map((p) => testOneProvider(p.name)));
    setTestingAll(false);
  };

  // Resolve provider rows
  const displayKeys: ProviderKey[] = isTauri()
    ? providers.map(providerInfoToKey)
    : customProviders;

  // Re-validate default hivemind when hivemind options finish loading
  useEffect(() => {
    if (hivemindOptions.length === 0) return;
    if (!defaultHivemind) return;
    const exists = hivemindOptions.some((h) => h.id === defaultHivemind);
    if (!exists) {
      setDefaultHivemind("");
      if (isTauri()) ipc.setDefaultHivemind("").catch(() => {});
    }
  }, [hivemindOptions]);

  const advancedSettingsChanged = Boolean(
    settings && localSettings && (
      localSettings.concurrency_cap !== String(settings.concurrency_cap) ||
      localSettings.max_pi_processes !== String(settings.max_pi_processes)
    )
  );

  return (
    <div className="h-full flex">
      {/* ── Tab Sidebar ── */}
      <aside className="w-[200px] shrink-0 border-r border-line bg-ink-900/50 flex flex-col h-full">
        <div className="px-4 py-5">
          <h1 className="text-[22px] font-bold tracking-tight">Settings</h1>
        </div>
        <nav className="flex-1 px-2 flex flex-col gap-0.5">
          {([
            { id: "general" as SettingsTab, label: "General" },
            { id: "defaults" as SettingsTab, label: "Defaults" },
            { id: "prompts" as SettingsTab, label: "Prompts" },
            { id: "extensions" as SettingsTab, label: "Provider Extensions" },
            { id: "other" as SettingsTab, label: "Other" },
          ]).map((tab) => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`w-full text-left px-3 py-2.5 rounded-lg text-[13px] font-medium transition-colors ${
                activeTab === tab.id
                  ? "bg-honey-500/10 text-honey-300"
                  : "text-muted hover:text-white hover:bg-ink-700/60"
              }`}
            >
              {tab.label}
            </button>
          ))}
        </nav>
      </aside>

      {/* ── Content Area ── */}
      <div className="flex-1 overflow-auto">
        <div className="max-w-[900px] mx-auto px-8 py-7">

          {/* Onboarding banner — shown when Pi is required but not installed */}
          {isTauri() && piStatus && !piStatus.installed && !piLoading && (
            <div className="mb-5 rounded-lg border border-honey-500/40 bg-honey-500/10 p-4">
              <div className="text-[14px] font-semibold text-honey-200">Welcome to Hyvemind</div>
              <div className="text-[12.5px] text-honey-100/80 mt-1">
                One last step: install <strong>Pi</strong> to unlock Tasks, Swarms, and Hiveminds.
                Run the command below in a terminal, then click Refresh.
              </div>
            </div>
          )}

          {/* Loading indicator */}
          {loading && (
            <div className="mb-4 text-[13px] text-honey-300 flex items-center gap-2">
              <span className="inline-block w-3 h-3 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
              Loading settings...
            </div>
          )}

          {/* Top-level error */}
          {error && (
            <div className="mb-4 px-3 py-2 rounded-md bg-red-900/30 border border-red-700/50 text-[13px] text-red-300">
              {error}
            </div>
          )}

          {/* ════════ General Tab ════════ */}
          {activeTab === "general" && (
            <>
              {/* Pi Agent */}
              {isTauri() && (
                <SettingsSection
                  title={
                    <div className="flex items-center gap-3">
                      <h2 className="text-[15px] font-semibold text-white">Pi Agent</h2>
                      {piStatus && (
                        <div className="flex items-center gap-1.5 text-[12px]">
                          <span className={`w-1.5 h-1.5 rounded-full ${piStatus.installed ? "bg-emerald-400" : "bg-red-400"}`} />
                          <span className={piStatus.installed ? "text-emerald-300" : "text-red-300"}>
                            {piStatus.installed ? "Installed" : "Not found"}
                          </span>
                        </div>
                      )}
                      {piStatus?.installed && piStatus.binary_name && (
                        <Pill tone="neutral">{piStatus.binary_name}</Pill>
                      )}
                      {piStatus?.install_method !== "unknown" && piStatus?.installed && (
                        <Pill tone={piStatus.install_method === "npm" ? "purple" : "blue"}>
                          {piStatus.install_method}
                        </Pill>
                      )}
                      {piStatus?.is_outdated && (
                        <Pill tone="honey">Update available</Pill>
                      )}
                    </div>
                  }
                  subtitle="Subprocess agent — required for chat and swarm execution"
                >
                  {piLoading ? (
                    <div className="text-[13px] text-honey-300 flex items-center gap-2">
                      <span className="inline-block w-3 h-3 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
                      Checking Pi status...
                    </div>
                  ) : piStatus ? (
                    <div className="rounded-lg border border-line bg-ink-850 p-4 space-y-3">
                      {piStatus.installed ? (
                        <>
                          <div className="grid grid-cols-[100px_1fr] gap-y-2 text-[13px]">
                            <span className="text-muted">Path</span>
                            <span className="font-mono text-white/80 truncate" title={piStatus.resolved_path ?? piStatus.binary_path ?? ""}>
                              {piStatus.binary_path}
                            </span>
                            <span className="text-muted">Version</span>
                            <span className="text-white/80">
                              {piStatus.version ?? "unknown"}
                            </span>
                            <span className="text-muted">Latest</span>
                            <span className={piStatus.is_outdated ? "text-honey-300" : "text-white/80"}>
                              {piStatus.latest_version ?? "unknown"}
                            </span>
                          </div>
                          <div className="flex items-center gap-2 pt-1">
                            <Btn kind="primary" size="sm" onClick={handleOpenPi} disabled={piOpening}>
                              {I.terminal({ size: 13 })}
                              <span className="ml-1">{piOpening ? "Opening…" : "Open Pi"}</span>
                            </Btn>
                            {piStatus.is_outdated && piStatus.install_method !== "unknown" && (
                              <Btn kind="primary" size="sm" onClick={handleUpdatePi} disabled={piUpdating}>
                                {piUpdating ? (
                                  <>
                                    <span className="inline-block w-3 h-3 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
                                    Updating...
                                  </>
                                ) : (
                                  "Update Pi"
                                )}
                              </Btn>
                            )}
                            <Btn kind="outline" size="sm" onClick={loadPiStatus} disabled={piLoading}>
                              {I.refresh({ size: 13 })}
                            </Btn>
                          </div>
                        </>
                      ) : (
                        <div className="space-y-3">
                          <div className="text-[13px] text-red-300">
                            Pi is required to use Hyvemind.
                          </div>

                          <div className="text-[12px] text-muted">
                            Install Pi by running this command in your terminal:
                          </div>

                          {/* Command code block with copy button */}
                          <div className="flex items-center gap-1 rounded-md bg-ink-900 border border-line px-3 py-2.5 font-mono text-[13px] text-green-300 select-all">
                            <span className="flex-1 truncate">{PI_INSTALL_CMD}</span>
                            <button
                              onClick={handleCopyInstallCmd}
                              className="shrink-0 h-6 w-6 flex items-center justify-center rounded hover:bg-ink-700 transition-colors"
                              title="Copy to clipboard"
                            >
                              {installCmdCopied
                                ? I.check({ size: 13, className: "text-emerald-400" })
                                : I.copy({ size: 13, className: "text-muted" })
                              }
                            </button>
                          </div>

                          <div className="text-[11px] text-dim">
                            After installation, click Refresh to verify.
                          </div>

                          <div className="flex items-center gap-2">
                            <Btn kind="outline" size="sm" onClick={loadPiStatus} disabled={piLoading}>
                              {I.refresh({ size: 13 })}
                              <span className="ml-1">Refresh</span>
                            </Btn>
                          </div>
                        </div>
                      )}
                      {piUpdateLog.length > 0 && (
                        <pre className="mt-2 p-3 rounded-md bg-ink-900 border border-line text-[11px] text-slate-300 font-mono overflow-auto max-h-[200px] whitespace-pre-wrap">
                          {piUpdateLog.join("\n")}
                          <div ref={logEndRef} />
                        </pre>
                      )}
                    </div>
                  ) : null}
                </SettingsSection>
              )}

              {/* API Keys */}
              <SettingsSection
                title={
                  <div className="flex items-center gap-3">
                    <h2 className="text-[15px] font-semibold text-white">API keys</h2>
                    <button
                      onClick={() => setShowAddProvider((v) => !v)}
                      className="w-5 h-5 rounded-md flex items-center justify-center text-muted hover:text-honey-300 hover:bg-honey-500/10 border border-line hover:border-honey-500/30 transition-colors"
                      title="Add custom provider"
                    >
                      {I.plus({ size: 11 })}
                    </button>
                    {isTauri() && (
                      <Btn
                        kind="outline"
                        size="sm"
                        onClick={handleTestAll}
                        disabled={testingAll}
                      >
                        {testingAll ? (
                          <>
                            <span className="inline-block w-3 h-3 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
                            Testing...
                          </>
                        ) : (
                          "Test"
                        )}
                      </Btn>
                    )}
                  </div>
                }
              >
                {/* Add custom provider form */}
                {showAddProvider && (
                  <div className="mb-3 rounded-lg border border-honey-500/30 bg-ink-850 p-3">
                    <div className="text-[12px] font-semibold text-white mb-2">Add custom provider</div>
                    <div className="flex items-end gap-2">
                      <div className="flex-1">
                        <label className="block text-[10.5px] text-muted font-medium mb-1">Provider name</label>
                        <Input
                          value={newProviderName}
                          onChange={(e: React.ChangeEvent<HTMLInputElement>) => setNewProviderName(e.target.value)}
                          placeholder="e.g. MyProvider"
                          onKeyDown={(e: React.KeyboardEvent) => {
                            if (e.key === "Enter") handleAddCustomProvider();
                            if (e.key === "Escape") { setShowAddProvider(false); setNewProviderName(""); setNewProviderEndpoint(""); }
                          }}
                        />
                      </div>
                      <div className="flex-1">
                        <label className="block text-[10.5px] text-muted font-medium mb-1">Endpoint URL</label>
                        <Input
                          value={newProviderEndpoint}
                          onChange={(e: React.ChangeEvent<HTMLInputElement>) => setNewProviderEndpoint(e.target.value)}
                          placeholder="https://api.example.com/v1"
                          className="font-mono"
                          onKeyDown={(e: React.KeyboardEvent) => {
                            if (e.key === "Enter") handleAddCustomProvider();
                            if (e.key === "Escape") { setShowAddProvider(false); setNewProviderName(""); setNewProviderEndpoint(""); }
                          }}
                        />
                      </div>
                      <Btn kind="primary" onClick={handleAddCustomProvider} disabled={!newProviderName.trim()}>
                        Save
                      </Btn>
                      <button
                        className="text-muted hover:text-white h-8 flex items-center"
                        onClick={() => { setShowAddProvider(false); setNewProviderName(""); setNewProviderEndpoint(""); }}
                      >
                        {I.x({ size: 13 })}
                      </button>
                    </div>
                  </div>
                )}

                <div className="rounded-lg border border-line bg-ink-850 overflow-hidden">
                  <div className="grid grid-cols-[1.2fr_110px_1.8fr_100px_85px_85px_85px_60px] gap-3 px-4 py-2.5 text-[10.5px] uppercase tracking-wider text-dim font-semibold border-b border-line bg-ink-800/40">
                    <div>Provider</div>
                    <div className="pl-4">Type</div>
                    <div>Key</div>
                    <div>Status</div>
                    <div>Models</div>
                    <div>Chat Test</div>
                    <div>Pi Test</div>
                    <div></div>
                  </div>
                  {displayKeys.map((p) => {
                    const ts = testStates[p.id];
                    const isSub = p.type === "Subscription";
                    return (
                      <div key={p.id}>
                        <div className="grid grid-cols-[1.2fr_110px_1.8fr_100px_85px_85px_85px_60px] gap-3 items-center px-4 py-2.5 border-b border-line/40 last:border-0 hover:bg-ink-800/40">
                          <div className="text-[13px] font-medium text-white">{p.name}</div>
                          <div className="pl-4">
                            <Pill
                              tone={
                                p.type === "Anthropic"
                                  ? "honey"
                                  : p.type === "Subscription"
                                    ? "purple"
                                    : "neutral"
                              }
                            >
                              {p.type}
                            </Pill>
                          </div>
                          <div className="font-mono text-[12px] text-white/80 truncate">
                            {isSub ? (
                              <Pill tone="purple">Subscription</Pill>
                            ) : p.set ? (
                              p.key.startsWith("http") || p.key.startsWith("/") ? (
                                p.key
                              ) : (
                                <>
                                  &bull;&bull;&bull;&bull; {p.key.slice(-4)}
                                </>
                              )
                            ) : (
                              <span className="text-dim">&mdash; not set &mdash;</span>
                            )}
                          </div>
                          <div className="flex items-center gap-1.5 text-[12px]">
                            {isSub ? (
                              p.set ? (
                                <>
                                  <span className="w-1.5 h-1.5 rounded-full bg-emerald-400" />
                                  <span className="text-emerald-300">Logged in</span>
                                </>
                              ) : (
                                <>
                                  <span className="w-1.5 h-1.5 rounded-full bg-amber-400" />
                                  <span className="text-amber-300">Not authenticated</span>
                                </>
                              )
                            ) : p.set ? (
                              <>
                                <span className="w-1.5 h-1.5 rounded-full bg-emerald-400" />
                                <span className="text-emerald-300">Configured</span>
                              </>
                            ) : (
                              <>
                                <span className="w-1.5 h-1.5 rounded-full bg-line-strong" />
                                <span className="text-dim">Not set</span>
                              </>
                            )}
                            {feedback?.provider === p.id && (
                              <span className={`ml-2 text-[11px] ${feedback.ok ? "text-emerald-300" : "text-red-300"}`}>
                                {feedback.msg}
                              </span>
                            )}
                          </div>
                          {/* Models test column */}
                          <div className="text-[12px]">
                            {isSub ? (
                              <span className="text-dim">N/A</span>
                            ) : (
                              <>
                                {ts?.models === "testing" && (
                                  <span className="text-honey-300 flex items-center gap-1">
                                    <span className="inline-block w-2.5 h-2.5 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
                                    Querying
                                  </span>
                                )}
                                {ts?.models === "pass" && (
                                  <span className="text-emerald-300" title={ts.modelsList.join(", ")}>
                                    {ts.modelsCount} models
                                  </span>
                                )}
                                {ts?.models === "fail" && (
                                  <span className="text-red-300 flex items-center gap-1">
                                    Failed
                                    <span className="relative group cursor-help">
                                      <svg viewBox="0 0 16 16" fill="currentColor" className="w-3.5 h-3.5 text-red-400/80 hover:text-red-300">
                                        <path d="M8 1a7 7 0 100 14A7 7 0 008 1zm-.75 3.75a.75.75 0 011.5 0v3.5a.75.75 0 01-1.5 0v-3.5zM8 11a1 1 0 100 2 1 1 0 000-2z" />
                                      </svg>
                                      <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 hidden group-hover:block z-50 w-max max-w-[260px] px-2.5 py-1.5 rounded-md bg-ink-900 border border-line shadow-lg text-[11px] text-red-200 leading-snug whitespace-pre-wrap">
                                        {ts.modelsError}
                                      </span>
                                    </span>
                                  </span>
                                )}
                                {(!ts || ts.models === "idle") && (
                                  <span className="text-dim">&mdash;</span>
                                )}
                              </>
                            )}
                          </div>
                          {/* Chat test column */}
                          <div className="text-[12px]">
                            {isSub ? (
                              <span className="text-dim">N/A</span>
                            ) : (
                              <>
                                {ts?.chat === "testing" && (
                                  <span className="text-honey-300 flex items-center gap-1">
                                    <span className="inline-block w-2.5 h-2.5 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
                                    Testing
                                  </span>
                                )}
                                {ts?.chat === "pass" && (
                                  <span className="text-emerald-300" title={ts.chatPreview}>
                                    Pass
                                  </span>
                                )}
                                {ts?.chat === "fail" && (
                                  <span className="text-red-300 flex items-center gap-1">
                                    Failed
                                    <span className="relative group cursor-help">
                                      <svg viewBox="0 0 16 16" fill="currentColor" className="w-3.5 h-3.5 text-red-400/80 hover:text-red-300">
                                        <path d="M8 1a7 7 0 100 14A7 7 0 008 1zm-.75 3.75a.75.75 0 011.5 0v3.5a.75.75 0 01-1.5 0v-3.5zM8 11a1 1 0 100 2 1 1 0 000-2z" />
                                      </svg>
                                      <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 hidden group-hover:block z-50 w-max max-w-[260px] px-2.5 py-1.5 rounded-md bg-ink-900 border border-line shadow-lg text-[11px] text-red-200 leading-snug whitespace-pre-wrap">
                                        {ts.chatError}
                                      </span>
                                    </span>
                                  </span>
                                )}
                                {(!ts || ts.chat === "idle") && (
                                  <span className="text-dim">&mdash;</span>
                                )}
                              </>
                            )}
                          </div>
                          {/* Pi RPC test column */}
                          <div className="text-[12px]">
                            {ts?.pi === "testing" && (
                              <span className="text-honey-300 flex items-center gap-1">
                                <span className="inline-block w-2.5 h-2.5 border-2 border-honey-400 border-t-transparent rounded-full animate-spin" />
                                Testing
                              </span>
                            )}
                            {ts?.pi === "pass" && (
                              <span className="flex items-center gap-1.5" title={ts.piPreview}>
                                <span className="w-2 h-2 rounded-full bg-emerald-400 shadow-[0_0_6px_rgba(52,211,153,0.5)]" />
                                <span className="text-emerald-300">Pass</span>
                              </span>
                            )}
                            {ts?.pi === "fail" && (
                              <span className="flex items-center gap-1.5">
                                <span className="w-2 h-2 rounded-full bg-red-400 shadow-[0_0_6px_rgba(248,113,113,0.5)]" />
                                <span className="text-red-300 flex items-center gap-1">
                                  Failed
                                  <span className="relative group cursor-help">
                                    <svg viewBox="0 0 16 16" fill="currentColor" className="w-3.5 h-3.5 text-red-400/80 hover:text-red-300">
                                      <path d="M8 1a7 7 0 100 14A7 7 0 008 1zm-.75 3.75a.75.75 0 011.5 0v3.5a.75.75 0 01-1.5 0v-3.5zM8 11a1 1 0 100 2 1 1 0 000-2z" />
                                    </svg>
                                    <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 hidden group-hover:block z-50 w-max max-w-[260px] px-2.5 py-1.5 rounded-md bg-ink-900 border border-line shadow-lg text-[11px] text-red-200 leading-snug whitespace-pre-wrap">
                                      {ts.piError}
                                    </span>
                                  </span>
                                </span>
                              </span>
                            )}
                            {(!ts || ts.pi === "idle") && (
                              <span className="flex items-center gap-1.5">
                                <span className="w-2 h-2 rounded-full bg-line-strong" />
                                <span className="text-dim">&mdash;</span>
                              </span>
                            )}
                          </div>
                          <div className="flex items-center gap-1 justify-self-end">
                            <button
                              className="text-muted hover:text-honey-300"
                              onClick={() => {
                                if (isSub) {
                                  // Subscription providers: show info popup instead of edit form
                                  setSubInfoProvider(subInfoProvider === p.id ? null : p.id);
                                } else if (editingProvider === p.id) {
                                  setEditingProvider(null);
                                  setKeyInput("");
                                  setNameInput("");
                                  setEndpointInput("");
                                } else {
                                  setEditingProvider(p.id);
                                  setKeyInput("");
                                  setNameInput(p.name);
                                  setEndpointInput(p.endpoint);
                                }
                              }}
                            >
                              {isSub ? I.info({ size: 13 }) : I.edit({ size: 13 })}
                            </button>
                            {isTauri() && p.set && !isSub && (
                              <button
                                className="text-muted hover:text-red-400"
                                onClick={() => handleDeleteKey(p.id, p.name)}
                              >
                                {I.trash({ size: 13 })}
                              </button>
                            )}
                          </div>
                        </div>
                        {/* Subscription info popup */}
                        {subInfoProvider === p.id && isSub && (
                          <div className="px-4 py-3 bg-ink-800/60 border-b border-line/40 space-y-2">
                            <div className="text-[13px] text-white font-medium">Subscription Authentication</div>
                            <div className="text-[12px] text-muted leading-relaxed space-y-1">
                              <p>{p.name} subscription authentication is managed through the Pi SDK.</p>
                              {piStatus?.installed ? (
                                <>
                                  <p>
                                    Click <strong className="text-white">Open Pi</strong> below to launch Pi in a terminal, then type{" "}
                                    <code className="bg-ink-900 px-1.5 py-0.5 rounded text-honey-300 font-mono text-[11px]">/login</code>{" "}
                                    and follow the prompts to add your provider.
                                  </p>
                                  <p>After logging in, return here and click Refresh to update your status.</p>
                                </>
                              ) : piStatus !== null ? (
                                <p>Pi must be installed to enable subscription login. Install Pi from the Welcome banner at the top of Settings, then restart the app.</p>
                              ) : null}
                            </div>
                            <div className="flex items-center gap-2 pt-1">
                              {piStatus?.installed && (
                                <Btn kind="primary" size="sm" onClick={handleOpenPi} disabled={piOpening}>
                                  {I.terminal({ size: 13 })}
                                  <span className="ml-1">{piOpening ? "Opening…" : "Open Pi"}</span>
                                </Btn>
                              )}
                              <Btn kind="outline" size="sm" onClick={async () => {
                                try {
                                  const sa = await ipc.checkSubscriptionAuth();
                                  setSubAuth(sa);
                                  const p2 = await ipc.getProviders();
                                  setProviders(p2);
                                  // audit 6.7 — sync the shared cache.
                                  void refreshSharedProviders();
                                } catch {}
                              }}>
                                {I.refresh({ size: 13 })} Refresh
                              </Btn>
                              <button
                                className="text-muted hover:text-white h-8 flex items-center"
                                onClick={() => setSubInfoProvider(null)}
                              >
                                {I.x({ size: 13 })}
                              </button>
                            </div>
                          </div>
                        )}
                        {/* Inline edit row (API key providers only) */}
                        {editingProvider === p.id && isTauri() && !isSub && (
                          <div className="px-4 py-3 bg-ink-800/60 border-b border-line/40 space-y-2">
                            <div className="flex items-center gap-2">
                              <div className="w-[140px] shrink-0">
                                <label className="block text-[10px] text-dim font-medium mb-0.5">Display name</label>
                                <Input
                                  value={nameInput}
                                  onChange={(e: React.ChangeEvent<HTMLInputElement>) => setNameInput(e.target.value)}
                                  placeholder="Provider name"
                                />
                              </div>
                              <div className="flex-1">
                                <label className="block text-[10px] text-dim font-medium mb-0.5">Endpoint</label>
                                <Input
                                  value={endpointInput}
                                  onChange={(e: React.ChangeEvent<HTMLInputElement>) => setEndpointInput(e.target.value)}
                                  placeholder="https://api.example.com"
                                  className="font-mono"
                                  wrapClass="w-full"
                                />
                              </div>
                            </div>
                            <div className="flex items-center gap-2">
                              <div className="flex-1">
                                <label className="block text-[10px] text-dim font-medium mb-0.5">API key</label>
                                <Input
                                  value={keyInput}
                                  onChange={(e: React.ChangeEvent<HTMLInputElement>) => setKeyInput(e.target.value)}
                                  placeholder={p.set ? "Leave blank to keep current key" : `Enter API key for ${p.name}`}
                                  className="font-mono"
                                  wrapClass="w-full"
                                  onKeyDown={(e: React.KeyboardEvent) => {
                                    if (e.key === "Enter") handleSaveProvider(p.id);
                                    if (e.key === "Escape") { setEditingProvider(null); setKeyInput(""); setNameInput(""); setEndpointInput(""); }
                                  }}
                                />
                              </div>
                              <div className="flex items-center gap-1 shrink-0 self-end">
                                <Btn
                                  kind="primary"
                                  onClick={() => handleSaveProvider(p.id)}
                                  disabled={saving}
                                >
                                  {saving ? "Saving..." : "Save"}
                                </Btn>
                                <button
                                  className="text-muted hover:text-white h-8 flex items-center"
                                  onClick={() => { setEditingProvider(null); setKeyInput(""); setNameInput(""); setEndpointInput(""); }}
                                >
                                  {I.x({ size: 13 })}
                                </button>
                              </div>
                            </div>
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              </SettingsSection>
            </>
          )}

          {/* ════════ Defaults Tab ════════ */}
          {activeTab === "defaults" && (
            <>
              {/* Default model */}
              {settings && localSettings && (
                <div className="mb-7">
                  <Field label="Default model">
                    <button
                      onClick={() => setShowModelBrowser(true)}
                      className="flex items-center gap-2 bg-ink-850 border border-line rounded-lg px-3 py-1.5 text-sm hover:border-honey-500/40 transition-colors cursor-pointer min-w-[200px]"
                    >
                      {localSettings.default_model ? (
                        <span className="font-mono text-slate-200 truncate">
                          {localSettings.default_model.includes("/")
                            ? localSettings.default_model.replace("/", " / ")
                            : localSettings.default_model}
                        </span>
                      ) : (
                        <span className="text-dim">Browse models...</span>
                      )}
                      <span className="text-muted ml-auto shrink-0">{I.chevR({ size: 12 })}</span>
                    </button>
                  </Field>
                </div>
              )}

              {/* Default project path */}
              {settings && (
                <div className="mb-7">
                  <Field label="Default project path">
                    <div className="flex items-center gap-2">
                      <div className="flex-1 flex items-center gap-2 bg-ink-850 border border-line rounded-lg px-3 py-1.5 text-sm min-w-[200px]">
                        {I.folder({ size: 14, className: "text-muted shrink-0" })}
                        <span className={`font-mono truncate ${defaultProjectPath ? "text-slate-200" : "text-dim"}`}>
                          {defaultProjectPath || "No default set"}
                        </span>
                      </div>
                      <button
                        onClick={async () => {
                          try {
                            const selected = await openFolderDialog({
                              directory: true,
                              multiple: false,
                              title: "Select default project folder",
                            });
                            if (selected) {
                              const p = selected as string;
                              setDefaultProjectPath(p);
                              ipc.setDefaultProjectPath(p).catch(() => {});
                              // Register as a project so QuickTask's dropdown sees it
                              addProject(projectFromPath(p));
                            }
                          } catch {}
                        }}
                        className="h-8 px-3 rounded-lg bg-ink-800 border border-line text-[12px] font-medium text-white hover:border-honey-500/40 transition-colors"
                      >
                        Browse…
                      </button>
                      {defaultProjectPath && (
                        <button
                          onClick={() => {
                            setDefaultProjectPath("");
                            ipc.setDefaultProjectPath("").catch(() => {});
                          }}
                          className="text-muted hover:text-red-400"
                          title="Clear default"
                        >
                          {I.x({ size: 13 })}
                        </button>
                      )}
                    </div>
                  </Field>
                </div>
              )}

              {/* Default hivemind */}
              {settings && (
                <div className="mb-7">
                  <Field label="Default hivemind (for new tasks)">
                    <div className="flex items-center gap-2">
                      <Select
                        wrapClass="flex-1"
                        value={defaultHivemind}
                        onChange={(e) => {
                          const val = e.target.value;
                          setDefaultHivemind(val);
                          if (isTauri()) ipc.setDefaultHivemind(val).catch(() => {});
                        }}
                        options={[
                          { value: "", label: "None — skip review" },
                          ...hivemindOptions.map((h) => ({
                            value: h.id,
                            label: h.name,
                          })),
                        ]}
                      />
                      {defaultHivemind && (
                        <button
                          onClick={() => {
                            setDefaultHivemind("");
                            if (isTauri()) ipc.setDefaultHivemind("").catch(() => {});
                          }}
                          className="text-muted hover:text-red-400"
                          title="Clear default"
                        >
                          {I.x({ size: 13 })}
                        </button>
                      )}
                    </div>
                  </Field>
                </div>
              )}

              {/* Task behavior */}
              <SettingsSection title="Task behavior" subtitle="Defaults for new tasks">
                <div className="rounded-lg border border-line bg-ink-850 p-4">
                  <div className="flex items-start justify-between gap-4">
                    <div>
                      <div className="text-[13px] font-medium text-white">Auto Mode</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        What new tasks do once the plan is ready. <span className="text-white/70">Review only</span> auto-runs the Hivemind review (if one is configured) but waits for you to click Implement.
                      </div>
                    </div>
                    <div className="flex shrink-0 rounded-md border border-line bg-ink-900 p-0.5">
                      {([
                        { value: "off" as const, label: "Off" },
                        { value: "review" as const, label: "Review only" },
                        { value: "full" as const, label: "Full" },
                      ]).map((opt) => {
                        const selected = autoModeDefault === opt.value;
                        return (
                          <button
                            key={opt.value}
                            onClick={() => {
                              setAutoModeDefault(opt.value);
                              localStorage.setItem("hyvemind:auto-mode-default", opt.value);
                            }}
                            className={`px-2.5 h-7 text-[11px] font-medium rounded-[5px] transition-colors ${
                              selected
                                ? "bg-honey-500/15 text-honey-400"
                                : "text-dim hover:text-white/70"
                            }`}
                          >
                            {opt.label}
                          </button>
                        );
                      })}
                    </div>
                  </div>
                  <div className="border-t border-line my-3" />
                  <div className="flex items-center justify-between">
                    <div>
                      <div className="text-[13px] font-medium text-white">Auto commit completed tasks</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        Automatically create a git commit when a task finishes implementation.
                      </div>
                    </div>
                    <button
                      onClick={() => {
                        setAutoCommitDefault((prev) => {
                          const next = !prev;
                          if (isTauri()) {
                            ipc.setAutoCommitTasks(next).catch(() => {
                              setAutoCommitDefault(prev);
                            });
                          }
                          return next;
                        });
                      }}
                      className={`relative w-10 h-[22px] rounded-full transition-colors shrink-0 ml-4 hover:opacity-90 ${
                        autoCommitDefault ? "bg-honey-500 border border-honey-500/30" : "bg-ink-700 border border-line"
                      }`}
                    >
                      <span
                        className={`absolute top-[3px] left-0 w-4 h-4 rounded-full bg-white shadow transition-transform pointer-events-none ${
                          autoCommitDefault ? "translate-x-[22px]" : "translate-x-[2px]"
                        }`}
                      />
                    </button>
                  </div>
                  <div className="border-t border-line my-3" />
                  <div className={`flex items-center justify-between ${autoCommitDefault ? "" : "opacity-50"}`}>
                    <div>
                      <div className="text-[13px] font-medium text-white">Use Conventional Commits style</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        Generate titles like <span className="font-mono">feat: …</span> or <span className="font-mono">fix: …</span> from the staged diff. Has no effect when auto-commit is off.
                      </div>
                    </div>
                    <button
                      disabled={!autoCommitDefault}
                      onClick={() => {
                        setAutoCommitConventional((prev) => {
                          const next = !prev;
                          if (isTauri()) {
                            ipc.setAutoCommitConventional(next).catch(() => {
                              setAutoCommitConventional(prev);
                            });
                          }
                          return next;
                        });
                      }}
                      className={`relative w-10 h-[22px] rounded-full transition-colors shrink-0 ml-4 ${
                        autoCommitDefault ? "hover:opacity-90 cursor-pointer" : "cursor-not-allowed"
                      } ${
                        autoCommitConventional ? "bg-honey-500 border border-honey-500/30" : "bg-ink-700 border border-line"
                      }`}
                    >
                      <span
                        className={`absolute top-[3px] left-0 w-4 h-4 rounded-full bg-white shadow transition-transform pointer-events-none ${
                          autoCommitConventional ? "translate-x-[22px]" : "translate-x-[2px]"
                        }`}
                      />
                    </button>
                  </div>
                  <div className="border-t border-line my-3" />
                  <div className="flex items-center justify-between">
                    <div>
                      <div className="text-[13px] font-medium text-white">Crash reporting</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        Send anonymized crash and error reports to help diagnose bugs. API keys, prompts, and file contents are scrubbed before sending. Takes effect on next launch.
                      </div>
                    </div>
                    <button
                      onClick={() => {
                        setCrashReporting((prev) => {
                          const next = !prev;
                          if (isTauri()) {
                            ipc.setCrashReporting(next).catch(() => {
                              setCrashReporting(prev);
                            });
                          }
                          return next;
                        });
                      }}
                      className={`relative w-10 h-[22px] rounded-full transition-colors shrink-0 ml-4 hover:opacity-90 ${
                        crashReporting ? "bg-honey-500 border border-honey-500/30" : "bg-ink-700 border border-line"
                      }`}
                    >
                      <span
                        className={`absolute top-[3px] left-0 w-4 h-4 rounded-full bg-white shadow transition-transform pointer-events-none ${
                          crashReporting ? "translate-x-[22px]" : "translate-x-[2px]"
                        }`}
                      />
                    </button>
                  </div>
                  <div className="border-t border-line my-3" />
                  <div className="flex items-center justify-between gap-4">
                    <div className="min-w-0">
                      <div className="text-[13px] font-medium text-white">Hivemind merge timeout</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        How long to wait for the Pi merge session to produce output before failing the round. Default {MERGE_TIMEOUT_DEFAULT_MIN} minutes.
                      </div>
                    </div>
                    <div className="flex items-center gap-2 shrink-0">
                      <Input
                        type="number"
                        min={1}
                        step={1}
                        aria-label="Hivemind merge timeout (minutes)"
                        value={mergeTimeoutMin}
                        onChange={(e: React.ChangeEvent<HTMLInputElement>) => {
                          const next = e.target.value;
                          setMergeTimeoutMin(next);
                          const parsed = Number(next);
                          if (Number.isFinite(parsed) && parsed >= 1) {
                            try { localStorage.setItem(MERGE_TIMEOUT_MIN_KEY, String(Math.floor(parsed))); } catch { /* ignore */ }
                          }
                        }}
                        className="font-mono w-20 text-right"
                        placeholder={String(MERGE_TIMEOUT_DEFAULT_MIN)}
                      />
                      <span className="text-[12px] text-muted">min</span>
                    </div>
                  </div>
                  <div className="border-t border-line my-3" />
                  <div className="flex items-center justify-between gap-4">
                    <div className="min-w-0">
                      <div className="text-[13px] font-medium text-white">Nurse chat check-in interval</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        Force a Nurse evaluation of any running chat session after N seconds. Nurse decides whether to leave it, steer it, restart it, or cancel. Lower this (e.g. 60) to exercise Nurse against real flows. Range {CHAT_CHECK_IN_SECS_MIN}-{CHAT_CHECK_IN_SECS_MAX}, default {CHAT_CHECK_IN_SECS_DEFAULT}.
                      </div>
                    </div>
                    <div className="flex items-center gap-2 shrink-0">
                      <Input
                        type="number"
                        min={CHAT_CHECK_IN_SECS_MIN}
                        max={CHAT_CHECK_IN_SECS_MAX}
                        step={1}
                        aria-label="Nurse chat check-in interval (seconds)"
                        value={chatCheckInSecs}
                        onChange={(e: React.ChangeEvent<HTMLInputElement>) => {
                          const next = e.target.value;
                          setChatCheckInSecs(next);
                          const parsed = Number(next);
                          if (
                            Number.isFinite(parsed) &&
                            parsed >= CHAT_CHECK_IN_SECS_MIN &&
                            parsed <= CHAT_CHECK_IN_SECS_MAX
                          ) {
                            const floored = Math.floor(parsed);
                            try { localStorage.setItem(CHAT_CHECK_IN_SECS_KEY, String(floored)); } catch { /* ignore */ }
                            // Persist to backend; ignore failures (the localStorage write keeps
                            // the watchdog working even if the backend is unreachable).
                            ipc.setChatCheckInSecs(floored).then((updated) => {
                              setSettings(updated);
                              // audit 6.7 — sync the shared cache.
                              patchSharedSettings(updated);
                            }).catch(() => {});
                          }
                        }}
                        className="font-mono w-20 text-right"
                        placeholder={String(CHAT_CHECK_IN_SECS_DEFAULT)}
                      />
                      <span className="text-[12px] text-muted">sec</span>
                    </div>
                  </div>
                  <div className="border-t border-line my-3" />
                  <div className="flex items-center justify-between gap-4">
                    <div className="min-w-0">
                      <div className="text-[13px] font-medium text-white">Nurse batch review interval</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        How often the batched LLM Nurse sweeps every active session in a single review call. Lower this to catch repetition / stuck loops sooner; higher to save tokens. Range 30-3600, default 120. Takes effect on the next scheduled tick — no restart needed.
                      </div>
                    </div>
                    <div className="flex items-center gap-2 shrink-0">
                      <Input
                        type="number"
                        min={30}
                        max={3600}
                        step={1}
                        aria-label="Nurse batch review interval (seconds)"
                        value={batchIntervalSecs}
                        onChange={(e: React.ChangeEvent<HTMLInputElement>) => {
                          const next = e.target.value;
                          setBatchIntervalSecs(next);
                          setBatchIntervalDirty(true);
                          // Release dirty flag after a brief idle so the
                          // next snapshot can resync if the user navigates
                          // away without finishing the edit.
                          window.setTimeout(() => setBatchIntervalDirty(false), 2000);
                          const parsed = Number(next);
                          if (Number.isFinite(parsed) && parsed >= 30 && parsed <= 3600) {
                            const floored = Math.floor(parsed);
                            ipc
                              .setNurseConfig({ nurse_batch_interval_secs: floored })
                              .catch(() => {
                                /* ignore — input retains the typed value */
                              });
                          }
                        }}
                        className="font-mono w-20 text-right"
                        placeholder="120"
                      />
                      <span className="text-[12px] text-muted">sec</span>
                    </div>
                  </div>
                </div>
              </SettingsSection>

              {/* Display Scaling */}
              <SettingsSection title="Display Scaling" subtitle="Adjust the UI zoom level for accessibility">
                <div className="rounded-lg border border-line bg-ink-850 p-4 space-y-3">
                  <div className="flex items-center justify-between">
                    <div>
                      <div className="text-[13px] font-medium text-white">Zoom level</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        Scale the entire interface. Use ⌘+ / ⌘- / ⌘0 or the controls below.
                      </div>
                    </div>
                    <div className="flex items-center gap-2 shrink-0">
                      <Btn kind="outline" size="sm" onClick={() => onZoomChange?.(zoomOut(zoomLevel ?? 1))}>
                        {I.minus({ size: 13 })}
                      </Btn>
                      <span className="text-[14px] font-mono font-semibold text-white min-w-[48px] text-center">
                        {formatZoomPercent(zoomLevel ?? 1)}
                      </span>
                      <Btn kind="outline" size="sm" onClick={() => onZoomChange?.(zoomIn(zoomLevel ?? 1))}>
                        {I.plus({ size: 13 })}
                      </Btn>
                      <Btn kind="ghost" size="sm" onClick={() => onZoomChange?.(zoomReset())}
                           disabled={(zoomLevel ?? 1) === 1}>
                        Reset
                      </Btn>
                    </div>
                  </div>
                  <div className="flex flex-wrap gap-1.5 pt-1">
                    {[0.75, 0.85, 1.0, 1.25, 1.5, 2.0].map((level) => (
                      <button
                        key={level}
                        onClick={() => onZoomChange?.(level)}
                        className={`px-2.5 py-1 rounded-md text-[12px] font-medium transition-colors ${
                          Math.round((zoomLevel ?? 1) * 100) === Math.round(level * 100)
                            ? "bg-honey-500/20 text-honey-300 border border-honey-500/40"
                            : "bg-ink-700 text-muted hover:text-white border border-line hover:border-line-strong"
                        }`}
                      >
                        {Math.round(level * 100)}%
                      </button>
                    ))}
                  </div>
                </div>
              </SettingsSection>

              {/* Completion Sound */}
              <SettingsSection title="Completion Sound" subtitle="Play a sound when a task or swarm completes">
                <div className="rounded-lg border border-line bg-ink-850 p-4 space-y-3">
                  <div className="flex items-center justify-between">
                    <div>
                      <div className="text-[13px] font-medium text-white">Play sound when task completes</div>
                      <div className="text-[12px] text-muted mt-0.5">
                        Play the selected sound when a task finishes implementation or a swarm completes.
                      </div>
                    </div>
                    <button
                      onClick={() => handleCompletionSoundToggle(!soundEnabled)}
                      className={`relative w-10 h-[22px] rounded-full transition-colors shrink-0 ml-4 hover:opacity-90 ${
                        soundEnabled ? "bg-honey-500 border border-honey-500/30" : "bg-ink-700 border border-line"
                      }`}
                    >
                      <span
                        className={`absolute top-[3px] left-0 w-4 h-4 rounded-full bg-white shadow transition-transform pointer-events-none ${
                          soundEnabled ? "translate-x-[22px]" : "translate-x-[2px]"
                        }`}
                      />
                    </button>
                  </div>
                  {soundEnabled && (
                    <div className="flex items-center gap-2">
                      <div className="flex-1">
                        <Select
                          value={selectedSound}
                          onChange={(e: React.ChangeEvent<HTMLSelectElement>) => handleCompletionSoundChange(e.target.value)}
                          options={AVAILABLE_SOUNDS.map((s) => ({ value: s.id, label: s.label }))}
                        />
                      </div>
                      <Btn
                        kind="outline"
                        size="sm"
                        onClick={() => playCompletionSound(selectedSound)}
                      >
                        Test
                      </Btn>
                    </div>
                  )}
                </div>
              </SettingsSection>
            </>
          )}

          {/* ════════ Prompts Tab ════════ */}
          {activeTab === "prompts" && <PromptsTab />}

          {/* ════════ Provider Extensions Tab ════════ */}
          {activeTab === "extensions" && <ProviderExtensionsTab />}

          {/* ════════ Other Tab ════════ */}
          {activeTab === "other" && (
            <>
              {/* Nurse Service */}
              <NurseSettingsSection go={go} />

              {/* Advanced (concurrency settings) */}
              {settings && localSettings && (
                <SettingsSection title="Advanced">
                  <div className="rounded-lg border border-line bg-ink-850 p-4 space-y-3">
                    <div className="grid grid-cols-2 gap-4">
                      <Field label="Concurrency cap">
                        <Input
                          type="number"
                          min={1}
                          step={1}
                          aria-label="Concurrency cap"
                          value={localSettings.concurrency_cap}
                          onChange={(e: React.ChangeEvent<HTMLInputElement>) => updateLocalSetting("concurrency_cap", e.target.value)}
                          className="font-mono"
                          placeholder="30"
                        />
                      </Field>
                      <Field label="Max Pi processes">
                        <Input
                          type="number"
                          min={1}
                          step={1}
                          aria-label="Max Pi processes"
                          value={localSettings.max_pi_processes}
                          onChange={(e: React.ChangeEvent<HTMLInputElement>) => updateLocalSetting("max_pi_processes", e.target.value)}
                          className="font-mono"
                          placeholder="6"
                        />
                      </Field>
                    </div>
                    <div className="text-[12px] text-muted leading-relaxed">
                      Max Pi processes is persisted immediately, but the existing Pi process pool may require an app restart to fully apply. Default is 6 — each Pi process holds 100-250&nbsp;MB RSS, so a small pool keeps Hyvemind laptop-friendly. The <code className="font-mono text-[11px]">HYVEMIND_PI_MAX_PROCESSES</code> env var overrides this at startup.
                    </div>
                    <div className="flex items-center gap-3">
                      <Btn
                        kind="primary"
                        size="sm"
                        onClick={handleSaveRuntimeSettings}
                        disabled={!advancedSettingsChanged || savingAdvanced}
                      >
                        {savingAdvanced ? "Saving..." : "Save advanced settings"}
                      </Btn>
                      {advancedFeedback && (
                        <span className={`text-[12px] ${advancedFeedback.ok ? "text-emerald-300" : "text-red-300"}`}>
                          {advancedFeedback.msg}
                        </span>
                      )}
                    </div>
                  </div>
                </SettingsSection>
              )}

              {/* Phase 5A: global daily spending cap. */}
              <SettingsSection title="Daily spending cap">
                <div className="rounded-lg border border-line bg-ink-850 p-4 space-y-3">
                  <Field label="Daily spending cap (USD, optional)">
                    <Input
                      type="number"
                      min={0}
                      step={0.01}
                      aria-label="Daily spending cap USD"
                      value={dailyBudgetUsd}
                      onChange={(e: React.ChangeEvent<HTMLInputElement>) =>
                        setDailyBudgetUsd(e.target.value)
                      }
                      className="font-mono max-w-[260px]"
                      placeholder="unlimited"
                    />
                  </Field>
                  <div className="text-[12px] text-muted leading-relaxed">
                    Applies across all swarms, hivemind reviews and Tasks-view sessions for the current UTC day. When the total cost meets this cap any running swarm is paused at the next safe yield. Leave blank for unlimited.
                  </div>
                  <div className="flex items-center gap-3">
                    <Btn
                      kind="primary"
                      size="sm"
                      onClick={handleSaveDailyBudget}
                      disabled={savingDailyBudget}
                    >
                      {savingDailyBudget ? "Saving..." : "Save daily cap"}
                    </Btn>
                    {dailyBudgetFeedback && (
                      <span
                        className={`text-[12px] ${
                          dailyBudgetFeedback.ok ? "text-emerald-300" : "text-red-300"
                        }`}
                      >
                        {dailyBudgetFeedback.msg}
                      </span>
                    )}
                  </div>
                </div>
              </SettingsSection>

              {/* Debug */}
              <SettingsSection title="Debug">
                <Btn
                  kind="outline"
                  size="sm"
                  onClick={() => {
                    const err = TEST_ERRORS[Math.floor(Math.random() * TEST_ERRORS.length)];
                    showError(err.title, err.detail);
                  }}
                >
                  Test Error
                </Btn>
              </SettingsSection>

              {/* Footer version info */}
              <div className="text-[11.5px] text-dim text-center pt-4 space-y-1">
                <div>Hyvemind Studio &middot; v0.4.2 &middot; Tauri 2 &middot; build 4f9a1c2</div>
                {settings && (
                  <>
                    <div className={`flex items-center justify-center gap-1.5 text-[11px] ${settings.stable_mode ? "text-emerald-400" : "text-amber-400"}`}>
                      <span className={`w-1.5 h-1.5 rounded-full ${settings.stable_mode ? "bg-emerald-400" : "bg-amber-400 animate-pulse"}`} />
                      {settings.stable_mode ? "Stable mode — file watcher disabled" : "Watch mode — Rust changes will reboot the app"}
                    </div>
                    <div className={`flex items-center justify-center gap-1.5 text-[11px] ${settings.debug_mode ? "text-emerald-400" : "text-dim"}`}>
                      <span className={`w-1.5 h-1.5 rounded-full ${settings.debug_mode ? "bg-emerald-400" : "bg-line-strong"}`} />
                      {settings.debug_mode ? "Debug mode — TRACE logs writing to ~/.hyvemind/debug/" : "Debug mode off — set HYVEMIND_DEBUG=1 to enable"}
                    </div>
                  </>
                )}
              </div>
            </>
          )}

        </div>
      </div>

      {/* Modal (always rendered, outside tab conditionals) */}
      <ModelBrowserModal
        open={showModelBrowser}
        onClose={() => setShowModelBrowser(false)}
        selectLabel="Set Default"
        onSelect={async (model) => {
          const fullModel = `${model.provider}/${model.id}`;
          const prevModel = localSettings?.default_model ?? "";
          updateLocalSetting("default_model", fullModel);
          updateLocalSetting("default_model_provider", model.provider);
          setShowModelBrowser(false);
          if (isTauri()) {
            try {
              await ipc.setDefaultModel(fullModel);
            } catch (err) {
              console.error("Failed to save default model:", err);
              updateLocalSetting("default_model", prevModel);
              updateLocalSetting("default_model_provider", "");
            }
          }
        }}
      />
    </div>
  );
}
