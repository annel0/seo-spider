import { useMemo } from "react";
import type { Overview as OverviewData, Progress, Summary } from "../api";
import { useColors, type SpeedPoint } from "../hooks";
import {
  contentTypeLabels,
  formatBytes,
  formatCount,
  indexabilityColor,
  indexabilityLabels,
  severityColor,
  severityLabels,
  statusColor,
  statusLabels,
} from "../labels";
import { Chart, type ChartOption } from "./Chart";

/// The table filter a status or indexability group opens, when there is one.
const statusFilter: Record<string, string> = {
  "2xx": "2xx",
  "3xx": "3xx",
  "4xx": "4xx",
  "5xx": "5xx",
  no_response: "no_response",
  blocked: "blocked",
};
const indexabilityFilter: Record<string, string> = {
  indexable: "html",
  noindex: "noindex",
  redirect: "3xx",
  error: "4xx",
  blocked: "blocked",
};

// Plex Condensed has no Cyrillic: Onest carries the letters next to its figures.
const CHART_FIGURES = "IBM Plex Sans Condensed, Onest, sans-serif";
const CHART_TEXT = "Onest, sans-serif";

const responseBuckets: [number, string][] = [
  [100, "до 0,1 с"],
  [300, "0,1–0,3"],
  [600, "0,3–0,6"],
  [1000, "0,6–1"],
  [2000, "1–2 с"],
  [0, "больше 2 с"],
];

