import { useEffect, useMemo, useState } from "react";
import { api, type StructureNode } from "../api";
import { useColors } from "../hooks";
import { formatCount } from "../labels";
import { Chart, type ChartOption } from "./Chart";
import { Icon } from "./Icon";

type Paint = "errors" | "issues" | "score";

const paints: [Paint, string][] = [
  ["errors", "Ошибки ответа"],
  ["issues", "Серьёзные проблемы"],
  ["score", "Link Score"],
];

/// `#rrggbb` between two others, `t` from 0 to 1.
function mix(from: string, to: string, t: number) {
  const parse = (hex: string) => [1, 3, 5].map((at) => parseInt(hex.slice(at, at + 2), 16));
  if (!/^#[0-9a-f]{6}$/i.test(from) || !/^#[0-9a-f]{6}$/i.test(to)) return t < 0.5 ? from : to;
  const [a, b] = [parse(from), parse(to)];
  return `#${a.map((value, at) => Math.round(value + (b[at] - value) * t).toString(16).padStart(2, "0")).join("")}`;
}

const share = (part: number, whole: number) => (whole > 0 ? part / whole : 0);

/// The site as nested rectangles: a section's area is its URLs, its colour
/// how many of them answered with an error, have issues, or how linked
/// its best page is. A click goes into a section and shows its figures
/// beside; its URLs open in the table.
export function Structure({ crawlId, onOpen }: { crawlId: number; onOpen: (search: string) => void }) {
  const [tree, setTree] = useState<StructureNode | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [paint, setPaint] = useState<Paint>("errors");
  const [all, setAll] = useState(false);
  const [picked, setPicked] = useState<StructureNode | null>(null);
  const { dark, color } = useColors();

  useEffect(() => {
    setTree(null);
    setPicked(null);
    let current = true;
    api
      .structure(crawlId, all)
      .then((found) => current && setTree(found))
      .catch((failure: Error) => current && setError(failure.message));
    return () => {
      current = false;
    };
  }, [crawlId, all]);

  const option = useMemo<ChartOption | null>(() => {
    if (!tree) return null;
    const [ok, warn, bad, accent, quiet, panel, ink] = ["--ok", "--warn", "--bad", "--accent", "--panel-3", "--panel", "--ink"].map(color);
    // Relative to the section that has the most: on some sites every
    // page has one.
    let mostIssues = 0;
    const visit = (node: StructureNode) => {
      mostIssues = Math.max(mostIssues, share(node.serious, node.urls));
      node.children?.forEach(visit);
    };
    visit(tree);
    const fill = (node: StructureNode) => {
      if (paint === "score") return mix(quiet, accent, (node.best_score ?? 0) / 100);
      const t = paint === "errors" ? Math.min(1, share(node.errors, node.urls) / 0.2) : share(share(node.serious, node.urls), mostIssues);
      return t <= 0.5 ? mix(ok, warn, t * 2) : mix(warn, bad, (t - 0.5) * 2);
    };
    const painted = (node: StructureNode): object => ({
      ...node,
      itemStyle: { color: fill(node), borderColor: panel },
      children: node.children?.map(painted),
    });
    return {
      tooltip: {
        backgroundColor: panel,
        borderColor: color("--rule-strong"),
        borderRadius: 0,
        textStyle: { color: ink, fontFamily: "Onest, sans-serif" },
        formatter: (params: { data: StructureNode }) => {
          const node = params.data;
          const lines = [
            `<b>${node.path}</b>`,
            `${formatCount(node.urls)} URL, страниц ${formatCount(node.pages)}`,
            `ошибок ответа ${formatCount(node.errors)}, редиректов ${formatCount(node.redirects)}`,
            `с проблемами ${formatCount(node.with_issues)}, из них серьёзными ${formatCount(node.serious)}`,
          ];
          if (node.best_score !== null) lines.push(`лучший Link Score ${node.best_score}`);
          return lines.join("<br>");
        },
      },
      series: [
        {
          type: "treemap",
          data: tree.children ? tree.children.map(painted) : [painted(tree)],
          name: tree.name,
          roam: false,
          nodeClick: "zoomToNode",
          leafDepth: 2,
          top: 30,
          left: 0,
          right: 0,
          bottom: 0,
          breadcrumb: {
            top: 0,
            left: 0,
            height: 24,
            itemStyle: { color: color("--panel-2"), borderColor: color("--rule-strong"), borderWidth: 1, shadowBlur: 0, textStyle: { color: ink } },
            emphasis: { itemStyle: { color: color("--panel-3") } },
          },
          label: { show: true, formatter: "{b}", color: "#fff", fontFamily: "Onest, sans-serif", fontSize: 12, overflow: "truncate" },
          upperLabel: { show: true, height: 20, color: "#fff", fontFamily: "Onest, sans-serif", fontSize: 12 },
          itemStyle: { borderColor: panel, borderWidth: 1, gapWidth: 1 },
          levels: [{ itemStyle: { borderWidth: 0, gapWidth: 2 } }, { itemStyle: { gapWidth: 1, borderWidth: 2, borderColor: panel } }],
        },
      ],
    };
  }, [tree, paint, dark]);

  if (error) return <div className="error-banner">{error}</div>;
  if (!tree || !option) return <div className="empty">Загрузка</div>;
  const shown = picked ?? tree;
  const parts = [...(shown.children ?? [])].sort((a, b) => b.urls - a.urls).slice(0, 12);
  const most = Math.max(1, ...parts.map((part) => part.urls));

  return (
    <>
      <div className="toolbar">
        <span className="title">Структура</span>
        <span className="figures" style={{ color: "var(--ink-3)", fontSize: 15 }}>
          {formatCount(tree.urls)}
        </span>
        <span style={{ flex: 1 }} />
        <div className="segmented" role="radiogroup" aria-label="Какие URL">
          <button role="radio" aria-checked={!all} className={all ? undefined : "on"} onClick={() => setAll(false)}>
            Страницы
          </button>
          <button role="radio" aria-checked={all} className={all ? "on" : undefined} onClick={() => setAll(true)}>
            Все URL
          </button>
        </div>
        <div className="segmented" role="radiogroup" aria-label="Цвет">
          {paints.map(([key, title]) => (
            <button key={key} role="radio" aria-checked={paint === key} className={paint === key ? "on" : undefined} onClick={() => setPaint(key)}>
              {title}
            </button>
          ))}
        </div>
      </div>
      <div className="structure">
        <div className="structure-map">
          <Chart option={option} height="100%" onItem={(data) => data && setPicked(data as StructureNode)} />
        </div>
        <aside className="structure-side">
          <div className="section-path" title={shown.path}>
            {shown.path}
          </div>
          <div className="section-figures">
            <Figure value={shown.urls} name="URL" />
            <Figure value={shown.pages} name="страниц" />
            <Figure value={shown.errors} name="ошибок" colour={shown.errors > 0 ? "--bad" : undefined} />
            <Figure value={shown.serious} name="с серьёзными проблемами" colour={shown.serious > 0 ? "--warn" : undefined} />
          </div>
          {parts.length > 0 && (
            <div className="section-parts">
              {parts.map((part) => (
                <div key={`${part.name}-${part.path}`} className="section-part" onClick={() => setPicked(part)} title={part.path}>
                  <span className="name">{part.name}</span>
                  <span className="track">
                    <span style={{ width: `${(part.urls * 100) / most}%`, background: part.errors > 0 ? "var(--bad)" : "var(--accent)" }} />
                  </span>
                  <span className="num">{formatCount(part.urls)}</span>
                </div>
              ))}
            </div>
          )}
          <button onClick={() => onOpen(shown.search)}>
            <Icon name="table" /> URL раздела в таблице
          </button>
        </aside>
      </div>
    </>
  );
}

function Figure({ value, name, colour }: { value: number; name: string; colour?: string }) {
  return (
    <div>
      <div className="value" style={{ color: colour ? `var(${colour})` : undefined }}>
        {formatCount(value)}
      </div>
      <div className="name">{name}</div>
    </div>
  );
}
