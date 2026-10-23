import { useEffect, useRef } from "react";
import * as echarts from "echarts/core";
import { BarChart, LineChart, TreemapChart } from "echarts/charts";
import { GridComponent, TooltipComponent } from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";

// Only the chart kinds the UI draws.
echarts.use([BarChart, LineChart, TreemapChart, GridComponent, TooltipComponent, CanvasRenderer]);

export type ChartOption = echarts.EChartsCoreOption;

/// An ECharts chart that fills its box and follows its size. `onClick`
/// gets the clicked item's name, `onItem` its data.
export function Chart({
  option,
  height,
  onClick,
  onItem,
}: {
  option: ChartOption;
  height: number | string;
  onClick?: (name: string) => void;
  onItem?: (data: unknown) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const chart = useRef<echarts.ECharts | null>(null);
  const click = useRef(onClick);
  click.current = onClick;
  const item = useRef(onItem);
  item.current = onItem;

  useEffect(() => {
    if (!box.current) return;
    const instance = echarts.init(box.current, undefined, { renderer: "canvas" });
    chart.current = instance;
    instance.on("click", (params) => {
      if (click.current && typeof params.name === "string") click.current(params.name);
      item.current?.(params.data);
    });
    const resized = new ResizeObserver(() => instance.resize());
    resized.observe(box.current);
    return () => {
      resized.disconnect();
      instance.dispose();
      chart.current = null;
    };
  }, []);

  useEffect(() => {
    chart.current?.setOption(option, { notMerge: true });
    // Canvas text uses the fonts loaded at the time: draw again once they are.
    document.fonts.ready.then(() => chart.current?.setOption(option, { notMerge: true }));
  }, [option]);

  return <div ref={box} style={{ height, width: "100%", cursor: onClick ? "pointer" : undefined }} />;
}
