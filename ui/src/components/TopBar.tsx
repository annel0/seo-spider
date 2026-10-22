import { useState } from "react";
import { isActive, type Crawl, type Overview, type Preset, type Progress } from "../api";
import type { SpeedPoint } from "../hooks";
import { formatBytes, formatCount, formatDuration, stateLabels, statusColor, statusLabels } from "../labels";
import { Icon } from "./Icon";

const presetLabels: Record<string, string> = {
  auto: "Авто",
  headers: "Заголовки",
  minimal: "Минимум",
  seo: "SEO",
  js: "SEO + JS",
  dump: "Дамп",
};

const stateColour: Record<string, string> = {
  probing: "--info-soft",
  crawling: "--accent",
  paused: "--warn",
  stopping: "--warn",
  checking: "--info-soft",
  finished: "--ok",
  stopped: "--warn",
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
  exportHref,
  preset,
  limit,
  limitHint,
  threads,
  changed,
  settingsOpen,
  onPreset,
  onLimit,
  onThreads,
  onSettings,
  onContinue,
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
  /// The whole crawl as XLSX, when there is one to export.
  exportHref: string | null;
  preset: string;
  /// The URL limit typed here, and the preset's.
  limit: number | null;
  limitHint: number | null;
  /// Pages rendered at once; null when the settings render nothing.
  threads: number | null;
  /// Settings that differ from the preset.
  changed: number;
  settingsOpen: boolean;
  onPreset: (name: string) => void;
  onLimit: (limit: number | undefined) => void;
  onThreads: (threads: number | undefined) => void;
  onSettings: () => void;
  /// Goes on with the selected crawl, interrupted or stopped.
  onContinue: () => void;
  onSelect: (id: number | null) => void;
  onStart: (startUrl: string) => void;
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
}) {
  const [url, setUrl] = useState("");
  const [invalid, setInvalid] = useState(false);
  const active = isActive(progress?.state);
  const chosen = presets.find((item) => item.name === preset);
  const selected = crawls.find((crawl) => crawl.id === crawlId);
  const continuable = !active && (selected?.state === "interrupted" || selected?.state === "stopped");

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
    onStart(address);
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
        style={{ maxWidth: 240 }}
        aria-label="Обход"
      >
        <option value="">Новый обход</option>
        {crawls.map((crawl) => (
          <option key={crawl.id} value={crawl.id}>
            #{crawl.id} {hostOf(crawl.start_url)}, {formatCount(crawl.urls)} URL, {stateLabels[crawl.state].toLowerCase()}
          </option>
        ))}
      </select>
      {exportHref && (
        <a className="button" href={exportHref} download title="Все вкладки обхода одной книгой Excel">
          <Icon name="download" /> XLSX
        </a>
      )}
      {continuable && (
        <button
          onClick={onContinue}
          disabled={busy}
          title={`Продолжить с того места, где обход ${selected?.state === "stopped" ? "остановили" : "прервался"}${limit ? `, до ${formatCount(limit)} URL` : ""}`}
        >
          <Icon name="play" /> Продолжить
        </button>
      )}

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
                onClick={() => onPreset(item.name)}
                title={item.description}
              >
                {presetLabels[item.name] ?? item.title}
              </button>
            ))}
          </div>
          <input
            value={limit ?? ""}
            onChange={(event) => {
              const digits = event.target.value.replace(/\D/g, "");
              onLimit(digits ? Number(digits) : undefined);
            }}
            placeholder={limitHint ? `лимит ${formatCount(limitHint)}` : "лимит URL"}
            title="Сколько внутренних страниц обойти, не больше"
            aria-label="Лимит URL"
            className="figures"
            style={{ width: 120 }}
          />
          {threads !== null && (
            <input
              value={threads || ""}
              onChange={(event) => {
                const digits = event.target.value.replace(/\D/g, "");
                onThreads(digits ? Number(digits) : undefined);
              }}
              title="Сколько страниц выполняют JavaScript одновременно"
              aria-label="Потоков JS-рендера"
              className="figures"
              style={{ width: 52 }}
            />
          )}
          <button className={settingsOpen ? "on" : undefined} onClick={onSettings} title="Все настройки обхода">
            <Icon name="settings" /> Настройки
            {changed > 0 && <span className="badge">{changed}</span>}
          </button>
          <button className="primary" onClick={start} disabled={busy}>
            <Icon name="play" /> Старт
          </button>
          {chosen && <div className="hint">{chosen.description}</div>}
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
        {progress.rendered > 0 && <Readout value={formatCount(progress.rendered)} name="с JS-рендером" />}
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
