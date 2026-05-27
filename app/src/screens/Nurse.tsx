import { useMemo, useState } from "react";
import type { GoFn } from "../App";
import { Btn, Select } from "../components/atoms";
import { I } from "../components/icons";
import { useNurseStatus } from "../hooks/useNurseStatus";
import { useNurseSessions } from "../hooks/useNurseSessions";
import { useNurseInterventionLog } from "../hooks/useNurseInterventionLog";
import { useNurseDetectorStats } from "../hooks/useNurseDetectorStats";
import { useNurseCtx } from "../lib/NurseProvider";
import { confirmDialog } from "../lib/confirm";
import { ModelBrowserModal } from "./ModelBrowser";
import * as ipc from "../lib/ipc";

import { NurseHeader } from "../components/nurse/NurseHeader";
import { NurseMetricCards } from "../components/nurse/NurseMetricCards";
import { NurseSessionCard } from "../components/nurse/NurseSessionCard";
import { NurseSessionDetailDrawer } from "../components/nurse/NurseSessionDetailDrawer";
import { NurseInterventionRow } from "../components/nurse/NurseInterventionRow";
import { NurseDetectorRow } from "../components/nurse/NurseDetectorRow";
import { NurseDetectorDetail } from "../components/nurse/NurseDetectorDetail";
import { NurseProfileEditor } from "../components/nurse/NurseProfileEditor";
import type {
  NurseProfile,
  NurseActionKind,
  NurseDispatchTier,
  Severity,
  SessionOwnerDto,
  ProviderHealthSnapshot,
} from "../lib/nurseTypes";

/* ── Tab routing ─────────────────────────────────────────────── */

type NurseTab = "live" | "log" | "detectors" | "profiles";

const TABS: Array<{ id: NurseTab; label: string }> = [
  { id: "live", label: "Live Sessions" },
  { id: "log", label: "Intervention Log" },
  { id: "detectors", label: "Detector Activity" },
  { id: "profiles", label: "Profiles" },
];

/* ── Screen ──────────────────────────────────────────────────── */

