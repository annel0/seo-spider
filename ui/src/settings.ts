// The crawl settings form: what each field of the config is called in
// Russian, how fields group into sections, and the JSON merge patch the
// form edits. The fields themselves come from the server's JSON Schema;
// one this file does not name still shows, under «Прочее», with its key
// and the schema's description.

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };

/// A schema node as schemars writes it (JSON Schema 2020-12, the parts used).
export type Schema = {
  type?: string | string[];
  description?: string;
  properties?: Record<string, Schema>;
  additionalProperties?: Schema | boolean;
  items?: Schema;
  $ref?: string;
  anyOf?: Schema[];
  oneOf?: Schema[];
  const?: Json;
  enum?: Json[];
  default?: Json;
  minimum?: number;
  maximum?: number;
  required?: string[];
  $defs?: Record<string, Schema>;
};

export type Field = { label: string; hint?: string; unit?: string; options?: Record<string, string> };

export const fields: Record<string, Field> = {
  max_urls: { label: "Лимит URL", hint: "Сколько внутренних страниц ставить в очередь. У ресурсов и внешних ссылок такой же отдельный бюджет." },
  follow_links: { label: "Ходить по ссылкам", hint: "Без этого обходятся только стартовый адрес, sitemap и редиректы." },
  max_depth: { label: "Глубина", unit: "кликов", hint: "Пусто: без ограничения." },
  fetch: {
    label: "Что скачивать",
    options: { status: "Код и заголовки", head: "До </head>", full: "Всю страницу" },
  },
  include: { label: "Только адреса по шаблону", hint: "Регулярные выражения, по одному в строке. Пусто: все адреса." },
  exclude: { label: "Исключить адреса", hint: "Регулярные выражения, по одному в строке." },
  strip_params: { label: "Срезать параметры", hint: "По одному в строке; utm_* срезает все с этим началом." },
  crawl_subdomains: { label: "Поддомены считать своими" },
  follow_nofollow: { label: "Ходить по nofollow" },
  check_external: { label: "Проверять внешние ссылки", hint: "Один запрос на ссылку, чтобы узнать код ответа." },
  check_resources: { label: "Проверять картинки, скрипты и стили" },
  use_sitemaps: { label: "Брать адреса из sitemap" },
  respect_robots: { label: "Соблюдать robots.txt", hint: "Закрытые адреса не скачиваются, но попадают в отчёт." },

  "http.user_agent": { label: "User-Agent" },
  "http.robots_agent": { label: "Имя для robots.txt", hint: "По нему выбирается группа правил." },
  "http.concurrency": { label: "Запросов к сайту одновременно" },
  "http.max_rps": { label: "Запросов в секунду, не больше", hint: "Пусто: без ограничения." },
  "http.auto_throttle": { label: "Притормаживать на 429 и 503" },
  "http.retries": { label: "Повторов при ошибке" },
  "http.timeout_ms": { label: "Ждать ответа", unit: "мс" },
  "http.connect_timeout_ms": { label: "Ждать соединения", unit: "мс" },
  "http.external_concurrency": { label: "Внешних запросов одновременно" },
  "http.external_per_host": { label: "Внешних на один хост" },
  "http.external_timeout_ms": { label: "Ждать внешний ответ", unit: "мс" },
  "http.headers": { label: "Заголовки запроса", hint: "По одному в строке: «Имя: значение». Cookie, Authorization, Accept-Language." },
  "http.cookies": { label: "Хранить cookies сайта" },
  "http.head_for_status": { label: "Проверять ресурсы запросом HEAD" },
  "http.max_body": { label: "Обрезать тело после", unit: "байт" },
  "http.head_max_bytes": { label: "Искать </head> в первых", unit: "байт" },
  "http.head_drain_bytes": { label: "Дочитывать после </head>", unit: "байт", hint: "Чтобы соединение переиспользовалось; 0 рвёт его сразу." },
  "http.http2": { label: "Предлагать HTTP/2", hint: "Выключено намеренно: часть серверов рвёт соединения на обрезанных чтениях." },
  "http.ca_certs": { label: "Свои корневые сертификаты", hint: "Пути к PEM-файлам на сервере, по одному в строке. Для сайтов на сертификатах Минцифры." },
  "http.accept_invalid_certs": { label: "Принимать любые сертификаты" },

  "extract.links": { label: "Ссылки" },
  "extract.resources": { label: "Картинки, скрипты, стили, медиа" },
  "extract.headings": { label: "Заголовки h1–h6" },
  "extract.text": { label: "Текст: слова и дубли" },
  "extract.structured_data": { label: "Микроразметка" },
  "extract.forms": { label: "Формы" },
  "extract.dom_stats": { label: "Размер DOM и повторы id" },
  "extract.similarity": { label: "Отпечаток для похожих страниц", hint: "Около 0,5 КБ на страницу." },

  render: { label: "Выполнять JavaScript", hint: "Каждую страницу открывает движок браузера: видно то, что видит Google. В несколько раз медленнее." },
  "render.threads": { label: "Страниц одновременно" },
  "render.navigation_timeout_ms": { label: "Ждать загрузки страницы", unit: "мс" },
  "render.settle_budget_ms": { label: "Ждать, пока страница успокоится, не дольше", unit: "мс" },
  "render.settle_quiet_ms": { label: "Тишина в DOM до load", unit: "мс" },
  "render.settle_quiet_after_load_ms": { label: "Тишина в DOM после load", unit: "мс" },
  "render.settle_content_quiet_ms": { label: "Контент неподвижен", unit: "мс", hint: "0: ждать, пока DOM совсем затихнет." },
  "render.settle_grace_ms": { label: "Ждать чужие запросы", unit: "мс" },
  "render.third_party_timeout_ms": { label: "Обрывать чужие запросы через", unit: "мс", hint: "Пусто: 10 секунд." },
  "render.user_agent": { label: "User-Agent браузера", hint: "Пусто: как у движка." },
  "render.locale": { label: "Язык браузера", hint: "Например ru-RU." },
  "render.timezone": { label: "Часовой пояс", hint: "Например Europe/Moscow. Пусто: как на сервере." },
  "render.block_trackers": { label: "Блокировать трекеры" },
  "render.cache_static": { label: "Общий кэш скриптов, стилей и шрифтов" },
  "render.cache_mb": { label: "Размер кэша", unit: "МБ" },
  "render.fetch_cache_ttl_ms": { label: "Кэшировать fetch и XHR", unit: "мс", hint: "Пусто: без кэша." },
  "render.layout_budget_ms": { label: "Бюджет раскладки", unit: "мс", hint: "Пусто: без ограничения." },
  "render.intersection_full_viewport": { label: "Ленивая подгрузка видит всю страницу" },
  "render.skip_images": { label: "Не грузить картинки" },
  "render.skip_fonts": { label: "Не грузить шрифты" },
  "render.save_data": { label: "Режим экономии трафика" },
  "render.allow_private_network": { label: "Пускать к локальным адресам" },
  "render.recycle_after": { label: "Пересоздавать вкладку через", unit: "страниц" },
  "render.reuse_documents": { label: "Рендерить скачанный HTML", hint: "На запрос меньше, но страница та, что отдали краулеру." },

  "custom.extract": { label: "Извлечение", hint: "Значения со страниц, каждое в свой столбец вкладки «Своё извлечение»." },
  "custom.search": { label: "Поиск", hint: "Сколько раз текст или шаблон встречается на странице." },

  "checks.near_duplicate_threshold": { label: "Порог похожести страниц", hint: "От 0 до 1: доля общих фраз основного текста, с которой страницы считаются почти дублями." },

  "store.raw_html": { label: "Сохранять исходный HTML" },
  "store.rendered_html": { label: "Сохранять отрисованный HTML" },
  "store.links": { label: "Ссылки отдельной таблицей", hint: "Для SQL по ссылкам; обход медленнее на треть." },
  "store.compression_level": { label: "Сжатие zstd", hint: "0: без сжатия." },
};

