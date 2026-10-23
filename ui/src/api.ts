// The server's API (crates/seo-server): the types mirror its JSON.

import type { Json, JsonObject, Schema } from "./settings";

export type Preset = { name: string; title: string; description: string };

export type CrawlState =
  | "probing"
  | "crawling"
  | "paused"
  | "stopping"
  | "checking"
  | "finished"
  | "stopped"
  | "failed"
  | "interrupted";

export type Crawl = {
  id: number;
  start_url: string;
  started_at: string;
  finished_at: string | null;
  urls: number;
  state: CrawlState;
};

export type Progress = {
  crawl_id: number;
  state: CrawlState;
  error: string | null;
  urls: number;
  pages: number;
  rendered: number;
  errors: number;
  queued: number;
  in_flight: number;
  bytes: number;
  elapsed_ms: number;
};

export type Severity = "high" | "medium" | "low";

export type IssueCount = {
  code: string;
  /// URLs with the issue.
  count: number;
  title: string | null;
  description: string | null;
  severity: Severity | null;
  category: string | null;
  /// The tab that lists it as a filter.
  tab: string | null;
};

/// What the auto mode decided about JavaScript, and the pages it read
/// both ways.
export type RenderDecision = {
  render: boolean;
  reason: string;
  samples: {
    url: string;
    words_raw: number;
    words_rendered: number;
    links_raw: number;
    links_rendered: number;
    changes?: string[];
    error?: string;
  }[];
};

export type Overview = {
  urls: number;
  pages: number;
  statuses: [string, number][];
  content_types: [string, number][];
  depths: [number, number][];
  response_times: [number, number][];
  indexability: [string, number][];
  average_ttfb_ms: number;
  bytes: number;
  severities: [Severity, number][];
  render_decision: RenderDecision | null;
};

/// How a column's values are drawn.
export type CellKind =
  | { type: "url" }
  | { type: "status" }
  | { type: "text" }
  | { type: "number" }
  | { type: "length"; low: number; high: number }
  | { type: "ms" }
  | { type: "bytes" }
  | { type: "bool" }
  | { type: "indexable" }
  | { type: "issues" };

export type TabColumn = { key: string; title: string; kind: CellKind; sortable: boolean; width: number };

/// A filter of a tab: a built-in one (`html`, `4xx`) or a check (`issue:<code>`).
export type TabFilter = {
  key: string;
  count: number;
  title?: string;
  description?: string;
  severity?: Severity;
};

export type TabDef = { key: string; title: string; total: number; columns: TabColumn[]; filters: TabFilter[] };

export type CellValue = string | number | boolean | null;

export type TabRows = { total: number; rows: { id: number; cells: CellValue[] }[] };

/// A column key of the tab.
export type Sort = { key: string; descending: boolean };

export type RowsQuery = { filter: string; search: string; sort: Sort };

export type UrlIssue = {
  code: string;
  detail: string | null;
  title: string | null;
  description: string | null;
  severity: Severity | null;
  category: string | null;
};

export type UrlDetail = {
  id: number;
  url: string;
  status: number;
  content_type: string | null;
  location: string | null;
  error: string | null;
  ttfb_ms: number;
  size: number;
  headers: [string, string][];
  internal: boolean;
  depth: number | null;
  in_sitemap: boolean;
  blocked_by_robots: boolean;
  facts: {
    source: "raw" | "rendered";
    lang: string | null;
    titles: string[];
    descriptions: string[];
    headings: [number, string][];
    canonicals: string[];
    meta_robots: string[];
    hreflangs: number;
    word_count: number;
    links: number;
    internal_links: number;
    images: number;
    images_without_alt: number;
    json_ld: number;
    microdata: number;
    custom: {
      extracted?: [string, string[]][];
      found?: [string, number][];
      errors?: [string, string][];
    };
  } | null;
  render: {
    error: string | null;
    settled: boolean;
    console_errors: number;
    first_console_error: string | null;
    failed_requests: number;
    first_failed_request: string | null;
  } | null;
  issues: UrlIssue[];
};