export function NurseScreen({ go: _go }: { go: GoFn }) {
  const [tab, setTab] = useState<NurseTab>("live");
  const { status, refresh } = useNurseStatus();
  const [drawerSession, setDrawerSession] = useState<string | null>(null);
  const [showModelBrowser, setShowModelBrowser] = useState(false);

  return (
    <div className="h-full overflow-y-auto bg-ink-900 flex flex-col">
      <NurseHeader
        config={status.config}
        health={status.health}
        stats={status.stats}
        providers={
          (status as { providers?: ProviderHealthSnapshot[] }).providers
        }
        onOpenModelBrowser={() => setShowModelBrowser(true)}
        onChangeConfig={refresh}
      />

      {/* Tab strip */}
      <div className="border-b border-line bg-ink-900 px-6">
        <div className="flex gap-1" role="tablist" aria-label="Nurse views">
          {TABS.map((t) => (
            <button
              key={t.id}
              role="tab"
              aria-selected={tab === t.id}
              data-testid={`nurse-tab-${t.id}`}
              onClick={() => setTab(t.id)}
              className={`px-3 py-2 text-[12px] font-medium border-b-2 transition ${
                tab === t.id
                  ? "border-honey-500 text-honey-300"
                  : "border-transparent text-muted hover:text-white"
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>
      </div>

      {/* Tab content */}
      <div className="flex-1 min-h-0 px-6 py-6 space-y-4">
        {tab === "live" && (
          <LiveSessionsTab
            onOpenSessionDetail={setDrawerSession}
          />
        )}
        {tab === "log" && <InterventionLogTab />}
        {tab === "detectors" && <DetectorActivityTab />}
        {tab === "profiles" && <ProfilesTab />}
      </div>

      <NurseSessionDetailDrawer
        sessionId={drawerSession}
        onClose={() => setDrawerSession(null)}
      />

      {showModelBrowser && (
        <ModelBrowserModal
          open
          onClose={() => setShowModelBrowser(false)}
          selectLabel="Set Nurse Model"
          initialModel={status.config.nurse_model}
          onSelect={async (m) => {
            const fullModel = `${m.provider}/${m.id}`;
            setShowModelBrowser(false);
            try {
              await ipc.setNurseConfig({
                nurse_model: fullModel,
                nurse_provider: "",
              });
              await refresh();
            } catch (err) {
              console.error("Failed to set nurse model:", err);
            }
          }}
        />
      )}
    </div>
  );
}

/* ── Live Sessions ───────────────────────────────────────────── */

function LiveSessionsTab({
  onOpenSessionDetail,
}: {
  onOpenSessionDetail: (sessionId: string) => void;
}) {
  const { sessions, isLoading, error } = useNurseSessions();
  const { status } = useNurseStatus();
  const { timeRange: _tr } = useNurseCtx();

  // Live + at-a-glance share the same sessions list.
  return (
    <div className="space-y-4">
      <NurseMetricCards
        sessions={sessions.map((d) => d.session)}
        interventionsInRange={status.recent_interventions}
        detectorStats={[]}
      />

      {isLoading && (
        <div className="text-[12px] text-muted">Loading sessions…</div>
      )}
      {error && (
        <div className="text-[12px] text-red-300">
          Failed to load sessions: {error}
        </div>
      )}
      {!isLoading && sessions.length === 0 && (
        <EmptyState
          title="No sessions monitored"
          body="Start a Task or Swarm — Nurse will begin watching its Pi sessions for stalls, loops, and tool failures."
        />
      )}
      <div
        className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-3"
        role="list"
      >
        {sessions
          // Highest tier first so critical sessions are immediately visible.
          .sort((a, b) => tierRank(b.tier) - tierRank(a.tier))
          .map((d) => (
            <div role="listitem" key={d.session.session_id}>
              <NurseSessionCard
                derived={d}
                onOpenDetail={onOpenSessionDetail}
              />
            </div>
          ))}
      </div>
    </div>
  );
}

function tierRank(t: string): number {
  return ({ critical: 4, stalled: 3, warning: 2, quiet: 1 } as Record<
    string,
    number
  >)[t] ?? 0;
}

/* ── Intervention Log ────────────────────────────────────────── */

function InterventionLogTab() {
  const [profile, setProfile] = useState<NurseProfile | "">("");
  const [action, setAction] = useState<NurseActionKind | "">("");
  const [tier, setTier] = useState<NurseDispatchTier | "">("");
  const [severity, setSeverity] = useState<Severity | "">("");
  const [ownerKind, setOwnerKind] = useState<SessionOwnerDto["kind"] | "">("");

  const query = useMemo(
    () => ({
      profile: profile || null,
      action: action || null,
      tier: tier || null,
      severity: severity || null,
      owner_kind: ownerKind || null,
    }),
    [profile, action, tier, severity, ownerKind],
  );

  const { rows, hasMore, isLoading, error, loadMore, clear } =
    useNurseInterventionLog(query);

  return (
    <div className="space-y-3">
      {/* Filters */}
      <div className="flex flex-wrap items-end gap-2 rounded-lg border border-line bg-ink-850 p-3">
        <FilterField label="Profile">
          <Select
            value={profile}
            onChange={(e) =>
              setProfile(e.target.value as NurseProfile | "")
            }
            options={[
              { value: "", label: "Any" },
              { value: "default", label: "Default" },
              { value: "tasks", label: "Tasks" },
              { value: "swarm", label: "Swarm" },
              { value: "hivemind", label: "Hivemind" },
              { value: "test", label: "Test" },
            ]}
          />
        </FilterField>
        <FilterField label="Action">
          <Select
            value={action}
            onChange={(e) =>
              setAction(e.target.value as NurseActionKind | "")
            }
            options={[
              { value: "", label: "Any" },
              { value: "leave_it", label: "Leave it" },
              { value: "steer", label: "Steer" },
              { value: "restart", label: "Restart" },
              { value: "cancel", label: "Cancel" },
            ]}
          />
        </FilterField>
        <FilterField label="Tier">
          <Select
            value={tier}
            onChange={(e) => setTier(e.target.value as NurseDispatchTier | "")}
            options={[
              { value: "", label: "Any" },
              { value: "deterministic", label: "Deterministic" },
              { value: "templated", label: "Templated" },
              { value: "llm", label: "LLM" },
              { value: "synthesized", label: "Synthesized" },
              { value: "manual", label: "Manual" },
            ]}
          />
        </FilterField>
        <FilterField label="Severity">
          <Select
            value={severity}
            onChange={(e) => setSeverity(e.target.value as Severity | "")}
            options={[
              { value: "", label: "Any" },
              { value: "info", label: "Info" },
              { value: "warn", label: "Warn" },
              { value: "stalled", label: "Stalled" },
              { value: "critical", label: "Critical" },
            ]}
          />
        </FilterField>
        <FilterField label="Owner">
          <Select
            value={ownerKind}
            onChange={(e) =>
              setOwnerKind(e.target.value as SessionOwnerDto["kind"] | "")
            }
            options={[
              { value: "", label: "Any" },
              { value: "task", label: "Task" },
              { value: "swarm", label: "Swarm" },
              { value: "review", label: "Review" },
              { value: "merge", label: "Merge" },
              { value: "unknown", label: "Unknown" },
            ]}
          />
        </FilterField>
        <Btn
          kind="danger"
          size="sm"
          className="ml-auto"
          onClick={async () => {
            const ok = await confirmDialog(
              "Clear all Nurse intervention history? This cannot be undone.",
              { kind: "warning" },
            );
            if (ok) {
              try {
                await clear();
              } catch (err) {
                console.error("Failed to clear Nurse intervention log:", err);
              }
            }
          }}
        >
          Clear log
        </Btn>
      </div>

      {error && (
        <div className="text-[12px] text-amber-300">
          {error} — showing cached results if available.
        </div>
      )}

      {rows.length === 0 && !isLoading && (
        <EmptyState
          title="No interventions in range"
          body="When Nurse acts on a session — Steer, Restart, Cancel, or Leave-It — it'll show up here. Until the new backend lands, the log may stay empty."
        />
      )}

      {rows.length > 0 && (
        <ul
          role="list"
          className="rounded-lg border border-line bg-ink-850 overflow-hidden"
        >
          {rows.map((r) => (
            <NurseInterventionRow key={r.id} record={r} />
          ))}
        </ul>
      )}

      {hasMore && (
        <div className="flex justify-center pt-2">
          <Btn
            size="sm"
            kind="outline"
            onClick={loadMore}
            loading={isLoading}
          >
            Load more
          </Btn>
        </div>
      )}
    </div>
  );
}

function FilterField({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="min-w-[120px]">
      <div className="text-[10px] text-dim uppercase tracking-wider mb-1">
        {label}
      </div>
      {children}
    </div>
  );
}

/* ── Detector Activity ───────────────────────────────────────── */

function DetectorActivityTab() {
  const { schemas, schemasLoading } = useNurseCtx();
  const { timeRange } = useNurseCtx();
  const { rows, isLoading, error } = useNurseDetectorStats(timeRange);
  const [detail, setDetail] = useState<string | null>(null);

  // Merge schemas + stats so detectors that exist but haven't raised
  // anything still render with `total = 0`.
  const merged = useMemo(() => {
    const statsByName = new Map(rows.map((r) => [r.detector, r]));
    return schemas.map((s) => ({
      schema: s,
      stats:
        statsByName.get(s.name) ?? {
          detector: s.name,
          total: 0,
          by_severity: {
            info: 0,
            warn: 0,
            stalled: 0,
            critical: 0,
          },
        },
    }));
  }, [schemas, rows]);

  return (
    <div className="space-y-3">
      {error && (
        <div className="text-[12px] text-amber-300">
          {error} — showing empty stats.
        </div>
      )}
      {(schemasLoading || isLoading) && (
        <div className="text-[12px] text-muted">Loading detector activity…</div>
      )}
      {!schemasLoading && merged.length === 0 ? (
        <EmptyState
          title="No detectors registered"
          body="When the new Nurse engine lands, every registered detector will show up here with its severity histogram and false-positive rate."
        />
      ) : (
        <table className="w-full rounded-lg overflow-hidden border border-line bg-ink-850 text-left">
          <thead>
            <tr className="text-[10px] text-dim uppercase tracking-wider border-b border-line">
              <th className="px-3 py-2 font-medium">Detector</th>
              <th className="px-3 py-2 font-medium">Total</th>
              <th className="px-3 py-2 font-medium">Severity</th>
              <th className="px-3 py-2 font-medium">Med. clear</th>
              <th className="px-3 py-2 font-medium">FP</th>
              <th className="px-3 py-2 font-medium">Default</th>
            </tr>
          </thead>
          <tbody>
            {merged.map(({ schema, stats }) => (
              <NurseDetectorRow
                key={schema.name}
                schema={schema}
                stats={stats}
                enabledInDefaultProfile={true}
                onToggleDefault={() => {
                  /* per-profile toggles live in Profiles tab */
                }}
                onOpenDetail={setDetail}
              />
            ))}
          </tbody>
        </table>
      )}
      <NurseDetectorDetail
        detector={detail}
        schema={schemas.find((s) => s.name === detail)}
        stats={rows.find((r) => r.detector === detail)}
        onClose={() => setDetail(null)}
      />
    </div>
  );
}

/* ── Profiles ────────────────────────────────────────────────── */

function ProfilesTab() {
  const [active, setActive] = useState<NurseProfile>("default");
  const profiles: NurseProfile[] = [
    "default",
    "tasks",
    "swarm",
    "hivemind",
    "test",
  ];

  return (
    <div className="space-y-4">
      <div className="flex gap-1 border-b border-line">
        {profiles.map((p) => (
          <button
            key={p}
            onClick={() => setActive(p)}
            className={`px-3 py-1.5 text-[11.5px] font-medium border-b-2 transition capitalize ${
              active === p
                ? "border-honey-500 text-honey-300"
                : "border-transparent text-muted hover:text-white"
            }`}
            aria-pressed={active === p}
          >
            {p}
          </button>
        ))}
      </div>
      <NurseProfileEditor key={active} profile={active} />
    </div>
  );
}

/* ── Empty state ─────────────────────────────────────────────── */

function EmptyState({ title, body }: { title: string; body: string }) {
  return (
    <div className="rounded-xl border border-dashed border-line bg-ink-850/40 p-8 text-center">
      <div className="text-[14px] font-semibold text-white mb-1">{title}</div>
      <p className="text-[12px] text-muted max-w-md mx-auto">{body}</p>
    </div>
  );
}

// Keep `I` accessible if future iterations grow icons in this file.
void I;
