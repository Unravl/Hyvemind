import { useEffect, useState } from "react";
import * as ipc from "../../lib/ipc";
import { isTauri } from "../../lib/tauri";
import { formatIpcError } from "../../lib/ipc";
import { Btn } from "../atoms";
import type {
  DecisionChain,
  NurseInterventionRecord,
} from "../../lib/nurseTypes";

interface Props {
  record: NurseInterventionRecord;
}

/**
 * Expanded view of one intervention row. Renders the triggering
 * SessionHealth, the classifier prompt/response (Tier 3) or playbook
 * entry (Tier 2), the dispatch outcome, and 👍/👎 feedback buttons
 * that fire `record_nurse_intervention_feedback`.
 */
export function NurseInterventionDetail({ record }: Props) {
  const [chain, setChain] = useState<DecisionChain | null>(null);
  const [chainErr, setChainErr] = useState<string | null>(null);
  const [prompt, setPrompt] = useState<string | null>(null);
  const [response, setResponse] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<"up" | "down" | null>(null);
  const [feedbackErr, setFeedbackErr] = useState<string | null>(null);
  const [feedbackBusy, setFeedbackBusy] = useState(false);

  useEffect(() => {
    const decisionId = record.decision_id;
    if (!decisionId || !isTauri()) return;
    let cancelled = false;
    (async () => {
      try {
        const c = await ipc.getNurseDecisionChain(decisionId);
        if (!cancelled) setChain(c);
      } catch (err) {
        if (!cancelled) setChainErr(formatIpcError(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [record.decision_id]);

  // Lazy-load classifier capture only when Tier 3.
  useEffect(() => {
    const decisionId = record.decision_id;
    if (!decisionId || record.tier !== "llm" || !isTauri()) return;
    let cancelled = false;
    (async () => {
      try {
        const [p, r] = await Promise.all([
          ipc.getNurseCapture(decisionId, "prompt").catch(() => null),
          ipc.getNurseCapture(decisionId, "response").catch(() => null),
        ]);
        if (!cancelled) {
          setPrompt(p);
          setResponse(r);
        }
      } catch {
        /* IPC errors swallowed — UI gracefully shows nothing */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [record.decision_id, record.tier]);

  const submitFeedback = async (rating: "up" | "down") => {
    if (feedbackBusy) return;
    setFeedbackBusy(true);
    setFeedbackErr(null);
    try {
      await ipc.recordNurseInterventionFeedback({
        intervention_id: record.id,
        rating,
      });
      setFeedback(rating);
    } catch (err) {
      setFeedbackErr(formatIpcError(err));
    } finally {
      setFeedbackBusy(false);
    }
  };

  return (
    <div className="rounded-lg border border-line bg-ink-850 p-3 space-y-3">
      {record.analysis && (
        <Section title="Reasoning">
          <p className="text-[12px] text-slate-200 whitespace-pre-wrap leading-relaxed">
            {record.analysis}
          </p>
        </Section>
      )}

      {record.action_taken?.message && (
        <Section title="Action message">
          <pre className="text-[11px] text-slate-300 bg-ink-900 border border-line rounded p-2 whitespace-pre-wrap break-words font-mono">
            {record.action_taken.message}
          </pre>
        </Section>
      )}

      {record.outcome && (
        <Section title="Outcome">
          <div className="text-[12px] text-muted">{record.outcome}</div>
        </Section>
      )}

      {record.tier === "llm" && (prompt || response) && (
        <Section title="Classifier capture">
          {prompt && (
            <details className="text-[10.5px] text-muted">
              <summary className="cursor-pointer text-honey-300">
                Prompt ({prompt.length.toLocaleString()} chars)
              </summary>
              <pre className="mt-1 bg-ink-900 border border-line rounded p-2 max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono">
                {prompt}
              </pre>
            </details>
          )}
          {response && (
            <details className="text-[10.5px] text-muted mt-1">
              <summary className="cursor-pointer text-honey-300">
                Response ({response.length.toLocaleString()} chars)
              </summary>
              <pre className="mt-1 bg-ink-900 border border-line rounded p-2 max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono">
                {response}
              </pre>
            </details>
          )}
        </Section>
      )}

      {chain && chain.events.length > 0 && (
        <Section title="Decision chain">
          <ul className="text-[10.5px] text-muted space-y-0.5 font-mono">
            {chain.events.map((e, i) => (
              <li key={i} className="flex items-start gap-2">
                <span className="text-dim w-16 shrink-0">
                  {new Date(e.timestamp).toLocaleTimeString()}
                </span>
                <span className="text-honey-300/80 shrink-0">{e.kind}</span>
              </li>
            ))}
          </ul>
        </Section>
      )}
      {chainErr && (
        <div className="text-[11px] text-amber-300/80">
          Decision chain unavailable: {chainErr}
        </div>
      )}

      <div className="flex items-center justify-between pt-2 border-t border-line">
        <div className="text-[10px] text-dim">Was this helpful?</div>
        <div className="flex items-center gap-1.5">
          <Btn
            size="sm"
            kind={feedback === "up" ? "success" : "ghost"}
            onClick={() => submitFeedback("up")}
            loading={feedbackBusy}
            aria-label="Thumbs up"
            aria-pressed={feedback === "up"}
          >
            👍
          </Btn>
          <Btn
            size="sm"
            kind={feedback === "down" ? "danger" : "ghost"}
            onClick={() => submitFeedback("down")}
            loading={feedbackBusy}
            aria-label="Thumbs down"
            aria-pressed={feedback === "down"}
          >
            👎
          </Btn>
        </div>
      </div>
      {feedbackErr && (
        <div className="text-[11px] text-red-300">{feedbackErr}</div>
      )}
    </div>
  );
}

function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section>
      <div className="text-[10px] text-dim uppercase tracking-wider mb-1">
        {title}
      </div>
      {children}
    </section>
  );
}
