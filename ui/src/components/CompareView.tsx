import { useEffect, useMemo, useState } from "react";
import { api, downloads, type Comparison, type Crawl, type DiffChange, type DiffIssue } from "../api";
import { formatCount, formatWhen, hostOf, readableUrl, severityColor, statusClass, statusColor } from "../labels";
import { LinkLine } from "./DetailPanel";
import { Icon } from "./Icon";

type Part = "added" | "removed" | "changed" | "new_issues" | "fixed_issues";

const fieldLabels: Record<string, string> = {
  status: "Код ответа",
  title: "Title",
  description: "Description",
  h1: "H1",
  canonical: "Canonical",
  robots: "Meta robots",
  words: "Слов",
};
const fields = Object.keys(fieldLabels);

/// The other crawls of the same site, newest first.
export const comparable = (crawl: Crawl | undefined, crawls: Crawl[]) =>
  crawl ? crawls.filter((other) => other.id !== crawl.id && hostOf(other.start_url) === hostOf(crawl.start_url)) : [];

/// The crawl against another one of the same site: URLs that came and
/// went, fields that changed, issues that appeared and were fixed. The
/// older crawl is "before" whichever is open.
export function CompareView({ crawl, crawls, onSelect }: { crawl: Crawl; crawls: Crawl[]; onSelect: (urlId: number) => void }) {
  const others = comparable(crawl, crawls);
  const [other, setOther] = useState<number | null>(() => (others.find((item) => item.id < crawl.id) ?? others[0])?.id ?? null);
  const [diff, setDiff] = useState<Comparison | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [part, setPart] = useState<Part>("changed");
  const [field, setField] = useState<string | null>(null);
  const [code, setCode] = useState<string | null>(null);

  useEffect(() => {
    if (other === null) return;
    setDiff(null);
    setError(null);
    let current = true;
    api
      .compare(crawl.id, other)
      .then((found) => {
        if (!current) return;
        setDiff(found);
        // The first part that has something.
        const counts: [Part, number][] = [
          ["changed", found.totals.changed_urls],
          ["new_issues", found.totals.new_issues],
          ["added", found.totals.added],
          ["removed", found.totals.removed],
          ["fixed_issues", found.totals.fixed_issues],
        ];
        setPart(counts.find(([, count]) => count > 0)?.[0] ?? "changed");
        setField(null);
        setCode(null);
      })
      .catch((failure: Error) => current && setError(failure.message));
    return () => {
      current = false;
    };
  }, [crawl.id, other]);

  const titles = useMemo(() => new Map(diff?.shifts.map((shift) => [shift.code, shift]) ?? []), [diff]);

  if (others.length === 0) return <div className="empty">Других обходов этого сайта нет</div>;

  const byId = new Map(crawls.map((item) => [item.id, item]));
  const before = diff ? byId.get(diff.old) : undefined;
  const after = diff ? byId.get(diff.new) : undefined;
  const open = (next: Part) => {
    setPart(next);
    setField(null);
    setCode(null);
  };

  return (
    <>
      <div className="toolbar">
        <span className="title">Сравнение</span>
        <select value={other ?? ""} onChange={(event) => setOther(Number(event.target.value))} aria-label="С каким обходом" style={{ maxWidth: 260 }}>
          {others.map((item) => (
            <option key={item.id} value={item.id}>
              #{item.id} {formatWhen(item.started_at)}, {formatCount(item.urls)} URL
            </option>
          ))}
        </select>
        {before && after && (
          <span className="compare-span">
            #{before.id} {formatWhen(before.started_at)} <span className="arrow">→</span> #{after.id} {formatWhen(after.started_at)}
          </span>
        )}
        <span style={{ flex: 1 }} />
        {other !== null && (
          <a className="button" href={downloads.compare(crawl.id, other)} download title="Все отличия: новые и исчезнувшие URL, изменения, проблемы">
            <Icon name="download" /> CSV
          </a>
        )}
      </div>
      {error ? (
        <div className="error-banner">{error}</div>
      ) : !diff ? (
        <div className="empty">Сравниваю</div>
      ) : (
        <div className="sheet" style={{ flex: 1, minHeight: 0 }}>
          {diff.notes.length > 0 && (
            <div className="compare-notes">
              {diff.notes.map((note) => (
                <span key={note}>
                  <Icon name="alert" size={14} />
                  {note}
                </span>
              ))}
            </div>
          )}
          <div className="ruled kpis">
            <Figure value={diff.totals.added} sign="+" name="новых URL" on={part === "added"} onClick={() => open("added")} />
            <Figure value={diff.totals.removed} sign="−" name="исчезли" colour="--warn" on={part === "removed"} onClick={() => open("removed")} />
            <Figure value={diff.totals.changed_urls} name="URL изменились" colour="--accent" on={part === "changed"} onClick={() => open("changed")} />
            <Figure value={diff.totals.new_issues} sign="+" name="новых проблем" colour="--bad" on={part === "new_issues"} onClick={() => open("new_issues")} />
            <Figure value={diff.totals.fixed_issues} sign="−" name="проблем исправлено" colour="--ok" on={part === "fixed_issues"} onClick={() => open("fixed_issues")} />
          </div>

          {diff.shifts.length > 0 && (
            <div className="panel wide shifts">
              <h3>Проблемы: было и стало</h3>
              <Shifts
                diff={diff}
                onOpen={(shift, appeared) => {
                  setPart(appeared ? "new_issues" : "fixed_issues");
                  setField(null);
                  setCode(shift);
                }}
              />
            </div>
          )}

          {part === "changed" && (
            <div className="compare-fields">
              <div className="segmented">
                <button className={field === null ? "on" : undefined} onClick={() => setField(null)}>
                  Все {formatCount(Object.values(diff.totals.changed).reduce((sum, count) => sum + count, 0))}
                </button>
                {fields
                  .filter((name) => diff.totals.changed[name])
                  .map((name) => (
                    <button key={name} className={field === name ? "on" : undefined} onClick={() => setField(name)}>
                      {fieldLabels[name]} {formatCount(diff.totals.changed[name])}
                    </button>
                  ))}
              </div>
            </div>
          )}

          <div className="links compare-list">
            {part === "added" || part === "removed" ? (
              <>
                {diff[part].map((url) => (
                  <LinkLine
                    key={url.id}
                    url={url.url}
                    status={url.status}
                    anchors={[]}
                    badges={[!url.internal && "внешний"]}
                    onOpen={() => onSelect(url.id)}
                  />
                ))}
                <Cut shown={diff[part].length} total={diff.totals[part]} empty={part === "added" ? "Новых URL нет" : "Ни один URL не исчез"} />
              </>
            ) : part === "changed" ? (
              <Changes changes={diff.changed.filter((change) => field === null || change.field === field)} totals={diff.totals.changed} field={field} onSelect={onSelect} />
            ) : (
              <Issues
                issues={diff[part].filter((issue) => code === null || issue.code === code)}
                total={code === null ? diff.totals[part] : (part === "new_issues" ? titles.get(code)?.appeared : titles.get(code)?.fixed) ?? 0}
                titles={titles}
                code={code}
                fixed={part === "fixed_issues"}
                onAll={() => setCode(null)}
                onSelect={onSelect}
              />
            )}
          </div>
        </div>
      )}
    </>
  );
}

