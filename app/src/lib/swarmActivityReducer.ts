import type { SwarmActivityEvent } from "./events";
import type {
  ChatBubbleEntry,
  ErrorEntry,
  SessionMarkerEntry,
  StreamEntry,
} from "./streamEntry";
import type { ToolCallState } from "./types";

export type AgentRole =
  | "scout"
  | "worker"
  | "guard"
  | "hivemind-context"
  | "hivemind-merge";

const AGENT_LABEL: Record<AgentRole, string> = {
  scout: "scout",
  worker: "worker",
  guard: "guard",
  "hivemind-context": "hivemind ctx",
  "hivemind-merge": "hivemind merge",
};

function shortClockTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return "";
  }
}

/** Convert a backend ISO timestamp string into epoch-ms. Falls back to
 *  `Date.now()` when parsing fails so the relative-time label is always
 *  monotonic-ish rather than NaN. */
function epochMs(ts: string): number {
  const n = Date.parse(ts);
  return Number.isFinite(n) ? n : Date.now();
}

export interface AgentDividerItem {
  kind: "agent_divider";
  id: string;
  agent: AgentRole;
  featureId: string;
  sessionId: string;
  model?: string;
  startedAt: string;
  endedAt?: string;
  success?: boolean;
  createdAt: number;
}

export interface AgentBubbleItem {
  kind: "agent_bubble";
  id: string;
  agent: AgentRole;
  featureId: string;
  sessionId: string;
  text: string;
  reasoning?: string;
  reasoningStartedAt?: number;
  reasoningDurationMs?: number;
  tools: ToolCallState[];
  t: string;
  createdAt: number;
}

export interface ErrorItem {
  kind: "error";
  id: string;
  agent: AgentRole;
  featureId: string;
  sessionId: string;
  message: string;
  t: string;
  createdAt: number;
}

// Canonical render-time end signal for this session. The start divider
// (AgentDividerItem) retains endedAt/success for invariants/testing only;
// toStreamEntries reads this item to emit the phase:"end" marker.
export interface AgentEndItem {
  kind: "agent_end_marker";
  id: string;
  agent: AgentRole;
  featureId: string;
  sessionId: string;
  model?: string; // copied FROM the divider (agent_end event lacks model)
  endedAt: string;
  success: boolean;
  createdAt: number;
}

export type ActivityItem =
  | AgentDividerItem
  | AgentBubbleItem
  | ErrorItem
  | AgentEndItem;

export interface SwarmActivityState {
  items: ActivityItem[];
  /** session_id → most recent bubble id, so deltas append to the right bubble */
  bubbleBySession: Record<string, string>;
  /** session_id → most recent open divider id, so agent_end / errors can patch it */
  dividerBySession: Record<string, string>;
  /** bubble id → bubble item (mirror of items[idx] for O(1) lookup) */
  bubblesById: Record<string, AgentBubbleItem>;
  /** bubble id → index in items, for O(1) patch */
  bubbleIndexById: Record<string, number>;
  /** divider id → index in items, for O(1) agent_end patch */
  dividerIndexById: Record<string, number>;
  /** monotonic counter so generated ids never collide across renders */
  seq: number;
  /**
   * Highest backend `seq` ever applied. Used to drop duplicates if the
   * same event somehow reaches the reducer twice (e.g. a transient
   * double-listener bug). Defence-in-depth — the store's hydration +
   * live-buffer dedup is the primary guard. `0` means "no event applied
   * yet"; backend seq numbers start at 1.
   */
  lastAppliedSeq: number;
}

export const initialActivityState: SwarmActivityState = {
  items: [],
  bubbleBySession: {},
  dividerBySession: {},
  bubblesById: {},
  bubbleIndexById: {},
  dividerIndexById: {},
  seq: 0,
  lastAppliedSeq: 0,
};

function shortTime(ts: string): string {
  try {
    return new Date(ts).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return "";
  }
}

/** Replace `items[idx]` immutably via slice + assign. Cheaper than
 * `state.items.map(...)` because it skips N callback invocations and
 * comparisons — only the array backing store is copied. */
function replaceItemAt(items: ActivityItem[], idx: number, next: ActivityItem): ActivityItem[] {
  const copy = items.slice();
  copy[idx] = next;
  return copy;
}

