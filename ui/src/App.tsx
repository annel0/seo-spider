import { useCallback, useEffect, useMemo, useState } from "react";
import {
  api,
  downloads,
  isActive,
  type Crawl,
  type IssueCount,
  type Overview as OverviewData,
  type Preset,
  type Sort,
  type TabDef,
} from "./api";
import { BrokenLinks } from "./components/BrokenLinks";
import { CompareView, comparable } from "./components/CompareView";
import { DetailPanel } from "./components/DetailPanel";
import { Icon } from "./components/Icon";
import { IssuesView } from "./components/IssuesView";
import { Overview } from "./components/Overview";
import { Schedules } from "./components/Schedules";
import { Settings, countChanges } from "./components/Settings";
import { Sidebar, filterTitle, type View } from "./components/Sidebar";
import { Structure } from "./components/Structure";
import { TabGrid } from "./components/TabGrid";
import { CommandBar, Telemetry } from "./components/TopBar";
import { useInterval, useProgress } from "./hooks";
import { formatCount, severityColor } from "./labels";
import { get, merge, setPath, type Json, type JsonObject, type Schema } from "./settings";

const BY_URL: Sort = { key: "url", descending: false };

export function App() {
  const [presets, setPresets] = useState<Preset[]>([]);
  const [crawls, setCrawls] = useState<Crawl[]>([]);
  const [crawlId, setCrawlId] = useState<number | null>(null);
  const [view, setView] = useState<View>({ kind: "overview" });
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<Sort>(BY_URL);
  const [selected, setSelected] = useState<number | null>(null);
  const [tabs, setTabs] = useState<TabDef[]>([]);
  const [issues, setIssues] = useState<IssueCount[]>([]);
  // Broken links of the crawl; null until its link graph exists.
  const [broken, setBroken] = useState<number | null>(null);
  const [overview, setOverview] = useState<OverviewData | null>(null);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The next crawl's settings: a preset, and what the form changed in it.
  const [preset, setPreset] = useState("auto");
  const [patch, setPatch] = useState<JsonObject>({});
  const [schema, setSchema] = useState<Schema | null>(null);
  const [base, setBase] = useState<Json>(null);
  const [renderDefaults, setRenderDefaults] = useState<Json>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [schedulesOpen, setSchedulesOpen] = useState(false);
  const settings = useMemo(() => merge(base, patch), [base, patch]);
  // Bumped when a crawl is continued: its progress is listened to again.
  const [session, setSession] = useState(0);
  const { progress, speed } = useProgress(crawlId, session);
  const live = isActive(progress?.state);

  const loadCrawls = useCallback(() => api.crawls().then(setCrawls).catch(() => undefined), []);

  useEffect(() => {
    api.presets().then(setPresets).catch((failure: Error) => setError(failure.message));
    api.configSchema().then(setSchema).catch(() => undefined);
    api.presetConfig("js").then((config) => setRenderDefaults(get(config, "render") ?? null)).catch(() => undefined);
    api
      .crawls()
      .then((found) => {
        setCrawls(found);
        // A report's link opens its crawl: ?crawl=12.
        const wanted = Number(new URLSearchParams(window.location.search).get("crawl"));
        if (found.length > 0) setCrawlId(found.some((crawl) => crawl.id === wanted) ? wanted : found[0].id);
      })
      .catch((failure: Error) => setError(failure.message));
  }, []);

  useEffect(() => {
    api.presetConfig(preset).then(setBase).catch((failure: Error) => setError(failure.message));
  }, [preset]);

  // A preset that renders nothing drops the rendering settings, so that
  // render threads typed for another preset do not turn rendering on.
  const choosePreset = (name: string) => {
    setPreset(name);
    api
      .presetConfig(name)
      .then((config) => {
        if (!get(config, "render")) {
          setPatch((current) => {
            const next = { ...current };
            delete next.render;
            return next;
          });
        }
      })
      .catch(() => undefined);
  };

  const refresh = useCallback(() => {
    if (crawlId === null) return;
    api.tabs(crawlId).then(setTabs).catch(() => undefined);
    api.issues(crawlId).then(setIssues).catch(() => undefined);
    api
      .brokenLinks(crawlId, 0, 1)
      .then((page) => setBroken(page.ready ? page.total : null))
      .catch(() => setBroken(null));
    api.overview(crawlId).then(setOverview).catch(() => undefined);
  }, [crawlId]);

  useEffect(() => {
    setTabs([]);
    setIssues([]);
    setBroken(null);
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

  const start = async (startUrl: string) => {
    setBusy(true);
    setError(null);
    try {
      const { id } = await api.start({ start_url: startUrl, preset, config: withoutBlankRules(patch) });
      setSettingsOpen(false);
      setView({ kind: "overview" });
      setCrawlId(id);
      loadCrawls();
    } catch (failure) {
      setError((failure as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const continueCrawl = async () => {
    if (crawlId === null) return;
    setBusy(true);
    setError(null);
    try {
      await api.continueCrawl(crawlId, typeof patch.max_urls === "number" ? { max_urls: patch.max_urls } : {});
      setSession((value) => value + 1);
      setView({ kind: "overview" });
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

  const open = (next: View) => {
    if (next.kind === "tab" && (view.kind !== "tab" || view.tab !== next.tab)) setSort(BY_URL);
    setView(next);
    setSelected(null);
  };
  const openFilter = (tab: string, filter: string) => open({ kind: "tab", tab, filter });

  const crawl = crawls.find((item) => item.id === crawlId);
  const origin = crawl ? new URL(crawl.start_url).origin : "";
  const tab = view.kind === "tab" ? tabs.find((item) => item.key === view.tab) : undefined;
  const filter = view.kind === "tab" ? tab?.filters.find((item) => item.key === view.filter) : undefined;
  const query = { filter: view.kind === "tab" ? view.filter : "all", search, sort };
  const detail = selected !== null && (
    <div className="detail">
      <button className="ghost" onClick={() => setSelected(null)} aria-label="Закрыть" style={{ position: "absolute", right: 4, top: 4 }}>
        <Icon name="cross" />
      </button>
      <DetailPanel urlId={selected} onOpen={setSelected} />
    </div>
  );

  return (
    <div className="app">
      <CommandBar
        presets={presets}
        crawls={crawls}
        crawlId={crawlId}
        progress={progress}
        busy={busy}
        exportHref={crawlId !== null && !live ? downloads.xlsx(crawlId) : null}
        preset={preset}
        limit={typeof patch.max_urls === "number" ? patch.max_urls : null}
        limitHint={typeof get(base, "max_urls") === "number" ? (get(base, "max_urls") as number) : null}
        threads={get(settings, "render") ? Number(get(settings, "render.threads") ?? 0) : null}
        changed={schema && base ? countChanges(schema, base, settings) : 0}
        settingsOpen={settingsOpen}
        schedulesOpen={schedulesOpen}
        onSchedules={() => {
          setSchedulesOpen((open) => !open);
          setSettingsOpen(false);
        }}
        onPreset={choosePreset}
        onLimit={(limit) => setPatch((current) => setPath(current, base, "max_urls", limit))}
        onThreads={(threads) =>
          setPatch((current) => setPath(current, base, "render.threads", threads ?? get(base, "render.threads") ?? get(renderDefaults, "threads")))
        }
        onSettings={() => {
          setSettingsOpen((open) => !open);
          setSchedulesOpen(false);
        }}
        onContinue={continueCrawl}
        onSelect={(id) => {
          setCrawlId(id);
          setSettingsOpen(false);
          setSchedulesOpen(false);
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
      {settingsOpen && !live && schema && base ? (
        <Settings
          schema={schema}
          base={base}
          value={settings}
          patch={patch}
          presetTitle={presets.find((item) => item.name === preset)?.title ?? preset}
          renderDefaults={renderDefaults}
          onPatch={setPatch}
          onClose={() => setSettingsOpen(false)}
        />
      ) : schedulesOpen ? (
        <Schedules
          presets={presets}
          crawls={crawls}
          preset={preset}
          patch={withoutBlankRules(patch)}
          startUrl={crawl?.start_url ?? ""}
          onOpen={(id) => {
            setCrawlId(id);
            setSchedulesOpen(false);
            setView({ kind: "overview" });
          }}
          onClose={() => setSchedulesOpen(false)}
        />
      ) : (
      <div className="main">
        <Sidebar tabs={tabs} issues={issues} broken={broken} comparable={comparable(crawl, crawls).length} view={view} onView={open} />
        <main className="work">
          {crawlId === null ? (
            <div className="empty">
              <div>
                <Icon name="globe" size={40} />
                <div style={{ marginTop: 12, fontSize: 15 }}>Введите адрес сайта, выберите пресет и запустите обход</div>
              </div>
            </div>
          ) : view.kind === "overview" ? (
            <Overview overview={overview} issues={issues} progress={progress} speed={speed} onOpen={openFilter} />
          ) : view.kind === "issues" ? (
            <IssuesView issues={issues} tabs={tabs} pages={overview?.pages ?? 0} onOpen={openFilter} />
          ) : view.kind === "broken" ? (
            <>
              <BrokenLinks crawlId={crawlId} onSelect={setSelected} />
              {detail}
            </>
          ) : view.kind === "structure" ? (
            <Structure
              key={crawlId}
              crawlId={crawlId}
              onOpen={(path) => {
                open({ kind: "tab", tab: "internal", filter: "all" });
                setSearchInput(path === "/" ? "" : path);
              }}
            />
          ) : view.kind === "compare" && crawl ? (
            <>
              <CompareView key={crawl.id} crawl={crawl} crawls={crawls} onSelect={setSelected} />
              {detail}
            </>
          ) : !tab ? (
            <div className="empty">Загрузка</div>
          ) : (
            <>
              <div className="toolbar">
                <span className="title">{tab.title}</span>
                {filter && filter.key !== "all" && (
                  <span className="chip" onClick={() => openFilter(tab.key, "all")} title="Снять фильтр">
                    {filter.severity && <span className="square" style={{ background: `var(${severityColor(filter.severity)})` }} />}
                    {filterTitle(filter)}
                    <Icon name="cross" size={12} />
                  </span>
                )}
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
                <a className="button" href={downloads.csv(crawlId, tab.key, query)} download title="Строки этой таблицы с фильтром, поиском и сортировкой">
                  <Icon name="download" /> CSV
                </a>
              </div>
              <div className="grid-area">
                <TabGrid
                  key={`${crawlId}-${tab.key}`}
                  crawlId={crawlId}
                  tab={tab.key}
                  columns={tab.columns}
                  origin={origin}
                  query={query}
                  live={live}
                  onSort={setSort}
                  onSelect={setSelected}
                  onTotal={setTotal}
                />
              </div>
              {detail}
            </>
          )}
        </main>
      </div>
      )}
    </div>
  );
}

/// The patch without custom rules that have no expression yet: the server
/// would refuse them.
function withoutBlankRules(patch: JsonObject): JsonObject {
  const custom = patch.custom as JsonObject | undefined;
  if (!custom) return patch;
  const filled = (rules: Json | undefined) =>
    Array.isArray(rules)
      ? rules.filter((rule) => {
          const fields = rule as JsonObject;
          return ["css", "xpath", "regex", "text"].some((key) => typeof fields[key] === "string" && (fields[key] as string).trim() !== "");
        })
      : rules;
  const cleaned: JsonObject = { ...custom };
  if (custom.extract !== undefined) cleaned.extract = filled(custom.extract) ?? null;
  if (custom.search !== undefined) cleaned.search = filled(custom.search) ?? null;
  return { ...patch, custom: cleaned };
}
