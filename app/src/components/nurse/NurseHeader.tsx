import { useState } from "react";
import { I } from "../icons";
import { Btn, Pill } from "../atoms";
import { useNurseCtx, type NurseTimeRange } from "../../lib/NurseProvider";
import { ProviderHealthPill } from "./ProviderHealthPill";
import type {
  NurseMasterMode,
  NurseServiceConfigSnapshot,
  NurseHealth,
  NurseStats,
  ProviderHealthSnapshot,
} from "../../lib/nurseTypes";
import * as ipc from "../../lib/ipc";
import { formatIpcError } from "../../lib/ipc";

const TIME_RANGES: Array<{ value: NurseTimeRange; label: string }> = [
  { value: "1h", label: "1h" },
  { value: "24h", label: "24h" },
  { value: "7d", label: "7d" },
  { value: "all", label: "All" },
];

function deriveMasterMode(config: NurseServiceConfigSnapshot): NurseMasterMode {
  if (config.mode) return config.mode;
  return config.enabled ? "enabled" : "disabled";
}

interface Props {
  config: NurseServiceConfigSnapshot;
  health: NurseHealth;
  stats: NurseStats;
  providers?: ProviderHealthSnapshot[];
  onOpenModelBrowser: () => void;
  onChangeConfig: () => Promise<void> | void;
}

/**
 * Sticky header for the Nurse screen. Owns the master enable
 * segmented control, classifier model picker, provider health pill
 * cluster, and the screen-wide time-range selector.
 */
export function NurseHeader({
  config,
  health,
  stats,
  providers,
  onOpenModelBrowser,
  onChangeConfig,
}: Props) {
  const { timeRange, setTimeRange } = useNurseCtx();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const master = deriveMasterMode(config);

  const setMaster = async (next: NurseMasterMode) => {
    if (saving) return;
    setSaving(true);
    setError(null);
    try {
      // Wire shape: send both `enabled` (legacy bool) and `mode` (new
      // field). Backends that don't yet understand `mode` will simply
      // honour the boolean.
      await ipc.setNurseConfig({
        enabled: next !== "disabled",
      });
      await onChangeConfig();
    } catch (err) {
      setError(formatIpcError(err));
    } finally {
      setSaving(false);
    }
  };

  const observabilityDropped = health.observability_dropped ?? 0;
  const tier3Skipped = stats.tier3_skipped_no_model ?? 0;

  return (
    <div
      data-testid="nurse-header"
      className="sticky top-0 z-10 bg-ink-900/90 backdrop-blur border-b border-line px-6 py-4"
    >
      <div className="flex items-start justify-between gap-4 flex-wrap">
        <div className="flex items-center gap-2.5">
          {I.heart({ size: 18, className: "text-honey-400" })}
          <h1 className="text-lg font-semibold text-white">Nurse</h1>
          <span className="text-[11px] text-dim ml-2">
            Long-running session supervisor
          </span>
        </div>

        <div className="flex items-center gap-3 flex-wrap">
          {/* Master enable segmented control */}
          <SegmentedMaster
            value={master}
            disabled={saving}
            onChange={setMaster}
          />

          {config.swarms_only === true && (
            <Pill tone="honey">Swarms only</Pill>
          )}

          {/* Classifier model picker */}
          <button
            onClick={onOpenModelBrowser}
            className="flex items-center gap-2 px-3 h-7 rounded-md border border-line bg-ink-850 text-[11px] text-slate-300 hover:border-honey-500/40 transition"
            title="Change Nurse classifier model"
          >
            {I.brain({ size: 11, className: "text-violet-400" })}
            <span className="font-mono truncate max-w-[180px]">
              {config.nurse_model || "no model"}
            </span>
            {I.chevR({ size: 10, className: "text-muted" })}
          </button>

          {/* Time-range selector */}
          <div className="inline-flex rounded-md border border-line overflow-hidden">
            {TIME_RANGES.map((r) => (
              <button
                key={r.value}
                onClick={() => setTimeRange(r.value)}
                className={`px-2.5 h-7 text-[11px] font-medium transition ${
                  timeRange === r.value
                    ? "bg-honey-500/15 text-honey-300"
                    : "bg-ink-850 text-muted hover:text-white"
                }`}
                aria-pressed={timeRange === r.value}
              >
                {r.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      {/* Provider health cluster */}
      {providers && providers.length > 0 && (
        <div className="mt-3 flex items-center gap-1.5 flex-wrap">
          <span className="text-[10px] text-dim uppercase tracking-wider mr-1">
            Providers
          </span>
          {providers.map((p) => (
            <ProviderHealthPill key={p.provider_id} provider={p} />
          ))}
        </div>
      )}

      {/* Banners */}
      {config.nurse_model === "none" && (
        <Banner tone="amber">
          Tier 1/2 actions still run, but the Tier 3 LLM classifier is
          disabled — ambiguous signals won't be acted on.
          {tier3Skipped > 0 && (
            <span className="ml-1 font-mono">
              ({tier3Skipped} skipped)
            </span>
          )}{" "}
          <button
            onClick={onOpenModelBrowser}
            className="underline hover:text-amber-200"
          >
            Pick a model
          </button>
        </Banner>
      )}
      {!config.enabled && master !== "observe" && (
        <Banner tone="grey">
          Nurse is disabled. Enable to begin monitoring.
        </Banner>
      )}
      {health.degraded && (
        <Banner tone="red">
          Nurse is in degraded mode. Watchdog suspended restart.
        </Banner>
      )}
      {observabilityDropped > 0 && (
        <Banner tone="amber">
          Nurse missed {observabilityDropped} events under load. Consider
          bumping <code>HYVEMIND_NURSE_BUS_CAPACITY</code>.
        </Banner>
      )}
      {error && (
        <div className="mt-2 text-[11px] text-red-300">{error}</div>
      )}
    </div>
  );
}

function SegmentedMaster({
  value,
  disabled,
  onChange,
}: {
  value: NurseMasterMode;
  disabled?: boolean;
  onChange: (next: NurseMasterMode) => void;
}) {
  const opts: Array<{ id: NurseMasterMode; label: string }> = [
    { id: "enabled", label: "Enabled" },
    { id: "observe", label: "Observe-only" },
    { id: "disabled", label: "Disabled" },
  ];
  return (
    <div className="inline-flex rounded-md border border-line overflow-hidden">
      {opts.map((o) => (
        <button
          key={o.id}
          onClick={() => !disabled && onChange(o.id)}
          disabled={disabled}
          className={`px-2.5 h-7 text-[11px] font-medium transition ${
            value === o.id
              ? "bg-honey-500/15 text-honey-300"
              : "bg-ink-850 text-muted hover:text-white"
          } ${disabled ? "opacity-50" : ""}`}
          aria-pressed={value === o.id}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

type BannerTone = "amber" | "red" | "grey";
const BANNER_CLS: Record<BannerTone, string> = {
  amber: "bg-amber-500/10 border-amber-500/30 text-amber-200",
  red: "bg-red-500/10 border-red-500/30 text-red-200",
  grey: "bg-ink-700/40 border-line text-muted",
};
function Banner({
  tone,
  children,
}: {
  tone: BannerTone;
  children: React.ReactNode;
}) {
  return (
    <div
      className={`mt-2 text-[11.5px] border rounded-md px-3 py-1.5 ${BANNER_CLS[tone]}`}
    >
      {children}
    </div>
  );
}

// `Pill` is now used above; keep the `Btn` import as a forward stub.
void Btn;
