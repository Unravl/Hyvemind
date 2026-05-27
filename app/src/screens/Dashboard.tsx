import { useState, useEffect } from "react";
import type React from "react";
import * as ipc from "../lib/ipc";
import type { GoFn } from "../App";
import type { DashboardStats, ModelUsageSummary, CostSummary, ActivityEntry } from "../lib/types";
import { timeAgo } from "../lib/formatDate";
import { useErrorToast } from "../components/Toast";

// ── Helpers ──

function formatTokens(n: number): string {
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1_000) return (n / 1_000).toFixed(0) + "K";
  return n.toString();
}

function formatCost(n: number): string {
  return "$" + n.toFixed(2);
}

function parseProviderModel(provider: string, modelId: string) {
  const idx = modelId.indexOf("/");
  if (idx !== -1) {
    return {
      provider: modelId.slice(0, idx),
      modelId: modelId.slice(idx + 1),
    };
  }
  return { provider, modelId };
}

const PROVIDER_COLORS: Record<string, string> = {
  anthropic: "bg-orange-400",
  openai: "bg-emerald-400",
  deepseek: "bg-blue-400",
  openrouter: "bg-purple-400",
  glm: "bg-cyan-400",
  ollama: "bg-gray-400",
  mistral: "bg-red-400",
};

const SOURCE_TONES: Record<string, { bg: string; text: string }> = {
  chat: { bg: "bg-blue-500/15", text: "text-blue-300" },
  hivemind: { bg: "bg-purple-500/15", text: "text-purple-300" },
  swarm: { bg: "bg-emerald-500/15", text: "text-emerald-300" },
};

const TIME_RANGES = [
  { value: "all", label: "All Time" },
  { value: "month", label: "Month" },
  { value: "week", label: "Week" },
  { value: "day", label: "Today" },
];

// ── Panel wrapper ──

function Panel({ title, children, actions }: { title: string; children: React.ReactNode; actions?: React.ReactNode }) {
  return (
    <div className="rounded-xl border border-line bg-ink-900/60 overflow-hidden">
      <div className="flex items-center justify-between px-5 py-3.5 border-b border-line">
        <h2 className="text-[13px] font-semibold text-white/90">{title}</h2>
        {actions}
      </div>
      <div className="p-5">{children}</div>
    </div>
  );
}

// ── Main Screen ──

const EMPTY_STATS: DashboardStats = { active_tasks: 0, running_swarms: 0, paused_swarms: 0, total_reviews: 0, cost_today: 0 };
const EMPTY_COST: CostSummary = { today: 0, week: 0, month: 0, all_time: 0 };

