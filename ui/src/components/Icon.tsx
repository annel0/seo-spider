// A few line icons (24×24, stroke currentColor), drawn here rather than
// pulled from an icon package.

const paths: Record<string, string> = {
  play: "M7 4v16l13-8z",
  pause: "M8 5v14M16 5v14",
  stop: "M6 6h12v12H6z",
  overview: "M4 20V10M10 20V4M16 20v-7M22 20H2",
  table: "M3 5h18v14H3zM3 10h18M3 15h18M9 5v14",
  alert: "M12 3l10 18H2zM12 10v5M12 18h.01",
  check: "M5 12l5 5 9-10",
  cross: "M6 6l12 12M18 6L6 18",
  search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4.5-4.5",
  globe: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM3 12h18M12 3c3 3.5 3 14.5 0 18M12 3c-3 3.5-3 14.5 0 18",
  map: "M9 4l-6 2v14l6-2 6 2 6-2V4l-6 2zM9 4v14M15 6v14",
  code: "M8 8l-4 4 4 4M16 8l4 4-4 4",
  link: "M10 14a4 4 0 0 0 5.66 0l3-3a4 4 0 0 0-5.66-5.66l-1 1M14 10a4 4 0 0 0-5.66 0l-3 3a4 4 0 0 0 5.66 5.66l1-1",
  image: "M4 5h16v14H4zM4 15l5-5 5 5 3-3 3 3M15 9h.01",
  download: "M12 4v11M7 10l5 5 5-5M4 20h16",
  settings: "M4 6h16M4 12h16M4 18h16M15 4v4M9 10v4M17 16v4",
};

export function Icon({ name, size = 16, title }: { name: keyof typeof paths | string; size?: number; title?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden={title ? undefined : true}
      role={title ? "img" : undefined}
    >
      {title && <title>{title}</title>}
      <path d={paths[name] ?? ""} />
    </svg>
  );
}
