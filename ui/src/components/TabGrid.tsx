import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { DataEditor, GridCellKind, type GridCell, type GridColumn, type Item, type Rectangle, type Theme } from "@glideapps/glide-data-grid";
import "@glideapps/glide-data-grid/dist/index.css";
import { api, type CellKind, type CellValue, type RowsQuery, type Sort, type TabColumn, type TabRows } from "../api";
import { useColors, useInterval } from "../hooks";
import { formatBytes, formatCount, indexabilityColor, indexabilityLabels, readableUrl, statusClass, statusColor } from "../labels";

/// Rows fetched at a time.
const PAGE = 100;

type Row = TabRows["rows"][number];

const text = (value: string, colour?: string, align?: "right"): GridCell => ({
  kind: GridCellKind.Text,
  data: value,
  displayData: value,
  allowOverlay: false,
  contentAlign: align,
  themeOverride: colour ? { textDark: colour } : undefined,
});

const number = (value: number, colour?: string, suffix = ""): GridCell => ({
  kind: GridCellKind.Number,
  data: value,
  displayData: `${formatCount(value)}${suffix}`,
  allowOverlay: false,
  contentAlign: "right",
  themeOverride: colour ? { textDark: colour } : undefined,
});

/// A value as its column's kind draws it: colour carries the judgement, so
/// the eye finds what is off without reading.
function draw(kind: CellKind, value: CellValue, color: (name: string) => string, path: (url: string) => string): GridCell {
  if (value === null || value === "") return text("");
  switch (kind.type) {
    case "url":
      return text(path(String(value)));
    case "status": {
      const status = Number(value);
      return text(status ? String(status) : "—", color(statusColor(statusClass(status))));
    }
    case "number":
      return typeof value === "number" ? number(value) : text(String(value));
    case "length": {
      const length = Number(value);
      return number(length, color(length < kind.low || length > kind.high ? "--warn" : "--ok"));
    }
    case "ms": {
      const ms = Number(value);
      return number(ms, color(ms < 300 ? "--ok" : ms < 1000 ? "--warn" : "--bad"), " мс");
    }
    case "bytes":
      return text(formatBytes(Number(value)), undefined, "right");
    case "bool":
      return value ? text("✓", color("--ok")) : text("—", color("--ink-3"));
    case "indexable":
      return text(indexabilityLabels[String(value)] ?? String(value), color(indexabilityColor(String(value))));
    case "issues": {
      const count = Number(value);
      return number(count, color(count > 0 ? "--warn" : "--ink-3"));
    }
    default:
      return text(String(value));
  }
}

/// One tab's rows, fetched a page at a time as they scroll into view; the
/// server sorts, filters and searches.
export function TabGrid({
  crawlId,
  tab,
  columns,
  origin,
  query,
  live,
  onSort,
  onSelect,
  onTotal,
}: {
  crawlId: number;
  tab: string;
  columns: TabColumn[];
  origin: string;
  query: RowsQuery;
  live: boolean;
  onSort: (sort: Sort) => void;
  onSelect: (urlId: number) => void;
  onTotal: (total: number) => void;
}) {
  const { dark, color } = useColors();
  const [widths, setWidths] = useState<Record<string, number>>({});
  const cache = useRef(new Map<number, Row[]>());
  const pending = useRef(new Set<number>());
  const visible = useRef<Rectangle>({ x: 0, y: 0, width: 1, height: 60 });
  const generation = useRef(0);
  const [total, setTotal] = useState(0);
  const [tick, setTick] = useState(0);
  // A ref, so a parent's new callback does not reload the table.
  const reportTotal = useRef(onTotal);
  reportTotal.current = onTotal;
  const { filter, search, sort } = query;

  const load = useCallback(
    async (page: number, again = false) => {
      if (pending.current.has(page) || (!again && cache.current.has(page))) return;
      pending.current.add(page);
      const asked = generation.current;
      try {
        const result = await api.rows(crawlId, tab, { filter, search, sort }, page * PAGE, PAGE);
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
    [crawlId, tab, filter, search, sort],
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

  const path = useCallback(
    (url: string) => readableUrl(origin && url.startsWith(origin) ? url.slice(origin.length) || "/" : url),
    [origin],
  );

  const getCellContent = useCallback(
    ([col, row]: Item): GridCell => {
      const found = cache.current.get(Math.floor(row / PAGE))?.[row % PAGE];
      if (!found) return { kind: GridCellKind.Loading, allowOverlay: false };
      return draw(columns[col].kind, found.cells[col] ?? null, color, path);
    },
    // `tick` redraws the cells as pages arrive.
    [columns, tick, dark, path],
  );

  const gridColumns: GridColumn[] = columns.map((column) => ({
    title: column.sortable && column.key === sort.key ? `${column.title} ${sort.descending ? "↓" : "↑"}` : column.title,
    id: column.key,
    width: widths[column.key] ?? column.width,
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
        const column = columns[col];
        if (!column.sortable) return;
        // Text sorts A to Z first, figures largest first.
        const textual = column.kind.type === "url" || column.kind.type === "text";
        onSort({ key: column.key, descending: column.key === sort.key ? !sort.descending : !textual });
      }}
      onCellClicked={([, row]) => {
        const found = cache.current.get(Math.floor(row / PAGE))?.[row % PAGE];
        if (found) onSelect(found.id);
      }}
      onColumnResize={(column, size) => setWidths((current) => ({ ...current, [column.id ?? ""]: size }))}
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
