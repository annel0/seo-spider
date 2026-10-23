import { useEffect, useMemo, useState } from "react";
import { api, type PageContent as Content, type TextBlock } from "../api";
import { formatCount, plural } from "../labels";

type View = "text" | "raw" | "rendered";

type Marked = {
  block: TextBlock;
  /// Not in the server's document at all.
  added: boolean;
  /// For a block JavaScript changed: which of its words are new.
  fresh?: boolean[];
};

const split = (text: string) => text.split(/\s+/).filter(Boolean);

/// Which words of `after` are not in `before`, by their longest common
/// subsequence.
function freshWords(before: string[], after: string[]): boolean[] {
  const columns = after.length + 1;
  const table = new Uint32Array((before.length + 1) * columns);
  for (let i = before.length - 1; i >= 0; i--) {
    for (let j = after.length - 1; j >= 0; j--) {
      table[i * columns + j] =
        before[i] === after[j]
          ? table[(i + 1) * columns + j + 1] + 1
          : Math.max(table[(i + 1) * columns + j], table[i * columns + j + 1]);
    }
  }
  const fresh = after.map(() => true);
  let i = 0;
  let j = 0;
  while (i < before.length && j < after.length) {
    if (before[i] === after[j]) {
      fresh[j] = false;
      i++;
      j++;
    } else if (table[(i + 1) * columns + j] >= table[i * columns + j + 1]) i++;
    else j++;
  }
  return fresh;
}

/// How much two blocks share, by their words.
function overlap(a: string[], b: string[]) {
  const set = new Set(a);
  const common = b.filter((word) => set.has(word)).length;
  return common / Math.max(a.length, b.length, 1);
}

/// Rendered blocks against the server's: the same text, a block
/// JavaScript changed (most words kept: only its new words count), or a
/// block that is new altogether; and the server's blocks that are gone.
function compare(raw: TextBlock[], rendered: TextBlock[]) {
  const left = new Map<string, number>();
  for (const block of raw) left.set(block.text, (left.get(block.text) ?? 0) + 1);
  const marked: Marked[] = rendered.map((block) => {
    const have = left.get(block.text) ?? 0;
    if (have > 0) left.set(block.text, have - 1);
    return { block, added: have === 0 };
  });
  const right = new Map<string, number>();
  for (const block of rendered) right.set(block.text, (right.get(block.text) ?? 0) + 1);
  let removed = raw.filter((block) => {
    const have = right.get(block.text) ?? 0;
    if (have > 0) right.set(block.text, have - 1);
    return have === 0;
  });
  // A new block that mostly repeats a gone one is that block changed.
  for (const item of marked.filter((item) => item.added)) {
    const words = split(item.block.text);
    let best = -1;
    let score = 0.5;
    removed.forEach((block, at) => {
      const similarity = overlap(split(block.text), words);
      if (similarity >= score) {
        best = at;
        score = similarity;
      }
    });
    if (best >= 0 && words.length <= 2000) {
      item.fresh = freshWords(split(removed[best].text), words);
      item.added = false;
      removed = removed.filter((_, at) => at !== best);
    }
  }
  return { marked, removed };
}

const newWords = (marked: Marked[]) =>
  marked.reduce((sum, item) => sum + (item.added ? split(item.block.text).length : (item.fresh?.filter(Boolean).length ?? 0)), 0);

