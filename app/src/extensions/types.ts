// TS mirrors of the backend extension types (see
// `app/src-tauri/src/extensions/types.rs`). Keep these in sync; the
// IPC payloads are deserialised verbatim into these shapes.

export type Capability =
  | "usage"
  | "billing"
  | "rate_limit_probe"
  | "model_catalog";

export type MetricKind =
  | "currency"
  | "percentage"
  | "tokens"
  | "count"
  | "duration";

export type Tone = "ok" | "warn" | "crit" | "neutral";

export type SnapshotStatus =
  | "loading"
  | "ok"
  | "error"
  | "unsupported"
  | "disabled";

export interface UsageMetric {
  key: string;
  label: string;
  /** Pre-formatted display string ("$8.42", "73%", "2.1M / 5M tok"). */
  display: string;
  value: number;
  kind: MetricKind;
  tone: Tone;
}

export interface UsageSnapshot {
  extension_id: string;
  provider_id: string;
  /** Unix seconds, UTC. */
  fetched_at: number;
  headline: UsageMetric | null;
  metrics: UsageMetric[];
  /** Capped at 64KB by the backend. May be null. */
  raw: unknown;
}

export interface ExtensionManifest {
  /** Composite id: `type_id:provider_id`. */
  id: string;
  type_id: string;
  provider_id: string;
  display_name: string;
  description: string;
  capabilities: Capability[];
  requires_api_key: boolean;
  docs_url: string | null;
}

export interface ExtensionUserSettings {
  enabled: boolean;
  show_in_topbar: boolean;
  preferences: Record<string, string>;
}

export interface SnapshotEntry {
  manifest: ExtensionManifest;
  snapshot: UsageSnapshot | null;
  last_error: string | null;
  last_fetched_at: number | null;
  status: SnapshotStatus;
  user_settings: ExtensionUserSettings;
}

/** Payload for the `usage-snapshot-updated` Tauri event.
 *
 *  The backend omits `snapshot.raw` from the event payload to keep IPC
 *  bandwidth small. Callers that need the full payload should re-fetch
 *  via `get_usage_snapshots()`. */
export interface UsageSnapshotEvent {
  extension_id: string;
  status: SnapshotStatus;
  snapshot: Omit<UsageSnapshot, "raw"> | null;
  error: string | null;
}
