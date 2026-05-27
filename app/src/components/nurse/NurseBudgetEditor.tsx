import { useMemo, useState } from "react";
import type { BudgetConfig } from "../../lib/nurseTypes";
import { Input } from "../atoms";

interface Props {
  budget: BudgetConfig;
  onChange: (next: BudgetConfig) => void;
}

const FIELDS: Array<{
  key: keyof BudgetConfig;
  label: string;
  unit: string;
  helper: string;
  direction: string;
}> = [
  {
    key: "initial_cap",
    label: "Initial cap",
    unit: "interventions",
    helper:
      "How many interventions a brand-new session starts with before falling back to decay.",
    direction: "higher = more interventions allowed",
  },
  {
    key: "decay_per_hour",
    label: "Decay per hour",
    unit: "interventions/hour",
    helper:
      "Replenishment rate. Higher means budget regenerates faster between bursts.",
    direction: "higher = more interventions over time",
  },
  {
    key: "max_cap",
    label: "Max cap",
    unit: "interventions",
    helper:
      "Hard ceiling. Even with decay, current_budget never exceeds this number.",
    direction: "higher = more interventions allowed",
  },
  {
    key: "per_detector_cap",
    label: "Per-detector cap",
    unit: "interventions",
    helper:
      "Independent cap per detector — keeps one chatty detector from exhausting the whole budget.",
    direction: "higher = more per detector",
  },
  {
    key: "per_key_cooldown_secs",
    label: "Per-key cooldown",
    unit: "seconds",
    helper:
      "Minimum gap between back-to-back interventions sharing a dedup key.",
    direction: "higher = fewer back-to-back interventions",
  },
];

/**
 * Five hand-built numeric steppers for the profile-level budget. Each
 * carries explicit units, direction copy, and a one-line helper.
 * Validates: initial ≤ max, per-detector ≤ initial. Errors surface
 * inline; the parent only sees `onChange` for valid edits.
 */
export function NurseBudgetEditor({ budget, onChange }: Props) {
  const [local, setLocal] = useState<BudgetConfig>(budget);
  // Track which field last produced an error so the message lives
  // next to the right input.
  const [errors, setErrors] = useState<
    Partial<Record<keyof BudgetConfig, string>>
  >({});

  // Sync local state when the parent rehydrates the profile (e.g.
  // first IPC response or a Reset-to-defaults).
  useMemo(() => {
    setLocal(budget);
    setErrors({});
  }, [budget]);

  const tryPatch = (key: keyof BudgetConfig, next: number) => {
    const candidate: BudgetConfig = { ...local, [key]: next };
    const errs: typeof errors = {};
    if (candidate.initial_cap < 0) errs.initial_cap = "Must be ≥ 0";
    if (candidate.decay_per_hour < 0)
      errs.decay_per_hour = "Must be ≥ 0";
    if (candidate.max_cap < candidate.initial_cap) {
      errs.max_cap = "Max cap must be ≥ initial cap";
    }
    if (candidate.per_detector_cap > candidate.initial_cap) {
      errs.per_detector_cap = "Per-detector cap must be ≤ initial cap";
    }
    if (candidate.per_key_cooldown_secs < 0)
      errs.per_key_cooldown_secs = "Must be ≥ 0";

    setLocal(candidate);
    setErrors(errs);
    if (Object.keys(errs).length === 0) {
      onChange(candidate);
    }
  };

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
      {FIELDS.map((f) => (
        <div
          key={f.key}
          data-testid="nurse-budget-field"
          data-field={f.key}
          className="space-y-1.5"
        >
          <label className="flex items-center justify-between text-[12px] text-white font-medium">
            <span>
              {f.label}{" "}
              <span className="text-dim text-[10.5px] font-mono">
                ({f.unit})
              </span>
            </span>
          </label>
          <Input
            type="number"
            min={0}
            value={local[f.key]}
            onChange={(e) =>
              tryPatch(
                f.key,
                e.target.value === "" ? 0 : Number(e.target.value),
              )
            }
            aria-label={f.label}
          />
          <div className="text-[10.5px] text-honey-300/70">{f.direction}</div>
          <div className="text-[11px] text-muted leading-snug">{f.helper}</div>
          {errors[f.key] && (
            <div
              role="alert"
              className="text-[11px] text-red-300"
              data-testid="nurse-budget-error"
            >
              {errors[f.key]}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
