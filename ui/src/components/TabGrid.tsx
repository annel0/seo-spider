import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  DataEditor,
  GridCellKind,
  type GridCell,
  type GridColumn,
  type Item,
  type Rectangle,
  type Theme,
} from "@glideapps/glide-data-grid";
import "@glideapps/glide-data-grid/dist/index.css";
import { api, type Sort, type SortKey, type UrlRow } from "../api";
import { useColors, useInterval } from "../hooks";
import { formatCount, statusClass, statusColor } from "../labels";

/// Rows fetched at a time.
const PAGE = 100;

type Column = {
  title: string;
  width: number;
  sort?: SortKey;
  cell: (row: UrlRow, color: (name: string) => string) => GridCell;
};

const text = (value: string | null | undefined, colour?: string): GridCell => ({
  kind: GridCellKind.Text,
  data: value ?? "",
  displayData: value ?? "",
  allowOverlay: false,
  themeOverride: colour ? { textDark: colour } : undefined,
});

const number = (value: number | null | undefined, colour?: string, suffix = ""): GridCell => ({
  kind: GridCellKind.Number,
  data: value ?? undefined,
  displayData: value === null || value === undefined ? "" : `${formatCount(value)}${suffix}`,
  allowOverlay: false,
  contentAlign: "right",
  themeOverride: colour ? { textDark: colour } : undefined,
});

const isHtml = (row: UrlRow) => (row.content_type ?? "").toLowerCase().startsWith("text/html");

/// Indexable as the overview counts it: 2xx HTML, no noindex, no canonical
/// to another URL. `null` for what is not a page.
const indexable = (row: UrlRow) => {
  if (!isHtml(row) && row.status !== 0 && !(row.status >= 300 && row.status < 400)) return null;
  if (row.status < 200 || row.status >= 300) return false;
  if ((row.robots ?? "").includes("noindex")) return false;
  return !row.canonical || row.canonical === row.url;
};

const lengthColour = (length: number | null, low: number, high: number, html: boolean) =>
  length === null ? (html ? "--bad" : undefined) : length < low || length > high ? "--warn" : "--ok";

function columnsFor(origin: string): Column[] {
  const path = (url: string) => (url.startsWith(origin) ? url.slice(origin.length) || "/" : url);
  return [
    { title: "Адрес", width: 330, sort: "url", cell: (row) => text(path(row.url)) },
    {
      title: "Код",
      width: 64,
      sort: "status",
      cell: (row, color) => text(row.status ? String(row.status) : "—", color(statusColor(statusClass(row.status)))),
    },
    {
      title: "Индекс",
      width: 64,
      cell: (row, color) => {
        const state = indexable(row);
        return state === null ? text("") : text(state ? "✓" : "✕", color(state ? "--ok" : "--bad"));
      },
    },
    { title: "Title", width: 260, sort: "title", cell: (row) => text(row.title) },
    {
      title: "Длина",
      width: 64,
      sort: "title_length",
      cell: (row, color) => {
        const colour = lengthColour(row.title_length, 30, 60, isHtml(row));
        return number(row.title_length, colour ? color(colour) : undefined);
      },
    },
    { title: "Description", width: 220, sort: "description", cell: (row) => text(row.description) },
    {
      title: "Длина",
      width: 64,
      sort: "description_length",
      cell: (row, color) => {
        const colour = lengthColour(row.description_length, 70, 160, isHtml(row));
        return number(row.description_length, colour ? color(colour) : undefined);
      },
    },
    { title: "H1", width: 200, sort: "h1", cell: (row) => text(row.h1) },
    { title: "Слов", width: 70, sort: "word_count", cell: (row) => number(row.word_count) },
    { title: "Глубина", width: 72, sort: "depth", cell: (row) => number(row.depth) },
    {
      title: "Ответ",
      width: 80,
      sort: "ttfb_ms",
      cell: (row, color) =>
        row.status === 0
          ? text("")
          : number(row.ttfb_ms, color(row.ttfb_ms < 300 ? "--ok" : row.ttfb_ms < 1000 ? "--warn" : "--bad"), " мс"),
    },
    { title: "Размер", width: 84, sort: "size", cell: (row) => number(Math.round(row.size / 1024), undefined, " КБ") },
    { title: "Ссылок", width: 72, sort: "links", cell: (row) => number(row.links) },
    {
      title: "Проблем",
      width: 80,
      cell: (row, color) => number(row.issues, row.issues > 0 ? color("--warn") : color("--ink-3")),
    },
    { title: "JS", width: 46, cell: (row, color) => text(row.rendered ? "●" : "", color("--accent")) },
  ];
}