/// Sections in order: a title and the paths, or prefixes ending in `.`, it holds.
export const sections: { key: string; title: string; paths: string[] }[] = [
  {
    key: "crawl",
    title: "Обход",
    paths: ["max_urls", "follow_links", "max_depth", "fetch", "use_sitemaps", "respect_robots", "check_resources", "check_external", "crawl_subdomains", "follow_nofollow", "include", "exclude", "strip_params"],
  },
  { key: "http", title: "Сеть", paths: ["http."] },
  { key: "extract", title: "Что читать", paths: ["extract."] },
  { key: "render", title: "JavaScript", paths: ["render"] },
  { key: "custom", title: "Своё извлечение", paths: ["custom."] },
  { key: "checks", title: "Проверки", paths: ["checks."] },
  { key: "store", title: "Хранение", paths: ["store."] },
];

/// Fields the form leaves out: the start line sets the address.
export const hidden = new Set(["start_urls"]);

/// Resolves `$ref` and makes a nullable schema plain: (schema, nullable).
export function resolve(schema: Schema, root: Schema): [Schema, boolean] {
  let node = schema;
  let nullable = false;
  if (node.anyOf) {
    const rest = node.anyOf.filter((part) => part.type !== "null");
    nullable = rest.length < node.anyOf.length;
    node = { ...rest[0], description: node.description ?? rest[0]?.description };
  }
  if (node.$ref) {
    const name = node.$ref.replace("#/$defs/", "");
    node = { ...root.$defs?.[name], description: node.description ?? root.$defs?.[name]?.description };
  }
  if (Array.isArray(node.type)) {
    nullable ||= node.type.includes("null");
    const type = node.type.find((item) => item !== "null");
    node = { ...node, type };
  }
  return [node, nullable];
}

