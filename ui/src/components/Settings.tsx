import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  equal,
  fields,
  get,
  leaves,
  resolve,
  sectionOf,
  sections,
  setPath,
  type Json,
  type JsonObject,
  type Schema,
} from "../settings";
import { api } from "../api";
import { formatBytes } from "../labels";
import { Icon } from "./Icon";

type Leaf = { path: string; schema: Schema; nullable: boolean };

/// How many settings differ from the preset: turning rendering on or off
/// counts once, not once per field of it.
export function countChanges(root: Schema, base: Json, value: Json): number {
  let count = 0;
  for (const leaf of leaves(root)) {
    if (leaf.path === "render") {
      const before = get(base, "render");
      const after = get(value, "render");
      if (!before !== !after) count += 1;
      else if (after) count += renderFields(root).filter((field) => !equal(get(after, field.path), get(before, field.path))).length;
    } else if (!equal(get(value, leaf.path), get(base, leaf.path))) {
      count += 1;
    }
  }
  return count;
}

/// The fields of the render settings, by their key inside `render`.
const renderFields = (root: Schema): Leaf[] => {
  const [render] = resolve(root.properties?.render ?? {}, root);
  return Object.entries(render.properties ?? {}).map(([path, schema]) => {
    const [node, nullable] = resolve(schema, root);
    return { path, schema: node, nullable };
  });
};

