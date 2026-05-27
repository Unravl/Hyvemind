import type {
  EnumChoices,
  NumericRange,
  TunableDef,
  TunableDirection,
} from "../../lib/nurseTypes";
import { Input, Select } from "../atoms";
import { Markdown } from "../Markdown";

const DIRECTION_LABEL: Record<TunableDirection, string> = {
  higher_more_sensitive: "Higher = more sensitive (more interventions)",
  higher_less_sensitive: "Higher = less sensitive (fewer interventions)",
  neutral: "",
};

/**
 * Auto-generic input for one detector tunable. Renders the right
 * primitive based on `def.kind` and ALWAYS surfaces unit, default,
 * direction, and markdown description.
 *
 * **Regression guard**: every consumer of `TunableDef` MUST go
 * through this component. A bare i32 slider with no unit / no
 * description is treated as a bug per the plan.
 */
export function NurseTunableField({
  def,
  value,
  onChange,
}: {
  def: TunableDef;
  value: unknown;
  onChange: (next: unknown) => void;
}) {
  const directionCopy = DIRECTION_LABEL[def.direction];
  const defaultLabel = describeDefault(def);

  return (
    <div
      data-testid="nurse-tunable-field"
      data-kind={def.kind}
      data-tunable-name={def.name}
      className="space-y-1.5"
    >
      <label className="flex items-center justify-between text-[12px] text-white font-medium">
        <span>
          {humanize(def.name)}{" "}
          {def.unit && (
            <span className="text-dim text-[10.5px] font-mono">
              ({def.unit})
            </span>
          )}
        </span>
        <span className="text-[10px] text-dim font-mono" title="Default value">
          default: {defaultLabel}
        </span>
      </label>

      <FieldByKind def={def} value={value} onChange={onChange} />

      {directionCopy && (
        <div
          data-testid="nurse-tunable-direction"
          className="text-[10.5px] text-honey-300/70"
        >
          {directionCopy}
        </div>
      )}
      {def.description && (
        <div
          data-testid="nurse-tunable-description"
          className="text-[11px] text-muted leading-snug"
        >
          <Markdown text={def.description} variant="assistant" />
        </div>
      )}
    </div>
  );
}

function FieldByKind({
  def,
  value,
  onChange,
}: {
  def: TunableDef;
  value: unknown;
  onChange: (next: unknown) => void;
}) {
  switch (def.kind) {
    case "numeric_range": {
      const range = (def.safe_range as NumericRange | null) ?? {
        min: 0,
        max: 100,
      };
      const v = typeof value === "number" ? value : Number(def.default);
      const step = range.step ?? 1;
      return (
        <div className="flex items-center gap-2">
          <input
            type="range"
            min={range.min}
            max={range.max}
            step={step}
            value={v}
            onChange={(e) => onChange(Number(e.target.value))}
            aria-label={humanize(def.name)}
            className="flex-1 accent-honey-500"
          />
          <span className="text-[11px] text-white font-mono w-12 text-right">
            {v}
          </span>
        </div>
      );
    }
    case "stepper": {
      const range = (def.safe_range as NumericRange | null) ?? {
        min: 0,
        max: 9999,
      };
      const v = typeof value === "number" ? value : Number(def.default);
      return (
        <Input
          type="number"
          min={range.min}
          max={range.max}
          step={range.step ?? 1}
          value={v}
          onChange={(e) =>
            onChange(e.target.value === "" ? null : Number(e.target.value))
          }
          aria-label={humanize(def.name)}
        />
      );
    }
    case "enum": {
      const choices = (def.safe_range as EnumChoices | null)?.choices ?? [];
      const v = typeof value === "string" ? value : String(def.default ?? "");
      return (
        <Select
          value={v}
          onChange={(e) => onChange(e.target.value)}
          options={choices}
          aria-label={humanize(def.name)}
        />
      );
    }
    case "toggle": {
      const v = Boolean(value ?? def.default);
      return (
        <label className="inline-flex items-center cursor-pointer gap-2 text-[11px] text-muted">
          <input
            type="checkbox"
            checked={v}
            onChange={(e) => onChange(e.target.checked)}
            aria-label={humanize(def.name)}
            className="accent-honey-500"
          />
          {v ? "on" : "off"}
        </label>
      );
    }
    case "text": {
      const v = typeof value === "string" ? value : String(def.default ?? "");
      return (
        <Input
          type="text"
          value={v}
          onChange={(e) => onChange(e.target.value)}
          aria-label={humanize(def.name)}
        />
      );
    }
  }
}

function humanize(name: string): string {
  return name
    .replace(/[_-]+/g, " ")
    .replace(/\b\w/g, (c) => c.toUpperCase());
}

function describeDefault(def: TunableDef): string {
  const d = def.default;
  if (d === null || d === undefined) return "—";
  if (typeof d === "boolean") return d ? "on" : "off";
  if (typeof d === "number") return String(d);
  if (typeof d === "string") return d;
  try {
    return JSON.stringify(d);
  } catch {
    return String(d);
  }
}
