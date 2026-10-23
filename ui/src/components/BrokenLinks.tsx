import { useEffect, useState } from "react";
import { api, downloads, type LinkItem, type LinkPage } from "../api";
import { formatCount, readableUrl, statusClass, statusColor } from "../labels";
import { LinkLine } from "./DetailPanel";
import { Icon } from "./Icon";

/// Every broken link of the crawl, by broken target: the target with its
/// status, then the pages it sits on with their anchors. A page opens in
/// the detail panel; the whole list goes out as CSV.
export function BrokenLinks({ crawlId, onSelect }: { crawlId: number; onSelect: (urlId: number) => void }) {
  const [page, setPage] = useState<LinkPage | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setPage(null);
    let current = true;
    api
      .brokenLinks(crawlId)
      .then((found) => current && setPage(found))
      .catch((failure: Error) => current && setError(failure.message));
    return () => {
      current = false;
    };
  }, [crawlId]);

  const more = () => {
    if (!page) return;
    api.brokenLinks(crawlId, page.rows.length).then((next) => setPage({ ...next, rows: [...page.rows, ...next.rows] }));
  };

  if (error) return <div className="error-banner">{error}</div>;
  if (!page) return <div className="empty">Загрузка</div>;
  if (!page.ready) return <div className="empty">Ссылки появятся, когда обход и проверки закончатся</div>;

  const groups: { target: LinkItem; sources: LinkItem[] }[] = [];
  for (const row of page.rows) {
    const last = groups[groups.length - 1];
    if (last && last.target.to_id === row.to_id) last.sources.push(row);
    else groups.push({ target: row, sources: [row] });
  }

  return (
    <>
      <div className="toolbar">
        <span className="title">Битые ссылки</span>
        <span className="figures" style={{ color: "var(--ink-3)", fontSize: 15 }}>
          {formatCount(page.total)}
        </span>
        <span style={{ flex: 1 }} />
        <a className="button" href={downloads.brokenLinks(crawlId)} download title="Все битые ссылки: страница, куда ведёт, код, анкор">
          <Icon name="download" /> CSV
        </a>
      </div>
      <div className="sheet" style={{ flex: 1, minHeight: 0 }}>
        {page.total === 0 ? (
          <div className="empty" style={{ height: 240 }}>
            Битых ссылок нет
          </div>
        ) : (
          <div className="links">
            {groups.map(({ target, sources }) => (
              <div key={target.to_id} className="broken-group">
                <div className="broken-target">
                  <span className="code" style={{ background: `var(${statusColor(statusClass(target.status))})` }}>
                    {target.status || "—"}
                  </span>
                  <span className="where">{readableUrl(target.to_url)}</span>
                </div>
                {sources.map((link) => (
                  <LinkLine
                    key={`${link.from_id}-${link.redirect}`}
                    url={link.from_url}
                    onOpen={() => onSelect(link.from_id)}
                    anchors={link.anchors}
                    badges={[link.redirect && "редирект", link.nofollow && "nofollow", link.links > 1 && `×${link.links}`]}
                  />
                ))}
              </div>
            ))}
            {page.rows.length < page.total && (
              <button className="ghost" onClick={more}>
                Ещё {formatCount(page.total - page.rows.length)}
              </button>
            )}
          </div>
        )}
      </div>
    </>
  );
}
