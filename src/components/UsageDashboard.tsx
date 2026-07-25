import { useEffect, useState, type ReactNode } from "react";
import { X } from "lucide-react";
import { api, type UsageSummary } from "../lib/api";
import anthropicIcon from "../assets/anthropic.svg";
import openaiIcon from "../assets/openai.svg";
import geminiIcon from "../assets/gemini.svg";

const AGENT_ICON: Record<string, string> = {
  "claude-code": anthropicIcon,
  codex: openaiIcon,
  gemini: geminiIcon,
};

interface Props {
  onClose: () => void;
}

const REFRESH_MS = 8_000;
const RECENT_DAYS_SHOWN = 14;
const MIN_BAR_PCT = 3;

function formatTokens(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(2)}B`;
  if (abs >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (abs >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

// `date.toISOString().slice(0, 10)` gives the UTC calendar day, not the local one — wrong for
// "Today"/"Last 7 days" anywhere off UTC+0 (e.g. a 9-hour-wide daily mismatch at UTC+9). Backed
// by local `getFullYear`/`getMonth`/`getDate` instead, matching `db.rs`'s `usage_per_day` query
// (which buckets with SQLite's `'localtime'` modifier for the same reason).
function localDateString(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

function agentLabel(agent: string): string {
  if (agent === "claude-code") return "Claude Code";
  if (agent === "codex") return "Codex";
  if (agent === "gemini") return "Gemini";
  return agent.replace(/-/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

function exactTitle(tokensIn: number, tokensOut: number): string {
  const total = tokensIn + tokensOut;
  return `${total.toLocaleString()} tokens (${tokensIn.toLocaleString()} in, ${tokensOut.toLocaleString()} out)`;
}

interface Row {
  label: string;
  tokensIn: number;
  tokensOut: number;
  muted?: boolean;
}

function StatTile({
  label,
  tokensIn,
  tokensOut,
  emphasize,
}: {
  label: string;
  tokensIn: number;
  tokensOut: number;
  emphasize?: boolean;
}) {
  return (
    <div
      className={`usage-stat ${emphasize ? "usage-stat-emphasis" : ""}`}
      title={exactTitle(tokensIn, tokensOut)}
    >
      <span className="usage-stat-value">{formatTokens(tokensIn + tokensOut)}</span>
      <span className="usage-stat-label">{label}</span>
      <span className="usage-stat-breakdown">
        {formatTokens(tokensIn)} in · {formatTokens(tokensOut)} out
      </span>
    </div>
  );
}

function formatLimitLabel(headerName: string): string {
  return headerName.replace(/-/g, " ");
}

function ClaudeLimitsSection() {
  const [hasKey, setHasKey] = useState<boolean | null>(null);
  const [keyInput, setKeyInput] = useState("");
  const [limits, setLimits] = useState<[string, string][] | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api.hasAnthropicApiKey().then(setHasKey);
  }, []);

  async function saveKey() {
    const trimmed = keyInput.trim();
    if (!trimmed) return;
    await api.setAnthropicApiKey(trimmed);
    setKeyInput("");
    setHasKey(true);
  }

  async function removeKey() {
    await api.clearAnthropicApiKey();
    setHasKey(false);
    setLimits(null);
    setError(null);
  }

  async function checkLimits() {
    setChecking(true);
    setError(null);
    try {
      const result = await api.checkClaudeLimits();
      setLimits(result.limits);
    } catch (err) {
      setError(String(err));
    } finally {
      setChecking(false);
    }
  }

  if (hasKey === null) return null;

  return (
    <div className="usage-section">
      <div className="usage-section-header">
        <h3>Claude API limits</h3>
      </div>
      {!hasKey ? (
        <div className="claude-key-prompt">
          <p>
            TermHub can't read your Claude Pro/Max 5-hour usage limit — that's only shown
            inside Claude Code itself, with no public API. If you have an Anthropic API key,
            add it here to see your live API rate-limit usage instead (a different quota, but
            real data).
          </p>
          <div className="claude-key-input-row">
            <input
              type="password"
              placeholder="sk-ant-…"
              value={keyInput}
              onChange={(e) => setKeyInput(e.currentTarget.value)}
              onKeyDown={(e) => e.key === "Enter" && saveKey()}
            />
            <button onClick={saveKey}>Save</button>
          </div>
        </div>
      ) : (
        <div className="claude-key-active">
          <div className="claude-key-active-row">
            <button onClick={checkLimits} disabled={checking}>
              {checking ? "Checking…" : "Check now"}
            </button>
            <button className="claude-key-remove" onClick={removeKey}>
              Remove key
            </button>
          </div>
          <p className="claude-key-note">
            Checking makes one tiny real API request (~1 token) to read the rate-limit
            headers — it isn't free and isn't automatic.
          </p>
          {error && <p className="usage-empty">{error}</p>}
          {limits && (
            <div className="claude-limits-list">
              {limits.map(([name, value]) => (
                <div className="claude-limits-row" key={name}>
                  <span className="claude-limits-label">{formatLimitLabel(name)}</span>
                  <span className="claude-limits-value">{value}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function UsageTable({ title, rows }: { title: string; rows: Row[] }) {
  if (rows.length === 0) return null;
  const max = Math.max(...rows.map((r) => r.tokensIn + r.tokensOut), 1);
  return (
    <div className="usage-section">
      <div className="usage-section-header">
        <h3>{title}</h3>
        <span className="usage-legend">
          <span className="legend-dot in" />
          in
          <span className="legend-dot out" />
          out
        </span>
      </div>
      <div className="usage-table">
        {rows.map((r) => {
          const total = r.tokensIn + r.tokensOut;
          const rawPct = (total / max) * 100;
          const totalPct = total > 0 ? Math.max(rawPct, MIN_BAR_PCT) : 0;
          const inPct = total > 0 ? (r.tokensIn / total) * totalPct : 0;
          const outPct = totalPct - inPct;
          return (
            <div className={`usage-row ${r.muted ? "muted" : ""}`} key={r.label}>
              <span className="usage-row-label" title={r.label}>
                {r.label}
              </span>
              <div className="usage-bar-track">
                {inPct > 0 && (
                  <div className="usage-bar-fill in" style={{ width: `${inPct}%` }} />
                )}
                {outPct > 0 && (
                  <div className="usage-bar-fill out" style={{ width: `${outPct}%` }} />
                )}
              </div>
              <span
                className="usage-row-value"
                title={exactTitle(r.tokensIn, r.tokensOut)}
              >
                {formatTokens(total)}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function UsageDashboard({ onClose }: Props) {
  const [summary, setSummary] = useState<UsageSummary | null>(null);
  const [activeTab, setActiveTab] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    function load() {
      api
        .getUsageSummary()
        .then((s) => {
          if (!cancelled) setSummary(s);
        })
        .catch((err) => console.error("Failed to load usage summary:", err));
    }
    load();
    const interval = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    if (!activeTab && summary && summary.per_agent.length > 0) {
      setActiveTab(summary.per_agent[0].agent);
    }
  }, [summary, activeTab]);

  let content: ReactNode = <div className="usage-loading">Loading…</div>;

  if (summary) {
    if (summary.per_agent.length === 0) {
      content = (
        <p className="usage-empty">
          No usage recorded yet. Run Claude Code or Codex CLI inside a session and check
          back — this reads their local logs every few seconds.
        </p>
      );
    } else {
      const agent = activeTab ?? summary.per_agent[0].agent;
      const agentTotal = summary.per_agent.find((a) => a.agent === agent);
      const dayRows = summary.per_day.filter((d) => d.agent === agent);
      const sessionRowsRaw = summary.per_session.filter((s) => s.agent === agent);

      const todayStr = localDateString(new Date());
      const sevenDaysAgoStr = localDateString(new Date(Date.now() - 6 * 86_400_000));
      const todayEntry = dayRows.find((d) => d.day === todayStr);
      const last7 = dayRows.filter((d) => d.day >= sevenDaysAgoStr);
      const last7In = last7.reduce((sum, d) => sum + d.tokens_in, 0);
      const last7Out = last7.reduce((sum, d) => sum + d.tokens_out, 0);

      const namedSessions = sessionRowsRaw.filter((s) => s.session_id !== null);
      const outsideRow = sessionRowsRaw.find((s) => s.session_id === null);
      const sessionRows: Row[] = [
        ...namedSessions.map((s) => ({
          label: s.session_name,
          tokensIn: s.tokens_in,
          tokensOut: s.tokens_out,
        })),
        ...(outsideRow
          ? [
              {
                label: outsideRow.session_name,
                tokensIn: outsideRow.tokens_in,
                tokensOut: outsideRow.tokens_out,
                muted: true,
              },
            ]
          : []),
      ];

      content = (
        <>
          <div className="usage-tabs">
            {summary.per_agent.map((a) => (
              <button
                key={a.agent}
                className={`usage-tab ${a.agent === agent ? "active" : ""}`}
                onClick={() => setActiveTab(a.agent)}
              >
                {AGENT_ICON[a.agent] && (
                  <img className="usage-tab-icon" src={AGENT_ICON[a.agent]} alt="" />
                )}
                {agentLabel(a.agent)}
                <span className="usage-tab-badge">
                  {formatTokens(a.tokens_in + a.tokens_out)}
                </span>
              </button>
            ))}
          </div>

          <div className="usage-totals">
            <StatTile
              label="Today"
              tokensIn={todayEntry?.tokens_in ?? 0}
              tokensOut={todayEntry?.tokens_out ?? 0}
              emphasize
            />
            <StatTile label="Last 7 days" tokensIn={last7In} tokensOut={last7Out} />
            <StatTile
              label="All time"
              tokensIn={agentTotal?.tokens_in ?? 0}
              tokensOut={agentTotal?.tokens_out ?? 0}
            />
          </div>

          <UsageTable title="By session" rows={sessionRows} />
          <UsageTable
            title="By day"
            rows={dayRows.slice(0, RECENT_DAYS_SHOWN).map((d) => ({
              label: d.day,
              tokensIn: d.tokens_in,
              tokensOut: d.tokens_out,
            }))}
          />

          {agent === "claude-code" && <ClaudeLimitsSection />}
        </>
      );
    }
  }

  return (
    <div className="usage-overlay" onClick={onClose}>
      <div className="usage-panel" onClick={(e) => e.stopPropagation()}>
        <div className="usage-header">
          <h2>Token usage</h2>
          <button className="usage-close-btn" onClick={onClose} title="Close">
            <X size={16} />
          </button>
        </div>
        {content}
      </div>
    </div>
  );
}
