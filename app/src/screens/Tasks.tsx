import React, { useState, useMemo, useRef, useEffect, useCallback, forwardRef, useImperativeHandle } from "react";
import { GoFn } from "../App";
import { I } from "../components/icons";
import { workspaceColor, workspaceLabel, workspaceKey } from "../lib/categories";
import { timeGroup, relativeTime } from "../lib/time";
import { reorderInGroup } from "../lib/sortOrder";
import { SortableTaskItem } from "../components/SortableTaskItem";
import {
  DndContext,
  closestCenter,
  PointerSensor,
  KeyboardSensor,
  useSensors,
  useSensor,
  type DragEndEvent,
} from "@dnd-kit/core";
import {
  SortableContext,
  verticalListSortingStrategy,
  sortableKeyboardCoordinates,
} from "@dnd-kit/sortable";
import { restrictToVerticalAxis, restrictToParentElement } from "@dnd-kit/modifiers";
import { Btn, Input } from "../components/atoms";
import { useContextMenu } from "../components/ContextMenu";
import { ProjectPicker, useProject, projectFromPath, pathForCompare } from "../components/ProjectPicker";
import { TaskConfigChip } from "../components/TaskConfigChip";
import { isTauri } from "../lib/tauri";
import * as ipc from "../lib/ipc";
import { SwarmQuestionModal, type SwarmQuestionAnswer } from "../components/SwarmQuestionModal";
import { HivemindReviewLivePanel } from "../components/HivemindReviewLivePanel";
import { HivemindReviewCollapsedBar } from "../components/HivemindReviewCollapsedBar";
import { MergedPlanModal } from "../components/MergedPlanModal";
import { QuestionsDock } from "../components/QuestionsDock";
import { useErrorToast } from "../components/Toast";
import { useHivemindReviewState } from "../lib/hivemindEventStore";
import type { ReviewState } from "../lib/hivemindReducer";
import type { TaskQuestion } from "../lib/questions";
import { MODELS } from "../data/mock";
import {
  applyTaskEvent,
  makeInitialTaskState,
  toStreamEntries,
  type AutoMode,
  type TaskMessage,
  type TaskRuntimeState,
} from "../lib/taskReducer";
import { ActivityStream } from "../components/ActivityStream";
import { ActivityFooter } from "../components/ActivityFooter";
import { NurseMessage } from "../components/NurseMessage";
import type { NurseEntry } from "../lib/streamEntry";
import {
  useTaskRuntime,
  useDefaults,
  canRetryErroredReviewState,
  type TaskListItem,
  type AwaitingInputKind,
} from "../lib/taskRuntime";
import type { ImageAttachment, ReviewInterruptedState } from "../lib/types";
import { FileMentionPicker } from "../components/FileMentionPicker";
import { NurseTestDropdown } from "../components/NurseTestDropdown";
import { detectMention, type MentionSpan } from "../lib/mention";
import type { ProjectFileEntry } from "../lib/ipc";
import {
  SHOW_REASONING_KEY,
  SHOW_TOOL_CALLS_KEY,
  loadShowReasoning,
  loadShowToolCalls,
  parseCtx2,
} from "../lib/uiPrefs";

/* ── Review dock mode ─────────────────────────────────────── */

type ReviewDockMode = "hidden" | "expanded" | "collapsed";

// Keep the live dock visible while a review runs. ~5s after a terminal
// status, collapse it to a one-line summary bar (staying mounted
// indefinitely) so the user retains a persistent reference to the last
// review without losing composer space entirely. The user can manually
// expand from the collapsed bar (sticky for that jobId), or collapse the
// expanded panel after a terminal status. A new `jobId` resets the mode
// to expanded and re-arms the 5s auto-collapse.
function useReviewDockMode(state: ReviewState | undefined): {
  mode: ReviewDockMode;
  expand: () => void;
  collapse: () => void;
} {
  const [mode, setMode] = useState<ReviewDockMode>("hidden");
  const userExpandedJobIdRef = useRef<string | null>(null);
  const autoCollapseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    // Clear any pending timer from a previous transition before deciding.
    if (autoCollapseTimerRef.current) {
      clearTimeout(autoCollapseTimerRef.current);
      autoCollapseTimerRef.current = null;
    }

    if (!state) {
      setMode("hidden");
      userExpandedJobIdRef.current = null;
      return;
    }

    if (state.status === "running") {
      // New run (or continuing run) — clear sticky-expand so the next
      // terminal status for this jobId will re-arm the auto-collapse.
      userExpandedJobIdRef.current = null;
      setMode("expanded");
      return;
    }

    // Terminal status: completed / failed / cancelled / skipped.
    if (userExpandedJobIdRef.current === state.jobId) {
      // User has manually expanded this jobId — respect that, no timer.
      setMode("expanded");
      return;
    }

    setMode("expanded");
    autoCollapseTimerRef.current = setTimeout(() => {
      setMode("collapsed");
      autoCollapseTimerRef.current = null;
    }, 5000);

    return () => {
      if (autoCollapseTimerRef.current) {
        clearTimeout(autoCollapseTimerRef.current);
        autoCollapseTimerRef.current = null;
      }
    };
  }, [state?.jobId, state?.status]);

  const expand = useCallback(() => {
    if (!state) return;
    if (autoCollapseTimerRef.current) {
      clearTimeout(autoCollapseTimerRef.current);
      autoCollapseTimerRef.current = null;
    }
    setMode("expanded");
    userExpandedJobIdRef.current = state.jobId;
  }, [state]);

  const collapse = useCallback(() => {
    if (autoCollapseTimerRef.current) {
      clearTimeout(autoCollapseTimerRef.current);
      autoCollapseTimerRef.current = null;
    }
    setMode("collapsed");
  }, []);

  return { mode, expand, collapse };
}

/* ── Phase pipeline ───────────────────────────────────────── */

const PHASES = [
  { id: "intake", label: "Intake", icon: "edit" },
  { id: "plan", label: "Plan", icon: "doc" },
  { id: "review", label: "Hivemind Review", icon: "hex" },
  { id: "implement", label: "Implementation", icon: "rocket" },
] as const;

const TaskPipeline = ({
  active,
  isPlanReady,
  isDone,
  hivemind,
  hivemindDone,
}: {
  active: string;
  isPlanReady?: boolean;
  isDone?: boolean;
  hivemind?: string | null;
  hivemindDone?: boolean;
}) => {
  const displayPhase = active === "plan-ready" ? "plan" : active === "implement-done" ? "implement" : active;
  const idx = PHASES.findIndex((p) => p.id === displayPhase);

  return (
    <div className="flex items-center gap-1.5 flex-wrap">
      {PHASES.map((p, i) => {
        const Ico = I[p.icon];
        const done = i < idx || (p.id === "review" && hivemindDone);
        const cur = i === idx;
        const isSkipped = p.id === "review" && !hivemind;
        const planReady = p.id === "plan" && isPlanReady && displayPhase === "plan";
        const isImplementDone = p.id === "implement" && isDone && displayPhase === "implement";

        let pillClasses: string;
        if (cur) {
          if (planReady) {
            pillClasses = "bg-emerald-500/12 text-emerald-200 border-emerald-500/35";
          } else if (isImplementDone) {
            pillClasses = "bg-emerald-500/12 text-emerald-200 border-emerald-500/35";
          } else if (isSkipped) {
            pillClasses = "bg-red-500/8 text-red-300/70 border-red-500/30 line-through decoration-red-500/50";
          } else {
            pillClasses = "bg-honey-500/12 text-honey-200 border-honey-500/35";
          }
        } else if (done) {
          pillClasses = isSkipped
            ? "bg-transparent text-dim border-red-500/20 line-through decoration-red-500/30"
            : "bg-ink-800 text-white/85 border-line";
        } else {
          pillClasses = isSkipped
            ? "bg-transparent text-dim/60 border-red-500/15"
            : "bg-transparent text-dim border-line";
        }

        return (
          <React.Fragment key={p.id}>
            <div
              className={`h-7 px-2.5 rounded-md flex items-center gap-1.5 text-[11.5px] font-medium border transition-colors ${pillClasses}`}
            >
              {(done || isImplementDone) ? (
                isSkipped
                  ? I.x({ size: 11, className: "text-red-400/60" })
                  : I.check({ size: 11, className: "text-emerald-400" })
              ) : (
                Ico({ size: 11, className: planReady ? "text-emerald-400" : cur ? "text-honey-400" : "" })
              )}
              <span>{p.label}</span>
              {cur && !isSkipped && !isImplementDone && (
                <span className={`w-1.5 h-1.5 rounded-full ${planReady ? "bg-emerald-400 pulse-green" : "bg-honey-400 pulse-amber"} ml-0.5`} />
              )}
            </div>
            {i < PHASES.length - 1 && (
              <div
                className={`w-3 h-px ${
                  PHASES[i + 1].id === "review" && !hivemind
                    ? "bg-red-500/30"
                    : i < idx || (p.id === "review" && hivemindDone) ? "bg-honey-500/60" : "bg-line"
                }`}
              />
            )}
          </React.Fragment>
        );
      })}

    </div>
  );
};

/* ── Composer ─────────────────────────────────────────────── */

export type ComposerHandle = {
  getValue: () => string;
  setValue: (v: string) => void;
  focus: () => void;
};

interface ComposerProps {
  activeId: string;
  streaming: boolean;
  headerModel: string;
  autoMode: AutoMode;
  hasHivemind: boolean;
  pendingImagesCount: number;
  attachedFilesCount: number;
  projectPath: string | null;
  onAddAttachedFile: (path: string) => void;
  messagesRef: React.MutableRefObject<TaskMessage[]>;
  onSend: () => void;
  onStop: () => void;
  onSetAutoMode: (mode: AutoMode) => void;
  onPaste: (e: React.ClipboardEvent) => void;
  reviewLocked?: boolean;
  reviewLockReason?: string;
  steerableWhileStreaming?: boolean;
  questionsPending?: boolean;
}

/** Memoized composer. Owns its own draft text in local state so typing
 *  does NOT trigger context updates or re-renders of the parent. Drafts
 *  are flushed to the runtime ref-store on task switch / unmount. */