/// The page's text as a reader sees it, what JavaScript added marked, and
/// its HTML as sent and as rendered.
export function PageContent({ urlId }: { urlId: number }) {
  const [content, setContent] = useState<Content | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<View>("text");

  useEffect(() => {
    setContent(null);
    setError(null);
    let current = true;
    api
      .content(urlId)
      .then((found) => current && setContent(found))
      .catch((failure: Error) => current && setError(failure.message));
    return () => {
      current = false;
    };
  }, [urlId]);

  const fetchNow = () => {
    setLoading(true);
    setError(null);
    api
      .content(urlId, true)
      .then(setContent)
      .catch((failure: Error) => setError(failure.message))
      .finally(() => setLoading(false));
  };

  const diff = useMemo(
    () => (content?.raw && content.rendered ? compare(content.raw.blocks, content.rendered.blocks) : null),
    [content],
  );

  if (error) return <div className="kv"><div>Содержимое</div><div style={{ color: "var(--bad)" }}>{error}</div></div>;
  if (!content || loading) {
    return <div className="kv"><div>Содержимое</div><div>{loading ? "загружаю страницу и выполняю JavaScript…" : "загрузка"}</div></div>;
  }
  if (!content.raw && !content.rendered) {
    return (
      <div className="content-empty">
        <span>Этот обход не сохранял HTML страниц (его сохраняет пресет «Дамп»).</span>
        <button className="primary" onClick={fetchNow}>
          Загрузить сейчас
        </button>
        {content.error && <span style={{ color: "var(--bad)" }}>{content.error}</span>}
      </div>
    );
  }

  const shown = content.rendered ?? content.raw;
  const html = view === "raw" ? content.raw : view === "rendered" ? content.rendered : null;
  const changed = diff?.marked.filter((item) => item.added || item.fresh?.some(Boolean)) ?? [];
  const wordsAdded = diff ? newWords(diff.marked) : 0;

  return (
    <div className="page-content">
      <div className="content-bar">
        <div className="segmented">
          <button className={view === "text" ? "on" : undefined} onClick={() => setView("text")}>
            Текст
          </button>
          {content.raw && (
            <button className={view === "raw" ? "on" : undefined} onClick={() => setView("raw")}>
              HTML от сервера
            </button>
          )}
          {content.rendered && (
            <button className={view === "rendered" ? "on" : undefined} onClick={() => setView("rendered")}>
              HTML после JS
            </button>
          )}
        </div>
        {diff && (
          <>
            <span className="badge-soft" style={{ color: wordsAdded ? "var(--accent)" : undefined }}>
              {wordsAdded === 0 && changed.length === 0
                ? "JS ничего не добавил"
                : `JS добавил ${formatCount(wordsAdded)} ${plural(wordsAdded, ["слово", "слова", "слов"])} в ${formatCount(changed.length)} ${plural(changed.length, ["блоке", "блоках", "блоках"])}`}
            </span>
            {diff.removed.length > 0 && (
              <span className="badge-soft" style={{ color: "var(--warn)" }}>
                убрал {formatCount(diff.removed.length)} {plural(diff.removed.length, ["блок", "блока", "блоков"])}
              </span>
            )}
          </>
        )}
        <span style={{ flex: 1 }} />
        {content.live && <span className="note">загружено сейчас, может отличаться от обхода</span>}
        {content.live && (
          <button className="ghost" onClick={fetchNow}>
            Обновить
          </button>
        )}
      </div>
      {content.error && <div className="note" style={{ padding: "6px 12px", color: "var(--bad)" }}>{content.error}</div>}
      {html ? (
        <pre className="html-source">
          {html.html}
          {html.cut && "\n… (обрезано на 2 МБ)"}
        </pre>
      ) : (
        <div className="text-blocks">
          {(diff ? diff.marked : shown!.blocks.map((block): Marked => ({ block, added: false }))).map(({ block, added: isNew, fresh }, at) => (
            <div
              key={at}
              className={`text-block tag-${block.tag} ${isNew ? "added" : fresh?.some(Boolean) ? "changed" : ""}`}
              title={isNew ? "Только после JavaScript" : fresh?.some(Boolean) ? "JavaScript дописал выделенное" : undefined}
            >
              {fresh
                ? split(block.text).map((word, index) => (
                    <span key={index} className={fresh[index] ? "fresh" : undefined}>
                      {index > 0 ? " " : ""}
                      {word}
                    </span>
                  ))
                : block.text}
            </div>
          ))}
          {diff && diff.removed.length > 0 && (
            <>
              <div className="text-removed-title">Было в HTML от сервера, после JavaScript нет</div>
              {diff.removed.map((block, at) => (
                <div key={`r${at}`} className={`text-block tag-${block.tag} removed`}>
                  {block.text}
                </div>
              ))}
            </>
          )}
        </div>
      )}
    </div>
  );
}
