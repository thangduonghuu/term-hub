import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { api, type UsageSummary } from "../lib/api";

interface Props {
  onClose: () => void;
}

const REFRESH_MS = 8_000;

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

interface Row {
  label: string;
  tokensIn: number;
  tokensOut: number;
}

function UsageTable({ title, rows }: { title: string; rows: Row[] }) {
  if (rows.length === 0) return null;
  const max = Math.max(...rows.map((r) => r.tokensIn + r.tokensOut), 1);
  return (
    <div className="usage-section">
      <h3>{title}</h3>
      <div className="usage-table">
        {rows.map((r) => {
          const total = r.tokensIn + r.tokensOut;
          return (
            <div className="usage-row" key={r.label}>
              <span className="usage-row-label" title={r.label}>
                {r.label}
              </span>
              <div className="usage-bar-track">
                <div className="usage-bar-fill" style={{ width: `${(total / max) * 100}%` }} />
              </div>
              <span className="usage-row-value">{formatTokens(total)}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

export function UsageDashboard({ onClose }: Props) {
  const [summary, setSummary] = useState<UsageSummary | null>(null);

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

  return (
    <div className="usage-overlay" onClick={onClose}>
      <div className="usage-panel" onClick={(e) => e.stopPropagation()}>
        <div className="usage-header">
          <h2>Token usage</h2>
          <button className="usage-close-btn" onClick={onClose} title="Close">
            <X size={16} />
          </button>
        </div>

        {!summary ? (
          <div className="usage-loading">Loading…</div>
        ) : (
          <>
            <div className="usage-totals">
              <div className="usage-stat">
                <span className="usage-stat-value">{formatTokens(summary.total_tokens_in)}</span>
                <span className="usage-stat-label">tokens in</span>
              </div>
              <div className="usage-stat">
                <span className="usage-stat-value">{formatTokens(summary.total_tokens_out)}</span>
                <span className="usage-stat-label">tokens out</span>
              </div>
              <div className="usage-stat usage-stat-grand">
                <span className="usage-stat-value">
                  {formatTokens(summary.total_tokens_in + summary.total_tokens_out)}
                </span>
                <span className="usage-stat-label">grand total</span>
              </div>
            </div>

            <UsageTable
              title="By agent"
              rows={summary.per_agent.map((a) => ({
                label: a.agent,
                tokensIn: a.tokens_in,
                tokensOut: a.tokens_out,
              }))}
            />
            <UsageTable
              title="By session"
              rows={summary.per_session.map((s) => ({
                label: s.session_name,
                tokensIn: s.tokens_in,
                tokensOut: s.tokens_out,
              }))}
            />
            <UsageTable
              title="By day"
              rows={summary.per_day.map((d) => ({
                label: d.day,
                tokensIn: d.tokens_in,
                tokensOut: d.tokens_out,
              }))}
            />

            {summary.per_agent.length === 0 && (
              <p className="usage-empty">
                No usage recorded yet. Run Claude Code or Codex CLI inside a session and check
                back — this reads their local logs every few seconds.
              </p>
            )}
          </>
        )}
      </div>
    </div>
  );
}