/// Every setting of a crawl, from the config's schema, grouped into
/// sections. Values start from the preset; a field that differs is marked
/// and can be put back.
export function Settings({
  schema,
  base,
  value,
  patch,
  preset,
  presetTitle,
  renderDefaults,
  onPatch,
  onClose,
}: {
  schema: Schema;
  base: Json;
  value: Json;
  patch: JsonObject;
  /// The preset's name, for the TOML file.
  preset: string;
  presetTitle: string;
  renderDefaults: Json;
  onPatch: (patch: JsonObject) => void;
  onClose: () => void;
}) {
  const all = useMemo(() => leaves(schema), [schema]);
  const render = useMemo(() => renderFields(schema), [schema]);
  const [current, setCurrent] = useState(sections[0].key);
  const body = useRef<HTMLDivElement>(null);
  const [fileError, setFileError] = useState<string | null>(null);

  const readFile = (file: File | undefined) => {
    if (!file) return;
    setFileError(null);
    file
      .text()
      .then((text) => api.importConfig(preset, text))
      .then((imported) => onPatch(imported.patch))
      .catch((failure: Error) => setFileError(failure.message));
  };

  const writeFile = () => {
    setFileError(null);
    api
      .exportConfig(preset, patch)
      .then((blob) => {
        const link = document.createElement("a");
        link.href = URL.createObjectURL(blob);
        link.download = `seo-spider-${preset}.toml`;
        link.click();
        URL.revokeObjectURL(link.href);
      })
      .catch((failure: Error) => setFileError(failure.message));
  };
  // Named paths in the section's order, the rest in the schema's.
  const grouped = [...sections, { key: "other", title: "Прочее", paths: [] as string[] }]
    .map((section) => {
      const rank = (leaf: Leaf) => {
        const at = section.paths.indexOf(leaf.path);
        return at < 0 ? section.paths.length : at;
      };
      const mine = all.filter((leaf) => sectionOf(leaf.path) === section.key);
      return { ...section, leaves: mine.map((leaf, at) => ({ leaf, at })).sort((a, b) => rank(a.leaf) - rank(b.leaf) || a.at - b.at).map(({ leaf }) => leaf) };
    })
    .filter((section) => section.leaves.length > 0);
  const changed = countChanges(schema, base, value);
  const set = (path: string, next: Json | undefined) => onPatch(setPath(patch, base, path, next));
  const rendering = get(value, "render");

  const jump = (key: string) => body.current?.querySelector(`#settings-${key}`)?.scrollIntoView({ block: "start" });

  return (
    <div className="settings">
      <nav className="nav" aria-label="Разделы настроек">
        {grouped.map((section) => (
          <div key={section.key} className={`nav-row top ${current === section.key ? "on" : ""}`} onClick={() => jump(section.key)}>
            <span className="label">{section.title}</span>
          </div>
        ))}
      </nav>
      <div className="work">
        <div className="toolbar">
          <span className="title">Настройки обхода</span>
          <span style={{ color: "var(--ink-3)" }}>от пресета {presetTitle}</span>
          {changed > 0 && <span className="badge">изменено {changed}</span>}
          <span style={{ flex: 1 }} />
          {fileError && <span style={{ color: "var(--bad)" }}>{fileError}</span>}
          <label className="button" title="Настройки из TOML-файла поверх пресета, как seo-spider crawl --config">
            <Icon name="upload" /> Из TOML
            <input type="file" accept=".toml,text/plain" hidden onChange={(event) => readFile(event.target.files?.[0])} />
          </label>
          <button onClick={writeFile} title="Эти настройки файлом для seo-spider crawl --config">
            <Icon name="download" /> TOML
          </button>
          <button onClick={() => onPatch({})} disabled={changed === 0}>
            Вернуть как в пресете
          </button>
          <button className="primary" onClick={onClose}>
            <Icon name="check" /> Готово
          </button>
        </div>
        <div
          className="settings-body"
          ref={body}
          onScroll={(event) => {
            // The section under the top edge is the current one.
            const top = event.currentTarget.getBoundingClientRect().top + 48;
            const passed = [...event.currentTarget.querySelectorAll<HTMLElement>(".settings-section")].filter(
              (section) => section.getBoundingClientRect().top <= top,
            );
            const last = passed[passed.length - 1];
            if (last) setCurrent(last.id.replace("settings-", ""));
          }}
        >
          {grouped.map((section) => (
            <section key={section.key} id={`settings-${section.key}`} className="settings-section">
              <h3>{section.title}</h3>
              {section.leaves.map((leaf) =>
                leaf.path === "render" ? (
                  <div key="render">
                    <Row
                      path="render"
                      leaf={leaf}
                      value={rendering ? true : false}
                      base={get(base, "render") ? true : false}
                      onReset={() => set("render", get(base, "render"))}
                    >
                      <Toggle
                        on={!!rendering}
                        onChange={(on) => set("render", on ? (get(base, "render") ?? renderDefaults) : undefined)}
                      />
                    </Row>
                    {rendering &&
                      render.map((field) => {
                        const path = `render.${field.path}`;
                        const before = get(base, path) ?? get(renderDefaults, field.path);
                        return (
                          <Row key={path} path={path} leaf={field} value={get(value, path)} base={before} sub onReset={() => set(path, before)}>
                            <Control leaf={field} root={schema} value={get(value, path)} onChange={(next) => set(path, next)} />
                          </Row>
                        );
                      })}
                  </div>
                ) : (
                  <Row
                    key={leaf.path}
                    path={leaf.path}
                    leaf={leaf}
                    value={get(value, leaf.path)}
                    base={get(base, leaf.path)}
                    onReset={() => set(leaf.path, get(base, leaf.path))}
                  >
                    <Control leaf={leaf} root={schema} value={get(value, leaf.path)} onChange={(next) => set(leaf.path, next)} />
                  </Row>
                ),
              )}
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}

/// One setting: its name and what it does, the control, and a mark with a
/// way back when it is not the preset's.
function Row({
  path,
  leaf,
  value,
  base,
  sub,
  onReset,
  children,
}: {
  path: string;
  leaf: Leaf;
  value: Json | undefined;
  base: Json | undefined;
  sub?: boolean;
  onReset: () => void;
  children: ReactNode;
}) {
  const field = fields[path];
  const changed = !equal(value, base);
  const wide = leaf.schema.type === "array" || (leaf.schema.type === "object" && path !== "render");
  return (
    <div className={`setting ${changed ? "changed" : ""} ${sub ? "sub" : ""} ${wide ? "wide" : ""}`}>
      <div className="what">
        <div className="name">
          {field?.label ?? path}
          {field?.unit && <span className="unit">, {field.unit}</span>}
        </div>
        {(field?.hint ?? (!field ? leaf.schema.description : undefined)) && (
          <div className="hint">{field?.hint ?? leaf.schema.description}</div>
        )}
      </div>
      <div className="control">{children}</div>
      <div className="reset">
        {changed && (
          <button className="ghost" onClick={onReset} title="Как в пресете" aria-label="Вернуть как в пресете">
            ↺
          </button>
        )}
      </div>
    </div>
  );
}

/// The control a schema type calls for.
function Control({ leaf, root, value, onChange }: { leaf: Leaf; root: Schema; value: Json | undefined; onChange: (value: Json | undefined) => void }) {
  const { schema, nullable } = leaf;
  const field = fields[leaf.path];
  const choices = schema.oneOf?.filter((part) => part.const !== undefined).map((part) => part.const) ?? schema.enum;
  if (choices) {
    return (
      <div className="segmented">
        {choices.map((choice) => (
          <button key={String(choice)} className={equal(choice, value) ? "on" : undefined} onClick={() => onChange(choice)}>
            {fields[`${leaf.path}`]?.options?.[String(choice)] ?? choiceLabel(leaf.path, String(choice)) ?? String(choice)}
          </button>
        ))}
      </div>
    );
  }
  switch (schema.type) {
    case "boolean":
      return <Toggle on={value === true} onChange={onChange} />;
    case "integer":
    case "number":
      return (
        <>
          <NumberInput
            integer={schema.type === "integer"}
            min={schema.minimum}
            max={schema.maximum}
            nullable={nullable}
            value={typeof value === "number" ? value : null}
            onChange={onChange}
          />
          {field?.unit === "байт" && typeof value === "number" && value >= 1024 && <span className="readable">{formatBytes(value)}</span>}
        </>
      );
    case "string":
      return (
        <input
          value={typeof value === "string" ? value : ""}
          placeholder={nullable ? "нет" : undefined}
          onChange={(event) => onChange(event.target.value === "" && nullable ? undefined : event.target.value)}
          style={{ width: "100%" }}
        />
      );
    case "array": {
      const [items] = resolve(schema.items ?? {}, root);
      if (items.type === "object") return <Rules path={leaf.path} value={Array.isArray(value) ? value : []} onChange={onChange} />;
      return <Lines value={Array.isArray(value) ? value.map(String) : []} onChange={onChange} placeholder={field?.label} />;
    }
    case "object":
      return (
        <Lines
          value={Object.entries((value as JsonObject | undefined) ?? {}).map(([name, text]) => `${name}: ${String(text)}`)}
          onChange={(lines) => {
            const pairs = (lines ?? []).flatMap((line) => {
              const at = String(line).indexOf(":");
              return at > 0 ? [[String(line).slice(0, at).trim(), String(line).slice(at + 1).trim()] as const] : [];
            });
            onChange(Object.fromEntries(pairs));
          }}
          placeholder="Cookie: session=…"
        />
      );
    default:
      return <code>{JSON.stringify(value)}</code>;
  }
}

/// Labels of the choices in the custom rules.
const choiceLabels: Record<string, string> = {
  "output.text": "Текст",
  "output.html": "HTML элемента",
  "output.inner_html": "HTML внутри",
  "scope.html": "В HTML",
  "scope.text": "В видимом тексте",
};
const choiceLabel = (path: string, choice: string): string | undefined => choiceLabels[`${path.split(".").pop()}.${choice}`];

/// A number typed freely: the text is the field's own while it is edited,
/// and only a number (or nothing, where nothing is allowed) reaches the
/// settings.
function NumberInput({
  integer,
  min,
  max,
  nullable,
  value,
  onChange,
}: {
  integer: boolean;
  min?: number;
  max?: number;
  nullable: boolean;
  value: number | null;
  onChange: (value: Json | undefined) => void;
}) {
  const [text, setText] = useState(value === null ? "" : String(value));
  useEffect(() => {
    if (text.trim() === "" ? value !== null : Number(text) !== value) setText(value === null ? "" : String(value));
    // Only an outside change of the value resets the text.
  }, [value]);
  return (
    <input
      className="figures"
      type="number"
      step={integer ? 1 : 0.01}
      min={min}
      max={max}
      value={text}
      placeholder={nullable ? "нет" : undefined}
      aria-invalid={!nullable && text.trim() === ""}
      onChange={(event) => {
        const typed = event.target.value;
        setText(typed);
        if (typed.trim() === "") {
          if (nullable) onChange(undefined);
        } else if (Number.isFinite(Number(typed))) {
          onChange(integer ? Math.round(Number(typed)) : Number(typed));
        }
      }}
      style={{ width: 140 }}
    />
  );
}

function Toggle({ on, onChange }: { on: boolean; onChange: (on: boolean) => void }) {
  return (
    <button role="switch" aria-checked={on} className={`toggle ${on ? "on" : ""}`} onClick={() => onChange(!on)}>
      <span />
    </button>
  );
}

/// A list of strings as lines; empty lines are dropped. The text is kept
/// as typed and only reset when the value changes from outside.
function Lines({ value, onChange, placeholder }: { value: string[]; onChange: (value: Json[] | undefined) => void; placeholder?: string }) {
  const [text, setText] = useState(value.join("\n"));
  const parse = (raw: string) => raw.split("\n").map((line) => line.trim()).filter(Boolean);
  useEffect(() => {
    if (!equal(parse(text), value)) setText(value.join("\n"));
    // Only an outside change of the value resets the text.
  }, [JSON.stringify(value)]);
  return (
    <textarea
      value={text}
      rows={Math.min(8, Math.max(2, text.split("\n").length + 1))}
      placeholder={placeholder}
      onChange={(event) => {
        setText(event.target.value);
        onChange(parse(event.target.value));
      }}
    />
  );
}

type Rule = { [key: string]: Json };

/// Custom extraction or search rules: one line each, the method picked
/// from a list rather than three fields of which one may be filled.
function Rules({ path, value, onChange }: { path: string; value: Json[]; onChange: (value: Json[]) => void }) {
  const extract = path === "custom.extract";
  const methods = extract ? ["css", "xpath", "regex"] : ["text", "regex"];
  const rules = value as Rule[];
  const update = (at: number, rule: Rule) => onChange(rules.map((item, index) => (index === at ? rule : item)));
  const methodOf = (rule: Rule) => methods.find((method) => typeof rule[method] === "string") ?? methods[0];
  return (
    <div className="rules">
      {rules.map((rule, at) => {
        const method = methodOf(rule);
        const setMethod = (next: string) => {
          const changed: Rule = { ...rule, [next]: rule[method] ?? "" };
          delete changed[method];
          if (next !== "css") delete changed.attribute;
          update(at, changed);
        };
        return (
          <div key={at} className="rule">
            <input value={String(rule.name ?? "")} placeholder="Название" onChange={(event) => update(at, { ...rule, name: event.target.value })} style={{ width: 140 }} />
            <select value={method} onChange={(event) => setMethod(event.target.value)} aria-label="Как искать">
              {methods.map((item) => (
                <option key={item} value={item}>
                  {{ css: "CSS", xpath: "XPath", regex: "Регулярка", text: "Текст" }[item]}
                </option>
              ))}
            </select>
            <input
              value={String(rule[method] ?? "")}
              placeholder={{ css: ".price", xpath: "//h1", regex: "SKU: (\\d+)", text: "нет в наличии" }[method]}
              onChange={(event) => update(at, { ...rule, [method]: event.target.value })}
              style={{ flex: 1, minWidth: 160 }}
            />
            {extract && method === "css" && (
              <input
                value={String(rule.attribute ?? "")}
                placeholder="атрибут"
                onChange={(event) => {
                  const rest = { ...rule };
                  delete rest.attribute;
                  update(at, event.target.value ? { ...rest, attribute: event.target.value } : rest);
                }}
                style={{ width: 100 }}
              />
            )}
            {extract && method !== "regex" && (
              <select value={String(rule.output ?? "text")} onChange={(event) => update(at, { ...rule, output: event.target.value })} aria-label="Что брать">
                <option value="text">Текст</option>
                <option value="html">HTML элемента</option>
                <option value="inner_html">HTML внутри</option>
              </select>
            )}
            {!extract && (
              <select value={String(rule.scope ?? "html")} onChange={(event) => update(at, { ...rule, scope: event.target.value })} aria-label="Где искать">
                <option value="html">В HTML</option>
                <option value="text">В видимом тексте</option>
              </select>
            )}
            <button className="ghost" onClick={() => onChange(rules.filter((_, index) => index !== at))} aria-label="Убрать правило">
              <Icon name="cross" />
            </button>
          </div>
        );
      })}
      <button onClick={() => onChange([...rules, extract ? { name: `поле${rules.length + 1}`, css: "" } : { name: `поиск${rules.length + 1}`, text: "" }])}>
        + Правило
      </button>
    </div>
  );
}