function Figure({
  value,
  sign,
  name,
  colour,
  on,
  onClick,
}: {
  value: number;
  sign?: string;
  name: string;
  colour?: string;
  on: boolean;
  onClick: () => void;
}) {
  return (
    <div className={`kpi clickable ${on ? "picked" : ""}`} onClick={onClick}>
      <div className="value" style={{ color: value > 0 && colour ? `var(${colour})` : value === 0 ? "var(--ink-3)" : undefined }}>
        {value > 0 && sign}
        {formatCount(value)}
      </div>
      <div className="name">{name}</div>
    </div>
  );
}

/// Per issue: URLs that lost it to the left in green, that got it to the
/// right in red, and the count before and after.
function Shifts({ diff, onOpen }: { diff: Comparison; onOpen: (code: string, appeared: boolean) => void }) {
  const most = Math.max(1, ...diff.shifts.map((shift) => Math.max(shift.appeared, shift.fixed)));
  const width = (count: number) => `${count > 0 ? Math.max(2, (count * 100) / most) : 0}%`;
  return (
    <div className="shift-lines">
      {diff.shifts.map((shift) => {
        const delta = shift.after - shift.before;
        return (
          <div key={shift.code} className="shift-line">
            <span className="name" title={shift.title ?? shift.code}>
              <span className="square" style={{ background: `var(${severityColor(shift.severity)})` }} />
              {shift.title ?? shift.code}
            </span>
            <span className="axis">
              <span className="half fixed" onClick={() => shift.fixed > 0 && onOpen(shift.code, false)} title={`Исправлено на ${formatCount(shift.fixed)} URL`}>
                {shift.fixed > 0 && <span className="num">−{formatCount(shift.fixed)}</span>}
                <span className="fill" style={{ width: width(shift.fixed) }} />
              </span>
              <span className="half appeared" onClick={() => shift.appeared > 0 && onOpen(shift.code, true)} title={`Появилась на ${formatCount(shift.appeared)} URL`}>
                <span className="fill" style={{ width: width(shift.appeared) }} />
                {shift.appeared > 0 && <span className="num">+{formatCount(shift.appeared)}</span>}
              </span>
            </span>
            <span className="counts">
              {formatCount(shift.before)} <span className="arrow">→</span> {formatCount(shift.after)}
            </span>
            <span className="delta" style={{ color: delta > 0 ? "var(--bad)" : delta < 0 ? "var(--ok)" : "var(--ink-3)" }}>
              {delta > 0 ? "+" : delta < 0 ? "−" : ""}
              {formatCount(Math.abs(delta))}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function Changes({
  changes,
  totals,
  field,
  onSelect,
}: {
  changes: DiffChange[];
  totals: Record<string, number>;
  field: string | null;
  onSelect: (urlId: number) => void;
}) {
  const total = field === null ? Object.values(totals).reduce((sum, count) => sum + count, 0) : totals[field] ?? 0;
  return (
    <>
      {changes.map((change) => (
        <div key={`${change.id}-${change.field}`} className="change-line">
          <span className="field">{fieldLabels[change.field] ?? change.field}</span>
          <span className="where">
            <button className="link" onClick={() => onSelect(change.id)} title="Открыть эту страницу">
              {readableUrl(change.url)}
            </button>
            <span className="values">
              <Value field={change.field} value={change.before} old />
              <span className="arrow">→</span>
              <Value field={change.field} value={change.after} />
            </span>
          </span>
        </div>
      ))}
      <Cut shown={changes.length} total={total} empty="Ничего не изменилось" />
    </>
  );
}

function Value({ field, value, old }: { field: string; value: string | null; old?: boolean }) {
  if (value === null || value === "") return <span className="value none">нет</span>;
  if (field === "status") {
    const status = Number(value);
    return (
      <span className="code" style={{ background: `var(${statusColor(statusClass(status))})` }}>
        {status || "—"}
      </span>
    );
  }
  return <span className={`value ${old ? "old" : ""}`}>{field === "words" ? formatCount(Number(value)) : value}</span>;
}

function Issues({
  issues,
  total,
  titles,
  code,
  fixed,
  onAll,
  onSelect,
}: {
  issues: DiffIssue[];
  total: number;
  titles: Map<string, { title: string | null; severity: Comparison["shifts"][number]["severity"] }>;
  code: string | null;
  fixed: boolean;
  onAll: () => void;
  onSelect: (urlId: number) => void;
}) {
  const groups: { code: string; issues: DiffIssue[] }[] = [];
  for (const issue of issues) {
    const last = groups[groups.length - 1];
    if (last && last.code === issue.code) last.issues.push(issue);
    else groups.push({ code: issue.code, issues: [issue] });
  }
  return (
    <>
      {code !== null && (
        <button className="ghost" onClick={onAll}>
          Все проблемы
        </button>
      )}
      {groups.map((group) => {
        const shift = titles.get(group.code);
        return (
          <div key={group.code} className="broken-group">
            <div className="broken-target">
              <span className="code" style={{ background: `var(${fixed ? "--ok" : severityColor(shift?.severity ?? null)})` }}>
                <Icon name={fixed ? "check" : "alert"} size={14} />
              </span>
              <span className="where">{shift?.title ?? group.code}</span>
            </div>
            {group.issues.map((issue) => (
              <LinkLine key={`${issue.id}-${issue.code}`} url={issue.url} anchors={[]} badges={[]} onOpen={() => onSelect(issue.id)} />
            ))}
          </div>
        );
      })}
      <Cut shown={issues.length} total={total} empty={fixed ? "Ничего не исправлено" : "Новых проблем нет"} />
    </>
  );
}

/// The end of a list: nothing in it, or how much of it the CSV has beyond.
function Cut({ shown, total, empty }: { shown: number; total: number; empty: string }) {
  if (total === 0) return <div className="empty" style={{ height: 160 }}>{empty}</div>;
  if (shown >= total) return null;
  return (
    <div className="cut">
      Показано {formatCount(shown)} из {formatCount(total)}, все — в CSV
    </div>
  );
}