/// The leaves of the settings: paths the form shows a control for. Plain
/// nested objects (http, extract) open into their fields; `render`, which
/// can be absent, is a leaf of its own with fields under it.
export function leaves(root: Schema): { path: string; schema: Schema; nullable: boolean }[] {
  const found: { path: string; schema: Schema; nullable: boolean }[] = [];
  const walk = (schema: Schema, prefix: string) => {
    for (const [key, child] of Object.entries(schema.properties ?? {})) {
      const path = prefix + key;
      if (hidden.has(path)) continue;
      const [node, nullable] = resolve(child, root);
      if (node.type === "object" && node.properties && !nullable) walk(node, `${path}.`);
      else found.push({ path, schema: node, nullable });
    }
  };
  walk(root, "");
  return found;
}

export const sectionOf = (path: string) =>
  sections.find((section) => section.paths.some((item) => (item.endsWith(".") ? path.startsWith(item) : path === item)))?.key ?? "other";

export const get = (value: Json | undefined, path: string): Json | undefined =>
  path.split(".").reduce<Json | undefined>((at, key) => (at && typeof at === "object" && !Array.isArray(at) ? at[key] : undefined), value);

export const equal = (a: Json | undefined, b: Json | undefined) => JSON.stringify(a ?? null) === JSON.stringify(b ?? null);

/// JSON merge patch, as the server applies it.
export function merge(target: Json | undefined, patch: Json): Json {
  if (patch === null || typeof patch !== "object" || Array.isArray(patch)) return patch;
  const result: JsonObject = target && typeof target === "object" && !Array.isArray(target) ? { ...target } : {};
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) delete result[key];
    else result[key] = merge(result[key], value);
  }
  return result;
}

/// The patch with one path set to a value: dropped when the value is what
/// the preset has, `null` when the preset has something and the value is
/// nothing.
export function setPath(patch: JsonObject, base: Json, path: string, value: Json | undefined): JsonObject {
  const keys = path.split(".");
  const same = equal(value, get(base, path));
  const next: JsonObject = structuredClone(patch);
  let at: JsonObject = next;
  for (const key of keys.slice(0, -1)) {
    const child = at[key];
    if (!child || typeof child !== "object" || Array.isArray(child)) at[key] = {};
    at = at[key] as JsonObject;
  }
  const last = keys[keys.length - 1];
  if (same) delete at[last];
  else at[last] = value === undefined ? null : value;
  return prune(next);
}

/// Drops empty objects the edits left behind.
function prune(value: JsonObject): JsonObject {
  for (const [key, child] of Object.entries(value)) {
    if (child && typeof child === "object" && !Array.isArray(child)) {
      prune(child);
      if (Object.keys(child).length === 0) delete value[key];
    }
  }
  return value;
}

/// How many fields the patch changes.
export function changes(patch: Json): number {
  if (!patch || typeof patch !== "object" || Array.isArray(patch)) return 1;
  return Object.values(patch).reduce<number>((sum, child) => sum + changes(child), 0);
}