export function Overview({
  overview,
  summary,
  progress,
  speed,
  onOpenFilter,
}: {
  overview: OverviewData | null;
  summary: Summary | null;
  progress: Progress | null;
  speed: SpeedPoint[];
  onOpenFilter: (filter: string) => void;
}) {
  const { dark, color } = useColors();

  // Chart styling from the theme; `dark` stands for every colour it changes.
  const style = useMemo(() => {
    const axis = {
      axisLine: { lineStyle: { color: color("--rule-strong") } },
      axisTick: { lineStyle: { color: color("--rule-strong") } },
      axisLabel: { color: color("--ink-2"), fontFamily: CHART_FIGURES, fontSize: 12 },
      splitLine: { lineStyle: { color: color("--rule") } },
    };
    const tooltip = {
      backgroundColor: color("--panel"),
      borderColor: color("--rule-strong"),
      borderRadius: 0,
      textStyle: { color: color("--ink"), fontFamily: CHART_TEXT },
    };
    return { axis, tooltip };
  }, [dark]);

  const charts = useMemo(() => {
    if (!overview) return null;
    const { axis, tooltip } = style;
    const columns = (labels: string[], values: { value: number; color: string }[]): ChartOption => ({
      tooltip: { ...tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
      grid: { left: 44, right: 8, top: 8, bottom: 26 },
      xAxis: { type: "category", data: labels, ...axis, axisLabel: { ...axis.axisLabel, interval: 0 } },
      yAxis: { type: "value", ...axis, minInterval: 1 },
      series: [
        {
          type: "bar",
          barCategoryGap: "18%",
          data: values.map(({ value, color: fill }) => ({ value, itemStyle: { color: fill } })),
        },
      ],
    });
    const depths = columns(
      overview.depths.map(([depth]) => (depth < 0 ? "нет" : String(depth))),
      overview.depths.map(([, count]) => ({ value: count, color: color("--accent") })),
    );
    const times = new Map(overview.response_times);
    const paint = ["--ok", "--ok", "--info-soft", "--warn", "--bad", "--bad-strong"];
    const responseTimes = columns(
      responseBuckets.map(([, label]) => label),
      responseBuckets.map(([bucket], at) => ({ value: times.get(bucket) ?? 0, color: color(paint[at]) })),
    );
    const kinds: ChartOption = {
      tooltip: { ...tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
      grid: { left: 76, right: 40, top: 4, bottom: 4 },
      xAxis: { type: "value", show: false },
      yAxis: {
        type: "category",
        inverse: true,
        data: overview.content_types.map(([kind]) => contentTypeLabels[kind] ?? kind),
        ...axis,
        axisTick: { show: false },
      },
      series: [
        {
          type: "bar",
          barCategoryGap: "28%",
          data: overview.content_types.map(([, count]) => count),
          itemStyle: { color: color("--ink-2") },
          label: { show: true, position: "right", color: color("--ink-2"), fontFamily: CHART_FIGURES },
        },
      ],
    };
    return { depths, responseTimes, kinds };
  }, [overview, style]);

  const topIssues = useMemo(() => {
    const { axis, tooltip } = style;
    const issues = (summary?.issues ?? []).slice(0, 12);
    const byTitle = new Map(issues.map((issue) => [issue.title ?? issue.code, issue.code]));
    const option: ChartOption = {
      tooltip: { ...tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
      grid: { left: 270, right: 48, top: 2, bottom: 2 },
      xAxis: { type: "value", show: false },
      yAxis: {
        type: "category",
        inverse: true,
        data: issues.map((issue) => issue.title ?? issue.code),
        ...axis,
        axisTick: { show: false },
        axisLabel: { color: color("--ink"), fontFamily: CHART_TEXT, fontSize: 13, width: 255, overflow: "truncate" },
      },
      series: [
        {
          type: "bar",
          barCategoryGap: "30%",
          data: issues.map((issue) => ({ value: issue.count, itemStyle: { color: color(severityColor(issue.severity)) } })),
          label: { show: true, position: "right", color: color("--ink-2"), fontFamily: CHART_FIGURES, fontSize: 13 },
        },
      ],
    };
    return { option, byTitle, height: Math.max(80, issues.length * 28 + 8) };
  }, [summary, style]);

  const speedChart = useMemo<ChartOption>(() => {
    const { axis, tooltip } = style;
    return {
      tooltip: { ...tooltip, trigger: "axis", valueFormatter: (value: unknown) => `${Number(value).toFixed(0)} URL/с` },
      grid: { left: 44, right: 8, top: 8, bottom: 22 },
      xAxis: { type: "value", min: "dataMin", max: "dataMax", ...axis, axisLabel: { ...axis.axisLabel, formatter: "{value} с" } },
      yAxis: { type: "value", ...axis },
      series: [
        {
          type: "line",
          step: "end",
          showSymbol: false,
          data: speed.map((point) => [Math.round(point.seconds), Math.round(point.rate)]),
          lineStyle: { color: color("--accent"), width: 2 },
          areaStyle: { color: color("--accent-wash") },
        },
      ],
    };
  }, [speed, style]);

  if (!overview || !charts) {
    return <div className="empty">Загрузка</div>;
  }

  const statusCount = (key: string) => overview.statuses.find(([status]) => status === key)?.[1] ?? 0;
  const errors = statusCount("4xx") + statusCount("5xx") + statusCount("no_response");
  const indexable = overview.indexability.find(([state]) => state === "indexable")?.[1] ?? 0;
  const ttfb = overview.average_ttfb_ms;
  const percent = (part: number, whole: number) => (whole > 0 ? Math.round((part * 100) / whole) : 0);
  const running = progress !== null && progress.state !== "finished" && progress.state !== "interrupted";

  return (
    <div className="sheet">
      <div className="ruled kpis">
        <Kpi value={formatCount(overview.urls)} name="URL на сайте" />
        <Kpi value={formatCount(overview.pages)} name="HTML-страниц" />
        <Kpi
          value={`${percent(indexable, overview.pages)}%`}
          name={`индексируется, ${formatCount(indexable)}`}
          colour={indexable > 0 ? "--ok" : undefined}
          onClick={() => onOpenFilter("html")}
        />
        <Kpi
          value={formatCount(errors)}
          name="ошибок ответа"
          colour={errors > 0 ? "--bad" : "--ok"}
          onClick={errors > 0 ? () => onOpenFilter("4xx") : undefined}
        />
        <Kpi value={`${Math.round(ttfb)} мс`} name="средний ответ" colour={ttfb < 300 ? "--ok" : ttfb < 1000 ? "--warn" : "--bad"} />
        <Kpi value={formatBytes(progress?.bytes || overview.bytes)} name="скачано" />
        <div className="kpi">
          <div className="severities">
            {overview.severities.map(([severity, count]) => (
              <span key={severity} className="severity" title={`${severityLabels[severity]} важность`}>
                <span className="square" style={{ background: `var(${severityColor(severity)})` }} />
                {formatCount(count)}
              </span>
            ))}
          </div>
          <div className="name">проблем по важности</div>
        </div>
      </div>

      <div className="ruled panels">
        {running && speed.length > 1 && (
          <div className="panel wide">
            <h3>Скорость обхода, URL в секунду</h3>
            <Chart option={speedChart} height={130} />
          </div>
        )}
        <div className="panel">
          <h3>Коды ответов</h3>
          <ShareBar
            items={overview.statuses.map(([key, count]) => ({ key, label: statusLabels[key] ?? key, count, colour: statusColor(key) }))}
            onOpen={(key) => statusFilter[key] && onOpenFilter(statusFilter[key])}
          />
        </div>
        <div className="panel">
          <h3>Индексируемость HTML</h3>
          <ShareBar
            items={overview.indexability.map(([key, count]) => ({
              key,
              label: indexabilityLabels[key] ?? key,
              count,
              colour: indexabilityColor(key),
            }))}
            onOpen={(key) => indexabilityFilter[key] && onOpenFilter(indexabilityFilter[key])}
          />
        </div>
        <div className="panel">
          <h3>Глубина от главной, кликов</h3>
          <Chart option={charts.depths} height={180} />
        </div>
        <div className="panel">
          <h3>Время ответа сервера</h3>
          <Chart option={charts.responseTimes} height={180} />
        </div>
        <div className="panel">
          <h3>Типы контента</h3>
          <Chart option={charts.kinds} height={Math.max(90, overview.content_types.length * 26 + 8)} />
        </div>
        {topIssues.byTitle.size > 0 && (
          <div className="panel wide">
            <h3>Главные проблемы</h3>
            <Chart
              option={topIssues.option}
              height={topIssues.height}
              onClick={(title) => {
                const code = topIssues.byTitle.get(title);
                if (code) onOpenFilter(`issue:${code}`);
              }}
            />
          </div>
        )}
      </div>
    </div>
  );
}

function Kpi({ value, name, colour, onClick }: { value: string; name: string; colour?: string; onClick?: () => void }) {
  return (
    <div className={`kpi ${onClick ? "clickable" : ""}`} onClick={onClick}>
      <div className="value" style={{ color: colour ? `var(${colour})` : undefined }}>
        {value}
      </div>
      <div className="name">{name}</div>
    </div>
  );
}

type Share = { key: string; label: string; count: number; colour: string };

/// A group's parts as one bar, and a legend that counts them; a click on
/// either opens the matching table.
function ShareBar({ items, onOpen }: { items: Share[]; onOpen: (key: string) => void }) {
  const total = items.reduce((sum, item) => sum + item.count, 0);
  return (
    <>
      <div className="bar" role="img" aria-label={items.map((item) => `${item.label} ${item.count}`).join(", ")}>
        {items.map((item) => (
          <span
            key={item.key}
            title={`${item.label}: ${formatCount(item.count)}`}
            onClick={() => onOpen(item.key)}
            style={{ flexGrow: item.count, background: `var(${item.colour})` }}
          />
        ))}
      </div>
      <div className="legend">
        {items.map((item) => (
          <LegendRow key={item.key} colour={item.colour} label={item.label} count={item.count} total={total} onClick={() => onOpen(item.key)} />
        ))}
      </div>
    </>
  );
}

function LegendRow({
  colour,
  label,
  count,
  total,
  onClick,
}: {
  colour: string;
  label: string;
  count: number;
  total: number;
  onClick?: () => void;
}) {
  return (
    <div onClick={onClick}>
      <span className="key">
        <span className="square" style={{ background: `var(${colour})` }} />
      </span>
      <span>{label}</span>
      <span className="num">{formatCount(count)}</span>
      <span className="num pct">{total > 0 ? `${Math.round((count * 100) / total)}%` : ""}</span>
    </div>
  );
}
