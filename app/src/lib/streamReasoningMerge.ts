import { displayTextOf, type StreamAgent, type StreamEntry } from "./streamEntry";

export interface ReasoningMergeEntry {
  reasoning: string;
  durationMs?: number;
  lastIdx: number;
}

export interface ReasoningRenderPlan {
  lastReasoningIdx: number;
  mergeLeader: Map<number, ReasoningMergeEntry>;
  mergeSkip: Set<number>;
}

export function isReasoningBreaker(entry: StreamEntry): boolean {
  if (entry.kind !== "chat_bubble") return true;
  if (entry.who === "user") return true;
  return false;
}

export function computeReasoningRenderPlan(
  entries: readonly StreamEntry[],
  showToolCalls: boolean,
): ReasoningRenderPlan {
  let lastReasoningIdx = -1;
  for (let i = 0; i < entries.length; i++) {
    const e = entries[i];
    if (e.kind === "chat_bubble" && e.who === "asst" && (e.reasoning?.length ?? 0) > 0) {
      lastReasoningIdx = i;
    }
  }

  const mergeLeader = new Map<number, ReasoningMergeEntry>();
  const mergeSkip = new Set<number>();
  if (!showToolCalls) {
    let current:
      | {
          leader: number;
          agent: StreamAgent | undefined;
          reasoning: string;
          durationMs?: number;
          lastIdx: number;
        }
      | null = null;
    for (let i = 0; i < entries.length; i++) {
      const e = entries[i];

      if (
        current !== null &&
        e.kind === "chat_bubble" &&
        e.who === "asst" &&
        e.agent !== current.agent
      ) {
        current = null;
      }

      if (
        e.kind === "chat_bubble" &&
        e.who === "asst" &&
        (e.reasoning?.length ?? 0) > 0
      ) {
        if (current === null) {
          current = {
            leader: i,
            agent: e.agent,
            reasoning: e.reasoning!,
            durationMs: e.reasoningDurationMs,
            lastIdx: i,
          };
        } else {
          current.reasoning = current.reasoning + "\n\n" + e.reasoning!;
          if (e.reasoningDurationMs != null) {
            current.durationMs = (current.durationMs ?? 0) + e.reasoningDurationMs;
          }
          current.lastIdx = i;
          mergeSkip.add(i);
          mergeLeader.set(current.leader, {
            reasoning: current.reasoning,
            durationMs: current.durationMs,
            lastIdx: current.lastIdx,
          });
        }
      }

      const isBreaker =
        isReasoningBreaker(e) ||
        (e.kind === "chat_bubble" &&
          e.who === "asst" &&
          displayTextOf(e).trim().length > 1);
      if (isBreaker) current = null;
    }
  }

  return { lastReasoningIdx, mergeLeader, mergeSkip };
}

export function hasBreakerAfter(
  entries: readonly StreamEntry[],
  i: number,
): boolean {
  for (let j = i + 1; j < entries.length; j++) {
    const e = entries[j];
    if (isReasoningBreaker(e)) return true;
    if (e.kind === "chat_bubble" && e.who === "asst" && displayTextOf(e).trim().length > 1) {
      return true;
    }
  }
  return false;
}