function ensureBubble(
  state: SwarmActivityState,
  evt: SwarmActivityEvent,
): { state: SwarmActivityState; bubble: AgentBubbleItem } {
  const existingId = state.bubbleBySession[evt.session_id];
  if (existingId) {
    const bubble = state.bubblesById[existingId];
    if (bubble) return { state, bubble };
  }
  const seq = state.seq + 1;
  const id = `bubble-${seq}`;
  const bubble: AgentBubbleItem = {
    kind: "agent_bubble",
    id,
    agent: evt.agent,
    featureId: evt.feature_id,
    sessionId: evt.session_id,
    text: "",
    tools: [],
    t: shortTime(evt.timestamp),
    createdAt: epochMs(evt.timestamp),
  };
  const items = [...state.items, bubble];
  return {
    state: {
      ...state,
      seq,
      items,
      bubbleBySession: { ...state.bubbleBySession, [evt.session_id]: id },
      bubblesById: { ...state.bubblesById, [id]: bubble },
      bubbleIndexById: { ...state.bubbleIndexById, [id]: items.length - 1 },
    },
    bubble,
  };
}

function patchBubble(
  state: SwarmActivityState,
  bubbleId: string,
  patch: (b: AgentBubbleItem) => AgentBubbleItem,
): SwarmActivityState {
  const idx = state.bubbleIndexById[bubbleId];
  if (idx === undefined) return state;
  const old = state.bubblesById[bubbleId];
  if (!old) return state;
  const next = patch(old);
  if (next === old) return state;
  return {
    ...state,
    items: replaceItemAt(state.items, idx, next),
    bubblesById: { ...state.bubblesById, [bubbleId]: next },
  };
}

export function applyActivityEvent(
  state: SwarmActivityState,
  evt: SwarmActivityEvent,
): SwarmActivityState {
  // Idempotency guard. Backend assigns monotonic per-swarm `seq`s on
  // persisted+emitted events; if the same event reaches us twice (e.g.
  // a transient double-listener leak) we silently drop the second copy
  // instead of double-applying it and producing duplicate dividers or
  // duplicated text deltas. Events without a `seq` (synthetic / legacy)
  // fall through and are applied as-is.
  if (typeof evt.seq === "number" && evt.seq <= state.lastAppliedSeq) {
    return state;
  }
  const next = applyActivityEventInner(state, evt);
  if (next === state) return state;
  if (typeof evt.seq === "number" && evt.seq > next.lastAppliedSeq) {
    return { ...next, lastAppliedSeq: evt.seq };
  }
  return next;
}

