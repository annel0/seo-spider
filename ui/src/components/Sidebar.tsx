import type { Severity, Summary } from "../api";
import { filterLabels, formatCount, severityColor, severityLabels, statusColor } from "../labels";
import { Icon } from "./Icon";

export type View = { kind: "overview" } | { kind: "table"; filter: string };

/// Where to go: the overview, the internal table by filter, the issues by
/// severity. Each row carries a bar of its share.
export function Sidebar({ summary, view, onView }: { summary: Summary | null; view: View; onView: (view: View) => void }) {
  const all = summary?.filters.find((filter) => filter.key === "all")?.count ?? 0;
  const tableFilter = view.kind === "table" ? view.filter : null;
  const severities: Severity[] = ["high", "medium", "low"];
  return (
    <nav className="nav" aria-label="Разделы">
      <div className={`nav-row ${view.kind === "overview" ? "on" : ""}`} onClick={() => onView({ kind: "overview" })} style={{ height: 40 }}>
        <Icon name="overview" />
        <span className="label" style={{ fontWeight: 600 }}>
          Обзор
        </span>
      </div>

      <div className="nav-group">
        <Icon name="table" size={14} />
        Внутренние URL
      </div>
      {(summary?.filters ?? []).map((filter) => (
        <NavRow
          key={filter.key}
          label={filterLabels[filter.key] ?? filter.key}
          count={filter.count}
          share={all > 0 ? filter.count / all : 0}
          colour={filter.key === "all" || filter.key === "html" ? "--accent" : filter.key === "noindex" ? "--warn" : statusColor(filter.key)}
          on={tableFilter === filter.key}
          onClick={() => onView({ kind: "table", filter: filter.key })}
        />
      ))}

      {severities.map((severity) => {
        const issues = (summary?.issues ?? []).filter((issue) => issue.severity === severity);
        if (issues.length === 0) return null;
        const most = Math.max(...issues.map((issue) => issue.count));
        return (
          <div key={severity}>
            <div className="nav-group">
              <span className="square" style={{ background: `var(${severityColor(severity)})` }} />
              {severityLabels[severity]} важность
            </div>
            {issues.map((issue) => (
              <NavRow
                key={issue.code}
                label={issue.title ?? issue.code}
                title={issue.description ?? undefined}
                count={issue.count}
                share={most > 0 ? issue.count / most : 0}
                colour={severityColor(severity)}
                on={tableFilter === `issue:${issue.code}`}
                onClick={() => onView({ kind: "table", filter: `issue:${issue.code}` })}
              />
            ))}
          </div>
        );
      })}
    </nav>
  );
}

function NavRow({
  label,
  title,
  count,
  share,
  colour,
  on,
  onClick,
}: {
  label: string;
  title?: string;
  count: number;
  share: number;
  colour: string;
  on: boolean;
  onClick: () => void;
}) {
  return (
    <div className={`nav-row ${on ? "on" : ""} ${count === 0 ? "zero" : ""}`} onClick={onClick} title={title}>
      <span className="label">{label}</span>
      <span className="count">{formatCount(count)}</span>
      {!on && <span className="share" style={{ width: `${Math.round(share * 100)}%`, background: `var(${colour})` }} />}
    </div>
  );
}
