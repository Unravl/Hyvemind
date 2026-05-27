import React, { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { Btn, Pill, Input, Modal, Select } from "../components/atoms";
import { HIVEMINDS } from "../data/mock";
import { isTauri } from "../lib/tauri";
import { listReviews, getReviewState, listRoundVerdicts, getMergeRun, readMergeOutput, getOrchestratorUsage, getReviewPlan } from "../lib/ipc";
import { subscribeHivemindEventListener } from "../lib/hivemindEventStore";
import { useTaskRuntime } from "../lib/taskRuntime";
import { canonicalizeReviewerModel, dedupeReviewerLabels } from "../lib/review-mode";
import { formatRelativeDate, normalizeDateStr } from "../lib/formatDate";
import type {
  ReviewSummary,
  ReviewStateSnapshot,
  HivemindProgressEvent,
  RoundVerdict,
  OrchestratorUsage,
  PhaseUsage,
} from "../lib/types";

/* ── Filter/sort constants ────────────────────────────────── */

const REVIEW_SORT_MODES = ["newest", "oldest", "status"] as const;
type ReviewSortMode = typeof REVIEW_SORT_MODES[number];

type ReviewRunStatus =
  | "ok"
  | "issues"
  | "fail"
  | "cancelled"
  | "running"
  | "interrupted";

const REVIEW_HIVEMIND_FILTER_ALL = "__all__";
const REVIEW_PROJECT_FILTER_ALL = "__all_projects__";
const REVIEW_PROJECT_FILTER_NONE = "__no_project__";

const REVIEW_HIVEMIND_FILTER_STORAGE_KEY = "hyvemind:review-hivemind-filter";
const REVIEW_PROJECT_FILTER_STORAGE_KEY = "hyvemind:review-project-filter";
const REVIEW_SORT_STORAGE_KEY = "hyvemind:review-sort-mode";

/** Last path segment as a short label (e.g. /Users/h/Documents/Hyvemind → Hyvemind).
 *  Falls back to the full string for short paths or anything without a slash. */
function projectLabel(path: string | null | undefined): string {
  if (!path) return "(no project)";
  const trimmed = path.replace(/[/\\]+$/, "");
  if (!trimmed) return "(no project)";
  const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return idx >= 0 ? trimmed.slice(idx + 1) || trimmed : trimmed;
}

// Active states first, then most problematic completed states, then clean.
// Cancelled is positioned between fail (real error) and issues (clean-but-flagged)
// because it's user intent — not a failure but worth surfacing above clean runs.
const REVIEW_STATUS_ORDER: Record<ReviewRunStatus, number> = {
  running: 0,
  interrupted: 1,
  fail: 2,
  cancelled: 3,
  issues: 4,
  ok: 5,
};

const normalizeHivemindId = (raw?: string | null): string | null => {
  const trimmed = raw?.trim();
  return trimmed ? trimmed : null;
};

const reviewTimestamp = (raw?: string | null): number | null => {
  const ts = raw ? Date.parse(raw) : NaN;
  return Number.isFinite(ts) ? ts : null;
};

const compareReviewCreatedAt = (
  a: ReviewRun,
  b: ReviewRun,
  direction: "asc" | "desc",
): number => {
  const ats = a.created_at_ts;
  const bts = b.created_at_ts;

  // Unknown/bad timestamps always sort last, regardless of direction.
  if (ats == null && bts == null) return 0;
  if (ats == null) return 1;
  if (bts == null) return -1;

  return direction === "asc" ? ats - bts : bts - ats;
};

const compareReviewIdValues = (
  aId: number | string,
  bId: number | string,
  direction: "asc" | "desc",
): number => {
  const a = String(aId);
  const b = String(bId);
  const an = Number(a);
  const bn = Number(b);

  const cmp =
    Number.isFinite(an) && Number.isFinite(bn)
      ? an - bn
      : a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" });

  return direction === "asc" ? cmp : -cmp;
};

/* ── Local data ───────────────────────────────────────────── */

const mockIsoMinutesAgo = (minutes: number) =>
  new Date(Date.now() - minutes * 60_000).toISOString();

const mockReviewRun = (
  run: Omit<ReviewRun, "created_at_ts" | "project_path"> & {
    project_path?: string | null;
  }
): ReviewRun => ({
  ...run,
  hivemind_id: normalizeHivemindId(run.hivemind_id),
  project_path: run.project_path ?? null,
  created_at_ts: reviewTimestamp(run.created_at_raw),
});

const REVIEW_RUNS: ReviewRun[] = [
  mockReviewRun({
    id: 23,
    child_job_ids: [],
    status: "ok",
    duration: "14.2s",
    models: 5,
    rounds: 2,
    date: "2m ago",
    prompt: "Review the rotate-refresh-tokens implementation. Watch for race\u2026",
    hivemind_id: "security-audit",
    project_path: "/Users/me/Documents/Hyvemind",
    created_at_raw: mockIsoMinutesAgo(2),
  }),
  mockReviewRun({
    id: 22,
    child_job_ids: [],
    status: "issues",
    duration: "18.4s",
    models: 5,
    rounds: 2,
    date: "1h ago",
    prompt: "Plan review for payments-rewrite M2 \u2014 ledger schema and double-\u2026",
    hivemind_id: "arch-council",
    project_path: "/Users/me/Documents/payments-service",
    created_at_raw: mockIsoMinutesAgo(60),
  }),
  mockReviewRun({
    id: 21,
    child_job_ids: [],
    status: "ok",
    duration: "11.0s",
    models: 3,
    rounds: 1,
    date: "3h ago",
    prompt: "Quick check on csrf-double-submit-cookie patch.",
    hivemind_id: "fast-review",
    project_path: "/Users/me/Documents/Hyvemind",
    created_at_raw: mockIsoMinutesAgo(180),
  }),
  mockReviewRun({
    id: 20,
    child_job_ids: [],
    status: "fail",
    duration: " 4.1s",
    models: 5,
    rounds: 2,
    date: "yesterday",
    prompt: "Hivemind timeout: gpt-5-codex did not respond in 60s.",
    hivemind_id: null,
    created_at_raw: mockIsoMinutesAgo(24 * 60),
  }),
  mockReviewRun({
    id: 19,
    child_job_ids: [],
    status: "issues",
    duration: "16.8s",
    models: 5,
    rounds: 2,
    date: "2d ago",
    prompt: "Review oauth-state-binding before commit. Look for replay\u2026",
    hivemind_id: "security-audit",
    created_at_raw: mockIsoMinutesAgo(2 * 24 * 60),
  }),
  mockReviewRun({
    id: 18,
    child_job_ids: [],
    status: "ok",
    duration: "12.1s",
    models: 5,
    rounds: 2,
    date: "2d ago",
    prompt: "Synthesis pass on M1 features \u2014 production-readiness check.",
    hivemind_id: "enhance",
    created_at_raw: mockIsoMinutesAgo(2 * 24 * 60 + 30),
  }),
  mockReviewRun({
    id: 17,
    child_job_ids: [],
    status: "ok",
    duration: "10.7s",
    models: 5,
    rounds: 2,
    date: "3d ago",
    prompt: "argon2id-rehash-on-login \u2014 edge cases around password resets.",
    hivemind_id: null,
    created_at_raw: mockIsoMinutesAgo(3 * 24 * 60),
  }),
];

const SAMPLE_RESPONSE = {
  approved:
    "Implementation looks correct. The rotation flow correctly mints a new refresh token, stores the family-id, and revokes the prior token atomically when the family lookup succeeds. TTL handling matches the spec (14d). Audit log entries are written before the response is sent.\n\nNo blocking issues found.",
  issues:
    "1. Logic \u2014 `rotate_refresh()` reads the family-id without a lock. Two concurrent rotations on the same token can both succeed and end up minting two valid refresh tokens for the same family. Recommend `SELECT \u2026 FOR UPDATE` or a lua-atomic in Redis.\n\n2. Edge Case \u2014 when the family is null on first rotation the code path takes a different branch that does not write to the audit log. Likely an oversight; add the audit write before the early return.",
};

interface RoundResult {
  /** Display id ("provider/model_id" when available, else model_id). */
  model: string;
  /** Provider segment, shown as a sub-label in the dense row. */
  provider: string;
  /** Numeric metrics for the dense row. */
  tokInNum: number;
  tokOutNum: number;
  durationMs: number;
  cost: number | null;
  /** Coarse status from regex parse; used only for fallback tally in RoundBlock header when no orchestrator verdicts exist. */
  verdict: "issues" | "approved";
  /** Whether the model call itself failed (timeout, error, cancelled — did not produce a valid response). */
  failed: boolean;
  /** Whether the model call is still in progress (pending/running — not yet completed). */
  pending: boolean;
  /** Reason the model call failed, if available. */
  error: string | null;
  issues: Record<string, number>;
  tokIn: string;
  tokOut: string;
  time: string;
  body: string;
}

const ROUND1_RESULTS: RoundResult[] = [
  {
    model: "claude-opus-4.1",
    provider: "anthropic",
    tokInNum: 2418,
    tokOutNum: 1124,
    durationMs: 8200,
    cost: 0.061,
    verdict: "issues",
    failed: false,
    pending: false,
    error: null,
    issues: { Architecture: 0, Logic: 1, "Edge Cases": 1, Performance: 0 },
    tokIn: "2,418",
    tokOut: "1,124",
    time: "8.2s",
    body: SAMPLE_RESPONSE.issues,
  },
  {
    model: "gpt-5-codex",
    provider: "openai",
    tokInNum: 2418,
    tokOutNum: 982,
    durationMs: 11400,
    cost: 0.052,
    verdict: "issues",
    failed: false,
    pending: false,
    error: null,
    issues: { Architecture: 0, Logic: 1, "Edge Cases": 1, Performance: 0 },
    tokIn: "2,418",
    tokOut: "982",
    time: "11.4s",
    body: SAMPLE_RESPONSE.issues,
  },
  {
    model: "gemini-2.5-pro",
    provider: "google",
    tokInNum: 2418,
    tokOutNum: 648,
    durationMs: 6800,
    cost: 0.042,
    verdict: "approved",
    failed: false,
    pending: false,
    error: null,
    issues: { Architecture: 0, Logic: 0, "Edge Cases": 0, Performance: 0 },
    tokIn: "2,418",
    tokOut: "648",
    time: "6.8s",
    body: SAMPLE_RESPONSE.approved,
  },
];

const ROUND2_RESULTS: RoundResult[] = [
  {
    model: "glm-4.6",
    provider: "openrouter",
    tokInNum: 3860,
    tokOutNum: 412,
    durationMs: 1100,
    cost: 0.013,
    verdict: "approved",
    failed: false,
    pending: false,
    error: null,
    issues: { Logic: 0, "Edge Cases": 0 },
    tokIn: "3,860",
    tokOut: "412",
    time: "1.1s",
    body: "Synthesis confirms the lock-on-family-id fix is the correct minimal change. Other findings either duplicate or are out of scope.",
  },
  {
    model: "deepseek-v3.2",
    provider: "openrouter",
    tokInNum: 3860,
    tokOutNum: 388,
    durationMs: 1200,
    cost: 0.011,
    verdict: "approved",
    failed: false,
    pending: false,
    error: null,
    issues: { Logic: 0, "Edge Cases": 0 },
    tokIn: "3,860",
    tokOut: "388",
    time: "1.2s",
    body: "Agree with glm-4.6. The two flagged Logic issues collapse to one root cause.",
  },
];

const VERDICT: Record<string, { label: string; tone: "green" | "honey" }> = {
  approved: { label: "Accepted", tone: "green" },
  issues: { label: "Rejected", tone: "honey" },
};

/* ── Formatting helpers ───────────────────────────────────── */

function fmtNum(n: number): string {
  if (!Number.isFinite(n) || n === 0) return "—";
  return n.toLocaleString();
}

function fmtDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

/** Strip the "provider/" prefix for display; the provider label is shown separately. */
function displayModelName(model: string): string {
  const slashIdx = model.indexOf("/");
  return slashIdx === -1 ? model : model.slice(slashIdx + 1);
}

/* ── ModelResultRow — dense single row ───────────────────── */

function StatCell({ label, v }: { label: string; v: string }) {
  return (
    <div className="text-right">
      <div className="text-[9.5px] uppercase tracking-wider text-dim leading-none">{label}</div>
      <div className="font-mono text-[11.5px] text-white/85 tabular-nums leading-tight">{v}</div>
    </div>
  );
}

function BestFindStar() {
  // Filled star, honey-tinted. Inline SVG to avoid pulling in a new icon dep.
  return (
    <span
      title="Best find of this round"
      aria-label="Best find of this round"
      className="inline-flex items-center text-honey-500"
    >
      <svg
        width="13"
        height="13"
        viewBox="0 0 24 24"
        fill="currentColor"
        aria-hidden="true"
      >
        <path d="M12 2.5l2.92 6.34 6.96.69-5.21 4.66 1.51 6.81L12 17.55l-6.18 3.45 1.51-6.81L2.12 9.53l6.96-.69L12 2.5z" />
      </svg>
    </span>
  );
}

function ModelResultRow({
  m,
  verdicts,
  isBestFind,
  bestFindVerdictId,
  onShowReply,
  sharedBucketTag = false,
}: {
  m: RoundResult;
  verdicts: RoundVerdict[];
  isBestFind: boolean;
  bestFindVerdictId?: string;
  onShowReply: () => void;
  /**
   * Legacy review compatibility: when N reviewer rows share a single
   * non-attributable verdict bucket (because the merge agent emitted
   * identical `reviewer` strings for every duplicate instance), the
   * bucket is rendered only on the first row in the group. Subsequent
   * rows in the same group receive `sharedBucketTag=true` and render
   * an inline tag in place of the accepted/total count.
   */
  sharedBucketTag?: boolean;
}) {
  const [expanded, setExpanded] = useState(false);
  const accepted = verdicts.filter((v) => v.verdict === "accepted").length;
  const total = verdicts.length;
  const tps = m.durationMs > 0 ? m.tokOutNum / (m.durationMs / 1000) : 0;
  // Three-state status dot: red = failed, gray = pending/running, green = completed.
  const statusDotTone = m.failed ? "bg-red-400" : m.pending ? "bg-zinc-400" : "bg-emerald-400";

  return (
    <div className="border-b border-line/60 last:border-b-0">
      <div
        className="grid items-center gap-3 px-3 py-2 hover:bg-ink-800/30 cursor-pointer text-[12px]"
        style={{
          gridTemplateColumns:
            "10px minmax(0, 1.4fr) 70px 70px 70px 70px 70px 110px 92px",
        }}
        onClick={() => setExpanded((x) => !x)}
      >
        <span className={`inline-block w-2 h-2 rounded-full ${statusDotTone}`} />
        <div className="min-w-0">
          <div className="font-mono text-white truncate flex items-center gap-1.5">
            {isBestFind && <BestFindStar />}
            <span className="truncate">{displayModelName(m.model)}</span>
          </div>
          <div className="text-[10.5px] text-dim truncate">{m.provider || "—"}</div>
        </div>
        <StatCell label="in" v={fmtNum(m.tokInNum)} />
        <StatCell label="out" v={fmtNum(m.tokOutNum)} />
        <StatCell label="t/s" v={tps > 0 ? `${tps.toFixed(1)}` : "—"} />
        <StatCell label="time" v={fmtDuration(m.durationMs)} />
        <StatCell
          label="cost"
          v={m.cost != null && m.cost > 0 ? `$${m.cost.toFixed(4)}` : "—"}
        />
        <div className="text-[11.5px] font-mono tabular-nums text-right">
          {sharedBucketTag ? (
            <span
              className="text-[10.5px] text-dim italic"
              title="This is a duplicate instance of the same model. Its verdicts could not be attributed independently and are shown on the first row of the group."
            >
              shared with first instance
            </span>
          ) : total > 0 ? (
            <>
              <span className="text-emerald-300">{accepted}</span>
              <span className="text-dim">/{total}</span>
              <span className="text-muted ml-1">accepted</span>
            </>
          ) : (
            <span className="text-dim">—</span>
          )}
        </div>
        <div className="flex items-center justify-end">
          <button
            onClick={(e) => {
              e.stopPropagation();
              onShowReply();
            }}
            className="text-[11px] px-2 py-1 rounded border border-line/60 text-muted hover:text-honey-300 hover:border-honey-500/40"
          >
            view reply
          </button>
        </div>
      </div>

      {expanded && m.failed && m.error && (
        <div className="px-3 pb-2.5 pt-1 bg-red-500/5 border-t border-red-500/20">
          <div className="text-[10px] uppercase tracking-wider text-red-400/80 mb-1">Error</div>
          <div className="text-[11.5px] text-red-300/90 font-mono leading-relaxed break-words">
            {m.error}
          </div>
        </div>
      )}
      {expanded && !m.failed && total > 0 && (
        <SuggestionList verdicts={verdicts} bestFindId={bestFindVerdictId} />
      )}
      {expanded && !m.failed && total === 0 && sharedBucketTag && (
        <div className="px-3 pb-2 text-[11px] text-dim italic">
          Verdicts for this model instance are not independently
          attributable. See the first row in this group.
        </div>
      )}
      {expanded && !m.failed && total === 0 && !sharedBucketTag && (
        <div className="px-3 pb-2 text-[11px] text-dim italic">
          No orchestrator verdicts persisted for this review.
        </div>
      )}
      {expanded && m.failed && !m.error && (
        <div className="px-3 pb-2 text-[11px] text-red-400/80 italic">
          Model call failed — no error details available.
        </div>
      )}
    </div>
  );
}

/* ── SuggestionList — per-suggestion table when row expanded ── */

function SuggestionList({
  verdicts,
  bestFindId,
}: {
  verdicts: RoundVerdict[];
  bestFindId?: string;
}) {
  const tone: Record<string, string> = {
    accepted: "border-emerald-500/30 bg-emerald-500/5 text-emerald-200",
    rejected: "border-red-500/30 bg-red-500/5 text-red-200",
    modified: "border-honey-500/30 bg-honey-500/5 text-honey-200",
  };
  return (
    <div className="px-3 pb-3 pt-1 bg-ink-900/40 border-t border-line/40">
      <div className="text-[10px] uppercase tracking-wider text-dim mb-1.5">
        Orchestrator verdicts
      </div>
      <div className="space-y-1">
        {verdicts.map((v) => {
          const isBest = bestFindId != null && v.id === bestFindId;
          return (
            <div
              key={v.id}
              className="grid items-start gap-2 text-[11.5px]"
              style={{ gridTemplateColumns: "84px minmax(0, 1fr) 28px" }}
            >
              <span
                className={`px-1.5 py-0.5 rounded text-[10.5px] font-medium border text-center ${
                  tone[v.verdict] ?? tone.rejected
                }`}
              >
                {v.verdict}
              </span>
              <div className="text-white/85 min-w-0">
                <div>
                  {isBest && (
                    <span
                      title="Designated best find of this round"
                      className="inline-block align-baseline mr-1.5 text-honey-300 border border-honey-500/40 bg-honey-500/10 rounded px-1.5 py-[1px] text-[10px] font-mono uppercase tracking-wide"
                    >
                      Best Find
                    </span>
                  )}
                  {v.suggestion}
                </div>
                {v.reason && (
                  <div className="text-[10.5px] text-dim mt-0.5">{v.reason}</div>
                )}
              </div>
              <span className="text-dim font-mono text-[10.5px] text-right">
                {v.severity != null ? `S${v.severity}` : ""}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/* ── ReplyModal — full reply text, reuses Modal atom ─────── */

function ReplyModal({
  open,
  onClose,
  model,
  body,
}: {
  open: boolean;
  onClose: () => void;
  model: string;
  body: string;
}) {
  return (
    <Modal open={open} onClose={onClose} title={`Full reply — ${displayModelName(model)}`} wide>
      <pre className="whitespace-pre-wrap font-mono text-[12px] leading-relaxed text-white/85 max-h-[60vh] overflow-auto bg-ink-900/60 border border-line/60 rounded p-3">
        {body}
      </pre>
    </Modal>
  );
}

/* ── RoundBlock ───────────────────────────────────────────── */

interface RoundBlockProps {
  idx: number;
  label: string;
  prompt: string;
  tokens: string;
  results: RoundResult[];
  /** Verdicts for this round, bucketed by reviewer key (`provider/model_id`). */
  verdictsByModel: Record<string, RoundVerdict[]>;
  /**
   * The merge model's designated best find for this round. `id` is the
   * verdict id (drives the suggestion-row tag); `models` contains only
   * the primary reviewer credited with the find (drives the star next
   * to the model name). Co-reviewers are not starred.
   */
  bestFind?: { id: string; models: Set<string> } | null;
  verdictTone: "honey" | "green";
  verdictText: string;
  /** Agent ID (model_id) for the merge session of this round. */
  agentId?: string;
  /** Session UUID for the merge session of this round. */
  sessionId?: string;
}

function RoundBlock({
  idx,
  label,
  prompt,
  tokens,
  results,
  verdictsByModel,
  bestFind,
  verdictTone,
  verdictText,
  agentId,
  sessionId,
}: RoundBlockProps) {
  const [reply, setReply] = useState<{ model: string; body: string } | null>(null);
  const [showPrompt, setShowPrompt] = useState(false);
  const [copied, setCopied] = useState(false);
  const copyTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (copyTimerRef.current) {
        clearTimeout(copyTimerRef.current);
        copyTimerRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    setCopied(false);
    if (copyTimerRef.current) {
      clearTimeout(copyTimerRef.current);
      copyTimerRef.current = null;
    }
  }, [agentId, sessionId]);

  const handleCopySessionId = useCallback(async () => {
    const trimmedSessionId = sessionId?.trim();
    if (!trimmedSessionId) return;

    if (!navigator.clipboard?.writeText) {
      console.warn("[review-history] clipboard API not available");
      return;
    }

    try {
      await navigator.clipboard.writeText(trimmedSessionId);
      if (!mountedRef.current) return;

      setCopied(true);

      if (copyTimerRef.current) {
        clearTimeout(copyTimerRef.current);
      }

      copyTimerRef.current = setTimeout(() => {
        copyTimerRef.current = null;
        if (mountedRef.current) {
          setCopied(false);
        }
      }, 1500);
    } catch (err) {
      console.warn("[review-history] failed to copy merge session ID:", err);
    }
  }, [sessionId]);

  // Round-level tally — orchestrator verdicts when present, regex fallback otherwise.
  //
  // We iterate `results` to honour the model order (and pick up live counts
  // even if `verdictsByModel` is partially populated), but each unique model
  // key must contribute its verdict bucket EXACTLY ONCE. Legacy rows in a
  // round with N duplicate instances of the same `provider/model_id` all
  // share a single bucket (because verdicts were persisted before per-
  // instance suffixes existed), so iterating rows and summing per-row would
  // multiply that shared bucket by N. The `seen` set prevents that.
  //
  // The final loop is a drift safety net: include any stored bucket whose
  // key has no representative row (e.g. a reviewer step that never
  // streamed back to the UI but was persisted).
  const roundVerdictTotals = useMemo(() => {
    const seen = new Set<string>();
    let accepted = 0;
    let total = 0;
    const addBucket = (vs: RoundVerdict[]) => {
      total += vs.length;
      accepted += vs.filter((v) => v.verdict === "accepted").length;
    };
    for (const m of results) {
      if (seen.has(m.model)) continue;
      seen.add(m.model);
      addBucket(verdictsByModel[m.model] || []);
    }
    for (const [k, vs] of Object.entries(verdictsByModel)) {
      if (seen.has(k)) continue;
      seen.add(k);
      addBucket(vs);
    }
    return { accepted, total };
  }, [results, verdictsByModel]);

  const acceptedRegex = results.filter((r) => r.verdict === "approved").length;
  const showOrchestrator = roundVerdictTotals.total > 0;

  // For legacy reviews where N rows share a single verdict bucket key
  // (because the merge agent emitted the same `reviewer` string for every
  // duplicate instance of one model), we render the bucket only on the
  // first row in the group and show a small "shared" tag on the rest.
  //
  // Grouping is by BASE label (= `m.model` with any trailing " #N" suffix
  // stripped) so the detection works regardless of whether the row keys
  // are bare base labels (legacy path) or suffixed (Step 4 / new path).
  // For NEW reviews each suffixed row carries its own non-empty bucket,
  // so the legacy-tag branch only fires when (a) the row's own bucket is
  // empty AND (b) another row in the same base group has a non-empty
  // bucket. That makes this branch a no-op for new reviews and a
  // cosmetic cleanup for old ones.
  const baseLabel = (m: string): string => m.replace(/\s+#\d+$/, "");
  const baseGroupCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const r of results) {
      const base = baseLabel(r.model);
      counts.set(base, (counts.get(base) ?? 0) + 1);
    }
    return counts;
  }, [results]);
  // Which base groups contain at least one row with a non-empty bucket?
  // Used to gate the "shared" tag so rows with genuinely-no-verdicts in
  // a non-duplicated group still show "—" as before.
  const baseGroupHasBucket = useMemo(() => {
    const has = new Set<string>();
    for (const r of results) {
      const vs = verdictsByModel[r.model];
      if (vs && vs.length > 0) has.add(baseLabel(r.model));
    }
    return has;
  }, [results, verdictsByModel]);

  return (
    <section className="rounded-xl border border-line bg-ink-800/40 overflow-hidden">
      <header className="flex items-center gap-2 px-4 h-10 border-b border-line bg-ink-850">
        <span className="font-mono text-[11px] text-honey-400 tracking-wider">R{idx}</span>
        <span className="text-[13px] font-semibold text-white">{label}</span>
        <span className="text-dim text-[11px]">·</span>
        <span className="text-[11.5px] text-muted">
          {results.length} model{results.length === 1 ? "" : "s"}
        </span>
        <div className="flex-1" />
        {agentId?.trim() && sessionId?.trim() && (
          <button
            type="button"
            onClick={handleCopySessionId}
            title={copied ? "Copied session ID" : "Copy session ID to clipboard"}
            aria-label={`Copy merge session ID for ${agentId}`}
            className="flex items-center gap-1 text-[11px] px-2 py-1 rounded border border-line/60 text-muted hover:text-honey-300 hover:border-honey-500/40 transition-colors"
          >
            {I.hexFill({ size: 10, className: "text-honey-400" })}
            <span className="max-w-[80px] truncate">{agentId}</span>
            {copied ? (
              <span className="text-[10px] text-emerald-400 font-medium">Copied!</span>
            ) : (
              I.copy({ size: 11 })
            )}
          </button>
        )}
        <span className="text-[11px] font-mono text-white/85 tabular-nums">
          {showOrchestrator ? (
            <>
              <span className="text-emerald-300">{roundVerdictTotals.accepted}</span>
              <span className="text-dim">/{roundVerdictTotals.total}</span>
              <span className="text-muted ml-1">accepted</span>
            </>
          ) : (
            <>
              <span className="text-emerald-300">{acceptedRegex}</span>
              <span className="text-dim">/{results.length}</span>
              <span className="text-muted ml-1">accepted</span>
            </>
          )}
        </span>
        <button
          onClick={() => setShowPrompt(true)}
          className="flex items-center gap-1.5 text-[11px] px-2 py-1 rounded border border-line/60 text-muted hover:text-honey-300 hover:border-honey-500/40 transition-colors"
          title="View input prompt"
        >
          {I.doc({ size: 12 })}
          <span>Prompt</span>
          <span className="text-dim font-mono">{tokens}</span>
        </button>
      </header>

      <div className="p-4 space-y-4">
        {/* Per-model rows in a single bordered container */}
        <div className="rounded-lg border border-line bg-ink-850 overflow-hidden">
          {results.map((m, i) => {
            const base = baseLabel(m.model);
            const ownVerdicts = verdictsByModel[m.model] || [];
            const isDupGroup = (baseGroupCounts.get(base) ?? 0) > 1;
            // Show the "shared with first instance" tag only when this row
            // has no verdicts of its own AND a sibling in the same base
            // group does have verdicts — i.e. genuinely-collapsed legacy
            // data. New reviews give every row its own non-empty bucket
            // and skip this branch.
            const showSharedTag =
              isDupGroup && ownVerdicts.length === 0 && baseGroupHasBucket.has(base);
            return (
              <ModelResultRow
                key={`${m.model}-${i}`}
                m={m}
                verdicts={ownVerdicts}
                sharedBucketTag={showSharedTag}
                isBestFind={!showSharedTag && (bestFind?.models.has(m.model) ?? false)}
                bestFindVerdictId={
                  !showSharedTag && bestFind && bestFind.models.has(m.model)
                    ? bestFind.id
                    : undefined
                }
                onShowReply={() => setReply({ model: m.model, body: m.body })}
              />
            );
          })}
          {results.length === 0 && (
            <div className="px-3 py-4 text-[11.5px] text-dim italic text-center">
              No model results yet.
            </div>
          )}
        </div>

        {/* Verdict for this round */}
        <div
          className={`rounded-lg border overflow-hidden max-w-full px-4 py-2.5 flex items-center gap-2 ${
            verdictTone === "honey"
              ? "border-honey-500/30 bg-honey-500/5"
              : "border-emerald-500/25 bg-emerald-500/5"
          }`}
          aria-label={`Round ${idx} verdict: ${verdictText?.trim() || "No verdict"}`}
        >
          <span aria-hidden="true">
            {I.hexFill({
              size: 11,
              className: `flex-shrink-0 ${verdictTone === "honey" ? "text-honey-400" : "text-emerald-400"}`,
            })}
          </span>
          <span className="text-[11.5px] font-semibold uppercase tracking-wider text-white/85 flex-shrink-0 whitespace-nowrap">
            Round {idx} verdict
          </span>
          {verdictText?.trim() && (
            <>
              <span className="text-white/50 text-[11.5px] flex-shrink-0" aria-hidden="true">
                —
              </span>
              <span className="text-[12.5px] leading-relaxed text-white/85 min-w-0 break-words">
                {verdictText}
              </span>
            </>
          )}
        </div>
      </div>

      {/* Single ReplyModal instance shared across all rows in this round. */}
      <ReplyModal
        open={reply !== null}
        onClose={() => setReply(null)}
        model={reply?.model ?? ""}
        body={reply?.body ?? ""}
      />

      {/* Prompt modal */}
      <Modal
        open={showPrompt}
        onClose={() => setShowPrompt(false)}
        title={`Input prompt — R${idx}`}
        wide
      >
        <div className="flex items-center justify-between mb-3">
          <span className="text-[11.5px] font-medium text-muted">Input prompt</span>
          <span className="text-[11px] text-dim font-mono">{tokens} tokens</span>
        </div>
        <pre className="whitespace-pre-wrap font-mono text-[12px] leading-relaxed text-white/85 max-h-[60vh] overflow-auto bg-ink-900/60 border border-line/60 rounded p-3">
          {prompt}
        </pre>
      </Modal>
    </section>
  );
}

/* ── Mapping helpers ──────────────────────────────────────── */

function computeDuration(createdAt: string, completedAt: string | null): string {
  if (!completedAt) return "";
  try {
    const start = new Date(normalizeDateStr(createdAt)).getTime();
    const end = new Date(normalizeDateStr(completedAt)).getTime();
    const diffMs = end - start;
    if (isNaN(diffMs) || diffMs < 0) return "";
    if (diffMs < 1000) return `${diffMs}ms`;
    return `${(diffMs / 1000).toFixed(1)}s`;
  } catch { return ""; }
}

function parseVerdict(output: string): "approved" | "issues" {
  const issuesSection = /## Issues Found/i.test(output);
  const layerEntries = output.match(/- \*\*\[Layer/g);
  if (issuesSection && layerEntries && layerEntries.length > 0) return "issues";
  return "approved";
}

function parseIssues(output: string): Record<string, number> {
  const counts: Record<string, number> = {};
  const re = /\*\*\[Layer\s+\d+\s*[—–-]\s*(\w[\w\s]*?)\]/g;
  let match;
  while ((match = re.exec(output)) !== null) {
    const layer = match[1].trim();
    counts[layer] = (counts[layer] || 0) + 1;
  }
  return counts;
}

const reviewToRun = (r: ReviewSummary): ReviewRun => ({
  id: r.job_id,
  child_job_ids: r.child_job_ids,
  status: r.status === "completed" ? "ok" as const
    : r.status === "failed" ? "fail" as const
    // User-initiated cancellation is not the same as a provider failure.
    // Surface it under its own status so the row pill matches the
    // "Cancelled by user" pill in the live panel.
    : r.status === "cancelled" ? "cancelled" as const
    : r.status === "merge_interrupted" ? "interrupted" as const
    : ["pending"].includes(r.status) || r.status.startsWith("round") ? "running" as const
    : "issues" as const,
  duration: computeDuration(r.created_at, r.completed_at),
  models: r.num_models,
  rounds: r.num_rounds,
  date: formatRelativeDate(r.created_at),
  prompt: r.plan_preview,
  name: r.name,
  hivemind_id: normalizeHivemindId(r.hivemind_id),
  project_path: r.project_path && r.project_path.trim() ? r.project_path : null,
  created_at_raw: r.created_at,
  created_at_ts: reviewTimestamp(r.created_at),
});

/** Resolve the logical review run ID to pass to getReviewState.
 *  reviewToRun maps r.job_id = COALESCE(NULLIF(review_id, ''), id),
 *  which SHOULD already be the logical run ID. This helper is a
 *  defense-in-depth safety net for edge cases where selected.id
 *  is a child job ID instead of the logical run ID. */
function resolveReviewId(selected: ReviewRun): string {
  const id = typeof selected.id === "string" ? selected.id : String(selected.id);
  if (selected.child_job_ids && selected.child_job_ids.length > 0) {
    // Has child jobs, so selected.id should already be the logical run ID
    return id;
  }
  return id;
}

/* ── ReviewHistoryScreen ──────────────────────────────────── */

interface ReviewRun {
  id: number | string;
  child_job_ids: string[];
  status: ReviewRunStatus;
  duration: string;
  models: number;
  rounds: number;
  date: string;
  prompt: string;
  name?: string | null;
  hivemind_id: string | null;
  /** Absolute path of the project the review ran against, when known.
   *  `null` for legacy rows persisted before migration 0019. */
  project_path: string | null;
  created_at_raw: string;
  created_at_ts: number | null;
}

/* ── InterruptedMergeBanner ──────────────────────────────── */

/** Banner shown on the Results tab when the selected review's most-recent
 *  merge_run is `interrupted` (host process died mid-merge). Surfaces a
 *  partial-text preview and a button that navigates to the originating
 *  Tasks-view conversation so the user can resume the merge. */
function InterruptedMergeBanner({
  selected,
  interrupted,
  taskRuntime,
  go,
}: {
  selected: ReviewRun;
  interrupted: { round: number; outputLen: number; error: string | null; preview: string };
  taskRuntime: ReturnType<typeof useTaskRuntime>;
  go: GoFn;
}) {
  // Find the originating task by scanning all task runtimes for an
  // activeReviewJobId match. If multiple, pick the most-recently-touched
  // (first one we find with that field is good enough — order is stable).
  const tasks = taskRuntime?.tasks ?? {};
  const matchingTaskId = Object.entries(tasks)
    .find(([, t]) => t.activeReviewJobId === selected.id)?.[0] ?? null;

  // Truncate preview at ~1 KB for display.
  const PREVIEW_BYTES = 1024;
  const previewText = interrupted.preview.length > PREVIEW_BYTES
    ? interrupted.preview.slice(0, PREVIEW_BYTES) + "…"
    : interrupted.preview;

  const handleResume = () => {
    if (!matchingTaskId) return;
    taskRuntime.setActiveTask(matchingTaskId);
    go("tasks");
  };

  return (
    <div className="rounded-lg border border-red-500/30 bg-red-500/5 px-4 py-3">
      <div className="flex items-center gap-2">
        <span className="text-[13px] font-semibold text-red-300">
          Merge interrupted by host restart
        </span>
        <Pill tone="red">round {interrupted.round}</Pill>
        <span className="text-[11px] text-dim font-mono ml-auto">
          {interrupted.outputLen.toLocaleString()} bytes captured
        </span>
      </div>
      {interrupted.error && (
        <div className="text-[11.5px] text-red-300/80 mt-1">{interrupted.error}</div>
      )}
      {previewText && (
        <pre className="mt-2 max-h-40 overflow-auto rounded bg-ink-900/60 px-3 py-2 text-[11.5px] text-white/80 whitespace-pre-wrap">
          {previewText}
        </pre>
      )}
      <div className="mt-3 flex items-center gap-2">
        <Btn
          kind="primary"
          size="sm"
          onClick={handleResume}
          disabled={!matchingTaskId}
          title={
            matchingTaskId
              ? "Open the originating Task to resume the merge"
              : "Original task not found — re-trigger from Tasks view."
          }
        >
          Open in Tasks view to resume
        </Btn>
      </div>
    </div>
  );
}

/* ── OrchestratorBlock — context + merge session usage ──── */

function OrchestratorBlock({ usage, loading }: { usage: OrchestratorUsage; loading?: boolean }) {
  const [expanded, setExpanded] = useState(false);
  const phases = [
    usage.context_session && { label: "Context gathering", ...usage.context_session },
    ...usage.merge_sessions.map((m) => ({ label: `Merge R${m.round}`, ...m })),
  ].filter(Boolean) as (PhaseUsage & { label: string })[];

  return (
    <section
      className={`rounded-xl border border-line bg-ink-800/40 overflow-hidden ${
        loading ? "opacity-60" : ""
      }`}
    >
      <header
        className="flex items-center gap-2 px-4 h-10 border-b border-line bg-ink-850 cursor-pointer select-none"
        onClick={() => setExpanded(!expanded)}
      >
        <span className="w-2 h-2 rounded-full bg-emerald-400" />
        <span className="text-xs font-semibold tracking-wide text-ink-300 flex-1">
          Orchestrator Agent
        </span>
        <span className="font-mono text-xs text-ink-400">{usage.model_id}</span>
        <span className="text-xs text-ink-500 tabular-nums ml-3">
          {usage.total_input_tokens.toLocaleString()} in ·{" "}
          {usage.total_output_tokens.toLocaleString()} out
        </span>
        {I.chevD({ size: 14, className: `text-ink-500 ml-1 transition-transform ${expanded ? "rotate-180" : ""}` })}
      </header>
      {expanded && (
        <div className="px-4 py-2 space-y-1 text-xs">
          {phases.map((p, i) => (
            <div key={i} className="flex items-center gap-2">
              <span className="text-ink-500 w-32">{p.label}</span>
              <span className="text-ink-400 font-mono text-[10px]">{p.model_id}</span>
              <span className="text-ink-400 tabular-nums ml-auto">
                {p.input_tokens.toLocaleString()} in ·{" "}
                {p.output_tokens.toLocaleString()} out
              </span>
            </div>
          ))}
          {phases.length === 0 && (
            <span className="text-ink-500">No phase data available</span>
          )}
        </div>
      )}
    </section>
  );
}

/* ── ReplayModal ────────────────────────────────────────── */

function ReplayModal({
  open,
  onClose,
  reviewId,
  taskRuntime,
  go,
}: {
  open: boolean;
  onClose: () => void;
  reviewId: string;
  taskRuntime: ReturnType<typeof useTaskRuntime>;
  go: GoFn;
}) {
  const [selectedHivemind, setSelectedHivemind] = useState<string>("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generationRef = useRef(0);

  const hivemindOptions = taskRuntime.hivemindOptions;

  // Default to first hivemind when modal opens; reset stale selection
  useEffect(() => {
    if (open && hivemindOptions.length > 0) {
      const stillValid = hivemindOptions.some((h) => h.id === selectedHivemind);
      if (!stillValid) {
        setSelectedHivemind(hivemindOptions[0].id);
      }
    }
  }, [open, hivemindOptions]);

  // Reset state when modal closes
  useEffect(() => {
    if (!open) {
      setError(null);
      setLoading(false);
      generationRef.current++;
    }
  }, [open]);

  const handleReplay = async () => {
    if (!selectedHivemind) return;
    setLoading(true);
    setError(null);
    const gen = ++generationRef.current;
    try {
      const enrichedPrompt = await getReviewPlan(reviewId);
      if (gen !== generationRef.current) return;
      taskRuntime.replayReview({
        enrichedPrompt,
        hivemindId: selectedHivemind,
        projectPath: taskRuntime.defaultProjectPath || null,
      });
      onClose();
      go("tasks");
    } catch (e) {
      if (gen !== generationRef.current) return;
      setError(e instanceof Error ? e.message : String(e));
      setLoading(false);
    }
  };

  const noHiveminds = hivemindOptions.length === 0;

  return (
    <Modal open={open} onClose={onClose} title="Replay Review">
      <p className="text-[12.5px] text-muted mb-4">
        Re-run this review with a different Hivemind. The same enriched prompt
        (plan + source context) from the original review will be sent to the
        new Hivemind&apos;s models, skipping context gathering.
      </p>

      {noHiveminds ? (
        <div className="text-[12px] text-amber-400 mb-4">
          No hiveminds configured. Create one in Settings before replaying.
        </div>
      ) : (
        <>
          <label className="text-[11.5px] text-dim uppercase tracking-wider mb-1.5 block">
            Select Hivemind
          </label>
          <Select
            options={hivemindOptions.map((h) => ({ value: h.id, label: h.name }))}
            value={selectedHivemind}
            onChange={(e) => setSelectedHivemind(e.target.value)}
            wrapClass="mb-4"
          />
        </>
      )}

      {error && (
        <div className="text-[12px] text-red-400 mb-3">{error}</div>
      )}

      <div className="flex justify-end gap-2">
        <Btn kind="ghost" size="sm" onClick={onClose}>Cancel</Btn>
        <Btn
          kind="primary"
          size="sm"
          onClick={handleReplay}
          disabled={!selectedHivemind || loading || noHiveminds}
        >
          {loading ? "Starting..." : "Replay"}
        </Btn>
      </div>
    </Modal>
  );
}

export function ReviewHistoryScreen({ go, hivemind: _hivemindProp }: { go: GoFn; hivemind?: any }) {
  // _hivemindProp kept for back-compat with App.tsx nav params; no longer used now
  // that the History view is global.
  const [runs, setRuns] = useState<ReviewRun[]>(isTauri() ? [] : REVIEW_RUNS);
  const [selected, setSelected] = useState<ReviewRun | null>(isTauri() ? null : REVIEW_RUNS[0]);
  const [tab, setTab] = useState<"results" | "metrics" | "timeline">("results");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [liveState, setLiveState] = useState<ReviewStateSnapshot | null>(null);
  const [verdicts, setVerdicts] = useState<RoundVerdict[]>([]);
  const [detailLoading, setDetailLoading] = useState(false);
  const [filterQuery, setFilterQuery] = useState("");
  const [openDropdown, setOpenDropdown] = useState<"filter" | "project" | "sort" | null>(null);
  const [hasLoadedReviewList, setHasLoadedReviewList] = useState(!isTauri());
  const [replayOpen, setReplayOpen] = useState(false);

  const [hivemindFilter, setHivemindFilter] = useState<string>(() => {
    try {
      const raw = localStorage.getItem(REVIEW_HIVEMIND_FILTER_STORAGE_KEY)?.trim();
      return raw || REVIEW_HIVEMIND_FILTER_ALL;
    } catch {
      return REVIEW_HIVEMIND_FILTER_ALL;
    }
  });

  // Project filter — value is either REVIEW_PROJECT_FILTER_ALL,
  // REVIEW_PROJECT_FILTER_NONE (matches rows with no stored project_path),
  // or an absolute project path string.
  const [projectFilter, setProjectFilter] = useState<string>(() => {
    try {
      const raw = localStorage.getItem(REVIEW_PROJECT_FILTER_STORAGE_KEY)?.trim();
      return raw || REVIEW_PROJECT_FILTER_ALL;
    } catch {
      return REVIEW_PROJECT_FILTER_ALL;
    }
  });

  const [sortMode, setSortMode] = useState<ReviewSortMode>(() => {
    try {
      const raw = localStorage.getItem(REVIEW_SORT_STORAGE_KEY);
      return raw && (REVIEW_SORT_MODES as readonly string[]).includes(raw)
        ? (raw as ReviewSortMode)
        : "newest";
    } catch {
      return "newest";
    }
  });

  // Orchestrator agent usage (context + merge sessions)
  const MOCK_ORCHESTRATOR: OrchestratorUsage = {
    model_id: "claude-sonnet-4",
    provider: "anthropic",
    total_input_tokens: 12840,
    total_output_tokens: 3210,
    total_cost: 0.087,
    total_duration_ms: 22400,
    context_session: { round: null, session_id: "mock-ctx-sid", model_id: "claude-sonnet-4", provider: "anthropic", input_tokens: 8200, output_tokens: 1880 },
    merge_sessions: [
      { round: 1, session_id: "mock-mr-sid-r1", model_id: "claude-sonnet-4", provider: "anthropic", input_tokens: 4640, output_tokens: 1330 },
      { round: 2, session_id: "mock-mr-sid-r2", model_id: "claude-sonnet-4", provider: "anthropic", input_tokens: 4200, output_tokens: 1180 },
    ],
  };
  const [orchestratorUsage, setOrchestratorUsage] = useState<OrchestratorUsage | null>(
    isTauri() ? null : MOCK_ORCHESTRATOR
  );
  const [orchestratorLoading, setOrchestratorLoading] = useState(false);

  // Crash-recovery: when the selected review's merge_run is in `interrupted`
  // status, surface a red banner with a partial-text preview and a "Open in
  // Tasks view to resume" button.
  const [interruptedMerge, setInterruptedMerge] = useState<{
    round: number;
    outputLen: number;
    error: string | null;
    preview: string;
  } | null>(null);

  // Pull task runtime context so we can find the originating task and
  // navigate the user there for resume.
  const taskRuntime = useTaskRuntime();

  const hivemindNames = useMemo(() => {
    const m = new Map<string, string>();
    for (const hm of taskRuntime.hivemindOptions || []) {
      m.set(hm.id, hm.name);
    }
    return m;
  }, [taskRuntime.hivemindOptions]);

  const hivemindLabel = useCallback(
    (id: string) => hivemindNames.get(id) || id,
    [hivemindNames],
  );

  useEffect(() => {
    try { localStorage.setItem(REVIEW_HIVEMIND_FILTER_STORAGE_KEY, hivemindFilter); } catch {}
  }, [hivemindFilter]);

  useEffect(() => {
    try { localStorage.setItem(REVIEW_PROJECT_FILTER_STORAGE_KEY, projectFilter); } catch {}
  }, [projectFilter]);

  useEffect(() => {
    try { localStorage.setItem(REVIEW_SORT_STORAGE_KEY, sortMode); } catch {}
  }, [sortMode]);

  const handleDropdownKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      setOpenDropdown(null);
    }
  }, []);

  const selectedIdRef = useRef<string | null>(null);
  const selectedChildIds = useRef<Set<string>>(new Set());
  const selectedRoundsRef = useRef(0);
  const prevSelectedKey = useRef<string | undefined>();
  const selectedStringId = selected && typeof selected.id === "string" ? selected.id : undefined;
  const selectedKey = selectedStringId ? `${selectedStringId}:${(selected?.child_job_ids ?? []).join("|")}` : undefined;
  if (selectedKey !== prevSelectedKey.current) {
    prevSelectedKey.current = selectedKey;
    selectedIdRef.current = selectedStringId ?? null;
    selectedChildIds.current = new Set(selected?.child_job_ids ?? []);
    selectedRoundsRef.current = selected?.rounds ?? 0;
  }

  const refreshGenRef = useRef(0);
  const listRefreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const detailRefreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refreshSelected = useCallback(async (jobId: string) => {
    const gen = ++refreshGenRef.current;
    setDetailLoading(true);

    // Determine if this is an hmr- review for orchestrator usage fetch.
    const reviewId = jobId.startsWith("hmr-") ? jobId : null;

    if (isTauri()) {
      setOrchestratorUsage(null);
      setOrchestratorLoading(Boolean(reviewId));
    }

    const fetches: Promise<any>[] = [
      getReviewState(jobId),
      listRoundVerdicts(jobId).catch((e) => {
        console.warn("[REVIEW_DEBUG] listRoundVerdicts failed for job", jobId, "(continuing without)", e);
        return [] as RoundVerdict[];
      }),
    ];

    if (reviewId && isTauri()) {
      const orchFetch = getOrchestratorUsage(reviewId)
        .then((data) => {
          if (gen === refreshGenRef.current) setOrchestratorUsage(data);
        })
        .catch((err) => {
          console.error("[review-history] getOrchestratorUsage failed:", err);
          if (gen === refreshGenRef.current) setOrchestratorUsage(null);
        })
        .finally(() => {
          if (gen === refreshGenRef.current) setOrchestratorLoading(false);
        });
      fetches.push(orchFetch);
    }

    try {
      const [snap, vs] = await Promise.all(fetches);
      if (gen !== refreshGenRef.current) return; // stale — discard
      setLiveState(snap);

      // Warn if live data is incomplete (fewer rounds than expected).
      if (snap && selectedRoundsRef.current > 0 && snap.total_rounds < selectedRoundsRef.current) {
        console.warn(
          `[REVIEW_DEBUG] liveState.total_rounds (${snap.total_rounds}) < selected.rounds (${selectedRoundsRef.current}). ` +
          `Steps: ${snap.steps.length}, round_numbers: [${snap.steps.map((s: { round_number: number }) => s.round_number).join(',')}]`
        );
      }

      setVerdicts(vs);
    } catch (e) {
      if (gen !== refreshGenRef.current) return; // stale — discard
      console.error("[REVIEW_DEBUG] getReviewState failed:", e);
      setLiveState(null);
      setVerdicts([]);
    } finally {
      if (gen === refreshGenRef.current) {
        setDetailLoading(false);
      }
    }
  }, []);

  const refreshList = useCallback(async () => {
    const response = await listReviews(50, 0);
    const mapped = response.reviews.map(reviewToRun);
    setRuns(mapped);
    setHasLoadedReviewList(true);
    setSelected((prev) => {
      if (!prev) return mapped[0] ?? null;
      const next = mapped.find((r) => r.id === prev.id);
      return next ?? prev;
    });
  }, []);

  // Load review list from backend when in Tauri mode, poll while any are running.
  // Global scope — no per-hivemind filter.
  useEffect(() => {
    if (!isTauri()) return;
    let timer: ReturnType<typeof setTimeout>;
    let mounted = true;
    const load = (isInitial: boolean) => {
      if (isInitial) { setLoading(true); setError(null); }
      listReviews(50, 0)
        .then((response) => {
          if (!mounted) return;
          const mapped = response.reviews.map(reviewToRun);
          setRuns(mapped);
          if (isInitial) setSelected(null);
          setLiveState(null);
          setVerdicts([]);
          if (isInitial && mapped.length > 0) setSelected(mapped[0]);
          if (mapped.some(r => r.status === "running")) {
            timer = setTimeout(() => load(false), 5000);
          }
        })
        .catch((err) => {
          if (!mounted) return;
          console.error("Failed to load reviews:", err);
          if (isInitial) setError(err instanceof Error ? err.message : String(err));
        })
        .finally(() => {
          if (isInitial && mounted) {
            setHasLoadedReviewList(true);
            setLoading(false);
          }
        });
    };
    load(true);
    return () => { mounted = false; clearTimeout(timer); };
  }, []);

  // Mount + selection-change resync (no polling).
  useEffect(() => {
    if (!isTauri() || !selected || typeof selected.id !== "string") {
      setLiveState(null);
      setVerdicts([]);
      setInterruptedMerge(null);
      return;
    }
    const reviewId = resolveReviewId(selected);
    refreshSelected(reviewId);
  }, [selected?.id, refreshSelected]);

  // Probe the merge_runs table whenever the selected review changes (or its
  // status flips to "interrupted"). For interrupted runs, also fetch the
  // partial merge text so the banner can show a preview.
  useEffect(() => {
    if (!isTauri() || !selected || typeof selected.id !== "string") {
      setInterruptedMerge(null);
      return;
    }
    if (selected.status !== "interrupted") {
      setInterruptedMerge(null);
      return;
    }
    // TODO: replace this probe with a new `get_review_artifacts` IPC that
    // reads the per-round merge artifacts from
    // `~/.hyvemind/reviews/{review_id}/merge-r{N}.txt`. The backend's
    // `get_merge_run` is retained for now so the recovery banner keeps
    // working on legacy interrupted reviews.
    let cancelled = false;
    const jobId = selected.id;
    (async () => {
      const MAX_ROUNDS_PROBE = 10;
      let bestRound: number | null = null;
      let bestErr: string | null = null;
      let bestLen = 0;
      for (let r = 1; r <= MAX_ROUNDS_PROBE; r++) {
        try {
          const run = await getMergeRun({ jobId, round: r });
          if (run && run.status === "interrupted") {
            if (bestRound === null || r > bestRound) {
              bestRound = r;
              bestErr = run.error;
              bestLen = run.output_len;
            }
          }
        } catch {
          break;
        }
      }
      if (cancelled) return;
      if (bestRound === null) {
        setInterruptedMerge(null);
        return;
      }
      let preview = "";
      try {
        const text = await readMergeOutput({ jobId, round: bestRound });
        preview = text || "";
      } catch {
        preview = "";
      }
      if (cancelled) return;
      setInterruptedMerge({
        round: bestRound,
        outputLen: bestLen,
        error: bestErr,
        preview,
      });
    })();
    return () => {
      cancelled = true;
    };
  }, [selected?.id, selected?.status]);

  // Window-focus resync: re-fetch the currently selected review's state.
  useEffect(() => {
    if (!isTauri()) return;
    const onFocus = () => {
      const id = selectedIdRef.current;
      if (id) refreshSelected(id);
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshSelected]);

  // Listen for "review-deleted" custom event dispatched by ContextMenu
  useEffect(() => {
    const handler = () => { refreshList(); };
    window.addEventListener("review-deleted", handler);
    return () => window.removeEventListener("review-deleted", handler);
  }, [refreshList]);

  // Listen for real-time hivemind progress events (registered once on mount).
  // Subscribes through the singleton `hivemindEventStore` so all consumers
  // share a single underlying Tauri `listen()` call.
  useEffect(() => {
    if (!isTauri()) return;
    let mounted = true;
    const scheduleListRefresh = () => {
      if (listRefreshTimer.current) clearTimeout(listRefreshTimer.current);
      listRefreshTimer.current = setTimeout(() => {
        listRefreshTimer.current = null;
        refreshList().catch((e) => console.error("Failed to refresh reviews", e));
      }, 1000);
    };
    const scheduleDetailRefresh = (jobId: string) => {
      if (detailRefreshTimer.current) clearTimeout(detailRefreshTimer.current);
      detailRefreshTimer.current = setTimeout(() => {
        detailRefreshTimer.current = null;
        refreshSelected(jobId);
      }, 1000);
    };
    const clearTimers = () => {
      if (listRefreshTimer.current) clearTimeout(listRefreshTimer.current);
      if (detailRefreshTimer.current) clearTimeout(detailRefreshTimer.current);
      listRefreshTimer.current = null;
      detailRefreshTimer.current = null;
    };
    const unsubscribe = subscribeHivemindEventListener((evt: HivemindProgressEvent) => {
      if (!mounted || evt.event_type === "model_chunk") return;
      const selectedId = selectedIdRef.current;
      const belongsToSelected =
        selectedChildIds.current.has(evt.job_id) ||
        (evt.review_id != null && evt.review_id === selectedId);
      const isTerminal = ["completed", "failed", "cancelled", "error"].includes(evt.event_type);

      // ── Crash-recovery sweep emit ──
      // Backend emits `merge_interrupted` once per orphan merge_run at boot.
      // Refresh the row's status badge and (if selected) the detail pane so
      // the "Resume merge" banner appears immediately.
      if (evt.event_type === "merge_interrupted") {
        setRuns((prev) =>
          prev.map((r) =>
            r.child_job_ids.includes(evt.job_id) ||
            r.id === evt.job_id ||
            r.id === evt.review_id
              ? { ...r, status: "interrupted" as const }
              : r,
          ),
        );
        clearTimers();
        refreshList().catch((e) => console.error("Failed to refresh reviews", e));
        if (belongsToSelected && selectedId) refreshSelected(selectedId);
        return;
      }

      setRuns((prev) =>
        prev.map((r) =>
          r.child_job_ids.includes(evt.job_id) || r.id === evt.job_id || r.id === evt.review_id
            ? {
                ...r,
                status:
                  evt.event_type === "failed" || evt.event_type === "error" || evt.event_type === "cancelled"
                    ? ("fail" as const)
                    : evt.event_type === "completed"
                    ? r.status // let the next refresh decide pass/issues
                    : ("running" as const),
              }
            : r
        )
      );

      if (isTerminal) {
        clearTimers();
        refreshList().catch((e) => console.error("Failed to refresh reviews", e));
        if (belongsToSelected && selectedId) refreshSelected(selectedId);
        return;
      }

      if (["step_started", "step_completed", "model_completed", "round_completed", "round_started", "model_failed", "verdicts_updated"].includes(evt.event_type)) {
        scheduleListRefresh();
        if (belongsToSelected && selectedId) scheduleDetailRefresh(selectedId);
      }
    });
    return () => {
      mounted = false;
      clearTimers();
      unsubscribe();
    };
  }, [refreshList, refreshSelected]);

  // Bucket verdicts by (round_number, reviewer_model). The reviewer_model is
  // the orchestrator-emitted "provider/model_id" form, which matches the join
  // key produced below for live rows (`${step.provider}/${step.model_id}`).
  const verdictsByRound = useMemo(() => {
    const out: Record<number, Record<string, RoundVerdict[]>> = {};
    for (const v of verdicts) {
      const round = (out[v.round_number] = out[v.round_number] || {});
      const list = (round[v.reviewer_model] = round[v.reviewer_model] || []);
      list.push(v);
    }
    return out;
  }, [verdicts]);

  const hivemindFilterOptions = useMemo(() => {
    const ids = new Set<string>();
    for (const r of runs) {
      if (r.hivemind_id) ids.add(r.hivemind_id);
    }
    return Array.from(ids).sort((a, b) =>
      hivemindLabel(a).localeCompare(hivemindLabel(b))
    );
  }, [runs, hivemindLabel]);

  // Distinct project paths across loaded runs + a `hasNone` flag so the
  // dropdown can offer a "(no project)" bucket for legacy/pre-0019 rows.
  const projectFilterOptions = useMemo(() => {
    const paths = new Set<string>();
    let hasNone = false;
    for (const r of runs) {
      if (r.project_path) paths.add(r.project_path);
      else hasNone = true;
    }
    const sorted = Array.from(paths).sort((a, b) =>
      projectLabel(a).localeCompare(projectLabel(b)),
    );
    return { paths: sorted, hasNone };
  }, [runs]);

  const mergeSessionByRound = useMemo(() => {
    const out = new Map<number, PhaseUsage>();

    for (const session of orchestratorUsage?.merge_sessions ?? []) {
      if (typeof session.round !== "number") continue;

      const existing = out.get(session.round);
      if (!existing) {
        out.set(session.round, session);
        continue;
      }

      const existingRenderable = Boolean(
        existing.model_id.trim() && existing.session_id.trim()
      );
      const candidateRenderable = Boolean(
        session.model_id.trim() && session.session_id.trim()
      );

      // Preserve backend order, but if the first duplicate is incomplete and a
      // later duplicate is copyable/displayable, prefer the usable entry.
      if (!existingRenderable && candidateRenderable) {
        out.set(session.round, session);
      }
    }

    return out;
  }, [orchestratorUsage]);

  const filteredRuns = useMemo(() => {
    let result = runs;

    if (hivemindFilter !== REVIEW_HIVEMIND_FILTER_ALL) {
      result = result.filter((r) => r.hivemind_id === hivemindFilter);
    }

    if (projectFilter === REVIEW_PROJECT_FILTER_NONE) {
      result = result.filter((r) => !r.project_path);
    } else if (projectFilter !== REVIEW_PROJECT_FILTER_ALL) {
      result = result.filter((r) => r.project_path === projectFilter);
    }

    const q = filterQuery.trim().toLowerCase();
    if (q) {
      result = result.filter((r) => {
        const id = String(r.id).toLowerCase();
        const hm = r.hivemind_id ? hivemindLabel(r.hivemind_id).toLowerCase() : "";
        return (
          id.includes(q) ||
          (r.name && r.name.toLowerCase().includes(q)) ||
          r.prompt.toLowerCase().includes(q) ||
          r.status.includes(q) ||
          r.date.toLowerCase().includes(q) ||
          hm.includes(q)
        );
      });
    }

    const sorted = [...result];

    if (sortMode === "newest") {
      sorted.sort((a, b) =>
        compareReviewCreatedAt(a, b, "desc") ||
        compareReviewIdValues(a.id, b.id, "desc")
      );
    } else if (sortMode === "oldest") {
      sorted.sort((a, b) =>
        compareReviewCreatedAt(a, b, "asc") ||
        compareReviewIdValues(a.id, b.id, "asc")
      );
    } else {
      sorted.sort((a, b) =>
        ((REVIEW_STATUS_ORDER[a.status] ?? 99) - (REVIEW_STATUS_ORDER[b.status] ?? 99)) ||
        compareReviewCreatedAt(a, b, "desc") ||
        compareReviewIdValues(a.id, b.id, "desc")
      );
    }

    return sorted;
  }, [runs, filterQuery, hivemindFilter, projectFilter, sortMode, hivemindLabel]);

  useEffect(() => {
    if (!hasLoadedReviewList || loading) return;
    if (hivemindFilter === REVIEW_HIVEMIND_FILTER_ALL) {
      return;
    }

    const exists = runs.some((r) => r.hivemind_id === hivemindFilter);
    if (!exists) setHivemindFilter(REVIEW_HIVEMIND_FILTER_ALL);
  }, [runs, hivemindFilter, loading, hasLoadedReviewList]);

  // If the persisted project filter no longer matches any loaded run, fall
  // back to "All Projects" so the list isn't silently empty.
  useEffect(() => {
    if (!hasLoadedReviewList || loading) return;
    if (projectFilter === REVIEW_PROJECT_FILTER_ALL) return;
    if (projectFilter === REVIEW_PROJECT_FILTER_NONE) {
      if (!projectFilterOptions.hasNone) setProjectFilter(REVIEW_PROJECT_FILTER_ALL);
      return;
    }
    if (!projectFilterOptions.paths.includes(projectFilter)) {
      setProjectFilter(REVIEW_PROJECT_FILTER_ALL);
    }
  }, [projectFilter, projectFilterOptions, loading, hasLoadedReviewList]);

  // Subset of RoundResult used only for the run-header accepted/total tally.
  // Does not carry `failed`/`pending` because it is derived at component level,
  // not passed to ModelResultRow.
  const liveRoundResults: { verdict: "approved" | "issues" }[] = liveState
    ? liveState.steps.map((step) => ({
        verdict:
          step.status === "completed"
            ? parseVerdict(step.output || "")
            : ("issues" as const),
      }))
    : [];

  const liveCost = liveState ? `$${liveState.total_cost.toFixed(3)}` : null;

  // Compute header stats — prefer orchestrator verdicts when present.
  const totalVerdicts = verdicts.length;
  const acceptedVerdicts = verdicts.filter((v) => v.verdict === "accepted").length;
  const liveAccepted = liveRoundResults.filter((r) => r.verdict === "approved").length;
  const liveTotal = liveRoundResults.length;
  const acceptedR1 = liveState
    ? totalVerdicts > 0
      ? acceptedVerdicts
      : liveAccepted
    : ROUND1_RESULTS.filter((r) => r.verdict === "approved").length;
  const totalR1 = liveState
    ? totalVerdicts > 0
      ? totalVerdicts
      : liveTotal
    : ROUND1_RESULTS.length;
  const rejectedR1 = totalR1 - acceptedR1;  // non-accepted = rejected + modified (rare); "Rejected" is the best aggregate label

  return (
    <div className="h-full flex flex-col">
      {/* Body */}
      <div className="flex-1 min-h-0 grid grid-cols-[420px_1fr] overflow-hidden">
        {/* Run list */}
        <div className="border-r border-line overflow-auto">
          <div className="px-4 py-3 sticky top-0 bg-ink-900/95 border-b border-line z-20 overflow-visible">
            <div className="flex items-center gap-2">
              <Input
                icon={I.search({ size: 13 })}
                placeholder="Filter loaded runs..."
                wrapClass="flex-1"
                value={filterQuery}
                onChange={(e) => setFilterQuery(e.target.value)}
              />
            </div>

            <div className="pt-2 flex items-center justify-between gap-1.5">
              <div className="relative">
                <button
                  onClick={() => setOpenDropdown((o) => o === "filter" ? null : "filter")}
                  aria-haspopup="menu"
                  aria-expanded={openDropdown === "filter"}
                  className={`h-6 px-2 rounded-md flex items-center gap-1.5 text-[10.5px] font-medium transition-colors ${
                    hivemindFilter !== REVIEW_HIVEMIND_FILTER_ALL
                      ? "text-honey-300 bg-honey-500/10 hover:bg-honey-500/15"
                      : "text-dim hover:text-white hover:bg-ink-700/60"
                  }`}
                >
                  {I.filter({ size: 11 })}
                  <span className="truncate max-w-[140px]">
                    {hivemindFilter === REVIEW_HIVEMIND_FILTER_ALL
                      ? "All Hiveminds"
                      : hivemindLabel(hivemindFilter)}
                  </span>
                  {I.chevD({ size: 10 })}
                </button>

                {openDropdown === "filter" && (
                  <>
                    <div className="fixed inset-0 z-40" onClick={() => setOpenDropdown(null)} />
                    <div
                      role="menu"
                      aria-label="Filter reviews by hivemind"
                      onKeyDown={handleDropdownKeyDown}
                      className="absolute top-full left-0 mt-1 z-50 w-52 bg-ink-800 border border-line rounded-lg shadow-2xl max-h-80 overflow-y-auto py-1"
                    >
                      <button
                        role="menuitemradio"
                        aria-checked={hivemindFilter === REVIEW_HIVEMIND_FILTER_ALL}
                        onClick={() => { setHivemindFilter(REVIEW_HIVEMIND_FILTER_ALL); setOpenDropdown(null); }}
                        className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${hivemindFilter === REVIEW_HIVEMIND_FILTER_ALL ? "text-honey-300" : "text-white/80"}`}
                      >
                        {hivemindFilter === REVIEW_HIVEMIND_FILTER_ALL && I.check({ size: 11, className: "text-honey-400" })}
                        <span className={hivemindFilter === REVIEW_HIVEMIND_FILTER_ALL ? "" : "pl-[19px]"}>All Hiveminds</span>
                      </button>



                      {hivemindFilterOptions.length > 0 && <div className="border-t border-line my-1" />}

                      {hivemindFilterOptions.map((id) => (
                        <button
                          key={id}
                          role="menuitemradio"
                          aria-checked={hivemindFilter === id}
                          onClick={() => { setHivemindFilter(id); setOpenDropdown(null); }}
                          className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${hivemindFilter === id ? "text-honey-300" : "text-white/80"}`}
                        >
                          {hivemindFilter === id && I.check({ size: 11, className: "text-honey-400" })}
                          <span className={`flex items-center gap-1.5 ${hivemindFilter === id ? "" : "pl-[19px]"}`}>
                            {I.hexFill({ size: 10, className: "text-honey-500/60 shrink-0" })}
                            <span className="truncate">{hivemindLabel(id)}</span>
                          </span>
                        </button>
                      ))}
                    </div>
                  </>
                )}
              </div>

              <div className="relative">
                <button
                  onClick={() => setOpenDropdown((o) => o === "project" ? null : "project")}
                  aria-haspopup="menu"
                  aria-expanded={openDropdown === "project"}
                  className={`h-6 px-2 rounded-md flex items-center gap-1.5 text-[10.5px] font-medium transition-colors ${
                    projectFilter !== REVIEW_PROJECT_FILTER_ALL
                      ? "text-honey-300 bg-honey-500/10 hover:bg-honey-500/15"
                      : "text-dim hover:text-white hover:bg-ink-700/60"
                  }`}
                >
                  {I.folder({ size: 11 })}
                  <span className="truncate max-w-[140px]">
                    {projectFilter === REVIEW_PROJECT_FILTER_ALL
                      ? "All Projects"
                      : projectFilter === REVIEW_PROJECT_FILTER_NONE
                        ? "(no project)"
                        : projectLabel(projectFilter)}
                  </span>
                  {I.chevD({ size: 10 })}
                </button>

                {openDropdown === "project" && (
                  <>
                    <div className="fixed inset-0 z-40" onClick={() => setOpenDropdown(null)} />
                    <div
                      role="menu"
                      aria-label="Filter reviews by project"
                      onKeyDown={handleDropdownKeyDown}
                      className="absolute top-full left-0 mt-1 z-50 w-64 bg-ink-800 border border-line rounded-lg shadow-2xl max-h-80 overflow-y-auto py-1"
                    >
                      <button
                        role="menuitemradio"
                        aria-checked={projectFilter === REVIEW_PROJECT_FILTER_ALL}
                        onClick={() => { setProjectFilter(REVIEW_PROJECT_FILTER_ALL); setOpenDropdown(null); }}
                        className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${projectFilter === REVIEW_PROJECT_FILTER_ALL ? "text-honey-300" : "text-white/80"}`}
                      >
                        {projectFilter === REVIEW_PROJECT_FILTER_ALL && I.check({ size: 11, className: "text-honey-400" })}
                        <span className={projectFilter === REVIEW_PROJECT_FILTER_ALL ? "" : "pl-[19px]"}>All Projects</span>
                      </button>

                      {(projectFilterOptions.paths.length > 0 || projectFilterOptions.hasNone) && (
                        <div className="border-t border-line my-1" />
                      )}

                      {projectFilterOptions.paths.map((p) => (
                        <button
                          key={p}
                          role="menuitemradio"
                          aria-checked={projectFilter === p}
                          onClick={() => { setProjectFilter(p); setOpenDropdown(null); }}
                          title={p}
                          className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${projectFilter === p ? "text-honey-300" : "text-white/80"}`}
                        >
                          {projectFilter === p && I.check({ size: 11, className: "text-honey-400" })}
                          <span className={`flex items-center gap-1.5 min-w-0 ${projectFilter === p ? "" : "pl-[19px]"}`}>
                            {I.folder({ size: 10, className: "text-honey-500/60 shrink-0" })}
                            <span className="truncate">{projectLabel(p)}</span>
                          </span>
                        </button>
                      ))}

                      {projectFilterOptions.hasNone && (
                        <button
                          role="menuitemradio"
                          aria-checked={projectFilter === REVIEW_PROJECT_FILTER_NONE}
                          onClick={() => { setProjectFilter(REVIEW_PROJECT_FILTER_NONE); setOpenDropdown(null); }}
                          className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${projectFilter === REVIEW_PROJECT_FILTER_NONE ? "text-honey-300" : "text-white/80"}`}
                        >
                          {projectFilter === REVIEW_PROJECT_FILTER_NONE && I.check({ size: 11, className: "text-honey-400" })}
                          <span className={`flex items-center gap-1.5 ${projectFilter === REVIEW_PROJECT_FILTER_NONE ? "" : "pl-[19px]"} italic text-dim`}>
                            (no project)
                          </span>
                        </button>
                      )}
                    </div>
                  </>
                )}
              </div>

              <div className="relative">
                <button
                  onClick={() => setOpenDropdown((o) => o === "sort" ? null : "sort")}
                  aria-haspopup="menu"
                  aria-expanded={openDropdown === "sort"}
                  className="h-6 px-2 rounded-md flex items-center gap-1.5 text-[10.5px] font-medium text-dim hover:text-white hover:bg-ink-700/60 transition-colors"
                >
                  {I.sort({ size: 11 })}
                  <span>{sortMode === "newest" ? "New" : sortMode === "oldest" ? "Old" : "Status"}</span>
                </button>

                {openDropdown === "sort" && (
                  <>
                    <div className="fixed inset-0 z-40" onClick={() => setOpenDropdown(null)} />
                    <div
                      role="menu"
                      aria-label="Sort reviews"
                      onKeyDown={handleDropdownKeyDown}
                      className="absolute top-full right-0 mt-1 z-50 w-36 bg-ink-800 border border-line rounded-lg shadow-2xl overflow-hidden py-1"
                    >
                      {REVIEW_SORT_MODES.map((mode) => (
                        <button
                          key={mode}
                          role="menuitemradio"
                          aria-checked={sortMode === mode}
                          onClick={() => { setSortMode(mode); setOpenDropdown(null); }}
                          className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${sortMode === mode ? "text-honey-300" : "text-white/80"}`}
                        >
                          {sortMode === mode && I.check({ size: 11, className: "text-honey-400" })}
                          <span className={sortMode === mode ? "" : "pl-[19px]"}>
                            {mode === "newest" ? "Newest first" : mode === "oldest" ? "Oldest first" : "By status"}
                          </span>
                        </button>
                      ))}
                    </div>
                  </>
                )}
              </div>
            </div>
          </div>
          <div className="divide-y divide-line/60">
            {loading && (
              <div className="px-4 py-6 text-center text-[12px] text-muted">Loading reviews...</div>
            )}
            {error && (
              <div className="px-4 py-4 text-center text-[12px] text-red-400 flex items-center justify-center gap-1.5">
                {I.x({ size: 13, className: "text-red-400" })}
                {error}
              </div>
            )}
            {!loading && !error && filteredRuns.length === 0 && (
              <div className="px-4 py-10 text-center">
                <div className="text-[13px] text-muted">
                  {runs.length === 0 ? "No reviews found" : "No matching reviews"}
                </div>
                <div className="text-[11px] text-dim mt-1">
                  {runs.length === 0
                    ? "Reviews will appear here after they complete."
                    : "Try changing the filter or search."}
                </div>
              </div>
            )}
            {filteredRuns.map((r) => {
              const sel = selected?.id === r.id;
              return (
                <button
                  key={r.id}
                  data-ctx-review-id={String(r.id)}
                  onClick={() => setSelected(r)}
                  className={`w-full text-left px-4 py-3 ${
                    sel
                      ? "bg-honey-500/15 border-l-2 border-honey-500"
                      : "hover:bg-ink-800/50 border-l-2 border-transparent"
                  }`}
                >
                  <div className="flex items-center gap-2.5">
                    <span className="font-mono text-[11px] text-dim">#{r.id}</span>
                    {r.status === "ok" && <Pill tone="green">clean</Pill>}
                    {r.status === "issues" && <Pill tone="honey">issues</Pill>}
                    {r.status === "fail" && <Pill tone="red">failed</Pill>}
                    {/* Neutral tone reads as user intent, not error — matches
                        the live panel's "Cancelled by user" pill. */}
                    {r.status === "cancelled" && <Pill tone="neutral">cancelled</Pill>}
                    {r.status === "running" && <Pill tone="blue">running</Pill>}
                    {r.status === "interrupted" && <Pill tone="red">interrupted</Pill>}
                    <span className="text-[11px] text-dim font-mono ml-auto">
                      {r.duration}
                    </span>
                    <span className="text-[11px] text-dim">{r.date}</span>
                  </div>
                  <div className="text-[12px] text-white/80 mt-1.5 truncate font-medium">{r.name || r.prompt}</div>
                  {r.name && (
                    <div className="text-[10.5px] text-dim mt-0.5 truncate">{r.prompt}</div>
                  )}
                  <div className="flex items-center justify-between mt-1">
                    <div className="text-[10.5px] text-dim font-mono">
                      {r.models} models · {r.rounds} round{r.rounds === 1 ? "" : "s"}
                    </div>
                    {r.hivemind_id && (
                      <div className="flex items-center gap-1 truncate text-[10px] text-dim">
                        {I.hexFill({ size: 9, className: "text-honey-500/50 shrink-0" })}
                        <span className="truncate max-w-[120px]">{hivemindLabel(r.hivemind_id)}</span>
                      </div>
                    )}
                  </div>
                </button>
              );
            })}
          </div>
        </div>

        {/* Detail */}
        <div className="overflow-auto">
          <div className="px-7 py-5">
            {!selected ? (
              <div className="flex flex-col items-center justify-center py-20 text-center">
                <div className="text-[14px] text-muted">No review selected</div>
                <div className="text-[12px] text-dim mt-1">Select a review from the sidebar or run a new one.</div>
              </div>
            ) : (<>
            {/* Concise run header */}
            <div className="flex items-center gap-3 flex-wrap">
              <h2 className="text-[18px] font-semibold">Run {selected.id}</h2>
              <span className="font-mono text-[12.5px] text-white/85 tabular-nums">
                <span className="text-emerald-300">{acceptedR1}</span>
                <span className="text-dim"> Accepted</span>
                {rejectedR1 > 0 && (
                  <>
                    <span className="text-rose-400"> {rejectedR1}</span>
                    <span className="text-dim"> Rejected</span>
                  </>
                )}
              </span>
              {(selected.duration || liveCost) && <span className="text-dim">·</span>}
              {selected.duration && <span className="text-[12px] text-muted font-mono">{selected.duration}</span>}
              {liveCost && <>
                <span className="text-dim">·</span>
                <span className="text-[12px] text-honey-300 font-mono">{liveCost}</span>
              </>}
              <span className="text-dim">·</span>
              <span className="text-[12px] text-muted">{selected.date}</span>
              {isTauri() && (
                <Btn
                  kind="ghost"
                  size="sm"
                  icon={I.play({ size: 12 })}
                  onClick={() => setReplayOpen(true)}
                  disabled={selected.status === "running"}
                >
                  Replay
                </Btn>
              )}
            </div>

            {/* Tabs */}
            <div className="flex items-center gap-1 mt-4 border-b border-line">
              {(["results", "metrics", "timeline"] as const).map((t) => (
                <button
                  key={t}
                  onClick={() => setTab(t)}
                  className={`px-3 h-9 text-[12.5px] font-medium border-b-2 -mb-px ${
                    tab === t
                      ? "border-honey-500 text-honey-300"
                      : "border-transparent text-muted hover:text-white"
                  }`}
                >
                  {t[0].toUpperCase() + t.slice(1)}
                </button>
              ))}
            </div>

            {tab === "results" && (
              <div className="mt-5 space-y-4">
                {interruptedMerge && (
                  <InterruptedMergeBanner
                    selected={selected}
                    interrupted={interruptedMerge}
                    taskRuntime={taskRuntime}
                    go={go}
                  />
                )}
                {orchestratorUsage && (orchestratorUsage.model_id || orchestratorUsage.provider) && (
                  <OrchestratorBlock usage={orchestratorUsage} loading={orchestratorLoading} />
                )}
                {orchestratorLoading && !orchestratorUsage && (
                  <div className="rounded-xl border border-line bg-ink-800/40 px-4 py-3 text-sm text-ink-500 animate-pulse">
                    Loading orchestrator usage…
                  </div>
                )}
                {liveState && liveRoundResults.length > 0 ? (
                  <>
                    {detailLoading && (
                      <div className="text-[12px] text-muted py-4 text-center">Loading review details...</div>
                    )}
                    {/* Render live results grouped by round */}
                    {Array.from({ length: liveState.total_rounds }, (_, roundIdx) => {
                      const roundNum = roundIdx + 1;
                      const roundSteps = liveState.steps.filter(
                        (s) => s.round_number === roundNum
                      );
                      // Disambiguate duplicate reviewer instances so each row
                      // gets a unique joinKey for its verdict bucket. MUST use
                      // the same ordering convention as `taskRuntime.tsx`
                      // (`handleRoundComplete`/`resumeMerge` walk
                      // `getReviewStepOutputs` results ordered by `step_idx`
                      // ascending — see
                      // `app/src-tauri/src/hivemind/engine.rs` where
                      // `step_idx = model_idx as i64`). `liveState.steps`
                      // filtered to this round preserves that natural order.
                      const stepLabels = dedupeReviewerLabels(
                        roundSteps.map((s) => ({
                          provider: s.provider,
                          model_id: s.model_id,
                        })),
                      );
                      const roundResults: RoundResult[] = roundSteps.map((step, idx) => {
                        const body = step.output || "";
                        // Use the orchestrator-emitted form so it joins with verdicts.
                        const joinKey = stepLabels[idx];
                        return {
                          model: joinKey,
                          provider: step.provider,
                          tokInNum: step.input_tokens ?? 0,
                          tokOutNum: step.output_tokens ?? 0,
                          durationMs: step.duration_ms ?? 0,
                          cost: step.cost,
                          verdict:
                            step.status === "completed"
                              ? parseVerdict(body)
                              : ("issues" as const),
                          failed:
                            step.status != null &&
                            (step.status === "failed" || step.status === "error" || step.status === "cancelled"),
                          pending:
                            step.status != null &&
                            (step.status === "pending" || step.status === "running"),
                          error: step.error ?? null,
                          issues:
                            step.status === "completed" ? parseIssues(body) : {},
                          tokIn:
                            step.input_tokens != null
                              ? step.input_tokens.toLocaleString()
                              : "",
                          tokOut:
                            step.output_tokens != null
                              ? step.output_tokens.toLocaleString()
                              : "",
                          time:
                            step.duration_ms != null
                              ? `${(step.duration_ms / 1000).toFixed(1)}s`
                              : "",
                          body,
                        };
                      });
                      const roundInputTokens = roundSteps.length > 0
                        ? roundSteps.reduce((sum, s) => sum + (s.input_tokens ?? 0), 0) / roundSteps.length
                        : 0;
                      const rawVerdictsForRound = verdictsByRound[roundNum] || {};
                      const reviewerLabels = roundResults.map((r) => r.model);
                      const verdictsForRound = Object.entries(rawVerdictsForRound).reduce(
                        (acc, [reviewer, vs]) => {
                          // Verdicts are already canonicalized at save time. Use stored
                          // value directly if it matches a live label; fall back to
                          // canonicalization only if the label has drifted.
                          const key = reviewerLabels.includes(reviewer)
                            ? reviewer
                            : canonicalizeReviewerModel(reviewer, reviewerLabels);
                          acc[key] = [...(acc[key] || []), ...vs];
                          return acc;
                        },
                        {} as Record<string, RoundVerdict[]>,
                      );
                      // Locate this round's best-find verdict, if the merge model
                      // designated one. Verdicts are already canonicalized at save
                      // time — only show the star on models whose stored
                      // reviewer_model appears in the live reviewerLabels set.
                      // No fallback canonicalization to avoid fuzzy-match errors.
                      const bestFindForRound = ((): { id: string; models: Set<string> } | null => {
                        const all = Object.values(verdictsForRound).flat();
                        const best = all.find((v) => v.best_find);
                        if (!best) return null;
                        const models = new Set<string>();
                        const addIfMatch = (name: string | undefined | null) => {
                          if (!name) return;
                          if (reviewerLabels.includes(name)) {
                            models.add(name);
                          }
                        };
                        addIfMatch(best.reviewer_model);
                        // co_reviewers are not starred — only the primary reviewer gets the star
                        if (models.size === 0) {
                          console.warn(
                            `[ReviewHistory] best-find verdict ${best.id} has no matching model ` +
                            `in reviewerLabels. Stored reviewer_model="${best.reviewer_model}", ` +
                            `co_reviewers=${JSON.stringify(best.co_reviewers)}`,
                          );
                        }
                        return models.size > 0 ? { id: best.id, models } : null;
                      })();
                      const verdictTotalForRound = Object.values(verdictsForRound)
                        .reduce((acc, vs) => acc + vs.length, 0);
                      const acceptedForRound = Object.values(verdictsForRound)
                        .reduce(
                          (acc, vs) => acc + vs.filter((v) => v.verdict === "accepted").length,
                          0,
                        );
                      // Round-summary text/tone — orchestrator verdicts when present,
                      // regex-derived fallback otherwise.
                      let verdictText: string;
                      let verdictTone: "green" | "honey";
                      if (verdictTotalForRound > 0) {
                        if (acceptedForRound === verdictTotalForRound) {
                          verdictTone = "green";
                          verdictText = `Orchestrator accepted all ${verdictTotalForRound} suggestion${verdictTotalForRound === 1 ? "" : "s"}.`;
                        } else {
                          verdictTone = "honey";
                          verdictText = `Orchestrator accepted ${acceptedForRound} of ${verdictTotalForRound} suggestions.`;
                        }
                      } else {
                        const accepted = roundResults.filter((r) => r.verdict === "approved").length;
                        verdictTone = accepted === roundResults.length ? "green" : "honey";
                        verdictText =
                          accepted === roundResults.length
                            ? `All ${roundResults.length} models accepted.`
                            : `${roundResults.length - accepted} of ${roundResults.length} models flagged issues.`;
                      }
                      const mergeSession = mergeSessionByRound.get(roundNum);
                      return (
                        <RoundBlock
                          key={roundIdx}
                          idx={roundNum}
                          label={roundIdx === 0 ? "Independent review" : "Synthesis"}
                          tokens={
                            roundInputTokens > 0 ? Math.floor(roundInputTokens).toLocaleString() : ""
                          }
                          prompt={roundSteps[0]?.prompt || liveState.final_output || selected?.prompt || ""}
                          results={roundResults}
                          verdictsByModel={verdictsForRound}
                          bestFind={bestFindForRound}
                          verdictTone={verdictTone}
                          verdictText={verdictText}
                          agentId={mergeSession?.model_id}
                          sessionId={mergeSession?.session_id}
                        />
                      );
                    })}
                    {liveState.error && (
                      <div className="rounded-lg border border-red-500/30 bg-red-500/5 px-4 py-3 text-[12.5px] text-red-300">
                        {liveState.error}
                      </div>
                    )}
                  </>
                ) : (
                  <>
                    <RoundBlock
                      idx={1}
                      label="Independent review"
                      tokens="2,418"
                      prompt="Review the proposed implementation of rotate-refresh-tokens. Look for: race conditions, theft-detection coverage, incorrect TTL handling, missing audit log entries, error path correctness, and resource leaks. Return findings grouped by layer (Architecture, Logic, Edge Cases, Performance). Be specific and cite line ranges. Examples of past failures are included below for reference; do not repeat their findings unless they recur. Keep verdicts terse — Approved | Issues found — followed by the structured list."
                      results={ROUND1_RESULTS}
                      verdictsByModel={{}}
                      verdictTone="honey"
                      verdictText="2 of 3 models flagged the same Logic issue (unlocked family-id read in rotate_refresh()). Forwarding to Round 2 for synthesis."
                      agentId={mergeSessionByRound.get(1)?.model_id}
                      sessionId={mergeSessionByRound.get(1)?.session_id}
                    />

                    <RoundBlock
                      idx={2}
                      label="Synthesis"
                      tokens="3,860"
                      prompt="Synthesize Round 1 findings. Three reviewers returned: 2 flagged a Logic issue, 1 approved. Determine whether the issues are duplicates of the same root cause, decide which are blocking, and produce a single recommended fix. Output: { blocking: [...], non_blocking: [...], recommended_fix: '...' }."
                      results={ROUND2_RESULTS}
                      verdictsByModel={{}}
                      verdictTone="green"
                      verdictText="Both Opus and Codex independently identified that rotate_refresh() reads the family-id without a lock, allowing two concurrent rotations to both succeed. Recommended fix: SELECT ... FOR UPDATE on the family row, or a lua atomic in Redis."
                      agentId={mergeSessionByRound.get(2)?.model_id}
                      sessionId={mergeSessionByRound.get(2)?.session_id}
                    />
                  </>
                )}
              </div>
            )}

            {tab === "metrics" && (
              <div className="mt-5 grid grid-cols-4 gap-3">
                {(liveState ? (() => {
                  const uniqueModels = new Set(liveState.steps.map((s) => s.model_id)).size;
                  const duration = computeDuration(liveState.created_at, liveState.completed_at);
                  const items: { l: string; v: string; tone?: "honey" }[] = [
                    { l: "Total tokens", v: (liveState.total_input_tokens + liveState.total_output_tokens).toLocaleString() },
                    { l: "Cost", v: `$${liveState.total_cost.toFixed(3)}`, tone: "honey" as const },
                    { l: "Input tokens", v: liveState.total_input_tokens.toLocaleString() },
                    { l: "Output tokens", v: liveState.total_output_tokens.toLocaleString() },
                    { l: "Unique models", v: String(uniqueModels) },
                    { l: "Rounds", v: String(liveState.total_rounds) },
                    { l: "Duration", v: duration || "—" },
                    { l: "Status", v: liveState.status },
                  ];
                  if (liveState.error) items.push({ l: "Error", v: liveState.error });
                  return items;
                })() : [
                  { l: "Total tokens", v: "14,628" },
                  { l: "Cost", v: "$0.184", tone: "honey" as const },
                  { l: "Wall time", v: "14.2s" },
                  { l: "Issues found", v: "2" },
                  { l: "Avg latency / model", v: "8.8s" },
                  { l: "P99 latency / model", v: "11.4s" },
                  { l: "Auto-resolved", v: "0" },
                  { l: "Forwarded to R2", v: "2" },
                ]).map((s, i) => (
                  <div key={i} className="bg-ink-850 border border-line rounded-lg px-3 py-2.5">
                    <div className="text-[10.5px] uppercase tracking-wider text-muted">{s.l}</div>
                    <div
                      className={`text-[18px] font-semibold mt-0.5 tabular-nums ${
                        s.tone === "honey" ? "text-honey-300" : "text-white"
                      }`}
                    >
                      {s.v}
                    </div>
                  </div>
                ))}
              </div>
            )}

            {tab === "timeline" && (
              <div className="mt-5 space-y-1.5 text-[12.5px] font-mono">
                {(liveState ? (() => {
                  const entries: [string, string, string][] = [["T+0.0s", `Run started · ${liveState.steps.length} model calls`, "meta"]];
                  let cumulMs = 0;
                  const rounds = new Set(liveState.steps.map((s) => s.round_number));
                  for (const rn of [...rounds].sort()) {
                    const roundSteps = liveState.steps.filter((s) => s.round_number === rn);
                    entries.push([`T+${(cumulMs / 1000).toFixed(1)}s`, `R${rn} dispatched (${roundSteps.length} models)`, "tool"]);
                    for (const s of roundSteps) {
                      const dur = s.duration_ms ?? 0;
                      cumulMs += dur;
                      const tokOut = s.output_tokens != null ? s.output_tokens.toLocaleString() : "?";
                      entries.push([`T+${(cumulMs / 1000).toFixed(1)}s`, `${s.model_id} returned · ${tokOut} tok out`, s.status === "completed" ? "ok" : "warn"]);
                    }
                    entries.push([`T+${(cumulMs / 1000).toFixed(1)}s`, `R${rn} complete`, "meta"]);
                  }
                  entries.push([`T+${(cumulMs / 1000).toFixed(1)}s`, "Run complete", "meta"]);
                  return entries;
                })() : ([
                    ["T+0.0s", "Run started · 5 models warmed", "meta"],
                    ["T+0.4s", "R1 prompt dispatched (3 models)", "tool"],
                    ["T+6.8s", "gemini-2.5-pro returned · 0 issues", "ok"],
                    ["T+8.2s", "claude-opus-4.1 returned · 2 issues", "warn"],
                    ["T+11.4s", "gpt-5-codex returned · 2 issues", "warn"],
                    ["T+11.5s", "R1 complete · forwarding 2 issues to R2", "meta"],
                    ["T+12.0s", "R2 prompt dispatched (2 models)", "tool"],
                    ["T+13.1s", "glm-4.6 returned · synthesis ok", "ok"],
                    ["T+14.2s", "deepseek-v3.2 returned · synthesis ok", "ok"],
                    ["T+14.2s", "Run complete", "meta"],
                  ] as [string, string, string][]
                )).map(([t, msg, k], i) => (
                  <div key={i} className="grid grid-cols-[80px_1fr] gap-3">
                    <span className="text-dim">{t}</span>
                    <span
                      className={
                        k === "ok"
                          ? "text-emerald-300"
                          : k === "warn"
                          ? "text-honey-300"
                          : k === "tool"
                          ? "text-blue-300"
                          : "text-white/85"
                      }
                    >
                      {msg}
                    </span>
                  </div>
                ))}
              </div>
            )}
            </>)}
          </div>
        </div>
      </div>
      <ReplayModal
        open={replayOpen && !!selected}
        onClose={() => setReplayOpen(false)}
        reviewId={selected ? resolveReviewId(selected) : ""}
        taskRuntime={taskRuntime}
        go={go}
      />
    </div>
  );
}
