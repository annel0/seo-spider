import { useEffect, useState } from "react";
import { api, type UrlDetail } from "../api";
import { formatBytes, formatCount, readableUrl, severityColor, severityLabels, statusClass, statusColor } from "../labels";
import { Icon } from "./Icon";

type Tab = "summary" | "issues" | "headers" | "custom";

/// The selected URL: the status as a block, a strip of facts, then tabs.
export function DetailPanel({ urlId }: { urlId: number }) {
  const [detail, setDetail] = useState<UrlDetail | null>(null);
  const [tab, setTab] = useState<Tab>("summary");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setDetail(null);
    setError(null);
    let current = true;
    api
      .url(urlId)
      .then((found) => current && setDetail(found))
      .catch((failure: Error) => current && setError(failure.message));
    return () => {
      current = false;
    };
  }, [urlId]);

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
