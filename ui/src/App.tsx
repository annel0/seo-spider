import { useCallback, useEffect, useState } from "react";
import { api, isActive, type Crawl, type Overview as OverviewData, type Preset, type Sort, type StartRequest, type Summary } from "./api";
import { DetailPanel } from "./components/DetailPanel";
import { Icon } from "./components/Icon";
import { Overview } from "./components/Overview";
import { Sidebar, type View } from "./components/Sidebar";
import { CommandBar, Telemetry } from "./components/TopBar";
import { UrlGrid } from "./components/UrlGrid";
import { useInterval, useProgress } from "./hooks";
import { filterLabels, formatCount } from "./labels";

export function App() {
  const [presets, setPresets] = useState<Preset[]>([]);
  const [crawls, setCrawls] = useState<Crawl[]>([]);
  const [crawlId, setCrawlId] = useState<number | null>(null);
  const [view, setView] = useState<View>({ kind: "overview" });
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<Sort>({ key: "url", descending: false });
  const [selected, setSelected] = useState<number | null>(null);
  const [summary, setSummary] = useState<Summary | null>(null);
  const [overview, setOverview] = useState<OverviewData | null>(null);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const { progress, speed } = useProgress(crawlId);
  const live = isActive(progress?.state);

  const loadCrawls = useCallback(() => api.crawls().then(setCrawls).catch(() => undefined), []);

  useEffect(() => {
    api.presets().then(setPresets).catch((failure: Error) => setError(failure.message));
    api
      .crawls()
      .then((found) => {
        setCrawls(found);
        if (found.length > 0) setCrawlId(found[0].id);
      })
      .catch((failure: Error) => setError(failure.message));
  }, []);

  const refresh = useCallback(() => {
    if (crawlId === null) return;
    api.summary(crawlId).then(setSummary).catch(() => undefined);
    api.overview(crawlId).then(setOverview).catch(() => undefined);
  }, [crawlId]);

  useEffect(() => {
    setSummary(null);
    setOverview(null);
    setSelected(null);
    refresh();
  }, [refresh]);

  // While the crawl runs, every two seconds; once more when it ends.
  useInterval(() => {
    refresh();
    loadCrawls();
  }, live ? 2000 : null);
  useEffect(() => {
    if (progress?.state === "finished" || progress?.state === "failed") {
      refresh();
      loadCrawls();
    }
  }, [progress?.state, refresh, loadCrawls]);

  useEffect(() => {
    const timer = window.setTimeout(() => setSearch(searchInput), 300);
    return () => window.clearTimeout(timer);
  }, [searchInput]);

  const start = async (request: StartRequest) => {
    setBusy(true);
    setError(null);
    try {
      const { id } = await api.start(request);
      setView({ kind: "overview" });
      setCrawlId(id);
      loadCrawls();
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const control = (action: (id: number) => Promise<void>) => () => {
    if (crawlId !== null) action(crawlId).catch((failure: Error) => setError(failure.message));
  };

  const openFilter = (filter: string) => {
    setView({ kind: "table", filter });
    setSelected(null);
  };

  const crawl = crawls.find((item) => item.id === crawlId);
  const origin = crawl ? new URL(crawl.start_url).origin : "";
  const filterTitle =
    view.kind === "table"
      ? view.filter.startsWith("issue:")
        ? summary?.issues.find((issue) => `issue:${issue.code}` === view.filter)?.title ?? view.filter.slice(6)
        : filterLabels[view.filter] ?? view.filter
      : "";

  return (
    <div className="app">
      <CommandBar
        presets={presets}
        crawls={crawls}
        crawlId={crawlId}
        progress={progress}
        busy={busy}
        onSelect={(id) => {
          setCrawlId(id);
          setView({ kind: "overview" });
        }}
        onStart={start}
        onPause={control(api.pause)}
        onResume={control(api.resume)}
        onStop={control(api.stop)}
      />
      <Telemetry progress={progress} overview={overview} speed={speed} />
      {error && (
        <div className="error-banner" onClick={() => setError(null)}>
          {error}
        </div>
      )}
      <div className="main">
        <Sidebar summary={summary} view={view} onView={(next) => (next.kind === "table" ? openFilter(next.filter) : setView(next))} />
        <main className="work">
          {crawlId === null ? (
            <div className="empty">
              <div>
                <Icon name="globe" size={40} />
                <div style={{ marginTop: 12, fontSize: 15 }}>Введите адрес сайта, выберите пресет и запустите обход</div>
              </div>
            </div>
          ) : view.kind === "overview" ? (
            <Overview overview={overview} summary={summary} progress={progress} speed={speed} onOpenFilter={openFilter} />
          ) : (
            <>
              <div className="toolbar">
                <span className="title">{filterTitle}</span>
                <span className="figures" style={{ color: "var(--ink-3)", fontSize: 15 }}>
                  {formatCount(total)}
                </span>
                <span style={{ flex: 1 }} />
                <Icon name="search" />
                <input
                  value={searchInput}
                  onChange={(event) => setSearchInput(event.target.value)}
                  placeholder="часть адреса"
                  aria-label="Поиск по адресу"
                  style={{ width: 220 }}
                />
              </div>
              <div className="grid-area">
                <UrlGrid
                  key={`${crawlId}-${view.filter}`}
                  crawlId={crawlId}
                  origin={origin}
                  filter={view.filter}
                  search={search}
                  sort={sort}
                  live={live}
                  onSort={setSort}
                  onSelect={setSelected}
                  onTotal={setTotal}
                />
              </div>
              {selected !== null && (
                <div className="detail">
                  <button className="ghost" onClick={() => setSelected(null)} aria-label="Закрыть" style={{ position: "absolute", right: 4, top: 4 }}>
                    <Icon name="cross" />
                  </button>
                  <DetailPanel urlId={selected} />
                </div>
              )}
            </>
          )}
        </main>
      </div>
    </div>
  );
}
