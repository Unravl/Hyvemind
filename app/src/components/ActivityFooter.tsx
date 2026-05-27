import React from "react";
import { ContextStatusBar } from "./ContextStatusBar";
import type { ActiveSession } from "../lib/streamEntry";

export interface ActivityFooterProps {
  activeSession: ActiveSession | null;
  ctx: {
    pct: number;
    label: string;
    tokIn: number;
    tokOut: number;
    tokPerSec?: number | null;
  };
  showReasoning: boolean;
  showToolCalls: boolean;
  onToggleReasoning: () => void;
  onToggleToolCalls: () => void;
  maxWidthClass?: string;
  isSwarmContext?: boolean;
}

export function ActivityFooter({
  activeSession,
  ctx,
  showReasoning,
  showToolCalls,
  onToggleReasoning,
  onToggleToolCalls,
  maxWidthClass = "max-w-[860px]",
  isSwarmContext = false,
}: ActivityFooterProps) {
  const sessionId = activeSession?.sessionId ?? null;
  const model = activeSession?.model ?? null;
  return (
    <div className="shrink-0 border-t border-line bg-ink-900/60">
      <ContextStatusBar
        sessionId={sessionId}
        modelLabel={model}
        modelTitle={model}
        showReasoning={showReasoning}
        showToolCalls={showToolCalls}
        onToggleReasoning={onToggleReasoning}
        onToggleToolCalls={onToggleToolCalls}
        ctxPct={ctx.pct}
        ctxLabel={ctx.label}
        tokIn={ctx.tokIn}
        tokOut={ctx.tokOut}
        tokPerSec={ctx.tokPerSec}
        maxWidthClass={maxWidthClass}
        isSwarmContext={isSwarmContext}
      />
    </div>
  );
}
