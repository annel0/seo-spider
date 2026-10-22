import { useEffect, useRef, useState } from "react";
import { api, type Progress } from "./api";

/// The theme's colours for charts: `color("--ok")` reads a CSS variable,
/// and `dark` changes with the system scheme so charts redraw.
export function useColors(): { dark: boolean; color: (name: string) => string } {
  const [dark, setDark] = useState(() => window.matchMedia("(prefers-color-scheme: dark)").matches);
  useEffect(() => {
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    const changed = () => setDark(query.matches);
    query.addEventListener("change", changed);
    return () => query.removeEventListener("change", changed);
  }, []);
  const style = getComputedStyle(document.documentElement);
  return { dark, color: (name: string) => style.getPropertyValue(name).trim() };
}

export function useInterval(callback: () => void, ms: number | null) {
  const saved = useRef(callback);
  saved.current = callback;
  useEffect(() => {
    if (ms === null) return;
    const timer = window.setInterval(() => saved.current(), ms);
    return () => window.clearInterval(timer);
  }, [ms]);
}

/// URLs a second, sampled from the progress events.
export type SpeedPoint = { seconds: number; rate: number };

/// The crawl's progress from the server's events, and its speed over time.
export function useProgress(crawlId: number | null) {
  const [progress, setProgress] = useState<Progress | null>(null);
  const [speed, setSpeed] = useState<SpeedPoint[]>([]);
  const last = useRef<{ urls: number; ms: number } | null>(null);
  useEffect(() => {
    setProgress(null);
    setSpeed([]);
    last.current = null;
    if (crawlId === null) return;
    const source = api.events(crawlId);
    source.addEventListener("progress", (event) => {
      const next = JSON.parse((event as MessageEvent).data) as Progress;
      setProgress(next);
      const previous = last.current;
      // A sample a second: the events come four times as often.
      if (next.elapsed_ms > 0 && (!previous || next.elapsed_ms - previous.ms >= 1000)) {
        if (previous) {
          const rate = ((next.urls - previous.urls) * 1000) / (next.elapsed_ms - previous.ms);
          setSpeed((points) => [...points.slice(-599), { seconds: next.elapsed_ms / 1000, rate }]);
        }
        last.current = { urls: next.urls, ms: next.elapsed_ms };
      }
      if (next.state === "finished" || next.state === "failed" || next.state === "interrupted") source.close();
    });
    return () => source.close();
  }, [crawlId]);
  return { progress, speed };
}
