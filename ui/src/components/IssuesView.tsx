import type { IssueCount, Severity, TabDef } from "../api";
import { formatCount, severityColor, severityLabels } from "../labels";

const severities: Severity[] = ["high", "medium", "low"];

/// Every issue the crawl found, by importance: how many URLs each hits as
/// a bar, a click opens the tab filtered to them.
export function IssuesView({
  issues,
  tabs,
  pages,
  onOpen,
}: {
  issues: IssueCount[];
  tabs: TabDef[];
  pages: number;
  onOpen: (tab: string, filter: string) => void;
}) {
  const found = issues.filter((issue) => issue.count > 0);
  if (found.length === 0) return <div className="empty">Проблем не найдено</div>;
  const most = Math.max(...found.map((issue) => issue.count));
  const tabTitle = new Map(tabs.map((tab) => [tab.key, tab.title]));
  return (
    <div className="sheet">
      <div className="ruled kpis">
        {severities.map((severity) => {
          const group = found.filter((issue) => issue.severity === severity);
          return (
            <div key={severity} className="kpi">
              <div className="value" style={{ color: group.length > 0 ? `var(${severityColor(severity)})` : "var(--ink-3)" }}>
                {group.length}
              </div>
              <div className="name">{severityLabels[severity].toLowerCase()} важность, типов</div>
            </div>
          );
        })}
        <div className="kpi">
          <div className="value">{formatCount(pages)}</div>
          <div className="name">HTML-страниц в обходе</div>
        </div>
      </div>
      {severities.map((severity) => {
        const group = found.filter((issue) => issue.severity === severity);
        if (group.length === 0) return null;
        return (
          <section key={severity} className="issue-group">
            <h3>
              <span className="square" style={{ background: `var(${severityColor(severity)})` }} />
              {severityLabels[severity]} важность
            </h3>
            {group.map((issue) => (
              <div
                key={issue.code}
                className={`issue-line ${issue.tab ? "clickable" : ""}`}
                title={issue.description ?? undefined}
                onClick={() => issue.tab && onOpen(issue.tab, `issue:${issue.code}`)}
              >
                <span className="name">{issue.title ?? issue.code}</span>
                <span className="where">{issue.tab ? tabTitle.get(issue.tab) : ""}</span>
                <span className="track">
                  <span style={{ width: `${Math.max(1, (issue.count * 100) / most)}%`, background: `var(${severityColor(severity)})` }} />
                </span>
                <span className="num">{formatCount(issue.count)}</span>
                <span className="num pct">{pages > 0 && issue.count <= pages ? `${Math.round((issue.count * 100) / pages)}%` : ""}</span>
              </div>
            ))}
          </section>
        );
      })}
    </div>
  );
}
