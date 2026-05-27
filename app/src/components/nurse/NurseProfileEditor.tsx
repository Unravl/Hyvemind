import { useState } from "react";
import { Btn } from "../atoms";
import { useNurseCtx } from "../../lib/NurseProvider";
import { useNurseProfile } from "../../hooks/useNurseProfile";
import { NurseBudgetEditor } from "./NurseBudgetEditor";
import { NurseTunableField } from "./NurseTunableField";
import type {
  NurseInterventionMode,
  NurseProfile,
  ProfileConfig,
  ProfileDetectorConfig,
  Severity,
} from "../../lib/nurseTypes";

const MODES: Array<{ id: NurseInterventionMode; label: string }> = [
  { id: "auto", label: "Auto" },
  { id: "observe", label: "Observe" },
];

const ESCALATION: Array<{ id: Severity; label: string }> = [
  { id: "warn", label: "Warn" },
  { id: "stalled", label: "Stalled" },
  { id: "critical", label: "Critical" },
];

/**
 * Per-profile tuning panel. Hand-built controls for profile-wide
 * fields (enable, mode, escalation, budget) plus per-detector cards
 * auto-rendered from `Detector::config_schema() -> Vec<TunableDef>`.
 */
export function NurseProfileEditor({ profile }: { profile: NurseProfile }) {
  const { schemas, schemasLoading } = useNurseCtx();
  const { config, isLoading, isSaving, error, lastError, patch, resetToDefaults } =
    useNurseProfile(profile);
  const [confirmReset, setConfirmReset] = useState(false);

  if (isLoading || !config) {
    return (
      <div className="text-[12px] text-muted py-6 text-center">
        Loading profile config…
      </div>
    );
  }

  const setEnabled = (next: boolean) => patch((p) => ({ ...p, enabled: next }));
  const setMode = (m: NurseInterventionMode) =>
    patch((p) => ({ ...p, intervention_mode: m }));
  const setEscalation = (s: Severity) =>
    patch((p) => ({ ...p, escalation_min_severity: s }));
  const setBudget = (b: ProfileConfig["budget"]) =>
    patch((p) => ({ ...p, budget: b }));

  const setDetectorEnabled = (name: string, next: boolean) =>
    patch((p) => ({
      ...p,
      detectors: {
        ...p.detectors,
        [name]: { ...(p.detectors[name] ?? defaultDetector()), enabled: next },
      },
    }));
  const setDetectorValue = (
    name: string,
    key: string,
    value: unknown,
  ) =>
    patch((p) => {
      const prev = p.detectors[name] ?? defaultDetector();
      return {
        ...p,
        detectors: {
          ...p.detectors,
          [name]: {
            ...prev,
            config: { ...prev.config, [key]: value },
          },
        },
      };
    });

  return (
    <div className="space-y-5">
      <header className="flex items-center justify-between">
        <div>
          <h2 className="text-[14px] font-semibold text-white capitalize">
            {profile} profile
          </h2>
          <p className="text-[11px] text-muted">
            Context-specific tuning. New sessions belonging to this
            profile pick up changes on their next observe tick.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {isSaving && (
            <span className="text-[11px] text-dim">saving…</span>
          )}
          <Btn
            size="sm"
            kind="outline"
            onClick={() => setConfirmReset(true)}
          >
            Reset to defaults
          </Btn>
        </div>
      </header>

      {(error || lastError) && (
        <div className="text-[11px] text-red-300">
          {lastError ?? error}
        </div>
      )}

      <section className="rounded-lg border border-line bg-ink-850 p-4 space-y-4">
        <Row label="Profile enabled" helper="Disable to skip every detector in this profile.">
          <Toggle value={config.enabled} onChange={setEnabled} />
        </Row>

        <Row
          label="Intervention mode"
          helper="Auto dispatches Steer / Restart / Cancel actions; Observe records would-intervene events without acting."
        >
          <Segmented value={config.intervention_mode} opts={MODES} onChange={setMode} />
        </Row>

        <Row
          label="Escalation minimum severity"
          helper="Signals below this severity never escalate to Tier 3 (LLM classifier)."
        >
          <Segmented
            value={config.escalation_min_severity}
            opts={ESCALATION}
            onChange={setEscalation}
          />
        </Row>
      </section>

      <section>
        <h3 className="text-[12px] text-white font-semibold uppercase tracking-wider mb-2">
          Budget
        </h3>
        <div className="rounded-lg border border-line bg-ink-850 p-4">
          <NurseBudgetEditor budget={config.budget} onChange={setBudget} />
        </div>
      </section>

      <section>
        <h3 className="text-[12px] text-white font-semibold uppercase tracking-wider mb-2">
          Detectors
        </h3>
        {schemasLoading ? (
          <div className="text-[12px] text-muted">Loading detector schemas…</div>
        ) : schemas.length === 0 ? (
          <div className="text-[12px] text-muted">
            No detector schemas available. The backend may not yet
            implement `get_nurse_detector_schemas`.
          </div>
        ) : (
          <div className="space-y-3">
            {schemas.map((s) => {
              const det = config.detectors[s.name] ?? defaultDetector();
              return (
                <div
                  key={s.name}
                  className="rounded-lg border border-line bg-ink-850 p-4 space-y-3"
                >
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <div className="text-[12.5px] text-white font-semibold">
                        {s.display_name}
                      </div>
                      <div className="text-[11px] text-muted">
                        {s.description}
                      </div>
                    </div>
                    <Toggle
                      value={det.enabled}
                      onChange={(v) => setDetectorEnabled(s.name, v)}
                    />
                  </div>
                  {s.tunables.length > 0 && (
                    <div className="grid grid-cols-1 md:grid-cols-2 gap-3 pt-2 border-t border-line">
                      {s.tunables.map((t) => (
                        <NurseTunableField
                          key={t.name}
                          def={t}
                          value={det.config[t.name] ?? t.default}
                          onChange={(v) =>
                            setDetectorValue(s.name, t.name, v)
                          }
                        />
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </section>

      {confirmReset && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/60"
            onClick={() => setConfirmReset(false)}
          />
          <div className="relative bg-ink-800 border border-line rounded-2xl w-[420px] p-5 shadow-2xl">
            <h3 className="text-base font-semibold text-white mb-2">
              Reset {profile} profile?
            </h3>
            <p className="text-[12px] text-muted mb-4">
              All tunings under this profile (enable flags, budget,
              per-detector knobs) revert to the code defaults.
            </p>
            <div className="flex items-center gap-2 justify-end">
              <Btn
                size="sm"
                kind="ghost"
                onClick={() => setConfirmReset(false)}
              >
                Cancel
              </Btn>
              <Btn
                size="sm"
                kind="danger"
                onClick={async () => {
                  await resetToDefaults();
                  setConfirmReset(false);
                }}
              >
                Reset profile
              </Btn>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function defaultDetector(): ProfileDetectorConfig {
  return { enabled: true, config: {} };
}

function Row({
  label,
  helper,
  children,
}: {
  label: string;
  helper?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <div className="text-[12px] text-white font-medium">{label}</div>
        {helper && (
          <div className="text-[11px] text-muted leading-snug">{helper}</div>
        )}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

function Toggle({
  value,
  onChange,
}: {
  value: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onChange(!value)}
      aria-pressed={value}
      className={`relative w-10 h-5 rounded-full transition-colors ${
        value ? "bg-emerald-500" : "bg-ink-700"
      }`}
    >
      <span
        className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform ${
          value ? "left-[22px]" : "left-0.5"
        }`}
      />
    </button>
  );
}

function Segmented<T extends string>({
  value,
  opts,
  onChange,
}: {
  value: T;
  opts: Array<{ id: T; label: string }>;
  onChange: (next: T) => void;
}) {
  return (
    <div className="inline-flex rounded-md border border-line overflow-hidden">
      {opts.map((o) => (
        <button
          key={o.id}
          onClick={() => onChange(o.id)}
          className={`px-2.5 h-7 text-[11px] font-medium transition ${
            value === o.id
              ? "bg-honey-500/15 text-honey-300"
              : "bg-ink-850 text-muted hover:text-white"
          }`}
          aria-pressed={value === o.id}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