const ComposerInner = forwardRef<ComposerHandle, ComposerProps>(function Composer(
  {
    activeId,
    streaming,
    headerModel,
    autoMode,
    hasHivemind,
    pendingImagesCount,
    attachedFilesCount,
    projectPath,
    onAddAttachedFile,
    messagesRef,
    onSend,
    onStop,
    onSetAutoMode,
    onPaste,
    reviewLocked = false,
    reviewLockReason,
    steerableWhileStreaming = false,
    questionsPending = false,
  },
  ref,
) {
  const { getDraft, setDraft } = useTaskRuntime();
  const [value, setValue] = useState(() => getDraft(activeId));
  const valueRef = useRef(value);
  valueRef.current = value;
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const historyIndexRef = useRef(-1);
  const draftBackupRef = useRef("");
  const prevActiveIdRef = useRef(activeId);
  const focusRafRef = useRef<number | null>(null);

  // ── Auto-mode dropdown ───────────────────────────────────────
  const [autoMenuOpen, setAutoMenuOpen] = useState(false);
  const autoMenuRootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!autoMenuOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (autoMenuRootRef.current && !autoMenuRootRef.current.contains(e.target as Node)) {
        setAutoMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [autoMenuOpen]);

  // ── Mention picker state (owned by Composer so keyboard handlers can
  //    drive selection without round-tripping through the picker). ──
  const [mention, setMention] = useState<MentionSpan | null>(null);
  const [mentionSelected, setMentionSelected] = useState(0);
  const mentionItemsRef = useRef<ProjectFileEntry[]>([]);
  const ignoredMentionStartRef = useRef<number | null>(null);
  const caretAfterPickRef = useRef<number | null>(null);

  /** Re-run mention detection from the current textarea cursor. */
  const updateMention = useCallback(
    (val: string, cursor: number) => {
      if (!projectPath) {
        if (mention !== null) setMention(null);
        return;
      }
      const next = detectMention(val, cursor);
      // Clear ignored marker when the @ at that position is gone or moved.
      if (
        ignoredMentionStartRef.current !== null &&
        (next === null || next.startIdx !== ignoredMentionStartRef.current)
      ) {
        ignoredMentionStartRef.current = null;
      }
      // Suppress reopening at a position the user just dismissed with Escape.
      if (
        next !== null &&
        ignoredMentionStartRef.current === next.startIdx
      ) {
        if (mention !== null) setMention(null);
        return;
      }
      // Shallow compare: only setState when the span actually changes.
      if (
        next === null && mention === null
      ) {
        return;
      }
      if (
        next !== null &&
        mention !== null &&
        next.startIdx === mention.startIdx &&
        next.tokenEnd === mention.tokenEnd
      ) {
        return;
      }
      setMention(next);
    },
    [mention, projectPath],
  );

  useEffect(() => {
    if (prevActiveIdRef.current === activeId) return;
    setDraft(prevActiveIdRef.current, valueRef.current);
    const next = getDraft(activeId);
    valueRef.current = next;
    setValue(next);
    historyIndexRef.current = -1;
    draftBackupRef.current = "";
    setMention(null);
    setMentionSelected(0);
    ignoredMentionStartRef.current = null;
    mentionItemsRef.current = [];
    prevActiveIdRef.current = activeId;
    focusRafRef.current = requestAnimationFrame(() => {
      const el = textareaRef.current;
      if (!el) return;
      el.focus();
      const end = el.value.length;
      el.setSelectionRange(end, end);
    });
    return () => {
      if (focusRafRef.current !== null) {
        cancelAnimationFrame(focusRafRef.current);
        focusRafRef.current = null;
      }
    };
  }, [activeId, getDraft, setDraft]);

  useEffect(() => {
    return () => {
      setDraft(prevActiveIdRef.current, valueRef.current);
    };
  }, [setDraft]);

  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    const raf = requestAnimationFrame(() => {
      if (!textareaRef.current) return;
      const el = textareaRef.current;
      el.style.height = "auto";
      const maxH = parseInt(getComputedStyle(el).maxHeight) || 192;
      el.style.height = Math.min(el.scrollHeight, maxH) + "px";
    });
    return () => cancelAnimationFrame(raf);
  }, [value]);

  useImperativeHandle(
    ref,
    () => ({
      getValue: () => valueRef.current,
      setValue: (v: string) => {
        valueRef.current = v;
        setValue(v);
      },
      focus: () => textareaRef.current?.focus(),
    }),
    [],
  );

  const handleChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const v = e.target.value;
    valueRef.current = v;
    setValue(v);
    const cursor = e.target.selectionStart ?? v.length;
    updateMention(v, cursor);
  }, [updateMention]);

  const handleSelect = useCallback(
    (e: React.SyntheticEvent<HTMLTextAreaElement>) => {
      const ta = e.currentTarget;
      updateMention(ta.value, ta.selectionStart ?? 0);
    },
    [updateMention],
  );

  const handleKeyUp = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      const ta = e.currentTarget;
      updateMention(ta.value, ta.selectionStart ?? 0);
    },
    [updateMention],
  );

  const handleFocus = useCallback(
    (e: React.FocusEvent<HTMLTextAreaElement>) => {
      const ta = e.currentTarget;
      updateMention(ta.value, ta.selectionStart ?? 0);
    },
    [updateMention],
  );

  const handleBlur = useCallback(() => {
    // Picker uses onMouseDown preventDefault, so clicking the picker does
    // NOT blur the textarea. Any real blur (click outside, Tab away) closes
    // the mention.
    setMention(null);
  }, []);

  /** Apply a file pick: replace the @token with empty (or single space)
   *  and notify parent to attach the file. */
  const applyPick = useCallback(
    (filePath: string) => {
      const m = mention;
      if (!m) return;
      const v = valueRef.current;
      const before = v.slice(0, m.startIdx);
      const after = v.slice(m.tokenEnd);
      const needsSpace =
        before.length > 0 &&
        !/\s$/.test(before) &&
        (after.length === 0 || !/^\s/.test(after));
      const newValue = before + (needsSpace ? " " : "") + after;
      const caretPos = m.startIdx + (needsSpace ? 1 : 0);
      valueRef.current = newValue;
      setValue(newValue);
      setMention(null);
      setMentionSelected(0);
      mentionItemsRef.current = [];
      ignoredMentionStartRef.current = null;
      caretAfterPickRef.current = caretPos;
      onAddAttachedFile(filePath);
      // Defer caret positioning + focus to next frame (after React commits).
      requestAnimationFrame(() => {
        const ta = textareaRef.current;
        if (!ta) return;
        ta.focus();
        if (caretAfterPickRef.current !== null) {
          ta.setSelectionRange(
            caretAfterPickRef.current,
            caretAfterPickRef.current,
          );
          caretAfterPickRef.current = null;
        }
      });
    },
    [mention, onAddAttachedFile],
  );

  const handleItemsChange = useCallback((items: ProjectFileEntry[]) => {
    mentionItemsRef.current = items;
    setMentionSelected(0);
  }, []);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      // Mention picker takes precedence over send/history when active.
      if (mention) {
        const items = mentionItemsRef.current;
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setMentionSelected((i) => Math.min(i + 1, Math.max(0, items.length - 1)));
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setMentionSelected((i) => Math.max(0, i - 1));
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          ignoredMentionStartRef.current = mention.startIdx;
          setMention(null);
          return;
        }
        if ((e.key === "Enter" && !e.shiftKey) || e.key === "Tab") {
          if (
            items.length > 0 &&
            mentionSelected >= 0 &&
            mentionSelected < items.length
          ) {
            e.preventDefault();
            const picked = items[mentionSelected];
            applyPick(picked.path);
            return;
          }
          // No items: close mention but let Enter fall through to send.
          setMention(null);
          // Tab with no items: also close, don't preventDefault.
          if (e.key === "Tab") return;
          // Fall through to normal send handling below.
        } else {
          // Other keys while mention active: let them pass to the textarea
          // (typing more letters refines the query).
          return;
        }
      }

      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (reviewLocked) return;
        // Typing-while-streaming is the soft "steer" path: the backend
        // routes the prompt through Pi's session.steer() RPC when a Pi
        // session is busy (see commands/chat.rs::send_message). The
        // QuestionsDock is the only legitimate input surface while it is
        // open, so we explicitly gate Enter on that — not on streaming
        // as a whole.
        if (questionsPending) return;
        historyIndexRef.current = -1;
        onSend();
        return;
      }
      const userMsgs = (messagesRef.current ?? [])
        .filter((m) => m.who === "user")
        .map((m) => m.text || "");
      if (!userMsgs.length) return;
      if (e.key === "ArrowUp") {
        const ta = e.currentTarget;
        if (ta.selectionStart !== 0 || ta.selectionEnd !== 0) return;
        e.preventDefault();
        if (historyIndexRef.current === -1) draftBackupRef.current = valueRef.current;
        const next = Math.min(historyIndexRef.current + 1, userMsgs.length - 1);
        historyIndexRef.current = next;
        const v = userMsgs[userMsgs.length - 1 - next];
        valueRef.current = v;
        setValue(v);
      } else if (e.key === "ArrowDown") {
        if (historyIndexRef.current < 0) return;
        const ta = e.currentTarget;
        if (ta.selectionStart !== ta.value.length) return;
        e.preventDefault();
        const next = historyIndexRef.current - 1;
        historyIndexRef.current = next;
        const v = next < 0 ? draftBackupRef.current : userMsgs[userMsgs.length - 1 - next];
        valueRef.current = v;
        setValue(v);
      }
    },
    [messagesRef, onSend, mention, mentionSelected, applyPick, reviewLocked, streaming, questionsPending],
  );

  const query = mention
    ? value.slice(
        mention.startIdx + 1,
        Math.min(
          textareaRef.current?.selectionStart ?? mention.tokenEnd,
          mention.tokenEnd,
        ),
      )
    : "";

  const composerPlaceholder = steerableWhileStreaming && streaming && !reviewLocked
    ? "Steer the context gatherer — your message is injected into the live session…"
    : reviewLocked
      ? (reviewLockReason ?? "Hivemind review in progress — use 'Cancel review' to interrupt.")
      : streaming
        ? `${headerModel} is responding — type to steer or press Stop…`
        : `Reply to ${headerModel}…`;

  return (
    <div
      className={`relative flex items-end gap-2 bg-ink-800 border rounded-lg px-2.5 py-1.5 ${
        reviewLocked
          ? "border-line opacity-60"
          : "border-line focus-within:border-honey-500/40"
      }`}
    >
      <FileMentionPicker
        open={!!mention}
        query={query}
        projectPath={projectPath}
        selectedIndex={mentionSelected}
        onSetSelection={setMentionSelected}
        onPick={applyPick}
        onItemsChange={handleItemsChange}
      />
      {I.chat({ size: 13, className: "text-dim mb-1.5" })}
      <textarea
        ref={textareaRef}
        rows={1}
        value={value}
        onChange={handleChange}
        onPaste={onPaste}
        onKeyDown={handleKeyDown}
        onKeyUp={handleKeyUp}
        onSelect={handleSelect}
        onFocus={handleFocus}
        onBlur={handleBlur}
        disabled={reviewLocked || questionsPending}
        placeholder={composerPlaceholder}
        className="flex-1 bg-transparent resize-none px-1 py-1 text-[13px] focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 placeholder:text-dim text-white/90 min-h-[24px] max-h-48 disabled:cursor-not-allowed"
      />
      <NurseTestDropdown
        activeId={activeId}
        streaming={streaming}
        projectPath={projectPath}
        setComposerValue={(v) => {
          setValue(v);
          setDraft(activeId, v);
          textareaRef.current?.focus();
        }}
      />
      <div ref={autoMenuRootRef} className="relative shrink-0">
        <button
          onClick={() => setAutoMenuOpen((o) => !o)}
          title={
            autoMode === "full"
              ? "Auto: Full — plan auto-reviews (if hivemind set) and auto-implements"
              : autoMode === "review"
                ? "Auto: Review Only — plan auto-reviews via hivemind, then waits for you to click Implement"
                : "Auto: Off — click to enable"
          }
          className={`h-7 px-2 rounded-md flex items-center gap-1 text-[11px] font-medium transition-all ${
            autoMode !== "off"
              ? "bg-honey-500/15 text-honey-400 border border-honey-500/30"
              : "text-dim hover:text-white/60 hover:bg-ink-700/60"
          }`}
        >
          {I.rocket({ size: 11 })}
          <span>
            {autoMode === "full" ? "Auto" : autoMode === "review" ? "Auto: Review" : "Auto"}
          </span>
          <svg width="8" height="8" viewBox="0 0 8 8" className="opacity-70">
            <path d="M1 2.5 L4 5.5 L7 2.5" stroke="currentColor" strokeWidth="1.2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </button>
        {autoMenuOpen && (
          <div
            className="absolute bottom-full right-0 mb-1 z-50 w-60 bg-ink-800 border border-line rounded-lg shadow-2xl py-1"
            role="menu"
          >
            {([
              {
                value: "off" as AutoMode,
                label: "Off",
                desc: "Stop at plan-ready. You click Implement.",
              },
              {
                value: "review" as AutoMode,
                label: "Review only",
                desc: hasHivemind
                  ? "Auto-run the Hivemind review, then wait for you to click Implement."
                  : "No Hivemind configured — review will be skipped, task stops at plan-ready.",
              },
              {
                value: "full" as AutoMode,
                label: "Full auto",
                desc: hasHivemind
                  ? "Auto-run the Hivemind review, then auto-implement the plan."
                  : "Auto-implement the plan as soon as it's ready.",
              },
            ] as const).map((opt) => {
              const selected = autoMode === opt.value;
              return (
                <button
                  key={opt.value}
                  onClick={() => {
                    onSetAutoMode(opt.value);
                    setAutoMenuOpen(false);
                  }}
                  role="menuitemradio"
                  aria-checked={selected}
                  className={`w-full text-left px-3 py-2 flex items-start gap-2 hover:bg-ink-700/60 ${
                    selected ? "text-honey-400" : "text-white/80"
                  }`}
                >
                  <span className="mt-[3px] w-3 inline-flex justify-center">
                    {selected ? I.check({ size: 11 }) : <span className="inline-block w-[11px]" />}
                  </span>
                  <span className="flex-1">
                    <span className="block text-[12px] font-medium">{opt.label}</span>
                    <span className="block text-[11px] text-muted leading-snug">{opt.desc}</span>
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </div>
      {streaming && (
        <button
          onClick={onStop}
          title="Stop the agent"
          className="h-7 px-2 rounded-md flex items-center gap-1.5 text-[11px] font-medium text-red-400 hover:bg-red-500/10 transition-all shrink-0"
        >
          {I.x({ size: 11 })}
          <span>Stop</span>
        </button>
      )}
      <button
        onClick={onSend}
        disabled={
          reviewLocked ||
          questionsPending ||
          (!value.trim() && pendingImagesCount === 0 && attachedFilesCount === 0)
        }
        title={
          reviewLocked
            ? (reviewLockReason ?? "Hivemind review in progress")
            : steerableWhileStreaming && streaming
              ? "Send steer to context gatherer"
              : streaming
                ? `Send steer to ${headerModel}`
                : "Send message"
        }
        className="h-7 w-7 rounded-md flex items-center justify-center text-dim hover:text-white hover:bg-ink-700/60 disabled:opacity-40 disabled:cursor-not-allowed"
      >
        {I.arrow({ size: 13 })}
      </button>
    </div>
  );
});
export const Composer = React.memo(ComposerInner);

/* ── Resume review banner ────────────────────────────────── */

function ResumeReviewBanner({
  state,
  onResume,
}: {
  state: ReviewInterruptedState;
  onResume: () => void;
}) {
  const label = (() => {
    switch (state.phase) {
      case "context":
        return "Review interrupted before context gather started";
      case "round":
        return `Round ${state.round} of ${state.totalRounds} was interrupted`;
      case "merge":
        return `Merge for round ${state.round} was interrupted`;
      case "between_rounds":
        return `Round ${state.round + 1} of ${state.totalRounds} ready to dispatch`;
      case "final":
        return "Final plan ready";
    }
  })();
  const buttonLabel = state.phase === "final" ? "Apply" : "Resume";
  return (
    <div className="px-6 py-2 flex items-center justify-between border-t border-amber-500/20 bg-amber-500/5">
      <span className="text-[12px] text-amber-300 truncate">{label}</span>
      <Btn kind="primary" size="sm" onClick={onResume}>
        {buttonLabel}
      </Btn>
    </div>
  );
}

/* ── Mock task data (non-Tauri preview) ───────────────────── */

const TASK_MESSAGES: TaskMessage[] = [
  { who: "user", t: "14m ago", text: "Rewrite the payments module to use the new ledger primitives. Goal is double-entry posting with strict per-tenant ordering." },
  { who: "asst", t: "14m ago", model: "claude-sonnet-4.5", queen: true, text: "I scanned the working tree (47 files, 3 services). Before I draft a plan I need a few answers -- see the questions card below." },
  // Mock questions message. This does NOT populate pendingQuestions
  // through the reducer, so the QuestionsDock may not appear in dev
  // mode. If the dock is needed for manual testing, inject a
  // structured_questions event or explicitly set pendingQuestions.
  {
    who: "questions",
    questions: [
      {
        id: "webhook-signing",
        kind: "choice",
        title: "Which webhook signing scheme should we use for v2?",
        sub: "Affects both producer + verifier surfaces.",
        options: [
          { id: "hmac-sha256", label: "HMAC-SHA256", hint: "Industry standard, simple verifier" },
          { id: "ed25519", label: "Ed25519", hint: "Asymmetric, allows public verification" },
        ],
      },
      {
        id: "idempotency-scope",
        kind: "choice",
        title: "Where should idempotency keys be scoped?",
        options: [
          { id: "tenant", label: "Per-tenant", recommended: true },
          { id: "global", label: "Global" },
        ],
      },
    ],
  },
  { who: "user", t: "12m ago", text: "Answers submitted. Hit it." },
  { who: "asst", t: "11m ago", model: "claude-sonnet-4.5", queen: true, text: "Drafting plan: 4 milestones, 27 features, ~14h estimated. Sending to hivemind for review before we commit features.json." },
  { who: "review" },
  { who: "asst", t: "6m ago", model: "claude-sonnet-4.5", queen: true, text: "Hivemind flagged 2 issues in R1: (a) idempotency keys not scoped to tenant, (b) no compensating action for partially-applied transfers. R2 confirmed both, plus suggested a saga pattern. I've folded both into the plan -- final draft ready." },
  { who: "user", t: "5m ago", text: "Looks good. Show me M1 before we launch." },
  { who: "asst", t: "4m ago", model: "claude-sonnet-4.5", queen: true, reasoning: "The user wants to see M1 before launching. Let me summarize the key features in a clear, scannable format.", reasoningDurationMs: 15000, text: "**M1 -- Foundations** (8 features, ~3h):\n• tenant-scoped idempotency keys\n• ledger schema + migrations\n• double-entry posting primitive\n• audit log append-only store\n• webhook event envelope v2\n• HMAC verifier with replay window\n• rate limit token bucket\n• health probes (auth-bypassed)\n\nReady to launch as a swarm." },
];

const TASK_THREADS: Record<string, TaskMessage[]> = {
  "t-1": [
    { who: "user", t: "1h ago", text: "I want to add JWT clock skew tolerance. What's a sensible design? Production has at least 4 issuers and we hit auth failures on token boundaries." },
    { who: "asst", t: "1h ago", model: "claude-opus-4.1", text: "Two questions before I draft a design:\n\n1. Is the skew bidirectional or only past-side (token issued slightly in the future)?\n2. Is per-issuer tolerance acceptable, or do you want one global value?" },
  ],
  "t-2": [
    { who: "user", t: "3h ago", text: "I have an N+1 in `OrderRepo.fetchAll`. The trace shows 1 select on orders then one per order for line items. What's the cleanest fix in our setup (sqlx + postgres)?" },
    { who: "asst", t: "3h ago", model: "claude-sonnet-4.5", text: "Two clean options:\n\n1. **Single join + manual grouping** -- fastest, but you assemble structs in code.\n2. **Two queries with `IN (?...)`** -- one for orders, one for line items keyed by order id. Slightly more roundtrips, much cleaner code.\n\nGiven your repo style I'd go with option 2. Want me to do it as a task?" },
    { who: "user", t: "3h ago", text: "Yeah, do it. Use the enhance hivemind for review." },
    { who: "review" },
    { who: "asst", t: "2h ago", model: "claude-sonnet-4.5", text: "Done. PR draft on `fix/order-repo-nplus1` -- both review rounds clean after one revision (Opus caught a bug where empty IDs would issue an `IN ()` query). Tests added covering the empty-orders case." },
  ],
  "t-3": [
    { who: "user", t: "5h ago", text: "Stripe webhook idempotency keys -- what should we key on?" },
    { who: "asst", t: "5h ago", model: "gpt-5-codex", text: "Use the Stripe `event.id` as the dedupe key with a 7-day TTL (Stripe's own retry window).\n\nStore as `(event_id, processed_at)` in a `webhook_dedup` table; on receive, `INSERT … ON CONFLICT DO NOTHING` and only process if the row was newly inserted. Cleanup job nightly drops rows older than 7 days." },
    { who: "user", t: "5h ago", text: "Ship it as a small change." },
    { who: "asst", t: "4h ago", model: "gpt-5-codex", text: "Merged on `feat/webhook-idempotency`. Migration + handler change + integration test against a fixture event replayed 3×." },
  ],
  "t-4": [
    { who: "user", t: "1d ago", text: "On successful login, should we rehash the user's password if our argon2id cost params have changed?" },
    { who: "asst", t: "1d ago", model: "claude-sonnet-4.5", text: "Yes -- rehash on successful login whenever the stored hash's cost params drift from current policy. It's the standard upgrade-on-touch pattern: cheap (you already have the plaintext at that moment) and self-healing." },
    { who: "user", t: "1d ago", text: "Implement it." },
    { who: "asst", t: "1d ago", model: "claude-sonnet-4.5", text: "Done on `feat/argon2-rehash-on-login`. Added `needs_rehash(hash) -> bool` helper and call it after `verify`. Test covers both directions (drift up, no drift)." },
  ],
  "t-5": [
    { who: "user", t: "1d ago", text: "How do we migrate mobile push tokens from APNs-only to APNs + FCM without dropping deliveries?" },
    { who: "asst", t: "1d ago", model: "deepseek-v3.2", text: "Two-phase rollout:\n\n1. **Dual-write** -- clients register with both APNs and FCM. Server stores both tokens.\n2. **Flip read** -- once parity ≥ 99.5% across an active cohort, switch the send path to prefer FCM with APNs fallback.\n\nKeep dual-write on for at least 30 days post-flip in case you need to revert." },
  ],
};

/* ── Task list ────────────────────────────────────────────── */

const now = Date.now();
const TASK_LIST: TaskListItem[] = [
  { id: "t-now", group: "Active", title: "Rewrite payments to ledger primitives", project: "payments", model: "claude-sonnet-4.5", phase: "plan", when: "14m", preview: "Plan drafted · awaiting your launch -- 4 milestones, 27 features.", active: true, isSwarm: true, swarmName: "payments-rewrite", sortOrder: 0, createdAt: now - 14 * 60_000 },
  { id: "t-1", group: "Active", title: "JWT clock skew tolerance design", project: "auth-service", model: "claude-opus-4.1", phase: "questions", when: "1h", preview: "2 questions waiting on you — needs your answers.", sortOrder: 1, createdAt: now - 60 * 60_000 },
  { id: "t-2", group: "Today", title: "N+1 in OrderRepo.fetchAll", project: "auth-service", model: "claude-sonnet-4.5", phase: "implement-done", when: "3h", preview: "PR draft on fix/order-repo-nplus1 · both review rounds clean.", sortOrder: 2, createdAt: now - 3 * 3600_000 },
  { id: "t-3", group: "Today", title: "Stripe webhook idempotency keys", project: "payments", model: "gpt-5-codex", phase: "implement-done", when: "5h", preview: "Use the event id as the dedupe key with a 7-day TTL.", sortOrder: 3, createdAt: now - 5 * 3600_000 },
  { id: "t-4", group: "Yesterday", title: "Argon2id rehash on login", project: "auth-service", model: "claude-sonnet-4.5", phase: "implement-done", when: "1d", preview: "Rehash whenever cost params drift from current policy.", sortOrder: 4, createdAt: now - 24 * 3600_000 },
  { id: "t-5", group: "Yesterday", title: "Mobile push token migration", project: "mobile", model: "deepseek-v3.2", phase: "implement-done", when: "1d", preview: "Two-phase: dual-write APNs+FCM, flip read at parity ≥ 99.5%.", sortOrder: 5, createdAt: now - 30 * 3600_000 },
  { id: "t-6", group: "This week", title: "Tenant isolation strategy", project: "payments", model: "claude-opus-4.1", phase: "implement-done", when: "3d", preview: "arch-council vote 3–1: schema-per-tenant.", sortOrder: 6, createdAt: now - 3 * 86400_000 },
  { id: "t-7", group: "This week", title: "Webhook retry backoff curve", project: "webhooks", model: "gpt-5-codex", phase: "implement-done", when: "6d", preview: "Exponential w/ jitter, 1m → 6h, max 14 attempts then DLQ.", sortOrder: 7, createdAt: now - 6 * 86400_000 },
];

const PHASE_TONE: Record<string, { label: string; dot: string; text: string }> = {
  intake: { label: "intake", dot: "bg-muted", text: "text-muted" },
  questions: { label: "needs answer", dot: "bg-honey-400 pulse-amber", text: "text-honey-300" },
  plan: { label: "planning", dot: "bg-honey-400", text: "text-honey-300" },
  "plan-ready": { label: "plan ready", dot: "bg-emerald-400 pulse-green", text: "text-emerald-300" },
  review: { label: "hivemind review", dot: "bg-honey-400", text: "text-honey-300" },
  implement: { label: "running", dot: "bg-emerald-400 pulse-green", text: "text-emerald-300" },
  "implement-done": { label: "done", dot: "bg-emerald-400", text: "text-emerald-300" },
};

const PHASE_ORDER: Record<string, number> = {
  intake: 0,
  questions: 1,
  plan: 2,
  "plan-ready": 3,
  review: 4,
  implement: 5,
  "implement-done": 6,
};

const TERMINAL_PHASES = new Set(["implement-done"]);

/* ── TasksSidebar ─────────────────────────────────────────── */

interface TaskGroup {
  key: string;
  label: string;
  items: TaskListItem[];
}

export const TasksSidebar = ({
  activeId,
  onPick,
  onNewTask,
  onDeleteTask,
  onMarkDone,
  onRenameTask,
  extraTasks,
  streamingTaskIds,
  awaitingInputTaskIds,
  projectFilter,
  onProjectFilterChange,
  sortMode,
  onSortModeChange,
  onDragEnd,
}: {
  activeId: string;
  onPick?: (t: TaskListItem) => void;
  onDeleteTask?: (id: string) => void;
  onMarkDone?: (id: string) => void;
  onRenameTask?: (id: string, title: string) => void;
  onNewTask?: () => void;
  extraTasks?: TaskListItem[];
  streamingTaskIds?: Record<string, boolean>;
  awaitingInputTaskIds?: Record<string, AwaitingInputKind>;
  projectFilter: string;
  onProjectFilterChange: (value: string) => void;
  sortMode: "newest" | "oldest" | "status";
  onSortModeChange: (value: "newest" | "oldest" | "status") => void;
  onDragEnd?: (activeId: string, overId: string, activeGroup: string, overGroup: string) => void;
}) => {
  const [q, setQ] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const renameInputRef = useRef<HTMLInputElement>(null);
  const markingDoneRef = useRef<Record<string, boolean>>({});
  const [openDropdown, setOpenDropdown] = useState<"filter" | "sort" | null>(null);

  const startRename = useCallback((id: string) => {
    const task = (extraTasks || []).find((t) => t.id === id);
    setRenamingId(id);
    setRenameValue(task?.title || "");
  }, [extraTasks]);

  const commitRename = useCallback(() => {
    if (renamingId && renameValue.trim()) {
      onRenameTask?.(renamingId, renameValue.trim());
    }
    setRenamingId(null);
    setRenameValue("");
  }, [renamingId, renameValue, onRenameTask]);

  const cancelRename = useCallback(() => {
    setRenamingId(null);
    setRenameValue("");
  }, []);

  // Focus the rename input when it appears
  useEffect(() => {
    if (renamingId) {
      requestAnimationFrame(() => renameInputRef.current?.select());
    }
  }, [renamingId]);

  const { registerTaskActions } = useContextMenu();

  useEffect(() => {
    registerTaskActions({
      onDelete: (id: string) => onDeleteTask?.(id),
      onMarkDone: (id: string) => onMarkDone?.(id),
      onRename: (id: string) => startRename(id),
    });
    return () => registerTaskActions(null);
  }, [onDeleteTask, onMarkDone, startRename, registerTaskActions]);

  const taskList = isTauri() ? (extraTasks || []) : TASK_LIST;

  const projectOptions = useMemo(() => {
    const projects = new Set<string>();
    for (const t of (taskList || [])) {
      const key = workspaceKey(t);
      if (key) projects.add(key);
    }
    return Array.from(projects).sort((a, b) =>
      workspaceLabel(a).localeCompare(workspaceLabel(b))
    );
  }, [taskList]);

  const filtered = useMemo(() => {
    const ql = q.trim().toLowerCase();
    return taskList.filter((t) => {
      // Apply project filter
      if (projectFilter !== "__all__") {
        if (workspaceKey(t) !== projectFilter) return false;
      }

      // Apply search query
      if (ql) {
        if (
          !t.title.toLowerCase().includes(ql) &&
          !t.preview.toLowerCase().includes(ql) &&
          !t.project.toLowerCase().includes(ql)
        ) return false;
      }

      return true;
    });
  }, [q, taskList, projectFilter]);

  const sortFn = useMemo(() => {
    if (sortMode === "newest") {
      return (a: TaskListItem, b: TaskListItem) =>
        (b.createdAt ?? 0) - (a.createdAt ?? 0)
        || (a.sortOrder ?? 0) - (b.sortOrder ?? 0)
        || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
    }
    if (sortMode === "oldest") {
      return (a: TaskListItem, b: TaskListItem) =>
        (a.createdAt ?? 0) - (b.createdAt ?? 0)
        || (a.sortOrder ?? 0) - (b.sortOrder ?? 0)
        || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
    }
    // "status" — sort by phase (active phases first, then done)
    return (a: TaskListItem, b: TaskListItem) => {
      const pa = PHASE_ORDER[a.phase] ?? 99;
      const pb = PHASE_ORDER[b.phase] ?? 99;
      if (pa !== pb) return pa - pb;
      return (a.sortOrder ?? 0) - (b.sortOrder ?? 0)
        || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
    };
  }, [sortMode]);

  const groups = useMemo((): TaskGroup[] => {
    const order = ["Active", "Today", "Yesterday", "This week", "Older"];
    const m: Record<string, TaskListItem[]> = {};
    filtered.forEach((t) => {
      let g: string;
      if (typeof t.createdAt !== "number") {
        g = t.group;
      } else if (streamingTaskIds?.[t.id] || !TERMINAL_PHASES.has(t.phase)) {
        g = "Active";
      } else {
        g = timeGroup(t.createdAt);
      }
      (m[g] ||= []).push(t);
    });
    const extra = Object.keys(m).filter((g) => !order.includes(g));
    return [...order, ...extra]
      .filter((g) => m[g]?.length)
      .map((g) => ({
        key: g,
        label: g,
        items: m[g].slice().sort(sortFn),
      }));
  }, [filtered, streamingTaskIds, sortFn]);

  const groupItemIds = useMemo(
    () => groups.map((g) => g.items.map((t) => t.id)),
    [groups],
  );

  const taskGroupMap = useMemo(() => {
    const m = new Map<string, string>();
    for (const g of groups) {
      for (const t of g.items) m.set(t.id, g.key);
    }
    return m;
  }, [groups]);

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const isDragDisabled = true; // All sort modes override manual order; DnD is disabled

  const handleDragEnd = useCallback(
    (event: DragEndEvent) => {
      const { active, over } = event;
      if (!over || active.id === over.id) return;
      const activeGroup = taskGroupMap.get(String(active.id)) ?? "";
      const overGroup = taskGroupMap.get(String(over.id)) ?? "";
      if (activeGroup !== overGroup) return;
      onDragEnd?.(String(active.id), String(over.id), activeGroup, overGroup);
    },
    [taskGroupMap, onDragEnd],
  );

  const renderTaskCard = (t: TaskListItem) => {
    const tone = PHASE_TONE[t.phase] || PHASE_TONE.intake;
    const sel = t.id === activeId;
    const isStreaming = streamingTaskIds?.[t.id];
    const isDone = t.phase === "implement-done";
    const wsKey = workspaceKey(t);
    const wsColor = wsKey ? workspaceColor(wsKey) : null;
    const awaitingKind = awaitingInputTaskIds?.[t.id];
    const showAwaitingBadge = !!awaitingKind && !sel && renamingId !== t.id;
    const awaitingLabel = (() => {
      switch (awaitingKind) {
        case "questions":
        case "swarm-questions":
          return "needs answer";
        case "swarm-plan-ready":
          return "ready to launch";
        case "plan-ready":
          return "ready to implement";
        default:
          return "";
      }
    })();
    const awaitingAria =
      awaitingKind === "plan-ready" || awaitingKind === "swarm-plan-ready"
        ? "Plan ready — awaiting your action"
        : "Awaiting your answer";
    return (
      <div
        data-ctx-task-id={t.id}
        data-ctx-task-phase={t.phase}
        className={`relative overflow-visible group/item w-full text-left pl-4 pr-2 py-2 rounded-md border transition-colors ${
          sel
            ? "bg-honey-500/8 border-honey-500/30"
            : "bg-transparent border-transparent hover:bg-ink-800 hover:border-line"
        }`}
      >
        <span
          className={`absolute left-1.5 top-1/2 -translate-y-1/2 w-1.5 h-1.5 rounded-full ${isStreaming ? "bg-cyan-400 pulse-cyan" : tone.dot}`}
        />
        {showAwaitingBadge && (
          <div
            aria-label={awaitingAria}
            title={awaitingLabel}
            className="pointer-events-none absolute top-0 right-0 z-10 flex items-center gap-1 px-2 py-0.5 rounded-bl-md rounded-tr-md bg-honey-500/15 border border-honey-500/30 shadow-[0_0_10px_rgba(245,185,25,.2)]"
          >
            <span className="w-1.5 h-1.5 rounded-full bg-honey-400 pulse-amber shrink-0" />
            <span className="text-[10px] font-semibold text-honey-300 tracking-wide uppercase whitespace-nowrap">
              {awaitingLabel}
            </span>
          </div>
        )}
        <button
          onClick={() => onPick?.(t)}
          className="w-full text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950"
        >
          <div className={`flex items-center mb-1 ${showAwaitingBadge ? "pr-28" : ""}`}>
            {renamingId === t.id ? (
              <input
                ref={renameInputRef}
                value={renameValue}
                onChange={(e) => setRenameValue(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") { e.preventDefault(); commitRename(); }
                  if (e.key === "Escape") { e.preventDefault(); cancelRename(); }
                  e.stopPropagation();
                }}
                onBlur={commitRename}
                onClick={(e) => e.stopPropagation()}
                className="text-[11.5px] flex-1 min-w-0 bg-ink-700 border border-honey-500/40 rounded px-1.5 py-0.5 text-white focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 focus:border-honey-500/70"
              />
            ) : (
              <span
                className={`text-[11.5px] truncate flex-1 ${
                  sel
                    ? "text-honey-100 font-semibold"
                    : "text-white/90 font-medium"
                }`}
              >
                {t.title}
              </span>
            )}
          </div>
          <div className="text-[11px] text-muted line-clamp-2 leading-snug">
            {t.preview}
          </div>
          <div className="flex items-center gap-1.5 mt-1 text-[10.5px]">
            <span className={`font-mono ${tone.text}`}>
              {tone.label}
            </span>
            {(t.createdAt || t.when) && (
              <>
                <span className="text-line">&middot;</span>
                <span className="text-dim font-mono shrink-0">
                  {relativeTime(t.createdAt, t.when)}
                </span>
              </>
            )}
            <span className="text-line">&middot;</span>
            <span className="font-mono text-blue-300/80 ml-auto">
              {t.model.includes("/") ? t.model.split("/").slice(1).join("/") : t.model}
            </span>
            {t.isSwarm && (
              <>
                <span className="text-line">&middot;</span>
                <span className="inline-flex items-center gap-1 text-honey-300">
                  {I.crown({ size: 9 })} swarm
                </span>
              </>
            )}
          </div>
        </button>


      </div>
    );
  };

  return (
    <aside className="w-[300px] shrink-0 border-r border-line bg-ink-900/60 flex flex-col min-h-0">
      <div className="px-3 pt-3 pb-2 shrink-0">
        <Btn
          kind="primary"
          size="md"
          icon={I.plus({ size: 13 })}
          className="w-full justify-center"
          onClick={onNewTask}
        >
          New Task
        </Btn>
      </div>
      <div className="px-3 pb-2 shrink-0">
        <Input
          icon={I.search({ size: 12 })}
          placeholder={"Search tasks…"}
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
      </div>
      {/* Filter & Sort toolbar */}
      <div className="px-3 pb-1.5 flex items-center justify-between gap-1.5">
        {/* Project filter dropdown */}
        <div className="relative">
          <button
            onClick={() => setOpenDropdown((o) => o === "filter" ? null : "filter")}
            aria-haspopup="listbox"
            aria-expanded={openDropdown === "filter"}
            className={`h-6 px-2 rounded-md flex items-center gap-1.5 text-[10.5px] font-medium transition-colors ${
              projectFilter !== "__all__"
                ? "text-honey-300 bg-honey-500/10 hover:bg-honey-500/15"
                : "text-dim hover:text-white hover:bg-ink-700/60"
            }`}
          >
            {I.filter({ size: 11 })}
            <span className="truncate max-w-[120px]">
              {projectFilter === "__all__"
                ? "All projects"
                : workspaceLabel(projectFilter)}
            </span>
            {I.chevD({ size: 10 })}
          </button>
          {openDropdown === "filter" && (
            <>
              <div className="fixed inset-0 z-40" onClick={() => setOpenDropdown(null)} />
              <div className="absolute top-full left-0 mt-1 z-50 w-48 bg-ink-800 border border-line rounded-lg shadow-2xl max-h-80 overflow-y-auto py-1">
                {/* All option */}
                <button onClick={() => { onProjectFilterChange("__all__"); setOpenDropdown(null); }}
                  className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${projectFilter === "__all__" ? "text-honey-300" : "text-white/80"}`}>
                  {projectFilter === "__all__" && I.check({ size: 11, className: "text-honey-400" })}
                  <span className={projectFilter === "__all__" ? "" : "pl-[19px]"}>All projects</span>
                </button>
                {/* Divider */}
                {projectOptions.length > 0 && <div className="border-t border-line my-1" />}
                {/* Each project */}
                {projectOptions.map((key) => (
                  <button key={key} onClick={() => { onProjectFilterChange(key); setOpenDropdown(null); }}
                    className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${projectFilter === key ? "text-honey-300" : "text-white/80"}`}>
                    {projectFilter === key && I.check({ size: 11, className: "text-honey-400" })}
                    <span className={`flex items-center gap-1.5 ${projectFilter === key ? "" : "pl-[19px]"}`}>
                      <span className={`w-2 h-2 rounded-full shrink-0 ${workspaceColor(key).dot}`} />
                      <span className="truncate">{workspaceLabel(key)}</span>
                    </span>
                  </button>
                ))}
              </div>
            </>
          )}
        </div>

        {/* Sort dropdown */}
        <div className="relative">
          <button
            onClick={() => setOpenDropdown((o) => o === "sort" ? null : "sort")}
            aria-haspopup="listbox"
            aria-expanded={openDropdown === "sort"}
            className="h-6 px-2 rounded-md flex items-center gap-1.5 text-[10.5px] font-medium text-dim hover:text-white hover:bg-ink-700/60 transition-colors"
          >
            {I.sort({ size: 11 })}
            <span>{sortMode === "newest" ? "New" : sortMode === "oldest" ? "Old" : "Status"}</span>
          </button>
          {openDropdown === "sort" && (
            <>
              <div className="fixed inset-0 z-40" onClick={() => setOpenDropdown(null)} />
              <div className="absolute top-full right-0 mt-1 z-50 w-36 bg-ink-800 border border-line rounded-lg shadow-2xl overflow-hidden py-1">
                {(["newest", "oldest", "status"] as const).map((mode) => (
                  <button key={mode} onClick={() => { onSortModeChange(mode); setOpenDropdown(null); }}
                    className={`w-full text-left px-3 py-1.5 text-[11.5px] hover:bg-ink-700/60 flex items-center gap-2 ${sortMode === mode ? "text-honey-300" : "text-white/80"}`}>
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
      <div className="flex-1 min-h-0 overflow-auto pb-3">
        <DndContext
          sensors={sensors}
          collisionDetection={closestCenter}
          onDragEnd={handleDragEnd}
          modifiers={[restrictToVerticalAxis, restrictToParentElement]}
        >
          {groups.map((g, gi) => (
            <div key={g.key} className="pb-1">
              <div className="px-3.5 py-1.5 text-[10px] uppercase tracking-[.16em] text-dim font-semibold flex items-center gap-2 sticky top-0 bg-ink-900/95 backdrop-blur z-10">
                <span>{g.label}</span>
                <span className="text-line">&middot;</span>
                <span className="font-mono normal-case tracking-normal">
                  {g.items.length}
                </span>
              </div>
              <SortableContext items={groupItemIds[gi]} strategy={verticalListSortingStrategy}>
                <div className="px-1.5 space-y-0.5">
                  {g.items.map((t) => (
                    <SortableTaskItem
                      key={t.id}
                      id={t.id}
                      disabled={isDragDisabled || !!streamingTaskIds?.[t.id]}
                    >
                      {renderTaskCard(t)}
                    </SortableTaskItem>
                  ))}
                </div>
              </SortableContext>
            </div>
          ))}
        </DndContext>
      </div>
    </aside>
  );
};

/* ── TasksScreen ──────────────────────────────────────────── */

export const TasksScreen = ({
  go,
  swarm,
  prefill,
}: {
  go: GoFn;
  swarm?: any;
  prefill?: string;
}) => {
  const isSwarmTask = !!swarm;
  const { project, setProject, projects, addProject } = useProject();
  const toast = useErrorToast();

  /* ── Provider-supplied runtime state + actions ──────────── */
  const {
    tasks,
    localTasks,
    activeId,
    hivemindOptions,
    defaultModel,
    streamingTaskIds,
    awaitingInputTaskIds,
    setActiveTask,
    updateTask,
    setLocalTasks,
    setDraft,
    createTask,
    submitMessage,
    stopTask,
    deleteTask,
    triggerReviewForTask,
    retryReview,
    implementPlan,
    answerQuestions,
    submitSwarmAnswers,
    skipSwarmQuestions,
    resumeReview,
    armFeaturesRefresh,
  } = useTaskRuntime();

  /** Task-scoped "retry in progress" state. Only the currently-retrying
   *  task's button shows the disabled/pending label; switching tasks while
   *  a retry is in flight leaves other tasks unaffected. */
  const [retryingTaskId, setRetryingTaskId] = useState<string | null>(null);

  /* ── Derived "active task" view ───────────────────────────── */
  const active: TaskRuntimeState = useMemo(
    () => tasks[activeId] ?? makeInitialTaskState(activeId, defaultModel),
    [tasks, activeId, defaultModel],
  );

  /* ── UI-only state (non-task-keyed) ──────────────────────── */
  const [launching, setLaunching] = useState(false);
  const [requestingFeatures, setRequestingFeatures] = useState(false);
  const [showReasoning, setShowReasoning] = useState(loadShowReasoning);
  const [showToolCalls, setShowToolCalls] = useState(loadShowToolCalls);

  /* ── Group mode + drag-and-drop state ─────────────────── */
  const VALID_SORT_MODES = ["newest", "oldest", "status"] as const;
  type SortMode = typeof VALID_SORT_MODES[number];

  const [projectFilter, setProjectFilter] = useState<string>(() => {
    try {
      return localStorage.getItem("hyvemind:sidebar-project-filter") || "__all__";
    } catch { return "__all__"; }
  });

  const [sortMode, setSortMode] = useState<SortMode>(() => {
    try {
      const raw = localStorage.getItem("hyvemind:sidebar-sort-mode");
      return raw && (VALID_SORT_MODES as readonly string[]).includes(raw)
        ? (raw as SortMode)
        : "newest";
    } catch { return "newest"; }
  });

  useEffect(() => {
    try { localStorage.setItem("hyvemind:sidebar-project-filter", projectFilter); } catch {}
  }, [projectFilter]);

  useEffect(() => {
    try { localStorage.setItem("hyvemind:sidebar-sort-mode", sortMode); } catch {}
  }, [sortMode]);

  // One-time cleanup of the old group-mode key
  useEffect(() => {
    try { localStorage.removeItem("hyvemind:sidebar-group-mode"); } catch {}
  }, []);

  // Migrate old "__none__" filter to "__all__"
  useEffect(() => {
    try {
      const saved = localStorage.getItem("hyvemind:sidebar-project-filter");
      if (saved === "__none__") {
        localStorage.setItem("hyvemind:sidebar-project-filter", "__all__");
        setProjectFilter("__all__");
      }
    } catch {}
  }, []);

  // Reset stale project filter when selected project no longer exists
  useEffect(() => {
    if (projectFilter === "__all__") return;
    if (!isTauri()) return;
    if (localTasks.length === 0) return;
    const exists = localTasks.some((t) => workspaceKey(t) === projectFilter);
    if (!exists) setProjectFilter("__all__");
  }, [localTasks, projectFilter]);

  const handleDragEnd = useCallback(
    (dragActiveId: string, overId: string, activeGroup: string, overGroup: string) => {
      if (activeGroup !== overGroup) return;
      setLocalTasks((prev) => {
        const groupTaskIds = prev
          .filter((t) => t.group === activeGroup)
          .sort((a, b) => (a.sortOrder ?? 0) - (b.sortOrder ?? 0))
          .map((t) => t.id);
        return reorderInGroup(prev, groupTaskIds, dragActiveId, overId);
      });
    },
    [setLocalTasks],
  );

  const [pendingImages, setPendingImages] = useState<ImageAttachment[]>([]);
  const pendingImagesRef = useRef<ImageAttachment[]>([]);
  pendingImagesRef.current = pendingImages;

  // ── Attached files (@-mention picker output) ──
  const [attachedFiles, setAttachedFiles] = useState<string[]>([]);
  const addAttachedFile = useCallback((p: string) => {
    setAttachedFiles((prev) => {
      if (prev.includes(p)) return prev;
      if (prev.length >= 20) {
        console.warn("File attachment limit (20) reached");
        return prev;
      }
      return [...prev, p];
    });
  }, []);
  const removeAttachedFile = useCallback(
    (p: string) => setAttachedFiles((prev) => prev.filter((x) => x !== p)),
    [],
  );

  /* ── Composer is now memoized and owns its own draft state. The parent
   *  reaches in via `composerApiRef` when it needs the current text
   *  (send / send-on-failure-restore). */
  const composerApiRef = useRef<ComposerHandle>(null);

  /* ── Restore per-task project path on switch ────────────── */
  /* NOTE: this effect is declared BEFORE the sync effect below so that on
   * an `activeId` change the picker is repositioned first (in the same
   * render commit cycle); otherwise the sync effect would fire against the
   * stale picker value, then restore would change it, producing a frame of
   * flicker. */
  const { defaultProjectPath } = useDefaults();
  const defaultProjectPathRef = useRef(defaultProjectPath);
  defaultProjectPathRef.current = defaultProjectPath;
  useEffect(() => {
    if (!isTauri()) return;
    const cur = tasks[activeId];
    const taskItem = localTasks.find((t) => t.id === activeId);
    const explicit = cur?.projectPath || taskItem?.projectPath || "";
    // Fall back to the default project when the task has none. Do NOT inherit
    // the previously-selected project — that's the silent-wrong-folder bug.
    const resolved = explicit || defaultProjectPathRef.current || "";

    if (!resolved) {
      // No saved path and no default → clear the picker so the user notices
      // they need to pick one, instead of silently inheriting the prior task.
      if (project !== null) setProject(null);
      return;
    }

    // `pathForCompare` (not raw `===`) — on Windows the saved path may
    // carry forward slashes while the project's cwd has backslashes (or
    // vice versa), and drive-letter case can differ. Without normalization
    // the lookup spuriously fails and we'd `projectFromPath` a duplicate
    // entry on every task switch.
    const key = pathForCompare(resolved);
    const existing = projects.find((p) => pathForCompare(p.cwd) === key);
    if (existing) {
      if (!project || pathForCompare(project.cwd) !== key) setProject(existing);
    } else {
      const p = projectFromPath(resolved);
      addProject(p);
      setProject(p);
    }

    // Back-fill the task's own projectPath when we had to fall back to a
    // default, so subsequent switches don't re-trigger the fallback dance
    // and so call sites that read `cur.projectPath` get a stable value.
    if (!explicit) {
      setLocalTasks((prev) => prev.map((t) =>
        t.id === activeId
          ? { ...t, projectPath: resolved, project: workspaceLabel(resolved) }
          : t,
      ));
      updateTask(activeId, (t) => ({ ...t, projectPath: resolved }));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId]);

  /* ── Sync ProjectPicker → active task projectPath ───────── */
  const prevProjectRef = useRef(project?.cwd);
  useEffect(() => {
    if (!isTauri()) return;
    const cwd = project?.cwd;
    const projectChanged = prevProjectRef.current !== cwd;
    prevProjectRef.current = cwd;
    if (!cwd) return;

    const cur = tasks[activeId];
    const item = localTasks.find((t) => t.id === activeId);
    const needsBackfill = !(cur?.projectPath || item?.projectPath);

    if (!projectChanged && !needsBackfill) return;

    setLocalTasks((prev) => prev.map((t) =>
      t.id === activeId
        ? { ...t, projectPath: cwd, project: workspaceLabel(cwd) }
        : t,
    ));
    updateTask(activeId, (t) => ({ ...t, projectPath: cwd }));
  }, [project?.cwd, activeId, updateTask, setLocalTasks, tasks, localTasks]);

  /* ── Header / display values (derived from active) ──────── */
  const taskListData = isTauri() ? localTasks : TASK_LIST;
  const activeTask = useMemo(
    () => taskListData.find((t) => t.id === activeId),
    [activeId, taskListData],
  );
  const isPrep = isSwarmTask || activeTask?.id === "t-now";
  const mockMessages: TaskMessage[] = isTauri()
    ? []
    : isSwarmTask || activeId === "t-now"
      ? TASK_MESSAGES
      : TASK_THREADS[activeId] || [
          { who: "user" as const, t: activeTask?.when || "", text: activeTask?.title || "" },
          { who: "asst" as const, t: activeTask?.when || "", model: activeTask?.model || "claude-sonnet-4.5", text: activeTask?.preview || "Task complete." },
        ];
  const headerTitle = isSwarmTask
    ? swarm.name
    : activeTask?.title || (isTauri() ? "New Task" : "Task");
  const configModel = isTauri() ? active.model : "claude-sonnet-4.5";
  const headerModel = isSwarmTask ? "claude-sonnet-4.5" : configModel || activeTask?.model || "";
  const headerModelRef = useRef(headerModel);
  headerModelRef.current = headerModel;
  const displayModel = headerModel
    ? headerModel.trim().includes("/")
      ? headerModel.trim().split("/").slice(1).join("/").trim()
      : headerModel.trim()
    : null;
  const taskHivemind = isTauri() ? active.hivemind : null;
  const taskThinking = isTauri() ? active.thinking : "high";
  const taskSessionId = active.sessionId;
  const taskMessages = active.messages;
  const taskStreaming = active.streaming;
  const streamPhase = active.streamPhase;
  const retryStatus = active.retryStatus;
  const taskError = active.error;
  const taskQueueState = active.queueState;
  const planText = active.planText;
  const pendingQuestions = active.pendingQuestions;
  const autoMode = active.autoMode;
  const taskPhase = active.phase;
  const sessionUsage = active.sessionUsage;
  const liveTps = active.liveTps;

  // Prioritize live TPS during streaming, fall back to final usage or 0.
  const effectiveTokPerSec =
    taskStreaming && liveTps != null
      ? liveTps
      : (sessionUsage?.tokPerSec || 0);
  // Hivemind review state for the active task, sourced from the
  // singleton store (one global `hivemind-progress` listener shared
  // across all subscribers — see `hivemindEventStore.ts`).
  const taskReviewState = useHivemindReviewState(`task:${activeId}`);
  const { mode: reviewDockMode, expand: expandReviewDock, collapse: collapseReviewDock } = useReviewDockMode(taskReviewState);
  // Screen-scoped merged-plan modal state. Hoisted out of the dock panels
  // so the modal's lifetime is decoupled from `reviewDockMode` swaps and
  // other parent re-mounts (the dock panel that owned the modal was
  // unmounted by the 5s auto-collapse, dragging the modal with it).
  const [activeMergedPlan, setActiveMergedPlan] = useState<
    { jobId: string; round: number; text: string; subtitle?: string } | null
  >(null);
  // Clear the merged-plan modal when the user switches tasks. The modal is
  // transient and per-job; carrying it across tasks would mis-attribute it.
  useEffect(() => {
    setActiveMergedPlan(null);
  }, [activeId]);
  const headerPhase = isSwarmTask ? "plan" : isTauri() ? taskPhase : (activeTask?.phase || "plan");

  // Use live messages when Tauri is running, otherwise mock
  const messages = isTauri() ? taskMessages : mockMessages;

  /* ── Focus-resync ────────────────────────────────────────── */
  useEffect(() => {
    if (!isTauri()) return;
    const onFocus = async () => {
      const cur = tasks[activeId];
      if (!cur) return;
      try {
        const snap = await ipc.getTaskState(activeId, cur.sessionId);
        const msgs = snap.messages_json
          ? (JSON.parse(snap.messages_json) as TaskMessage[])
          : undefined;
        updateTask(activeId, (t) =>
          applyTaskEvent(
            t,
            { kind: "resync", messages: msgs, sessionAlive: snap.session_alive },
            defaultModel,
          ),
        );
      } catch (e) {
        console.warn("getTaskState failed", e);
      }

      // Embedded-review reconciliation moved to TaskRuntimeProvider, which has
      // a window-focus handler that drives advance_to_merge / merge_stuck /
      // ended decisions across ALL active tasks (not just the visible one) and
      // also runs on a 5s interval while any review is active.
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId, updateTask, defaultModel]);

  // Ref kept in sync with current messages so memoized children (Composer)
  // can read the latest list inside event handlers without re-rendering on
  // every streamed chunk.
  const messagesRef = useRef<TaskMessage[]>(messages);
  messagesRef.current = messages;

  const streamEntries = useMemo(() => toStreamEntries(messages), [messages]);

  // Pinned Nurse card. When the assistant produces one continuous long
  // response (no yield points), the inline nurse card scrolls off-screen
  // below the visible viewport. Pinning the most recent intervention just
  // above the composer keeps it visible regardless of scroll position.
  // Dismissible per-interventionId — a new intervention auto-undismisses.
  const latestNurseEntry = useMemo<NurseEntry | null>(() => {
    for (let i = streamEntries.length - 1; i >= 0; i--) {
      const e = streamEntries[i];
      if (e.kind === "nurse") return e;
    }
    return null;
  }, [streamEntries]);
  const [dismissedNurseId, setDismissedNurseId] = useState<string | null>(null);
  const showStickyNurse =
    latestNurseEntry !== null &&
    latestNurseEntry.interventionId !== dismissedNurseId;

  /* ── Context / token stats ───────────────────────────────── */
  const ctxStats = useMemo(() => {
    if (sessionUsage && sessionUsage.contextWindow > 0) {
      return {
        tokIn: sessionUsage.input,
        tokOut: sessionUsage.output,
        pct: Math.min(100, Math.round(sessionUsage.contextPercent)),
        ctxLabel: sessionUsage.contextWindow >= 1_000_000
          ? `${(sessionUsage.contextWindow / 1_000_000).toFixed(0)}M`
          : `${Math.round(sessionUsage.contextWindow / 1000)}k`,
        tokPerSec: effectiveTokPerSec,
      };
    }

    // Resolution order when Pi can't report a context window:
    //  1) The user's per-task hint (set by ModelBrowser when the model's
    //     metadata is known, e.g. 1M for Gemini 2.5 Pro).
    //  2) The static MODELS catalog by bare id.
    //  3) 200_000 default.
    const bareModel = headerModel.includes("/") ? headerModel.split("/").slice(1).join("/") : headerModel;
    const meta = MODELS.find((m) => m.id === bareModel);
    const hint = active?.contextWindowHint;
    const ctxNum = (typeof hint === "number" && hint > 0)
      ? hint
      : (meta ? parseCtx2(meta.ctx) : 200_000);
    const ctxLabel = ctxNum >= 1_000_000
      ? `${(ctxNum / 1_000_000).toFixed(0)}M`
      : `${Math.round(ctxNum / 1000)}k`;

    if (!isTauri()) {
      const tokIn = 14_820;
      const tokOut = 3_240;
      const used = tokIn + tokOut;
      return { tokIn, tokOut, pct: Math.min(100, Math.round((used / ctxNum) * 100)), ctxLabel, tokPerSec: effectiveTokPerSec };
    }

    // Safety guard: messages is always [] in practice but defensive against
    // hypothetical race or invariant violation.
    const msgs = messages || [];
    let inChars = 0, outChars = 0;
    // Single forward pass: reset counters at each session-starting divider.
    // This scopes counting to messages after the last labeled divider, avoiding
    // the original bug of accumulating tokens across all sessions.
    //
    // Convention (must be maintained by all future transition code):
    //  - Session-starting dividers carry a non-empty dividerLabel,
    //    e.g. "Planning session started", "Hivemind review started",
    //    "Implementation session started". The label is the authoritative
    //    signal — there is no separate isSessionStart flag.
    //  - Dividers that record old session usage ({ dividerSessionId, dividerUsage })
    //    intentionally have no dividerLabel — they are NOT session boundaries.
    //  - If a future code change introduces a new divider with a label that is
    //    NOT a session start, the condition below must be updated accordingly
    //    (e.g. compared against a whitelist of known session-start labels).
    //
    // Known limitation — the first user message of a brand-new task is sent
    // BEFORE the "Planning session started" divider is appended (see
    // submitMessageImpl ~line 1151). That message is counted and then reset to
    // 0 when the divider is hit. The context bar briefly shows 0/0 until the
    // first usage event arrives (typically milliseconds). This is more honest
    // than showing inflated cumulative counts from the previous session.
    for (const m of msgs) {
      if (m.who === "session-divider" && typeof m.dividerLabel === "string" && m.dividerLabel.length > 0) {
        inChars = 0;
        outChars = 0;
        continue;
      }
      if (m.who !== "user" && m.who !== "asst") continue;
      const len = (m.text || "").length + (m.reasoning || "").length
        + (m.tools || []).reduce((s: number, t: any) => s + (t.output || "").length, 0);
      if (m.who === "user") inChars += len;
      else if (m.who === "asst") outChars += len;
    }
    const tokIn = Math.round(inChars / 4);
    const tokOut = Math.round(outChars / 4);
    const used = tokIn + tokOut;
    return { tokIn, tokOut, pct: Math.min(100, Math.round((used / ctxNum) * 100)), ctxLabel, tokPerSec: effectiveTokPerSec };
  }, [messages, headerModel, sessionUsage, effectiveTokPerSec, active?.contextWindowHint]);

  // Tool-call-driven structured outputs land synchronously via
  // `structured_*` chat events, so there's no streaming-extract spinner
  // state to compute any more. Kept as a typed constant so downstream
  // references compile.
  const delimiterLoading: null = null;

  /* ── Image paste handlers ────────────────────────────────── */
  const handlePaste = useCallback((e: React.ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    const imageItems: DataTransferItem[] = [];
    for (let i = 0; i < items.length; i++) {
      if (items[i].type.startsWith("image/")) imageItems.push(items[i]);
    }
    if (imageItems.length === 0) return;
    e.preventDefault();
    for (const item of imageItems) {
      const file = item.getAsFile();
      if (!file) continue;

      // Reject files larger than 5 MB
      if (file.size > 5 * 1024 * 1024) {
        console.warn("Skipping pasted image over 5 MB:", file.size);
        continue;
      }

      const reader = new FileReader();
      reader.onload = () => {
        const dataUrl = reader.result as string;
        const commaIdx = dataUrl.indexOf(",");
        const base64 = dataUrl.slice(commaIdx + 1);
        const mediaType = file.type || "image/png";
        const previewUrl = URL.createObjectURL(file);
        setPendingImages((prev) => {
          if (prev.length >= 10) return prev; // max 10 pending images
          return [
            ...prev,
            { id: crypto.randomUUID(), mediaType, data: base64, previewUrl },
          ];
        });
      };
      reader.onerror = () => {
        console.warn("Failed to read pasted image:", reader.error);
      };
      reader.readAsDataURL(file);
    }
  }, []);

  const removeImage = useCallback((id: string) => {
    setPendingImages((prev) => {
      const img = prev.find((i) => i.id === id);
      if (img) URL.revokeObjectURL(img.previewUrl);
      return prev.filter((i) => i.id !== id);
    });
  }, []);

  /* ── Send a message ──────────────────────────────────────── */
  const handleTaskSend = useCallback(async () => {
    const api = composerApiRef.current;
    const raw = api?.getValue() ?? "";
    const txt = raw.trim();
    if (!txt && pendingImages.length === 0 && attachedFiles.length === 0) return;

    const filesBlock = attachedFiles.length
      ? `\n\n[Attached files]\n${attachedFiles.map((p) => `- ${p}`).join("\n")}`
      : "";
    const finalText = (txt + filesBlock).trim();

    const hasImages = pendingImages.length > 0;
    const sentImages = hasImages ? [...pendingImages] : undefined;
    const sentFiles = attachedFiles.length > 0 ? [...attachedFiles] : undefined;

    api?.setValue("");
    setPendingImages([]);
    setAttachedFiles([]);
    try {
      await submitMessage(activeId, finalText, { images: sentImages });
    } catch (e) {
      // Restore state on failure
      api?.setValue(raw);
      if (sentImages) setPendingImages(sentImages);
      if (sentFiles) setAttachedFiles(sentFiles);
      console.error("Failed to send task message:", e);
    }
  }, [pendingImages, attachedFiles, activeId, submitMessage]);

  const handleTaskStop = useCallback(async () => {
    await stopTask(activeId);
  }, [activeId, stopTask]);

  const handleSetAutoMode = useCallback(
    (mode: AutoMode) => {
      updateTask(activeId, (t) => ({ ...t, autoMode: mode }));
    },
    [activeId, updateTask],
  );

  /* ── Revoke pending image URLs on unmount ──────────────── */
  useEffect(() => {
    return () => {
      pendingImagesRef.current.forEach((img) => URL.revokeObjectURL(img.previewUrl));
    };
  }, []);

  /* ── Clear pending images when active task changes ─────── */
  useEffect(() => {
    setPendingImages((prev) => {
      prev.forEach((img) => URL.revokeObjectURL(img.previewUrl));
      return [];
    });
  }, [activeId]);

  /* ── Clear attached files when active task changes ─────── */
  useEffect(() => {
    setAttachedFiles([]);
  }, [activeId]);

  /* ── Launch swarm ────────────────────────────────────────── */
  const handleLaunchSwarm = useCallback(async () => {
    if (!isTauri()) {
      go("swarm-control", { swarm: { name: headerTitle } });
      return;
    }
    try {
      setLaunching(true);
      updateTask(activeId, (t) => ({ ...t, error: null }));
      const swarmState = await ipc.createSwarm(
        headerTitle,
        `Task: ${headerTitle}`,
        ".",
        {
          primary_model: headerModel,
          scout_model: headerModel,
          use_hivemind_on_scout: true,
          use_hivemind_on_queen: true,
          hivemind_id: taskHivemind || null,
        },
      );
      await ipc.startSwarm(swarmState.id, []);
      go("swarm-control", { swarm: swarmState });
    } catch (e) {
      console.error("Failed to launch swarm:", e);
      updateTask(activeId, (t) => ({ ...t, error: String(e) }));
    } finally {
      setLaunching(false);
    }
  }, [headerTitle, headerModel, taskHivemind, go, activeId, updateTask]);

  /* ── SwarmQuestion modal handlers (Phase 4C) ────────────── */
  const handleSwarmQuestionSubmit = useCallback(
    async (answers: SwarmQuestionAnswer[]) => {
      await submitSwarmAnswers(activeId, answers);
    },
    [activeId, submitSwarmAnswers],
  );

  const handleSwarmQuestionSkip = useCallback(async () => {
    await skipSwarmQuestions(activeId);
  }, [activeId, skipSwarmQuestions]);

  /* ── Questions complete → re-send to Pi ─────────────────── */
  const handleQuestionsComplete = useCallback(
    async (questions: TaskQuestion[], answers: Record<string, any>) => {
      await answerQuestions(activeId, questions, answers);
    },
    [activeId, answerQuestions],
  );

  /* ── Persist question progress across task switches ────── */
  const handleQuestionProgress = useCallback(
    (idx: number, answers: Record<string, string>) => {
      updateTask(activeId, (t) => ({
        ...t,
        pendingQuestionIdx: idx,
        pendingQuestionAnswers: answers,
      }));
    },
    [activeId, updateTask],
  );

  /* ── Implement plan (manual) ────────────────────────────── */
  const handleImplementPlan = useCallback(async () => {
    await implementPlan(activeId, {
      input: ctxStats.tokIn,
      output: ctxStats.tokOut,
      contextPercent: ctxStats.pct,
    });
  }, [activeId, ctxStats, implementPlan]);

  /* ── Launch swarm from planning task ─────────────────────
   * For tasks linked to a swarm (via `swarmId`), the "Launch Swarm" CTA
   * replaces "Implement Plan". The planning agent emits a FEATURES JSON
   * block alongside the human-readable PLAN; we parse it and pass it to
   * `start_swarm` so the backend can skip Queen re-decomposition. */
  const handleLaunchPlanningSwarm = useCallback(async () => {
    const swarmId = active.swarmId;
    if (!swarmId) return;
    if (!active.planText) {
      updateTask(activeId, (t) => ({ ...t, error: "Plan has not been produced yet." }));
      return;
    }
    const features = active.swarmFeatures;
    const milestones = active.swarmMilestones ?? undefined;
    if (!features || features.length === 0) {
      updateTask(activeId, (t) => ({
        ...t,
        error: "Plan is missing a valid FEATURES JSON block. Ask the planning agent to emit one.",
      }));
      return;
    }
    if (!isTauri()) {
      go("swarm-control", { swarm: { id: swarmId, name: active.taskMeta?.title || "Swarm" } });
      return;
    }
    try {
      setLaunching(true);
      updateTask(activeId, (t) => ({ ...t, error: null }));
      await ipc.startSwarm(swarmId, features, milestones);
      go("swarm-control", { swarm: { id: swarmId } });
    } catch (e) {
      const msg = String(e);
      // Already-running is not an error from the user's perspective —
      // they clicked Launch on a swarm that's already in flight, so just
      // navigate to its control page. The backend rejects double-starts
      // by design (see start_swarm guard).
      if (/already running/i.test(msg)) {
        go("swarm-control", { swarm: { id: swarmId } });
      } else {
        console.error("Failed to launch swarm:", e);
        updateTask(activeId, (t) => ({ ...t, error: msg }));
      }
    } finally {
      setLaunching(false);
    }
  }, [active.swarmId, active.planText, active.swarmFeatures, active.swarmMilestones, active.taskMeta, activeId, go, updateTask]);

  /** When a task is swarm-linked and the plan is ready, derive whether the
   *  FEATURES JSON is parseable. Drives the disabled state on the CTA so
   *  the user gets a clear hint if the agent emitted a malformed block. */
  const swarmLaunchDisabledReason = useMemo(() => {
    if (!active.swarmId) return undefined;
    if (!active.planText) return "Waiting for plan…";
    // Active refresh wait — first, regardless of features state, because
    // the user is genuinely waiting on Queen and the missing-features
    // copy below would be misleading during the wait window.
    if (active.pendingFeaturesRefresh) {
      return "Waiting for Queen to refine features after the Hivemind review…";
    }
    // Failure path: button is intentionally enabled so the user can
    // launch with whatever feature set is present. The warning copy is
    // carried by the PlanCard footer (step 5), NOT here.
    if (active.featuresRefreshFailed && (active.swarmFeatures?.length ?? 0) > 0) {
      return undefined;
    }
    const parsed = active.swarmFeatures;
    if (!parsed || parsed.length === 0) {
      if (active.swarmFeaturesError) {
        return `Cannot launch: ${active.swarmFeaturesError}. Send a follow-up asking the agent to re-emit the FEATURES JSON.`;
      }
      return "Plan is missing the FEATURES JSON block";
    }
    return undefined;
  }, [
    active.swarmId,
    active.planText,
    active.swarmFeatures,
    active.swarmFeaturesError,
    active.pendingFeaturesRefresh,
    active.featuresRefreshFailed,
  ]);

  /** Render the Launch Swarm CTA only once the swarm is actually launchable
   *  (features parsed) OR the planner has finished streaming. While the
   *  planner is still emitting the FEATURES block, the "Decomposing
   *  Features…" spinner below the card tells the user something is
   *  happening — we don't surface a disabled button alongside it. If the
   *  planner finishes without producing a parseable FEATURES block, the
   *  button reappears in its disabled state so the user sees the failure
   *  reason instead of nothing.
   *
   *  Also suppressed while `pendingFeaturesRefresh` is true — that's the
   *  window between a Hivemind review completing and Queen re-emitting
   *  `submit_features` against the refined plan. Launching during that
   *  window would ship the pre-review features. */
  const showSwarmLaunchCta = useMemo(() => {
    if (!active.swarmId) return false;
    if (active.pendingFeaturesRefresh) return true;       // always visible while we wait/recover
    const featuresReady = (active.swarmFeatures?.length ?? 0) > 0;
    if (featuresReady) return true;
    return !active.streaming;
  }, [active.swarmId, active.swarmFeatures, active.streaming, active.pendingFeaturesRefresh]);

  const handleHivemindReview = useCallback(async () => {
    await triggerReviewForTask(activeId);
  }, [activeId, triggerReviewForTask]);

  /* ── Recovery: re-emit FEATURES JSON ─────────────────────
   * Surfaced when a swarm-planning task has a plan but no parseable
   * FEATURES (planner truncated, or post-Hivemind merge dropped them).
   * Sends a one-shot follow-up asking the planning agent to re-emit just
   * the FEATURES block; the reducer's streaming parser picks it up and
   * populates `swarmFeatures`, which re-enables Launch Swarm. */
  const handleRequestFeatures = useCallback(async () => {
    if (!active.swarmId || !active.planText) return;
    const prompt =
      "The implementation plan below is the canonical plan to emit features for " +
      "(it incorporates any Hivemind review revisions). Please call the " +
      "`submit_features` tool with the `{features, milestones}` shape your " +
      "planning prompt specifies — including ids, names, descriptions, " +
      "dependencies, and milestone fields — covering exactly this plan. " +
      "Do not include any preamble or commentary.\n\n" +
      "---\n\n" +
      active.planText +
      "\n\n---\n";
    try {
      setRequestingFeatures(true);
      // Flip the UI to "refining" immediately so the failure banner
      // clears and the reducer's done path can re-detect failure cleanly
      // on retry.
      updateTask(activeId, (t) => ({
        ...t,
        error: null,
        pendingFeaturesRefresh: !!t.swarmId,
        featuresRefreshFailed: false,
      }));
      // Arms watchdog + dispatches via queueMicrotask, with a catch that
      // sets featuresRefreshFailed: true on dispatch error. Mirrors the
      // post-Hivemind path in finishReviewFlow.
      armFeaturesRefresh(activeId, prompt);
    } catch (e) {
      console.error("Failed to request FEATURES re-emit:", e);
      // Synchronous failure before the helper could arm — clear the wait
      // state explicitly so we don't strand the UI in "Refining…" with
      // no backend turn to produce a terminal event.
      updateTask(activeId, (t) => ({
        ...t,
        pendingFeaturesRefresh: false,
        featuresRefreshFailed: true,
        error: `Failed to re-emit FEATURES: ${e}`,
      }));
    } finally {
      setRequestingFeatures(false);
    }
  }, [active.swarmId, active.planText, activeId, armFeaturesRefresh, updateTask]);

  /* ── Tab switching: pure pointer flip ─────────────────── */
  const handlePickTask = useCallback((t: TaskListItem) => {
    if (t.id === activeId) return;
    setActiveTask(t.id);
  }, [activeId, setActiveTask]);

  const handleNewTask = useCallback(() => {
    createTask({ setActive: true });
  }, [createTask]);

  const handleDeleteTask = useCallback((id: string) => {
    deleteTask(id);
  }, [deleteTask]);

  const markingDoneRef = useRef<Record<string, boolean>>({});
  const handleMarkDone = useCallback((id: string) => {
    // Guard against re-entry (rapid clicks, stale closures)
    if (markingDoneRef.current[id]) return;
    markingDoneRef.current[id] = true;
    setTimeout(() => { delete markingDoneRef.current[id]; }, 1000);

    // The actual Pi-session kill happens in TaskRuntimeProvider's
    // implement-done-transition effect — both manual mark-done and the
    // agent's `submit_task_complete` path funnel through that one site.
    updateTask(id, (t) => ({
      ...t,
      phase: "implement-done",
      messages: t.messages.some((m) => m.who === "complete")
        ? t.messages
        : [...t.messages, { who: "complete" as const, text: "Manually marked as done" }],
      streaming: false,
    }));
    setLocalTasks((prev) =>
      prev.map((t) =>
        t.id === id ? { ...t, phase: "implement-done" } : t
      )
    );
  }, [updateTask, setLocalTasks]);

  const handleRenameTask = useCallback((id: string, title: string) => {
    // Set titleEdited so the planning agent's TASK_META block won't
    // overwrite the user's custom title on subsequent plan turns.
    // The description/preview is still updated by the agent.
    setLocalTasks((prev) =>
      prev.map((t) => (t.id === id ? { ...t, title, titleEdited: true } : t))
    );
  }, [setLocalTasks]);

  /* ── Prefill prompt (e.g. from "Fix" button) ───────────── */
  const prefillHandled = useRef<string | undefined>();
  useEffect(() => {
    if (!prefill || prefill === prefillHandled.current) return;
    prefillHandled.current = prefill;
    const newId = createTask({ setActive: true });
    setDraft(newId, prefill);
  }, [prefill, createTask, setDraft]);

  // Quietly silence unused-variable warnings for queue/pending state we may surface later.
  void taskQueueState;
  void planText;
  void taskThinking;

  // Phase 4C — modal only pops once the Queen has finished its turn so we
  // never interrupt mid-stream. The reducer keeps the question list intact
  // across the streaming→done transition; this gate just chooses *when* to
  // surface it. Swarm-linked tasks only — the Queen-planning prompt is the
  // sole producer of ``swarm-question`` blocks.
  const swarmQuestions = active.pendingSwarmQuestions ?? null;
  const showSwarmQuestionModal =
    !!isSwarmTask &&
    !taskStreaming &&
    Array.isArray(swarmQuestions) &&
    swarmQuestions.length > 0;

  return (
    <div className="h-full flex relative">
      <TasksSidebar
        activeId={activeId}
        extraTasks={localTasks}
        onPick={handlePickTask}
        onNewTask={handleNewTask}
        onDeleteTask={handleDeleteTask}
        onMarkDone={handleMarkDone}
        onRenameTask={handleRenameTask}
        streamingTaskIds={streamingTaskIds}
        awaitingInputTaskIds={awaitingInputTaskIds}
        projectFilter={projectFilter}
        onProjectFilterChange={setProjectFilter}
        sortMode={sortMode}
        onSortModeChange={setSortMode}
        onDragEnd={handleDragEnd}
      />

      <div className="flex-1 min-w-0 flex flex-col relative">
        {/* Task header -- title, phase pipeline, status */}
        <div className="px-6 py-3.5 border-b border-line bg-ink-850/50 shrink-0 flex items-center gap-3">
          <ProjectPicker />
          <div className="w-px h-8 bg-line" />

          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              {isSwarmTask && (
                <span className="inline-flex items-center gap-1.5 text-honey-300 bg-honey-500/12 border border-honey-500/30 rounded px-1.5 h-5 text-[10.5px] uppercase tracking-wider font-semibold shrink-0">
                  {I.crown({ size: 10 })} Queen
                </span>
              )}
              <span className="text-[14px] font-semibold text-white truncate">
                {headerTitle}
              </span>
            </div>
            <div className="mt-1.5">
              <TaskPipeline
                active={headerPhase}
                isPlanReady={taskPhase === "plan-ready"}
                isDone={taskPhase === "implement-done"}
                hivemind={taskHivemind}
                hivemindDone={
                  taskReviewState
                    ? taskReviewState.status === "completed" || taskReviewState.status === "failed"
                    : false
                }
              />
            </div>
          </div>

          {isSwarmTask && (
            <Btn
              kind="outline"
              size="sm"
              icon={I.list({ size: 12 })}
            >
              features.json
            </Btn>
          )}

          <TaskConfigChip
            model={configModel}
            onModelChange={(m) => {
              updateTask(activeId, (t) => ({ ...t, model: m }));
              setLocalTasks((prev) => prev.map((t) => t.id === activeId ? { ...t, model: m } : t));
              // Also update the global default so new tasks use this model
              if (isTauri()) {
                ipc.setDefaultModel(m).catch((err) => {
                  toast.error("Failed to sync default model", err);
                });
              }
            }}
            hivemind={isTauri() ? taskHivemind : (taskHivemind ?? "enhance")}
            onHivemindChange={(h) => {
              updateTask(activeId, (t) => ({ ...t, hivemind: h }));
              setLocalTasks((prev) => prev.map((t) => t.id === activeId ? { ...t, hivemind: h } : t));
              // Also update the global default so new tasks use this hivemind
              if (isTauri()) {
                ipc.setDefaultHivemind(h || "").catch((err) => {
                  toast.error("Failed to sync default hivemind", err);
                });
              }
            }}
            onThinkingChange={(t) => updateTask(activeId, (rt) => ({ ...rt, thinking: t }))}
            hivemindOptions={isTauri() ? hivemindOptions : null}
          />
        </div>

        {/* Conversation */}
        {taskError && (
          <div className="shrink-0 px-6 pt-2">
            {/*
             * a11y: transient task errors pop into the conversation flow
             * (e.g. send failures, swarm launch errors). `role="alert"`
             * implies assertive live + atomic, so SR interrupts and reads
             * the full message immediately. The container only renders
             * while `taskError` is truthy, so each new error remounts and
             * re-announces.
             */}
            <div
              role="alert"
              aria-live="assertive"
              className="max-w-[860px] mx-auto px-4 py-2 text-[12px] text-red-400 bg-red-500/10 border border-red-500/20 rounded-md"
            >
              {taskError}
            </div>
          </div>
        )}
        <ActivityStream
          conversationKey={activeId}
          entries={streamEntries}
          showReasoning={showReasoning}
          showToolCalls={showToolCalls}
          streaming={taskStreaming}
          tailLimit={1000}
          emptyState={{ primary: "Start a conversation to begin." }}
          onImplementPlan={
            taskPhase === "plan-ready" && !active.swarmId ? handleImplementPlan : undefined
          }
          onHivemindReview={
            taskPhase === "plan-ready" && !!taskHivemind && !active.reviewCompleted
              ? handleHivemindReview
              : undefined
          }
          onLaunchSwarm={
            taskPhase === "plan-ready" && !!active.swarmId && showSwarmLaunchCta
              ? handleLaunchPlanningSwarm
              : undefined
          }
          onRequestFeatures={handleRequestFeatures}
          planCard={{
            implementing: taskPhase === "implement",
            autoMode: autoMode === "full" && taskPhase === "plan-ready" && !active.swarmId,
            launching,
            launchDisabledReason:
              !!active.swarmId && showSwarmLaunchCta ? swarmLaunchDisabledReason : undefined,
            showImplement: taskPhase === "plan-ready" && !active.swarmId,
            showLaunchSwarm: taskPhase === "plan-ready" && !!active.swarmId && showSwarmLaunchCta,
            showHivemindReview:
              taskPhase === "plan-ready" && !!taskHivemind && !active.reviewCompleted,
            showRequestFeatures:
              taskPhase === "plan-ready" &&
              !!active.swarmId &&
              !!active.planText &&
              ((active.swarmFeatures?.length ?? 0) === 0 || !!active.featuresRefreshFailed),
            requestingFeatures,
            pendingFeaturesRefresh: active.pendingFeaturesRefresh,
            featuresRefreshFailed: active.featuresRefreshFailed,
          }}
          inFlightOverlay={{
            retryStatus: retryStatus ?? undefined,
            streamPhase: streamPhase ?? undefined,
          }}
          delimiterLoading={delimiterLoading}
        />

        {active.reviewInterrupted && taskPhase === "review" && !active.streaming && (
          <ResumeReviewBanner
            state={active.reviewInterrupted}
            onResume={() => resumeReview(activeId)}
          />
        )}
        {canRetryErroredReviewState(active) && (
          <div className="px-6 py-2 flex items-center justify-between border-t border-red-500/20 bg-red-500/5">
            {/*
             * a11y: the retry banner surfaces a review-failed error. The
             * error text gets `role="alert"` so SR announces it when the
             * banner mounts; the retry button is separately focusable.
             */}
            <span role="alert" aria-live="assertive" className="text-[12px] text-red-400 truncate">{taskError}</span>
            <Btn
              kind="primary"
              size="sm"
              disabled={retryingTaskId === activeId}
              onClick={async () => {
                const taskId = activeId;
                setRetryingTaskId(taskId);
                try {
                  await retryReview(taskId);
                } catch (e) {
                  console.error("[review] Retry review failed", e);
                  updateTask(taskId, (t) => ({
                    ...t,
                    error: `Retry review failed: ${e}`,
                  }));
                } finally {
                  setRetryingTaskId((cur) => (cur === taskId ? null : cur));
                }
              }}
            >
              {retryingTaskId === activeId ? "Retrying…" : "Retry review"}
            </Btn>
          </div>
        )}

        {/* Hivemind review live dock — sits just above the composer.
            Visible while a review runs; collapses to a one-line summary
            5s after a terminal status. The user can re-expand it from
            the collapsed bar at any time. */}
        {reviewDockMode !== "hidden" && taskReviewState && (
          <div className="shrink-0 border-t border-line bg-ink-900/80">
            <div className="max-w-[860px] mx-auto px-6 py-2.5">
              {reviewDockMode === "expanded" ? (
                <HivemindReviewLivePanel
                  state={taskReviewState}
                  sourceLabel={taskHivemind ?? undefined}
                  // Multi-round Tasks reviews dispatch each round as a
                  // separate `start_review` job, so the panel's own
                  // `state.roundOrder.length` only ever sees the current
                  // round. Override with the configured total from the
                  // task's review-progress snapshot so the footer pill
                  // reads "Round 2/2" instead of "Round 2/1".
                  totalRoundsOverride={active.reviewProgress?.totalRounds}
                  onCancelReview={
                    active.activeReviewJobId
                      ? () => {
                          const jobId = active.activeReviewJobId;
                          if (!jobId) return;
                          ipc.cancelReview(jobId).catch((e) => console.error(e));
                        }
                      : undefined
                  }
                  onCollapse={
                    taskReviewState.status !== "running" ? collapseReviewDock : undefined
                  }
                  onViewMergedPlan={({ round, text }) =>
                    setActiveMergedPlan({
                      jobId: taskReviewState.jobId,
                      round,
                      text,
                      subtitle: taskHivemind ?? undefined,
                    })
                  }
                />
              ) : (
                <HivemindReviewCollapsedBar
                  state={taskReviewState}
                  sourceLabel={taskHivemind ?? undefined}
                  onExpand={expandReviewDock}
                  onViewMergedPlan={({ round, text }) =>
                    setActiveMergedPlan({
                      jobId: taskReviewState.jobId,
                      round,
                      text,
                      subtitle: taskHivemind ?? undefined,
                    })
                  }
                />
              )}
            </div>
          </div>
        )}

        {/* Screen-level merged-plan modal. Rendered as a sibling of the
            dock block (outside the `reviewDockMode` ternary) so the modal
            survives the dock-mode swap that fires 5s after a review
            completes — the prior in-panel modal got unmounted with its
            parent and the user perceived it as "flashes and disappears". */}
        <MergedPlanModal
          open={activeMergedPlan != null}
          title={
            activeMergedPlan
              ? `Merged plan \u2014 Round ${activeMergedPlan.round}`
              : "Merged plan"
          }
          subtitle={activeMergedPlan?.subtitle}
          planText={activeMergedPlan?.text ?? ""}
          onClose={() => setActiveMergedPlan(null)}
        />

        {/* Pending questions dock — sits between the activity stream and
            the chat composer. Visible only while `pendingQuestions` has
            entries (cleared by the reducer once the user submits). In
            practice questions only fire during planning (before any
            Hivemind review runs), so QuestionsDock and HivemindReviewLivePanel
            shouldn't typically coexist. If they do, the review dock is
            rendered above and questions dock just above the composer. */}
        {pendingQuestions && pendingQuestions.length > 0 && (
          <QuestionsDock
            questions={pendingQuestions}
            initialIdx={active.pendingQuestionIdx ?? 0}
            initialAnswers={active.pendingQuestionAnswers ?? {}}
            onProgress={handleQuestionProgress}
            onSubmit={(answers) => {
              try {
                const map: Record<string, string> = {};
                for (const a of answers) map[a.id] = a.answer;
                handleQuestionsComplete(pendingQuestions, map);
              } catch (err) {
                console.error("Failed to submit answers:", err);
                // Dock unmounts via pendingQuestions clearing — if IPC
                // fails the user message may not append. The try/catch
                // prevents a silent failure; the toast gives the user
                // visible feedback.
                toast.error("Failed to submit answers. Please try again.", err);
              }
            }}
          />
        )}

        {/* Pinned Nurse card — sits between any other docks and the
            composer so the user always sees the most recent intervention
            even if the assistant is mid-stream through a viewport-spanning
            response. Dismissible via the × button; auto-undismisses when a
            newer intervention arrives (different interventionId). */}
        {showStickyNurse && latestNurseEntry && (
          <div className="shrink-0 border-t border-line bg-ink-900/95">
            <div className="max-w-[860px] mx-auto px-6 py-2 relative">
              <button
                type="button"
                aria-label="Dismiss nurse intervention"
                onClick={() => setDismissedNurseId(latestNurseEntry.interventionId)}
                className="absolute top-3 right-7 z-10 w-5 h-5 rounded-full border border-line bg-ink-900 flex items-center justify-center text-dim hover:text-white hover:bg-ink-800 transition-colors"
                title="Dismiss"
              >
                {I.x({ size: 10 })}
              </button>
              <NurseMessage entry={latestNurseEntry} />
            </div>
          </div>
        )}

        {/* Chat composer */}
        <div
          className={`shrink-0 ${pendingQuestions && pendingQuestions.length > 0 ? "border-t-0" : "border-t border-line"} bg-ink-900`}
        >
          <div className="max-w-[860px] mx-auto px-6 py-2.5">
            {pendingImages.length > 0 && (
              <div className="flex gap-2 flex-wrap px-1 pb-1.5">
                {pendingImages.map((img) => (
                  <div key={img.id} className="relative group">
                    <img
                      src={img.previewUrl}
                      alt="Pending"
                      className="w-16 h-16 object-cover rounded-lg border border-line"
                    />
                    <button
                      onClick={() => removeImage(img.id)}
                      className="absolute -top-1.5 -right-1.5 w-5 h-5 rounded-full bg-ink-900 border border-line flex items-center justify-center text-dim hover:text-white hover:bg-red-500/80 transition-colors opacity-0 group-hover:opacity-100"
                    >
                      {I.x({ size: 10 })}
                    </button>
                  </div>
                ))}
              </div>
            )}
            {attachedFiles.length > 0 && (
              <div
                data-testid="attached-files"
                className="flex gap-1.5 flex-wrap px-1 pb-1.5"
              >
                {attachedFiles.map((p) => (
                  <span
                    key={p}
                    className="inline-flex items-center gap-1.5 h-6 px-2 rounded-md bg-ink-800 border border-line text-[11px] text-white/85"
                  >
                    {I.doc({ size: 11, className: "text-honey-400/80" })}
                    <span
                      className="font-mono truncate max-w-[260px]"
                      title={p}
                    >
                      {p}
                    </span>
                    <button
                      aria-label={`Remove ${p}`}
                      onClick={() => removeAttachedFile(p)}
                      className="text-dim hover:text-red-400"
                    >
                      {I.x({ size: 10 })}
                    </button>
                  </span>
                ))}
              </div>
            )}
            <Composer
              ref={composerApiRef}
              activeId={activeId}
              streaming={taskStreaming}
              headerModel={headerModel}
              autoMode={autoMode}
              hasHivemind={!!taskHivemind}
              pendingImagesCount={pendingImages.length}
              attachedFilesCount={attachedFiles.length}
              projectPath={(active.projectPath || project?.cwd) || null}
              onAddAttachedFile={addAttachedFile}
              messagesRef={messagesRef}
              onSend={handleTaskSend}
              onStop={handleTaskStop}
              onSetAutoMode={handleSetAutoMode}
              onPaste={handlePaste}
              reviewLocked={
                taskReviewState?.phase === "merge"
              }
              reviewLockReason={
                taskReviewState?.phase === "merge"
                  ? "Merging reviewer feedback — use 'Cancel review' to interrupt."
                  : undefined
              }
              steerableWhileStreaming={
                taskReviewState?.phase === "context"
              }
              questionsPending={!!(pendingQuestions && pendingQuestions.length > 0)}
            />
          </div>
        </div>

        {/* Context status bar */}
        <ActivityFooter
          activeSession={{ sessionId: taskSessionId, model: displayModel || null }}
          ctx={{
            pct: ctxStats.pct,
            label: ctxStats.ctxLabel,
            tokIn: ctxStats.tokIn,
            tokOut: ctxStats.tokOut,
            tokPerSec: ctxStats.tokPerSec,
          }}
          showReasoning={showReasoning}
          showToolCalls={showToolCalls}
          onToggleReasoning={() => {
            setShowReasoning((prev) => {
              const next = !prev;
              try { localStorage.setItem(SHOW_REASONING_KEY, String(next)); } catch { /* noop */ }
              return next;
            });
          }}
          onToggleToolCalls={() => {
            setShowToolCalls((prev) => {
              const next = !prev;
              try { localStorage.setItem(SHOW_TOOL_CALLS_KEY, String(next)); } catch { /* noop */ }
              return next;
            });
          }}
          maxWidthClass="max-w-[860px]"
        />

        {/* Launch bar */}
        {isPrep && (
          <div className="shrink-0 border-t border-line bg-gradient-to-b from-ink-850 to-ink-900">
            <div className="max-w-[860px] mx-auto px-6 py-3 flex items-center gap-3">
              <span className="w-2 h-2 rounded-full bg-emerald-400 pulse-green shrink-0" />
              <div className="min-w-0 flex-1">
                <div className="text-[12.5px] font-semibold text-white leading-tight">
                  {isSwarmTask
                    ? "Swarm ready to start"
                    : "Plan ready for implementation"}
                </div>
                <div className="text-[11px] text-muted mt-0.5 font-mono truncate">
                  {isSwarmTask
                    ? "4 milestones · 27 features · ~14h estimated · workers will be dispatched on launch"
                    : "4 milestones · 27 features · 2 review rounds clean · launch as a swarm to begin"}
                </div>
              </div>
              <Btn kind="ghost" size="sm" icon={I.edit({ size: 12 })}>
                Tweak plan
              </Btn>
              <Btn kind="primary" size="md" icon={I.rocket({ size: 13 })} onClick={handleLaunchSwarm} disabled={launching}>
                {launching ? "Launching..." : isSwarmTask ? "Launch swarm" : "Launch as swarm"}
              </Btn>
            </div>
          </div>
        )}
      </div>
      {showSwarmQuestionModal && swarmQuestions && (
        <SwarmQuestionModal
          questions={swarmQuestions}
          onSubmit={handleSwarmQuestionSubmit}
          onSkip={handleSwarmQuestionSkip}
        />
      )}
    </div>
  );
};
