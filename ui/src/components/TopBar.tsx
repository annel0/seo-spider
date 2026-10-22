import { useState } from "react";
import { isActive, type Crawl, type Overview, type Preset, type Progress, type StartRequest } from "../api";
import type { SpeedPoint } from "../hooks";
import { formatBytes, formatCount, formatDuration, stateLabels, statusColor, statusLabels } from "../labels";
import { Icon } from "./Icon";

const presetLabels: Record<string, string> = {
  headers: "Заголовки",
  minimal: "Минимум",
  seo: "SEO",
  js: "Рендер",
  dump: "Дамп",
};

const stateColour: Record<string, string> = {
  crawling: "--accent",
  paused: "--warn",
  stopping: "--warn",
  checking: "--info-soft",
  finished: "--ok",
  failed: "--bad",
  interrupted: "--muted",
};

/// The command strip: the crawl picker, the start form, the controls of a
/// running crawl.
export function CommandBar({
  presets,
  crawls,
  crawlId,
  progress,
  busy,
  onSelect,
  onStart,
  onPause,
  onResume,
  onStop,
}: {
  presets: Preset[];
  crawls: Crawl[];
  crawlId: number | null;
  progress: Progress | null;
  busy: boolean;
  onSelect: (id: number | null) => void;
  onStart: (request: StartRequest) => void;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
}) {
  const [url, setUrl] = useState("");
  const [preset, setPreset] = useState("seo");
  const [limit, setLimit] = useState("");
  const [threads, setThreads] = useState("4");
  const [invalid, setInvalid] = useState(false);
  const active = isActive(progress?.state);
  const renders = preset === "js" || preset === "dump";

  const start = () => {
    const typed = url.trim();
    const address = /^https?:\/\//.test(typed) ? typed : `https://${typed}`;
    let valid = typed.length > 0;
    try {
      new URL(address);
    } catch {
      valid = false;
    }
    if (!valid) {
      setInvalid(true);
      return;
    }
    onStart({
      start_url: address,
      preset,
      max_urls: limit ? Number(limit) : undefined,
      render_threads: renders ? Number(threads) || 4 : undefined,
    });
  };

  return (
    <header className="command">
      <span className="brand">
        <span className="brand-mark" />
        seo-spider
      </span>
      <select
        value={crawlId ?? ""}
        onChange={(event) => onSelect(event.target.value ? Number(event.target.value) : null)}
        style={{ maxWidth: 260 }}
        aria-label="Обход"
      >
        <option value="">Новый обход</option>
        {crawls.map((crawl) => (
          <option key={crawl.id} value={crawl.id}>
            #{crawl.id} {hostOf(crawl.start_url)}, {formatCount(crawl.urls)} URL, {stateLabels[crawl.state].toLowerCase()}
          </option>
        ))}
      </select>

      {active && crawlId !== null ? (
        <>
          <span style={{ flex: 1 }} />
          {progress?.state === "paused" ? (
            <button className="primary" onClick={onResume}>
              <Icon name="play" /> Продолжить
            </button>
          ) : (
            <button onClick={onPause} disabled={progress?.state !== "crawling"}>
              <Icon name="pause" /> Пауза
            </button>
          )}
          <button onClick={onStop} disabled={progress?.state === "stopping" || progress?.state === "checking"}>
            <Icon name="stop" /> Стоп
          </button>
        </>
      ) : (
        <>
          <input
            value={url}
            onChange={(event) => {
              setUrl(event.target.value);
              setInvalid(false);
            }}
            onKeyDown={(event) => event.key === "Enter" && start()}
            placeholder="example.com"
            aria-label="Адрес сайта"
            aria-invalid={invalid}
            style={{ flex: 1, minWidth: 220, borderColor: invalid ? "var(--bad)" : undefined }}
          />
          <div className="segmented" role="radiogroup" aria-label="Пресет">
            {presets.map((item) => (
              <button
                key={item.name}
                role="radio"
                aria-checked={item.name === preset}
                className={item.name === preset ? "on" : undefined}
                onClick={() => setPreset(item.name)}
                title={item.description}
              >
                {presetLabels[item.name] ?? item.title}
              </button>
            ))}
          </div>
          <input
            value={limit}
            onChange={(event) => setLimit(event.target.value.replace(/\D/g, ""))}
            placeholder="лимит URL"
            aria-label="Лимит URL"
            className="figures"
            style={{ width: 100 }}
          />
          {renders && (
            <input
              value={threads}
              onChange={(event) => setThreads(event.target.value.replace(/\D/g, ""))}
              title="Потоков рендера"
              aria-label="Потоков рендера"
              className="figures"
              style={{ width: 52 }}
            />
          )}
          <button className="primary" onClick={start} disabled={busy}>
            <Icon name="play" /> Старт
          </button>
        </>
      )}
    </header>
  );
}