/// A link between two of the crawl's URLs.
export type LinkItem = {
  from_id: number;
  from_url: string;
  to_id: number;
  to_url: string;
  /// The target's status.
  status: number;
  redirect: boolean;
  links: number;
  nofollow: boolean;
  anchors: string[];
};

/// `ready` is false while the crawl's link graph is not written (it is at
/// the end of the crawl's checks).
export type LinkPage = { ready: boolean; total: number; rows: LinkItem[] };

/// A link as the page has it, with its target's row when crawled.
export type Outlink = {
  url: string;
  anchor: string;
  nofollow: boolean;
  position: string;
  image: boolean;
  id: number | null;
  status: number | null;
};

/// A block of visible text; `template`: the pages of the crawl it is on,
/// when it is the site's template (menus, header, footer).
export type TextBlock = { tag: string; text: string; template?: number };

/// One document of a page: its HTML (cut at 2 MB) and visible text.
export type PageDocument = { html: string; cut: boolean; blocks: TextBlock[] };

/// A page's documents: the crawl's (`kept`), or fetched and rendered now
/// (`live`); neither when the crawl kept none and none was asked for.
export type PageContent = {
  kept: boolean;
  live: boolean;
  raw: PageDocument | null;
  rendered: PageDocument | null;
  error: string | null;
  /// The crawl knows its site's template.
  templated: boolean;
};

export type StartRequest = {
  start_url: string;
  preset: string;
  /// A JSON merge patch over the preset's settings.
  config?: JsonObject;
};

// A token in the page's address (`?token=`) is kept for the session and
// sent with every request: the server asks for one off localhost.
const token = (() => {
  const given = new URLSearchParams(window.location.search).get("token");
  try {
    if (given) sessionStorage.setItem("seo-spider-token", given);
    return given ?? sessionStorage.getItem("seo-spider-token");
  } catch {
    return given;
  }
})();

export class ApiError extends Error {}

async function call<T>(path: string, init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (token) headers.authorization = `Bearer ${token}`;
  const response = await fetch(path, { ...init, headers });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      if (body?.error) message = body.error;
    } catch {
      // Not JSON: the status says it.
    }
    throw new ApiError(message);
  }
  return response.status === 204 ? (undefined as T) : ((await response.json()) as T);
}

/// Two crawls compared, the older first; each list cut at 500 rows (of
/// each field, for changes), `totals` count them whole.
export type DiffUrl = { url: string; id: number; status: number; internal: boolean };
export type DiffChange = { url: string; id: number; field: string; before: string | null; after: string | null };
export type DiffIssue = { url: string; id: number; code: string };
export type IssueShift = {
  code: string;
  title: string | null;
  severity: Severity | null;
  before: number;
  after: number;
  appeared: number;
  fixed: number;
};
export type Comparison = {
  old: number;
  new: number;
  /// How the crawls' settings differ where it shows.
  notes: string[];
  totals: {
    added: number;
    removed: number;
    changed: Record<string, number>;
    changed_urls: number;
    new_issues: number;
    fixed_issues: number;
  };
  added: DiffUrl[];
  removed: DiffUrl[];
  changed: DiffChange[];
  new_issues: DiffIssue[];
  fixed_issues: DiffIssue[];
  shifts: IssueShift[];
};

/// A crawl the server starts on its own: `request` as `start` takes it.
export type Schedule = {
  id: number;
  request: { start_url?: string; preset?: string; config?: JsonObject } & JsonObject;
  every_hours: number;
  /// Unix seconds.
  next_run: number;
  telegram_chat: string | null;
  enabled: boolean;
  last_crawl: number | null;
};
/// `telegram`: the server can send reports (its bot token is set).
export type ScheduleList = { telegram: boolean; schedules: Schedule[] };