function applyActivityEventInner(
  state: SwarmActivityState,
  evt: SwarmActivityEvent,
): SwarmActivityState {
  switch (evt.kind) {
    case "agent_start": {
      const seq = state.seq + 1;
      const id = `divider-${seq}`;
      const divider: AgentDividerItem = {
        kind: "agent_divider",
        id,
        agent: evt.agent,
        featureId: evt.feature_id,
        sessionId: evt.session_id,
        model: evt.model,
        startedAt: evt.timestamp,
        createdAt: epochMs(evt.timestamp),
      };
      const items = [...state.items, divider];
      return {
        ...state,
        seq,
        items,
        dividerBySession: { ...state.dividerBySession, [evt.session_id]: id },
        dividerIndexById: { ...state.dividerIndexById, [id]: items.length - 1 },
      };
    }
    // agent_end handler:
    // - Clears bubbleBySession/dividerBySession so subsequent events for this
    //   session don't attach to the old divider/bubble.
    // - Patches the start divider with endedAt + success (kept for invariants
    //   and test assertions — NOT used for render).
    // - Appends a new AgentEndItem at the items tail; this is the canonical
    //   render-time end signal that toStreamEntries emits inline at the
    //   correct chronological position. Assumes backend events arrive in
    //   chronological order (existing reducer-wide invariant).
    // - Early-returns if no matching divider exists, preventing orphan end
    //   markers (also makes a second agent_end for the same session a no-op).
    case "agent_end": {
      const dividerId = state.dividerBySession[evt.session_id];
      const nextBubbleBySession = { ...state.bubbleBySession };
      delete nextBubbleBySession[evt.session_id];
      const nextDividerBySession = { ...state.dividerBySession };
      delete nextDividerBySession[evt.session_id];
      const next: SwarmActivityState = {
        ...state,
        bubbleBySession: nextBubbleBySession,
        dividerBySession: nextDividerBySession,
      };
      if (!dividerId) return next;
      const idx = state.dividerIndexById[dividerId];
      if (idx === undefined) return next;
      const old = state.items[idx];
      if (!old || old.kind !== "agent_divider") return next;

      const seq = state.seq + 1;
      const id = `end-${seq}`;
      const patched: AgentDividerItem = {
        ...old,
        endedAt: evt.timestamp,
        success: evt.success ?? false,
      };
      const endItem: AgentEndItem = {
        kind: "agent_end_marker",
        id,
        agent: patched.agent,
        featureId: patched.featureId,
        sessionId: patched.sessionId,
        model: patched.model, // copied from the start divider
        endedAt: evt.timestamp,
        success: evt.success ?? false,
        createdAt: epochMs(evt.timestamp),
      };
      // Single array copy: patch divider in place and append end marker.
      const items = state.items.slice();
      items[idx] = patched;
      items.push(endItem);
      return { ...next, seq, items };
    }
    case "text": {
      if (!evt.text) return state;
      // Mirror Tasks-view behaviour (processChunkEvent in taskReducer.ts): if
      // the current bubble has tool calls, the model has finished its tool
      // round and this text chunk is the start of a new assistant turn —
      // allocate a fresh bubble so each post-tool reply renders its own
      // bubble at the bottom of the feed.
      const existingId = state.bubbleBySession[evt.session_id];
      const existing = existingId ? state.bubblesById[existingId] : undefined;
      if (existing && existing.tools.length > 0) {
        // Finalize the prior bubble's reasoning duration (if any) so it
        // shows a stable value once it collapses.
        const now = Date.now();
        let finalized: SwarmActivityState = state;
        if (existing.reasoningStartedAt != null) {
          const updated: AgentBubbleItem = {
            ...existing,
            reasoningDurationMs: Math.max(0, now - existing.reasoningStartedAt),
          };
          const idx = state.bubbleIndexById[existing.id];
          if (idx !== undefined) {
            finalized = {
              ...state,
              items: replaceItemAt(state.items, idx, updated),
              bubblesById: { ...state.bubblesById, [existing.id]: updated },
            };
          }
        }
        const seq = finalized.seq + 1;
        const id = `bubble-${seq}`;
        const fresh: AgentBubbleItem = {
          kind: "agent_bubble",
          id,
          agent: evt.agent,
          featureId: evt.feature_id,
          sessionId: evt.session_id,
          text: evt.text,
          tools: [],
          t: shortTime(evt.timestamp),
          createdAt: epochMs(evt.timestamp),
        };
        const items = [...finalized.items, fresh];
        return {
          ...finalized,
          seq,
          items,
          bubbleBySession: {
            ...finalized.bubbleBySession,
            [evt.session_id]: id,
          },
          bubblesById: { ...finalized.bubblesById, [id]: fresh },
          bubbleIndexById: {
            ...finalized.bubbleIndexById,
            [id]: items.length - 1,
          },
        };
      }
      const { state: s2, bubble } = ensureBubble(state, evt);
      return patchBubble(s2, bubble.id, (b) => ({ ...b, text: b.text + evt.text }));
    }
    case "thinking": {
      if (!evt.text) return state;
      const now = Date.now();
      // Mirror Tasks-view behaviour (processThinkingEvent in taskReducer.ts):
      // a new bubble is allocated only when the previous turn is over, which
      // is signalled by the presence of tool calls on the current bubble
      // (matching the rule the `text` case applies above). Text presence is
      // NOT a turn boundary — extended-thinking models interleave thinking
      // and text inside a single response, so `thinking → text → thinking`
      // must accumulate into the same bubble instead of splitting the
      // visible answer in half.
      const existingThinkingId = state.bubbleBySession[evt.session_id];
      const existingThinking = existingThinkingId
        ? state.bubblesById[existingThinkingId]
        : undefined;
      if (existingThinking && existingThinking.tools.length > 0) {
        // Finalize the prior bubble's reasoning duration so it shows a stable
        // value once it collapses (it won't be `isLast` anymore).
        let finalized: SwarmActivityState = state;
        if (existingThinking.reasoningStartedAt != null) {
          const updated: AgentBubbleItem = {
            ...existingThinking,
            reasoningDurationMs: Math.max(0, now - existingThinking.reasoningStartedAt),
          };
          const idx = state.bubbleIndexById[existingThinking.id];
          if (idx !== undefined) {
            finalized = {
              ...state,
              items: replaceItemAt(state.items, idx, updated),
              bubblesById: { ...state.bubblesById, [existingThinking.id]: updated },
            };
          }
        }
        const seq = finalized.seq + 1;
        const id = `bubble-${seq}`;
        const fresh: AgentBubbleItem = {
          kind: "agent_bubble",
          id,
          agent: evt.agent,
          featureId: evt.feature_id,
          sessionId: evt.session_id,
          text: "",
          tools: [],
          reasoning: evt.text,
          reasoningStartedAt: now,
          reasoningDurationMs: 0,
          t: shortTime(evt.timestamp),
          createdAt: epochMs(evt.timestamp),
        };
        const items = [...finalized.items, fresh];
        return {
          ...finalized,
          seq,
          items,
          bubbleBySession: {
            ...finalized.bubbleBySession,
            [evt.session_id]: id,
          },
          bubblesById: { ...finalized.bubblesById, [id]: fresh },
          bubbleIndexById: {
            ...finalized.bubbleIndexById,
            [id]: items.length - 1,
          },
        };
      }
      const { state: s2, bubble } = ensureBubble(state, evt);
      return patchBubble(s2, bubble.id, (b) => {
        const started = b.reasoningStartedAt ?? now;
        return {
          ...b,
          reasoning: (b.reasoning ?? "") + evt.text,
          reasoningStartedAt: started,
          reasoningDurationMs: Math.max(0, now - started),
        };
      });
    }
    case "tool_start": {
      if (!evt.tool_call_id || !evt.tool_name) return state;
      const { state: s2, bubble } = ensureBubble(state, evt);
      return patchBubble(s2, bubble.id, (b) => ({
        ...b,
        tools: [
          ...b.tools,
          { tool_call_id: evt.tool_call_id!, name: evt.tool_name!, output: "", done: false },
        ],
      }));
    }
    case "tool_update": {
      if (!evt.tool_call_id) return state;
      const { state: s2, bubble } = ensureBubble(state, evt);
      return patchBubble(s2, bubble.id, (b) => ({
        ...b,
        tools: b.tools.map((t) =>
          t.tool_call_id === evt.tool_call_id
            ? { ...t, output: t.output + (evt.tool_output ?? "") }
            : t,
        ),
      }));
    }
    case "tool_end": {
      if (!evt.tool_call_id) return state;
      const { state: s2, bubble } = ensureBubble(state, evt);
      return patchBubble(s2, bubble.id, (b) => ({
        ...b,
        tools: b.tools.map((t) =>
          t.tool_call_id === evt.tool_call_id ? { ...t, done: true } : t,
        ),
      }));
    }
    case "error": {
      const seq = state.seq + 1;
      const err: ErrorItem = {
        kind: "error",
        id: `error-${seq}`,
        agent: evt.agent,
        featureId: evt.feature_id,
        sessionId: evt.session_id,
        message: evt.error ?? "unknown error",
        t: shortTime(evt.timestamp),
        createdAt: epochMs(evt.timestamp),
      };
      return { ...state, seq, items: [...state.items, err] };
    }
    default:
      return state;
  }
}