/// The crawl's signal: while it runs, a band of what came back fine, what
/// failed and (hatched) what is still queued, over every URL; once it is
/// over, the site's URLs by status. Then its readouts.
export function Telemetry({
  progress,
  overview,
  speed,
}: {
  progress: Progress | null;
  overview: Overview | null;
  speed: SpeedPoint[];
}) {
  if (!progress) return <div className="telemetry"><div className="spectrum" /></div>;
  const active = isActive(progress.state);
  const statuses = overview?.statuses ?? [];
  const recorded = statuses.reduce((sum, [, count]) => sum + count, 0);
  const waiting = active ? progress.queued + progress.in_flight : 0;
  const rate = speed.length > 0 ? speed[speed.length - 1].rate : 0;
  const colour = stateColour[progress.state] ?? "--muted";
  return (
    <div className="telemetry">
      <div className="spectrum" role="img" aria-label="Ответы и очередь обхода">
        {active || recorded === 0 ? (
          <>
            <span title={`ответили ${formatCount(progress.urls - progress.errors)}`} style={{ flexGrow: progress.urls - progress.errors, background: "var(--ok)" }} />
            <span title={`ошибок ${formatCount(progress.errors)}`} style={{ flexGrow: progress.errors, background: "var(--bad)" }} />
            <span className="queued" title={`в очереди ${formatCount(waiting)}`} style={{ flexGrow: waiting }} />
          </>
        ) : (
          statuses.map(([key, count]) => (
            <span
              key={key}
              title={`${statusLabels[key] ?? key}: ${formatCount(count)}`}
              style={{ flexGrow: count, background: `var(${statusColor(key)})` }}
            />
          ))
        )}
      </div>
      <div className="readouts">
        <span className="state" style={{ color: `var(${colour})` }}>
          <span className="square" style={{ background: `var(${colour})` }} />
          {stateLabels[progress.state]}
        </span>
        <Readout value={formatCount(progress.urls)} name="URL" />
        {active && <Readout value={formatCount(progress.queued)} name="в очереди" />}
        {progress.state === "crawling" && <Readout value={`${Math.round(rate)}`} name="URL в секунду" />}
        <Readout value={formatCount(progress.errors)} name="ошибок" colour={progress.errors > 0 ? "--bad" : undefined} />
        {progress.rendered > 0 && <Readout value={formatCount(progress.rendered)} name="отрисовано" />}
        {progress.bytes > 0 && <Readout value={formatBytes(progress.bytes)} name="скачано" />}
        {progress.elapsed_ms > 0 && <Readout value={formatDuration(progress.elapsed_ms)} name="время" />}
        {progress.error && <span className="state" style={{ color: "var(--bad)" }}>{progress.error}</span>}
      </div>
    </div>
  );
}

function Readout({ value, name, colour }: { value: string; name: string; colour?: string }) {
  return (
    <div className="readout">
      <span className="value" style={{ color: colour ? `var(${colour})` : undefined }}>
        {value}
      </span>
      <span className="name">{name}</span>
    </div>
  );
}

const hostOf = (url: string) => {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
};