export function UrlGrid({
  crawlId,
  origin,
  filter,
  search,
  sort,
  live,
  onSort,
  onSelect,
  onTotal,
}: {
  crawlId: number;
  origin: string;
  filter: string;
  search: string;
  sort: Sort;
  live: boolean;
  onSort: (sort: Sort) => void;
  onSelect: (urlId: number) => void;
  onTotal: (total: number) => void;
}) {
  const { dark, color } = useColors();
  const columns = useMemo(() => columnsFor(origin), [origin]);
  const [widths, setWidths] = useState<number[]>(() => columns.map((column) => column.width));
  const cache = useRef(new Map<number, UrlRow[]>());
  const pending = useRef(new Set<number>());
  const visible = useRef<Rectangle>({ x: 0, y: 0, width: 1, height: 60 });
  const generation = useRef(0);
  const [total, setTotal] = useState(0);
  const [tick, setTick] = useState(0);
  // A ref, so a parent's new callback does not reload the table.
  const reportTotal = useRef(onTotal);
  reportTotal.current = onTotal;

  const load = useCallback(
    async (page: number, again = false) => {
      if (pending.current.has(page) || (!again && cache.current.has(page))) return;
      pending.current.add(page);
      const asked = generation.current;
      try {
        const result = await api.rows(crawlId, { filter, search, sort, offset: page * PAGE, limit: PAGE });
        if (asked !== generation.current) return;
        cache.current.set(page, result.rows);
        setTotal(result.total);
        reportTotal.current(result.total);
        setTick((value) => value + 1);
      } catch {
        // The next scroll or refresh asks again.
      } finally {
        pending.current.delete(page);
      }
    },
    [crawlId, filter, search, sort],
  );

  const visiblePages = () => {
    const first = Math.floor(visible.current.y / PAGE);
    const last = Math.floor((visible.current.y + visible.current.height) / PAGE);
    return Array.from({ length: last - first + 1 }, (_, at) => first + at);
  };

  useEffect(() => {
    generation.current += 1;
    cache.current.clear();
    pending.current.clear();
    visible.current = { ...visible.current, y: 0 };
    load(0);
  }, [load]);

  useInterval(() => visiblePages().forEach((page) => load(page, true)), live ? 2000 : null);

  // The canvas draws with whatever font is loaded; redraw once ours is.
  useEffect(() => {
    document.fonts.ready.then(() => setTick((value) => value + 1));
  }, []);

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const rows = cache.current.get(Math.floor(row / PAGE));
      const found = rows?.[row % PAGE];
      if (!found) return { kind: GridCellKind.Loading, allowOverlay: false };
      return columns[col].cell(found, color);
    },
    // `tick` redraws the cells as pages arrive.
    [columns, tick, dark],
  );

  const gridColumns: GridColumn[] = columns.map((column, at) => ({
    title:
      column.sort && column.sort === sort.key ? `${column.title} ${sort.descending ? "↓" : "↑"}` : column.title,
    id: String(at),
    width: widths[at],
  }));

  const theme: Partial<Theme> = useMemo(
    () => ({
      accentColor: color("--accent"),
      accentLight: color("--accent-wash"),
      textDark: color("--ink"),
      textMedium: color("--ink-2"),
      textLight: color("--ink-3"),
      textHeader: color("--ink-2"),
      textHeaderSelected: "#ffffff",
      bgCell: color("--panel"),
      bgCellMedium: color("--panel-2"),
      bgHeader: color("--panel-2"),
      bgHeaderHovered: color("--panel-3"),
      bgHeaderHasFocus: color("--panel-3"),
      borderColor: color("--rule"),
      horizontalBorderColor: color("--rule"),
      fontFamily: "Onest, system-ui, sans-serif",
      baseFontStyle: "13px",
      headerFontStyle: "600 12px",
      cellHorizontalPadding: 10,
    }),
    [dark],
  );

  if (total === 0 && cache.current.has(0)) {
    return <div className="empty">Ничего не найдено</div>;
  }

  return (
    <DataEditor
      columns={gridColumns}
      rows={total}
      getCellContent={getCellContent}
      onVisibleRegionChanged={(range) => {
        visible.current = range;
        visiblePages().forEach((page) => load(page));
      }}
      onHeaderClicked={(col) => {
        const key = columns[col].sort;
        if (!key) return;
        onSort({ key, descending: key === sort.key ? !sort.descending : key !== "url" && key !== "title" });
      }}
      onCellClicked={([, row]) => {
        const found = cache.current.get(Math.floor(row / PAGE))?.[row % PAGE];
        if (found) onSelect(found.id);
      }}
      onColumnResize={(_, size, at) => setWidths((current) => current.map((width, index) => (index === at ? size : width)))}
      rowMarkers="none"
      smoothScrollX
      smoothScrollY
      theme={theme}
      width="100%"
      height="100%"
      rowHeight={30}
      headerHeight={34}
    />
  );
}
