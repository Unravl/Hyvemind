import type {
  DetectorSchema,
  DetectorStatsRow,
  Severity,
} from "../../lib/nurseTypes";
import { SEVERITY_DOT } from "./SeverityBadge";

const SEVS: Severity[] = ["info", "warn", "stalled", "critical"];

/**
 * Per-detector activity row. Columns: name + description, total
 * signals raised in range, severity histogram bars, intervention-to-
 * progress latency, false-positive ratio.
 *
 * Enable toggle here always reads from the Default profile — per-
 * profile overrides live in the Profiles tab.
 */
export function NurseDetectorRow({
  schema,
  stats,
  enabledInDefaultProfile,
  onToggleDefault,
  onOpenDetail,
}: {
  schema: DetectorSchema | undefined;
  stats: DetectorStatsRow;
  enabledInDefaultProfile: boolean;
  onToggleDefault: (next: boolean) => void;
  onOpenDetail: (detector: string) => void;
}) {
  const display = schema?.display_name || stats.detector;
  const description = schema?.description || "—";

  return (
    <tr className="border-b border-line last:border-b-0 hover:bg-ink-700/30 transition">
      <td className="px-3 py-2 align-top">
        <button
          onClick={() => onOpenDetail(stats.detector)}
          className="text-[12px] text-white text-left font-medium hover:text-honey-300 transition"
        >
          {display}
        </button>
        <div className="text-[10.5px] text-muted line-clamp-2 max-w-md">
          {description}
        </div>
      </td>
      <td className="px-3 py-2 align-top text-[12px] text-white font-mono">
        {stats.total}
      </td>
      <td className="px-3 py-2 align-top w-40">
        <SeverityHistogram by_severity={stats.by_severity} />
      </td>
      <td className="px-3 py-2 align-top text-[11px] text-muted">
        {stats.avg_clear_ms != null
          ? `${Math.round(stats.avg_clear_ms / 1000)}s`
          : "—"}
      </td>
      <td className="px-3 py-2 align-top text-[11px] text-muted">
        {stats.fp_count != null
          ? `${stats.fp_count} fp`
          : "—"}
      </td>
      <td className="px-3 py-2 align-top">
        <label className="inline-flex items-center cursor-pointer gap-2 text-[11px] text-muted">
          <input
            type="checkbox"
            checked={enabledInDefaultProfile}
            onChange={(e) => onToggleDefault(e.target.checked)}
            className="accent-honey-500"
          />
          enabled
        </label>
      </td>
    </tr>
  );
}

function SeverityHistogram({
  by_severity,
}: {
  by_severity: Record<Severity, number>;
}) {
  const max = Math.max(1, ...SEVS.map((s) => by_severity[s] ?? 0));
  return (
    <div className="flex items-end gap-0.5 h-6">
      {SEVS.map((s) => {
        const v = by_severity[s] ?? 0;
        const h = Math.max(2, Math.round((v / max) * 24));
        return (
          <span
            key={s}
            className={`${SEVERITY_DOT[s]} w-2 rounded-sm`}
            style={{ height: `${h}px` }}
            title={`${s}: ${v}`}
          />
        );
      })}
    </div>
  );
}