// Single-pass emit. Events are assumed to arrive in chronological order from
// the backend; the reducer appends items at the tail (and end markers at the
// tail when agent_end fires), so array position mirrors event arrival order.
// The exhaustive switch + `never` guard ensures any new ActivityItem kind
// must add a case here or fail to compile.
export function toStreamEntries(
  state: SwarmActivityState,
  featureId?: string,
): StreamEntry[] {
  const entries: StreamEntry[] = [];
  for (let i = 0; i < state.items.length; i++) {
    const item = state.items[i];
    if (featureId && item.featureId !== featureId) continue;
    switch (item.kind) {
      case "agent_divider": {
        const start: SessionMarkerEntry = {
          kind: "session_marker",
          phase: "start",
          surface: "swarm",
          id: `marker-start-${item.id}`,
          agent: item.agent,
          featureId: item.featureId,
          sessionId: item.sessionId,
          model: item.model,
          label: `${AGENT_LABEL[item.agent] ?? item.agent} · ${item.featureId}`,
          t: shortClockTime(item.startedAt),
          createdAt: item.createdAt,
        };
        entries.push(start);
        break;
      }
      case "agent_bubble": {
        const bubble: ChatBubbleEntry = {
          kind: "chat_bubble",
          who: "asst",
          surface: "swarm",
          id: `bubble-${item.id}`,
          agent: item.agent,
          featureId: item.featureId,
          sessionId: item.sessionId,
          text: item.text,
          reasoning: item.reasoning,
          reasoningStartedAt: item.reasoningStartedAt,
          reasoningDurationMs: item.reasoningDurationMs,
          tools: item.tools,
          t: item.t,
          createdAt: item.createdAt,
        };
        entries.push(bubble);
        break;
      }
      case "agent_end_marker": {
        const success = item.success;
        const end: SessionMarkerEntry = {
          kind: "session_marker",
          phase: "end",
          surface: "swarm",
          id: `marker-end-${item.id}`,
          agent: item.agent,
          featureId: item.featureId,
          sessionId: item.sessionId,
          model: item.model,
          success,
          label: `${AGENT_LABEL[item.agent] ?? item.agent} · ended${success ? " ✓" : " ✗"}`,
          t: shortClockTime(item.endedAt),
          createdAt: item.createdAt,
        };
        entries.push(end);
        break;
      }
      case "error": {
        const err: ErrorEntry = {
          kind: "error",
          surface: "swarm",
          id: `error-${item.id}`,
          agent: item.agent,
          featureId: item.featureId,
          sessionId: item.sessionId,
          message: item.message,
          t: item.t,
          createdAt: item.createdAt,
        };
        entries.push(err);
        break;
      }
      default: {
        // Exhaustive — any new ActivityItem kind must have a case above.
        const _unreachable: never = item;
        void _unreachable;
      }
    }
  }
  return entries;
}
