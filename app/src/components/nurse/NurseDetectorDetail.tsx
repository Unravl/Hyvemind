import { Modal } from "../atoms";
import type { DetectorSchema, DetectorStatsRow } from "../../lib/nurseTypes";
import { Markdown } from "../Markdown";

/**
 * Modal with the full raised-signal log for one detector, grouped by
 * `dedup_key`. v1 renders the stats row + schema description; the
 * full per-key drill-down lands when `get_nurse_decisions_for_session`
 * is wired across all sessions for a detector. Until then, the modal
 * is a friendly summary so the row click does something useful.
 */
export function NurseDetectorDetail({
  detector,
  schema,
  stats,
  onClose,
}: {
  detector: string | null;
  schema: DetectorSchema | undefined;
  stats: DetectorStatsRow | undefined;
  onClose: () => void;
}) {
  if (!detector) return null;
  return (
    <Modal open onClose={onClose} title={schema?.display_name || detector} wide>
      <div className="space-y-4">
        {schema?.description && (
          <Markdown text={schema.description} variant="assistant" />
        )}
        {stats && (
          <div className="rounded-lg border border-line bg-ink-850 p-3 text-[12px]">
            <div className="grid grid-cols-2 gap-3">
              <Metric label="Total raised" value={stats.total.toString()} />
              <Metric
                label="False positives"
                value={
                  stats.fp_count != null ? stats.fp_count.toString() : "—"
                }
              />
              <Metric
                label="Median clear"
                value={
                  stats.avg_clear_ms != null
                    ? `${Math.round(stats.avg_clear_ms / 1000)}s`
                    : "—"
                }
              />
              <Metric
                label="Severity mix"
                value={Object.entries(stats.by_severity)
                  .filter(([, v]) => v > 0)
                  .map(([k, v]) => `${k}:${v}`)
                  .join(" / ") || "—"}
              />
            </div>
          </div>
        )}
        {schema && schema.tunables.length > 0 && (
          <div className="text-[11px] text-muted">
            This detector has {schema.tunables.length} tunable
            {schema.tunables.length === 1 ? "" : "s"}. Edit them in
            the <span className="text-honey-300">Profiles</span> tab.
          </div>
        )}
        <div className="text-[11px] text-dim">
          Per-key drill-down (group by `dedup_key`) lands when
          `get_nurse_decisions_for_session` is wired across detectors.
        </div>
      </div>
    </Modal>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[10px] text-dim uppercase tracking-wider">
        {label}
      </div>
      <div className="text-[14px] text-white font-mono mt-0.5">{value}</div>
    </div>
  );
}