export const api = {
  presets: () => call<Preset[]>("/api/presets"),
  presetConfig: (name: string) => call<Json>(`/api/presets/${name}/config`),
  configSchema: () => call<Schema>("/api/config/schema"),
  crawls: () => call<Crawl[]>("/api/crawls"),
  start: (request: StartRequest) =>
    call<{ id: number }>("/api/crawls", { method: "POST", body: JSON.stringify(request) }),
  pause: (id: number) => call<void>(`/api/crawls/${id}/pause`, { method: "POST" }),
  resume: (id: number) => call<void>(`/api/crawls/${id}/resume`, { method: "POST" }),
  stop: (id: number) => call<void>(`/api/crawls/${id}/stop`, { method: "POST" }),
  /// Goes on with an interrupted or stopped crawl; `resume` ends a pause.
  continueCrawl: (id: number, request: { max_urls?: number }) =>
    call<{ id: number }>(`/api/crawls/${id}/continue`, { method: "POST", body: JSON.stringify(request) }),
  overview: (id: number) => call<Overview>(`/api/crawls/${id}/overview`),
  issues: (id: number) => call<IssueCount[]>(`/api/crawls/${id}/issues`),
  tabs: (id: number) => call<TabDef[]>(`/api/crawls/${id}/tabs`),
  rows: (id: number, tab: string, query: RowsQuery, offset: number, limit: number) =>
    call<TabRows>(`/api/crawls/${id}/tabs/${tab}/rows?${rowsParams(query, { offset: String(offset), limit: String(limit) })}`),
  url: (id: number) => call<UrlDetail>(`/api/urls/${id}`),
  inlinks: (id: number, offset = 0, limit = 100) => call<LinkPage>(`/api/urls/${id}/inlinks?offset=${offset}&limit=${limit}`),
  outlinks: (id: number) => call<Outlink[]>(`/api/urls/${id}/outlinks`),
  content: (id: number, live = false) => call<PageContent>(`/api/urls/${id}/content${live ? "?live=true" : ""}`),
  brokenLinks: (id: number, offset = 0, limit = 200) =>
    call<LinkPage>(`/api/crawls/${id}/broken-links?offset=${offset}&limit=${limit}`),
  compare: (id: number, other: number) => call<Comparison>(`/api/crawls/${id}/compare?with=${other}`),
  schedules: () => call<ScheduleList>("/api/schedules"),
  saveSchedule: (schedule: Schedule) => call<{ id: number }>("/api/schedules", { method: "POST", body: JSON.stringify(schedule) }),
  deleteSchedule: (id: number) => call<void>(`/api/schedules/${id}`, { method: "DELETE" }),
  runSchedule: (id: number) => call<void>(`/api/schedules/${id}/run`, { method: "POST" }),
  testTelegram: (chat: string) => call<void>("/api/telegram/test", { method: "POST", body: JSON.stringify({ chat }) }),
  // EventSource sends no headers: the token goes in the address.
  events: (id: number) =>
    new EventSource(`/api/crawls/${id}/events${token ? `?token=${encodeURIComponent(token)}` : ""}`),
};

function rowsParams(query: RowsQuery, extra: Record<string, string> = {}) {
  const params = new URLSearchParams({ filter: query.filter, sort: query.sort.key, desc: String(query.sort.descending), ...extra });
  if (query.search.trim()) params.set("q", query.search.trim());
  return params;
}

// Downloads are plain links, so the token goes in the address.
const withToken = (params: URLSearchParams) => {
  if (token) params.set("token", token);
  const text = params.toString();
  return text ? `?${text}` : "";
};

export const downloads = {
  xlsx: (id: number) => `/api/crawls/${id}/export.xlsx${withToken(new URLSearchParams())}`,
  brokenLinks: (id: number) => `/api/crawls/${id}/broken-links.csv${withToken(new URLSearchParams())}`,
  compare: (id: number, other: number) => `/api/crawls/${id}/compare.csv${withToken(new URLSearchParams({ with: String(other) }))}`,
  csv: (id: number, tab: string, query: RowsQuery) => `/api/crawls/${id}/tabs/${tab}/export.csv${withToken(rowsParams(query))}`,
};

export const isActive = (state: CrawlState | undefined) =>
  state === "probing" || state === "crawling" || state === "paused" || state === "stopping" || state === "checking";
