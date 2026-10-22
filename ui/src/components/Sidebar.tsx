import type { IssueCount, TabDef, TabFilter } from "../api";
import { filterLabels, formatCount, severityColor, statusColor } from "../labels";
import { Icon } from "./Icon";

export type View = { kind: "overview" } | { kind: "issues" } | { kind: "tab"; tab: string; filter: string };

/// Where to go: the overview, the issues, then the tabs. The open tab
/// unfolds into its filters, each with a bar of its share of the tab.
export function Sidebar({
  tabs,
  issues,
  view,
  onView,
}: {
  tabs: TabDef[];
  issues: IssueCount[];
  view: View;
  onView: (view: View) => void;
}) {
  const largest = Math.max(1, ...tabs.map((tab) => tab.total));
  const affected = issues.filter((issue) => issue.count > 0);
  const worst = affected.find((issue) => issue.severity === "high") ? "high" : affected.length > 0 ? "medium" : null;
  return (
    <nav className="nav" aria-label="Разделы">
      <div className={`nav-row top ${view.kind === "overview" ? "on" : ""}`} onClick={() => onView({ kind: "overview" })}>
        <Icon name="overview" />
        <span className="label">Обзор</span>
      </div>
      <div className={`nav-row top ${view.kind === "issues" ? "on" : ""}`} onClick={() => onView({ kind: "issues" })}>
        <Icon name="alert" />
        <span className="label">Проблемы</span>
        {worst && <span className="square" style={{ background: `var(${severityColor(worst)})` }} />}
        <span className="count">{formatCount(affected.length)}</span>
      </div>

      <div className="nav-group">
        <Icon name="table" size={14} />
        Вкладки
      </div>
      {tabs.map((tab) => {
        const open = view.kind === "tab" && view.tab === tab.key;
        return (
          <div key={tab.key}>
            <NavRow
              label={tab.title}
              count={tab.total}
              share={tab.total / largest}
              colour="--accent"
              on={open && view.filter === "all"}
              strong={open}
              onClick={() => onView({ kind: "tab", tab: tab.key, filter: "all" })}
            />
            {open &&
              tab.filters
                .filter((filter) => filter.key !== "all")
                .map((filter) => (
                  <NavRow
                    key={filter.key}
                    sub
                    label={filterTitle(filter)}
                    title={filter.description}
                    count={filter.count}
                    share={tab.total > 0 ? Math.min(1, filter.count / tab.total) : 0}
                    colour={filterColour(filter)}
                    marker={filter.severity ? severityColor(filter.severity) : undefined}
                    on={view.filter === filter.key}
                    onClick={() => onView({ kind: "tab", tab: tab.key, filter: filter.key })}
                  />
                ))}
          </div>
        );
      })}
    </nav>
  );
}

export const filterTitle = (filter: TabFilter) => filter.title ?? filterLabels[filter.key] ?? filter.key;

const filterColour = (filter: TabFilter) =>
  filter.severity
    ? severityColor(filter.severity)
    : filter.key === "noindex"
      ? "--warn"
      : /^(\dxx|no_response|blocked)$/.test(filter.key)
        ? statusColor(filter.key)
        : "--accent";

function NavRow({
  label,
  title,
  count,
  share,
  colour,
  marker,
  on,
  strong,
  sub,
  onClick,
}: {
  label: string;
  title?: string;
  count: number;
  share: number;
  colour: string;
  marker?: string;
  on: boolean;
  strong?: boolean;
  sub?: boolean;
  onClick: () => void;
}) {
  const classes = ["nav-row", on && "on", strong && "strong", sub && "sub", count === 0 && "zero"].filter(Boolean).join(" ");
  return (
    <div className={classes} onClick={onClick} title={title}>
      {marker && <span className="square" style={{ background: `var(${marker})` }} />}
      <span className="label">{label}</span>
      <span className="count">{formatCount(count)}</span>
      {!on && <span className="share" style={{ width: `${Math.round(share * 100)}%`, background: `var(${colour})` }} />}
    </div>
  );
}
