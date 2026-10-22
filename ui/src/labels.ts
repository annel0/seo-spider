// What the UI calls things, and the colours that carry meaning: status
// classes, severities, indexability. Colours are CSS variables from
// theme.css, read at run time for the charts.

import type { CrawlState, Severity } from "./api";

export const filterLabels: Record<string, string> = {
  all: "Все",
  html: "HTML",
  "2xx": "2xx",
  "3xx": "3xx",
  "4xx": "4xx",
  "5xx": "5xx",
  no_response: "Нет ответа",
  blocked: "robots.txt",
  noindex: "noindex",
};

export const stateLabels: Record<CrawlState, string> = {
  crawling: "Обход",
  paused: "Пауза",
  stopping: "Остановка",
  checking: "Проверки",
  finished: "Готово",
  failed: "Ошибка",
  interrupted: "Прерван",
};

export const severityLabels: Record<Severity, string> = {
  high: "Высокая",
  medium: "Средняя",
  low: "Низкая",
};

export const statusLabels: Record<string, string> = {
  "2xx": "2xx",
  "3xx": "3xx",
  "4xx": "4xx",
  "5xx": "5xx",
  no_response: "Нет ответа",
  blocked: "robots.txt",
};

export const indexabilityLabels: Record<string, string> = {
  indexable: "Индексируется",
  noindex: "noindex",
  canonicalised: "Canonical",
  redirect: "Редирект",
  error: "Ошибка",
  blocked: "robots.txt",
};

export const contentTypeLabels: Record<string, string> = {
  html: "HTML",
  css: "CSS",
  js: "JS",
  image: "Изобр.",
  font: "Шрифты",
  pdf: "PDF",
  data: "JSON/XML",
  other: "Прочее",
  none: "Нет",
};

/// The CSS variable that colours each group.
export const statusColor = (key: string) =>
  ({
    "2xx": "--ok",
    "3xx": "--info",
    "4xx": "--bad",
    "5xx": "--bad-strong",
    no_response: "--muted",
    blocked: "--violet",
  })[key] ?? "--muted";

export const indexabilityColor = (key: string) =>
  ({
    indexable: "--ok",
    noindex: "--warn",
    canonicalised: "--info",
    redirect: "--info-soft",
    error: "--bad",
    blocked: "--violet",
  })[key] ?? "--muted";

export const severityColor = (severity: Severity | null) =>
  severity === "high" ? "--bad" : severity === "medium" ? "--warn" : "--info-soft";

export const statusClass = (status: number) =>
  status === 0 ? "no_response" : `${Math.floor(status / 100)}xx`;

/// An address as people read it: percent-escapes of Cyrillic and the like
/// decoded; as is when it does not decode.
export const readableUrl = (url: string) => {
  try {
    return decodeURI(url);
  } catch {
    return url;
  }
};

export const formatCount = (value: number) => new Intl.NumberFormat("ru-RU").format(Math.round(value));

export const formatBytes = (bytes: number) => {
  if (bytes < 1024) return `${bytes} Б`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} КБ`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} МБ`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} ГБ`;
};

export const formatDuration = (ms: number) => {
  const seconds = Math.floor(ms / 1000);
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(minutes / 60);
  const two = (value: number) => String(value).padStart(2, "0");
  return hours > 0 ? `${hours}:${two(minutes % 60)}:${two(seconds % 60)}` : `${minutes}:${two(seconds % 60)}`;
};
