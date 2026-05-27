import type { ProviderHealthSnapshot } from "../../lib/nurseTypes";

const BREAKER_DOT: Record<ProviderHealthSnapshot["breaker_state"], string> = {
  closed: "bg-emerald-400",
  half_open: "bg-amber-400",
  open: "bg-red-400",
};

const BREAKER_LABEL: Record<ProviderHealthSnapshot["breaker_state"], string> = {
  closed: "Healthy",
  half_open: "Probing",
  open: "Open",
};

/**
 * One pill per configured provider in the Nurse screen header.
 * Hovering surfaces the retry-after timestamp when the breaker is
 * Open.
 */
export function ProviderHealthPill({
  provider,
}: {
  provider: ProviderHealthSnapshot;
}) {
  const title =
    provider.breaker_state === "open" && provider.retry_at
      ? `${provider.display_name} — Open until ${provider.retry_at}`
      : `${provider.display_name} — ${BREAKER_LABEL[provider.breaker_state]}`;

  return (
    <span
      className="inline-flex items-center gap-1.5 px-2 h-6 rounded-md border border-line bg-ink-850 text-[11px] text-muted"
      title={title}
    >
      <span
        className={`w-1.5 h-1.5 rounded-full ${BREAKER_DOT[provider.breaker_state]}`}
      />
      <span className="truncate max-w-[120px]">{provider.display_name}</span>
    </span>
  );
}
