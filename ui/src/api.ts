// The server's API (crates/seo-server): the types mirror its JSON.

export type Preset = { name: string; title: string; description: string };

export type CrawlState =
  | "crawling"
  | "paused"
  | "stopping"
  | "checking"
  | "finished"
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
  count: number;
  title: string | null;
  description: string | null;
  severity: Severity | null;
  category: string | null;
};

export type Summary = {
  filters: { key: string; count: number }[];
  issues: IssueCount[];
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
};

export type UrlRow = {
  id: number;
  url: string;
  status: number;
  content_type: string | null;
  error: string | null;
  depth: number | null;
  ttfb_ms: number;
  size: number;
  in_sitemap: boolean;
  title: string | null;
  title_length: number | null;
  description: string | null;
  description_length: number | null;
  h1: string | null;
  h1_length: number | null;
  word_count: number | null;
  links: number | null;
  canonical: string | null;
  robots: string | null;
  rendered: boolean;
  issues: number;
};

export type RowsPage = { total: number; rows: UrlRow[] };

export type SortKey =
  | "url"
  | "status"
  | "content_type"
  | "depth"
  | "ttfb_ms"
  | "size"
  | "title"
  | "title_length"
  | "description"
  | "description_length"
  | "h1"
  | "h1_length"
  | "word_count"
  | "links"
  | "canonical";

export type Sort = { key: SortKey; descending: boolean };

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

export type StartRequest = {
  start_url: string;
  preset: string;
  max_urls?: number;
  render_threads?: number;
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

export const api = {
  presets: () => call<Preset[]>("/api/presets"),
  crawls: () => call<Crawl[]>("/api/crawls"),
  start: (request: StartRequest) =>
    call<{ id: number }>("/api/crawls", { method: "POST", body: JSON.stringify(request) }),
  pause: (id: number) => call<void>(`/api/crawls/${id}/pause`, { method: "POST" }),
  resume: (id: number) => call<void>(`/api/crawls/${id}/resume`, { method: "POST" }),
  stop: (id: number) => call<void>(`/api/crawls/${id}/stop`, { method: "POST" }),
  summary: (id: number) => call<Summary>(`/api/crawls/${id}/summary`),
  overview: (id: number) => call<Overview>(`/api/crawls/${id}/overview`),
  rows: (id: number, query: { filter: string; search: string; sort: Sort; offset: number; limit: number }) => {
    const params = new URLSearchParams({
      filter: query.filter,
      sort: query.sort.key,
      desc: String(query.sort.descending),
      offset: String(query.offset),
      limit: String(query.limit),
    });
    if (query.search.trim()) params.set("q", query.search.trim());
    return call<RowsPage>(`/api/crawls/${id}/rows?${params}`);
  },
  url: (id: number) => call<UrlDetail>(`/api/urls/${id}`),
  // EventSource sends no headers: the token goes in the address.
  events: (id: number) =>
    new EventSource(`/api/crawls/${id}/events${token ? `?token=${encodeURIComponent(token)}` : ""}`),
};

export const isActive = (state: CrawlState | undefined) =>
  state === "crawling" || state === "paused" || state === "stopping" || state === "checking";
