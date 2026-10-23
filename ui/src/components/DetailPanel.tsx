import { useEffect, useState } from "react";
import { api, type LinkPage, type Outlink, type UrlDetail } from "../api";
import { formatBytes, formatCount, readableUrl, severityColor, severityLabels, statusClass, statusColor } from "../labels";
import { Icon } from "./Icon";
import { PageContent } from "./PageContent";

type Tab = "summary" | "issues" | "in" | "out" | "content" | "headers" | "custom";

/// The selected URL: the status as a block, a strip of facts, then tabs.
/// A page in its links opens here in turn.
export function DetailPanel({ urlId, onOpen }: { urlId: number; onOpen?: (urlId: number) => void }) {
  const [detail, setDetail] = useState<UrlDetail | null>(null);
  const [tab, setTab] = useState<Tab>("summary");
  const [error, setError] = useState<string | null>(null);
  const [inlinks, setInlinks] = useState<LinkPage | null>(null);
  const [outlinks, setOutlinks] = useState<Outlink[] | null>(null);

  useEffect(() => {
    setDetail(null);
    setError(null);
    setInlinks(null);
    setOutlinks(null);
    let current = true;
    api
      .url(urlId)
      .then((found) => current && setDetail(found))
      .catch((failure: Error) => current && setError(failure.message));
    api
      .inlinks(urlId)
      .then((found) => current && setInlinks(found))
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [urlId]);

  useEffect(() => {
    if (tab !== "out" || outlinks !== null) return;
    let current = true;
    api
      .outlinks(urlId)
      .then((found) => current && setOutlinks(found))
      .catch(() => current && setOutlinks([]));
    return () => {
      current = false;
    };
  }, [tab, urlId, outlinks]);

  const more = () => {
    if (!inlinks) return;
    api.inlinks(urlId, inlinks.rows.length).then((next) => setInlinks({ ...next, rows: [...inlinks.rows, ...next.rows] }));
  };

  if (error) return <div className="error-banner">{error}</div>;
  if (!detail) return <div className="empty">Загрузка</div>;

  const facts = detail.facts;
  const canonical = facts?.canonicals[0];
  const noindex = facts?.meta_robots.some((value) => value.includes("noindex")) ?? false;
  const selfCanonical = !canonical || canonical === detail.url;
  const indexable = detail.status >= 200 && detail.status < 300 && !!facts && !noindex && selfCanonical;
  const custom = facts?.custom;
  const hasCustom = !!custom && ((custom.extracted?.length ?? 0) > 0 || (custom.found?.length ?? 0) > 0);
  const h1 = facts?.headings.filter(([level]) => level === 1).map(([, heading]) => heading) ?? [];

  return (
    <div>
      <div className="detail-head">
        <span className="status-block" style={{ background: `var(${statusColor(statusClass(detail.status))})` }}>
          {detail.status || "—"}
        </span>
        <div className="detail-url">
          <a href={detail.url} target="_blank" rel="noreferrer">
            {readableUrl(detail.url)}
          </a>
        </div>
      </div>

      <div className="facts">
        {facts && (
          <Flag
            ok={indexable}
            label={indexable ? "Индексируется" : noindex ? "noindex" : !selfCanonical ? "Canonical на другой URL" : "Не индексируется"}
          />
        )}
        <Flag ok={detail.in_sitemap} label="В sitemap" quiet={!detail.in_sitemap} />
        {detail.render && <Flag ok={!detail.render.error} label={detail.render.error ? "JS-рендер с ошибкой" : "JS выполнен"} />}
        {detail.depth !== null && <Fact value={String(detail.depth)} name="клик от главной" />}
        {detail.status > 0 && <Fact value={`${detail.ttfb_ms} мс`} name="ответ" />}
        <Fact value={formatBytes(detail.size)} name="" />
        {facts && <Fact value={formatCount(facts.word_count)} name="слов" />}
        {facts && <Fact value={`${facts.internal_links} из ${facts.links}`} name="ссылок внутри" icon="link" />}
        {facts && facts.images > 0 && (
          <Fact
            value={`${facts.images_without_alt} из ${facts.images}`}
            name="картинок без alt"
            icon="image"
            colour={facts.images_without_alt > 0 ? "--warn" : undefined}
          />
        )}
        {facts && facts.json_ld + facts.microdata > 0 && <Fact value={String(facts.json_ld + facts.microdata)} name="блоков разметки" icon="code" />}
        {detail.issues.length > 0 && (
          <Fact value={String(detail.issues.length)} name="проблем" icon="alert" colour="--warn" onClick={() => setTab("issues")} />
        )}
      </div>

      <div className="tabs" role="tablist">
        <TabButton tab="summary" current={tab} onSelect={setTab} label="Страница" />
        <TabButton tab="issues" current={tab} onSelect={setTab} label={`Проблемы ${detail.issues.length}`} />
        <TabButton tab="in" current={tab} onSelect={setTab} label={`Входящие ${inlinks?.ready ? formatCount(inlinks.total) : "…"}`} />
        {facts && <TabButton tab="out" current={tab} onSelect={setTab} label={`Исходящие ${formatCount(facts.links)}`} />}
        {facts && <TabButton tab="content" current={tab} onSelect={setTab} label="Содержимое" />}
        <TabButton tab="headers" current={tab} onSelect={setTab} label="Заголовки ответа" />
        {hasCustom && <TabButton tab="custom" current={tab} onSelect={setTab} label="Своё извлечение" />}
      </div>

      {tab === "summary" && (
        <div className="kv">
          <Row name="Title" value={facts?.titles.join("\n")} missing={!!facts} />
          <Row name="Description" value={facts?.descriptions.join("\n")} missing={!!facts} />
          <Row name="H1" value={h1.join("\n")} missing={!!facts} />
          <Row name="Canonical" value={canonical ? (selfCanonical ? "на себя" : canonical) : undefined} />
          <Row name="Meta robots" value={facts?.meta_robots.join(", ")} />
          <Row name="Язык" value={facts?.lang ?? undefined} />
          {detail.location && <Row name="Редирект на" value={detail.location} />}
          {detail.error && <Row name="Ошибка" value={detail.error} colour="--bad" />}
          {detail.render && detail.render.console_errors > 0 && (
            <Row name={`Консоль, ${detail.render.console_errors}`} value={detail.render.first_console_error ?? undefined} />
          )}
        </div>
      )}
      {tab === "issues" &&
        (detail.issues.length === 0 ? (
          <div className="kv">
            <Row name="Проблем" value="нет" colour="--ok" />
          </div>
        ) : (
          detail.issues.map((issue) => (
            <div key={issue.code} className="issue-row" title={issue.description ?? undefined}>
              <div
                style={{ background: `var(${severityColor(issue.severity)})` }}
                aria-label={issue.severity ? `${severityLabels[issue.severity]} важность` : undefined}
              />
              <div>
                <div style={{ fontWeight: 600 }}>{issue.title ?? issue.code}</div>
                {issue.detail && <div style={{ color: "var(--ink-2)" }}>{issue.detail}</div>}
              </div>
            </div>
          ))
        ))}
      {tab === "in" &&
        (!inlinks ? (
          <div className="kv">
            <Row name="Входящие" value="загрузка" />
          </div>
        ) : !inlinks.ready ? (
          <div className="kv">
            <Row name="Входящие" value="появятся, когда обход и проверки закончатся" />
          </div>
        ) : inlinks.total === 0 ? (
          <div className="kv">
            <Row name="Входящие" value="ни одна страница обхода сюда не ссылается" colour="--warn" />
          </div>
        ) : (
          <div className="links">
            {inlinks.rows.map((link) => (
              <LinkLine
                key={`${link.from_id}-${link.redirect}`}
                url={link.from_url}
                onOpen={onOpen && (() => onOpen(link.from_id))}
                anchors={link.redirect ? [] : link.anchors}
                badges={[link.redirect && "редирект", link.nofollow && "nofollow", link.links > 1 && `×${link.links}`]}
              />
            ))}
            {inlinks.rows.length < inlinks.total && (
              <button className="ghost" onClick={more}>
                Ещё {formatCount(inlinks.total - inlinks.rows.length)}
              </button>
            )}
          </div>
        ))}
      {tab === "out" &&
        (!outlinks ? (
          <div className="kv">
            <Row name="Исходящие" value="загрузка" />
          </div>
        ) : (
          <div className="links">
            {outlinks.map((link, at) => (
              <LinkLine
                key={at}
                url={link.url}
                status={link.status}
                onOpen={onOpen && link.id !== null ? () => onOpen(link.id as number) : undefined}
                anchors={[link.anchor || (link.image ? "[картинка]" : "")].filter(Boolean)}
                badges={[link.nofollow && "nofollow", link.position !== "content" && positions[link.position]]}
              />
            ))}
          </div>
        ))}
      {tab === "content" && <PageContent urlId={urlId} />}
      {tab === "headers" && (
        <div className="kv">
          {detail.headers.map(([name, value], at) => (
            <Row key={at} name={name} value={value} />
          ))}
        </div>
      )}
      {tab === "custom" && custom && (
        <div className="kv">
          {(custom.found ?? []).map(([name, count]) => (
            <Row key={`s-${name}`} name={name} value={`найдено ${count}`} />
          ))}
          {(custom.extracted ?? []).map(([name, values]) => (
            <Row key={`e-${name}`} name={name} value={values.join("\n")} />
          ))}
          {(custom.errors ?? []).map(([name, message]) => (
            <Row key={`x-${name}`} name={name} value={message} colour="--bad" />
          ))}
        </div>
      )}
    </div>
  );
}

const positions: Record<string, string> = { header: "шапка", nav: "меню", footer: "подвал", aside: "сбоку" };

/// One link in a list: the page or target, its anchors, what is special
/// about it; the target's status as a block, for a list of targets.
export function LinkLine({
  url,
  status,
  anchors,
  badges,
  onOpen,
}: {
  url: string;
  status?: number | null;
  anchors: string[];
  badges: (string | false)[];
  onOpen?: () => void;
}) {
  return (
    <div className={`link-line ${status === undefined ? "bare" : ""}`}>
      {status !== undefined && (
        <span className="code" style={{ background: status === null ? "var(--panel-3)" : `var(${statusColor(statusClass(status))})` }}>
          {status === null ? "—" : status || "—"}
        </span>
      )}
      <span className="where">
        {onOpen ? (
          <button className="link" onClick={onOpen} title="Открыть эту страницу">
            {readableUrl(url)}
          </button>
        ) : (
          <span className="plain">{readableUrl(url)}</span>
        )}
        {anchors.length > 0 && <span className="anchors">{anchors.map((anchor) => `«${anchor}»`).join(", ")}</span>}
      </span>
      <span className="badges">
        {badges.filter(Boolean).map((badge) => (
          <span key={badge as string} className="badge-soft">
            {badge}
          </span>
        ))}
      </span>
    </div>
  );
}

function Row({ name, value, missing, colour }: { name: string; value?: string; missing?: boolean; colour?: string }) {
  const empty = !value;
  return (
    <>
      <div>{name}</div>
      <div style={{ whiteSpace: "pre-line", color: empty && missing ? "var(--bad)" : colour ? `var(${colour})` : undefined }}>
        {empty ? (missing ? "нет" : "—") : value}
      </div>
    </>
  );
}

function Fact({
  value,
  name,
  icon,
  colour,
  onClick,
}: {
  value: string;
  name: string;
  icon?: string;
  colour?: string;
  onClick?: () => void;
}) {
  return (
    <span className={`fact ${onClick ? "clickable" : ""}`} onClick={onClick} style={{ color: colour ? `var(${colour})` : undefined }}>
      {icon && <Icon name={icon} size={14} />}
      <span className="figures">{value}</span>
      {name && <span style={{ color: colour ? undefined : "var(--ink-3)" }}>{name}</span>}
    </span>
  );
}

function Flag({ ok, label, quiet }: { ok: boolean; label: string; quiet?: boolean }) {
  const colour = quiet ? "--ink-3" : ok ? "--ok" : "--bad";
  return (
    <span className="fact" style={{ color: `var(${colour})`, fontWeight: 600 }}>
      <Icon name={ok ? "check" : "cross"} size={14} /> {label}
    </span>
  );
}

function TabButton({ tab, current, onSelect, label }: { tab: Tab; current: Tab; onSelect: (tab: Tab) => void; label: string }) {
  return (
    <button role="tab" aria-selected={tab === current} className={tab === current ? "on" : undefined} onClick={() => onSelect(tab)}>
      {label}
    </button>
  );
}