export function DashboardScreen({ go }: { go: GoFn }) {
  const [loading, setLoading] = useState(true);
  const [stats, setStats] = useState<DashboardStats>(EMPTY_STATS);
  const [timeRange, setTimeRange] = useState("all");
  const [modelUsage, setModelUsage] = useState<ModelUsageSummary[]>([]);
  const [costSummary, setCostSummary] = useState<CostSummary>(EMPTY_COST);
  const [recentActivity, setRecentActivity] = useState<ActivityEntry[]>([]);
  const toast = useErrorToast();

  useEffect(() => {
    // `toast.error` is a stable memoised callback (see ToastProvider); the
    // empty deps array is intentional — we only want this on mount.
    Promise.allSettled([
      ipc.getDashboardStats().then(setStats).catch((e) => toast.error("Failed to load dashboard stats", e)),
      ipc.getCostSummary().then(setCostSummary).catch((e) => toast.error("Failed to load cost summary", e)),
      ipc.getRecentActivity(10).then(setRecentActivity).catch((e) => toast.error("Failed to load recent activity", e)),
    ]).finally(() => setLoading(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    ipc.getModelUsage(timeRange).then(setModelUsage).catch((e) => toast.error("Failed to load model usage", e));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [timeRange]);

  const maxTokens = Math.max(...modelUsage.map(m => m.total_tokens), 1);

  return (
    <div className="h-full overflow-auto">
      <div className="max-w-[1400px] mx-auto px-8 py-7 space-y-6">
        {/* Header */}
        <div>
          <h1 className="text-[22px] font-bold text-white tracking-tight">Dashboard</h1>
          <p className="text-[13px] text-muted mt-1">Overview of your AI development activity</p>
        </div>

        {/* Quick Stats Strip */}
        <div className="grid grid-cols-4 gap-4">
          {loading ? (
            Array.from({ length: 4 }).map((_, i) => (
              <div key={`stat-skel-${i}`} className="rounded-xl border border-line bg-ink-900/60 px-5 py-4 animate-pulse">
                <div className="flex items-center gap-2 mb-3">
                  <div className="w-1.5 h-1.5 rounded-full bg-ink-600" />
                  <div className="h-[28px] w-[60px] rounded bg-ink-600" />
                </div>
                <div className="h-[14px] w-[100px] rounded bg-ink-600" />
              </div>
            ))
          ) : (
            [
              { label: "Active Tasks", value: stats.active_tasks, color: "text-emerald-400" },
              { label: "Running Swarms", value: stats.running_swarms, color: "text-emerald-400", pulse: true },
              { label: "Total Reviews", value: stats.total_reviews, color: "text-blue-400" },
              { label: "Cost Today", value: formatCost(stats.cost_today), color: "text-honey-400" },
            ].map((stat) => (
              <div key={stat.label} className="rounded-xl border border-line bg-ink-900/60 px-5 py-4">
                <div className="flex items-center gap-2">
                  {stat.pulse && <span className="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse" />}
                  <span className={`text-[28px] font-bold ${stat.color}`}>{stat.value}</span>
                </div>
                <span className="text-[11.5px] text-muted font-medium mt-1 block">{stat.label}</span>
              </div>
            ))
          )}
        </div>

        {/* Model Usage */}
        <Panel
          title="Model Usage"
          actions={
            <select
              value={timeRange}
              onChange={(e) => setTimeRange(e.target.value)}
              className="text-[11.5px] bg-ink-800 border border-line rounded-md px-2.5 py-1.5 text-white/80 focus:outline-none focus-visible:ring-2 focus-visible:ring-honey-500 focus-visible:ring-offset-2 focus-visible:ring-offset-ink-950 focus:border-honey-500/50"
            >
              {TIME_RANGES.map(r => (
                <option key={r.value} value={r.value}>{r.label}</option>
              ))}
            </select>
          }
        >
          {loading ? (
            <div className="space-y-2.5 animate-pulse">
              {Array.from({ length: 4 }).map((_, i) => (
                <div key={`mu-skel-${i}`} className="flex items-center gap-3">
                  <div className="h-[14px] w-5 rounded bg-ink-600" />
                  <div className="w-2 h-2 rounded-full bg-ink-600 shrink-0" />
                  <div className="h-[14px] w-[160px] rounded bg-ink-600" />
                  <div className="flex-1 h-2 rounded-full bg-ink-600" />
                  <div className="h-[14px] w-[60px] rounded bg-ink-600" />
                  <div className="h-[14px] w-[60px] rounded bg-ink-600" />
                </div>
              ))}
            </div>
          ) : modelUsage.length === 0 ? (
            <p className="text-[12.5px] text-muted py-4 text-center">No usage data yet</p>
          ) : (
            <div className="space-y-2.5">
              {modelUsage.map((m, i) => {
                const parsed = parseProviderModel(m.provider, m.model_id);
                return (
                  <div key={`${parsed.provider}/${parsed.modelId}-${i}`} className="flex items-center gap-3">
                    <span className="text-[11px] text-dim w-5 text-right font-mono">{i + 1}</span>
                    <span className={`w-2 h-2 rounded-full shrink-0 ${PROVIDER_COLORS[parsed.provider] || "bg-gray-400"}`} />
                    <span className="text-[12.5px] text-white/85 font-medium min-w-[160px] truncate">{parsed.provider}/{parsed.modelId}</span>
                    <div className="flex-1 h-2 rounded-full bg-ink-700 overflow-hidden">
                      <div
                        className="h-full rounded-full bg-honey-500/50"
                        style={{ width: `${(m.total_tokens / maxTokens) * 100}%` }}
                      />
                    </div>
                    <span className="text-[11.5px] text-muted font-mono w-[60px] text-right">{formatTokens(m.total_tokens)}</span>
                    <span className="text-[11.5px] text-dim font-mono w-[60px] text-right">{formatCost(m.cost)}</span>
                  </div>
                );
              })}
            </div>
          )}
        </Panel>

        {/* Provider Usage + Cost Summary side by side */}
        <div className="grid grid-cols-2 gap-4">
          {/* Token Usage by Model (provider/model pairs) */}
          <Panel title="Token Usage by Model">
            {loading ? (
              <div className="space-y-2 animate-pulse">
                {Array.from({ length: 4 }).map((_, i) => (
                  <div key={`tusk-${i}`} className="flex items-center gap-3 py-2">
                    <div className="h-[14px] w-[130px] rounded bg-ink-600" />
                    <div className="flex-1" />
                    <div className="h-[14px] w-[50px] rounded bg-ink-600" />
                    <div className="h-[14px] w-[50px] rounded bg-ink-600" />
                    <div className="h-[14px] w-[50px] rounded bg-ink-600" />
                    <div className="h-[14px] w-[50px] rounded bg-ink-600" />
                  </div>
                ))}
              </div>
            ) : modelUsage.length === 0 ? (
              <p className="text-[12.5px] text-muted py-4 text-center">No usage data yet</p>
            ) : (
              <table className="w-full">
                <thead>
                  <tr className="text-[10.5px] text-dim uppercase tracking-wider">
                    <th className="text-left pb-2 font-medium">Provider / Model</th>
                    <th className="text-right pb-2 font-medium">Input</th>
                    <th className="text-right pb-2 font-medium">Output</th>
                    <th className="text-right pb-2 font-medium">Total</th>
                    <th className="text-right pb-2 font-medium">Cost</th>
                  </tr>
                </thead>
                <tbody>
                  {modelUsage.map((m, i) => {
                    const parsed = parseProviderModel(m.provider, m.model_id);
                    return (
                      <tr key={`${parsed.provider}/${parsed.modelId}-${i}`} className="border-t border-line/40">
                        <td className="py-2 text-[11.5px]">
                          <span className="flex items-center gap-2">
                            <span className={`w-2 h-2 rounded-full ${PROVIDER_COLORS[parsed.provider] || "bg-gray-400"}`} />
                            <span className="text-white/80 font-medium">{parsed.provider}<span className="text-dim">/</span>{parsed.modelId}</span>
                          </span>
                        </td>
                      <td className="py-2 text-[11.5px] text-muted font-mono text-right">{formatTokens(m.input_tokens)}</td>
                      <td className="py-2 text-[11.5px] text-muted font-mono text-right">{formatTokens(m.output_tokens)}</td>
                      <td className="py-2 text-[11.5px] text-white/80 font-mono text-right">{formatTokens(m.total_tokens)}</td>
                      <td className="py-2 text-[11.5px] text-honey-400/80 font-mono text-right">{formatCost(m.cost)}</td>
                    </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
          </Panel>

          {/* Cost Summary */}
          <Panel title="Cost Summary">
            {loading ? (
              <div className="grid grid-cols-2 gap-4 animate-pulse">
                {Array.from({ length: 4 }).map((_, i) => (
                  <div key={`cs-skel-${i}`} className="rounded-lg border border-line/40 bg-ink-850/50 p-4">
                    <div className="h-[12px] w-[60px] rounded bg-ink-600 mb-2" />
                    <div className="h-[26px] w-[80px] rounded bg-ink-600" />
                  </div>
                ))}
              </div>
            ) : (
              <div className="grid grid-cols-2 gap-4">
                <div className="rounded-lg border border-line/40 bg-ink-850/50 p-4">
                  <span className="text-[11px] text-muted font-medium block mb-1">This Week</span>
                  <span className="text-[22px] font-bold text-white">{formatCost(costSummary.week)}</span>
                </div>
                <div className="rounded-lg border border-line/40 bg-ink-850/50 p-4">
                  <span className="text-[11px] text-muted font-medium block mb-1">This Month</span>
                  <span className="text-[22px] font-bold text-white">{formatCost(costSummary.month)}</span>
                </div>
                <div className="rounded-lg border border-line/40 bg-ink-850/50 p-4">
                  <span className="text-[11px] text-muted font-medium block mb-1">Today</span>
                  <span className="text-[22px] font-bold text-honey-400">{formatCost(costSummary.today)}</span>
                </div>
                <div className="rounded-lg border border-line/40 bg-ink-850/50 p-4">
                  <span className="text-[11px] text-muted font-medium block mb-1">All Time</span>
                  <span className="text-[22px] font-bold text-white">{formatCost(costSummary.all_time)}</span>
                </div>
              </div>
            )}
          </Panel>
        </div>

        {/* Recent Activity */}
        <Panel title="Recent Activity">
          {loading ? (
            <div className="space-y-1 animate-pulse">
              {Array.from({ length: 4 }).map((_, i) => (
                <div key={`ra-skel-${i}`} className="flex items-center gap-3 px-2 py-2">
                  <div className="h-[12px] w-[50px] rounded bg-ink-600" />
                  <div className="h-[20px] w-[55px] rounded-full bg-ink-600" />
                  <div className="h-[12px] flex-1 rounded bg-ink-600" />
                  <div className="w-1.5 h-1.5 rounded-full bg-ink-600 shrink-0" />
                  <div className="h-[12px] w-[50px] rounded bg-ink-600" />
                  <div className="h-[12px] w-[45px] rounded bg-ink-600" />
                </div>
              ))}
            </div>
          ) : recentActivity.length === 0 ? (
            <p className="text-[12.5px] text-muted py-4 text-center">No activity yet</p>
          ) : (
            <div className="space-y-1">
              {recentActivity.map((a) => {
                const tone = SOURCE_TONES[a.source] || SOURCE_TONES.chat;
                const parsed = parseProviderModel(a.provider, a.model_id);
                return (
                  <div key={a.id} className="flex items-center gap-3 px-2 py-2 rounded-md hover:bg-ink-800/40">
                    <span className="text-[11px] text-dim font-mono w-[60px] shrink-0">{timeAgo(a.timestamp)}</span>
                    <span className={`text-[10.5px] font-medium px-2 py-0.5 rounded-full ${tone.bg} ${tone.text}`}>
                      {a.source}
                    </span>
                    <span className="text-[12px] text-white/80 truncate flex-1">{parsed.provider}/{parsed.modelId}</span>
                    <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${PROVIDER_COLORS[parsed.provider] || "bg-gray-400"}`} />
                    <span className="text-[11px] text-muted font-mono w-[55px] text-right">{formatTokens(a.input_tokens + a.output_tokens)}</span>
                    <span className="text-[11px] text-dim font-mono w-[50px] text-right">{formatCost(a.cost)}</span>
                  </div>
                );
              })}
            </div>
          )}
        </Panel>
      </div>
    </div>
  );
}
