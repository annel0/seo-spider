import { useEffect, useState } from "react";
import { api, type Crawl, type Preset, type Schedule, type ScheduleList } from "../api";
import { formatWhen, hostOf, stateLabels } from "../labels";
import type { JsonObject } from "../settings";
import { Icon } from "./Icon";

type Period = "day" | "week" | "12" | "6" | "1";

const periods: [Period, string, number][] = [
  ["day", "Каждый день", 24],
  ["week", "Каждую неделю", 168],
  ["12", "Каждые 12 часов", 12],
  ["6", "Каждые 6 часов", 6],
  ["1", "Каждый час", 1],
];

// By Date.getDay(): Sunday first.
const weekdays = ["воскресенье", "понедельник", "вторник", "среда", "четверг", "пятница", "суббота"];
const everyWeekday = [
  "каждое воскресенье",
  "каждый понедельник",
  "каждый вторник",
  "каждую среду",
  "каждый четверг",
  "каждую пятницу",
  "каждую субботу",
];

const presetTitle = (presets: Preset[], name: unknown) => presets.find((preset) => preset.name === name)?.title ?? String(name ?? "");

/// The first run, in Unix seconds: the next time of day (on the weekday,
/// for a week); the next full hour for a period in hours.
function firstRun(period: Period, time: string, weekday: number) {
  const now = new Date();
  const next = new Date(now);
  if (period !== "day" && period !== "week") {
    next.setMinutes(0, 0, 0);
    next.setHours(next.getHours() + 1);
    return Math.floor(next.getTime() / 1000);
  }
  const [hours, minutes] = time.split(":").map(Number);
  next.setHours(hours || 0, minutes || 0, 0, 0);
  if (period === "week") next.setDate(next.getDate() + ((weekday - next.getDay() + 7) % 7));
  if (next <= now) next.setDate(next.getDate() + (period === "week" ? 7 : 1));
  return Math.floor(next.getTime() / 1000);
}

/// How often, in words: каждый день в 03:00.
function cadence(schedule: Schedule) {
  const next = new Date(schedule.next_run * 1000);
  const time = `${String(next.getHours()).padStart(2, "0")}:${String(next.getMinutes()).padStart(2, "0")}`;
  if (schedule.every_hours === 24) return `каждый день в ${time}`;
  if (schedule.every_hours === 168) return `${everyWeekday[next.getDay()]} в ${time}`;
  return `каждые ${schedule.every_hours} ч`;
}

const unixWhen = (seconds: number) => formatWhen(new Date(seconds * 1000).toISOString());

/// Crawls the server starts on its own, and where their reports go: a
/// new one takes the site, preset and settings as they are set now.
export function Schedules({
  presets,
  crawls,
  preset,
  patch,
  startUrl,
  onOpen,
  onClose,
}: {
  presets: Preset[];
  crawls: Crawl[];
  preset: string;
  /// The settings form's changes to the preset.
  patch: JsonObject;
  /// The open crawl's site, to schedule it again.
  startUrl: string;
  onOpen: (crawlId: number) => void;
  onClose: () => void;
}) {
  const [list, setList] = useState<ScheduleList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [url, setUrl] = useState(startUrl);
  const [period, setPeriod] = useState<Period>("day");
  const [time, setTime] = useState("03:00");
  const [weekday, setWeekday] = useState(1);
  const [chat, setChat] = useState("");
  const [tested, setTested] = useState<string | null>(null);
  const changed = Object.keys(patch).length;

  const load = () => api.schedules().then(setList).catch((failure: Error) => setError(failure.message));
  useEffect(() => {
    load();
  }, []);

  const act = (work: Promise<unknown>) => {
    setError(null);
    work.then(load).catch((failure: Error) => setError(failure.message));
  };

  const add = () => {
    const typed = url.trim();
    const address = /^https?:\/\//.test(typed) ? typed : `https://${typed}`;
    const hours = periods.find(([key]) => key === period)![2];
    act(
      api.saveSchedule({
        id: 0,
        request: { start_url: address, preset, ...(changed > 0 ? { config: patch } : {}) },
        every_hours: hours,
        next_run: firstRun(period, time, weekday),
        telegram_chat: chat.trim() || null,
        enabled: true,
        last_crawl: null,
      }),
    );
  };

  const test = () => {
    setTested(null);
    setError(null);
    api
      .testTelegram(chat.trim())
      .then(() => setTested("сообщение отправлено"))
      .catch((failure: Error) => setError(failure.message));
  };

  const byId = new Map(crawls.map((crawl) => [crawl.id, crawl]));
  const daily = period === "day" || period === "week";

  return (
    <div className="schedules sheet">
      <div className="toolbar">
        <span className="title">Расписание обходов</span>
        <span style={{ flex: 1 }} />
        <button className="ghost" onClick={onClose} aria-label="Закрыть">
          <Icon name="cross" />
        </button>
      </div>
      {error && <div className="error-banner" onClick={() => setError(null)}>{error}</div>}

      <div className="schedule-form">
        <label>
          <span>Сайт</span>
          <input value={url} onChange={(event) => setUrl(event.target.value)} placeholder="example.com" />
        </label>
        <label>
          <span>Как</span>
          <span className="value">
            {presetTitle(presets, preset)}
            {changed > 0 && `, с изменёнными настройками (${changed})`}
          </span>
        </label>
        <label>
          <span>Когда</span>
          <span className="when">
            <select value={period} onChange={(event) => setPeriod(event.target.value as Period)}>
              {periods.map(([key, title]) => (
                <option key={key} value={key}>
                  {title}
                </option>
              ))}
            </select>
            {period === "week" && (
              <select value={weekday} onChange={(event) => setWeekday(Number(event.target.value))} aria-label="День недели">
                {[1, 2, 3, 4, 5, 6, 0].map((day) => (
                  <option key={day} value={day}>
                    {weekdays[day]}
                  </option>
                ))}
              </select>
            )}
            {daily && <input type="time" value={time} onChange={(event) => setTime(event.target.value)} aria-label="Время" />}
          </span>
        </label>
        <label>
          <span>Отчёт в Telegram</span>
          <span className="when">
            <input value={chat} onChange={(event) => setChat(event.target.value)} placeholder="ID чата или @канал" style={{ width: 220 }} />
            <button onClick={test} disabled={!chat.trim() || !list?.telegram}>
              Проверить
            </button>
            {tested && <span className="note">{tested}</span>}
          </span>
        </label>
        {list && !list.telegram && (
          <div className="note">Отчёты уходят, когда у сервера задан токен бота: переменная SEO_SPIDER_TELEGRAM_TOKEN.</div>
        )}
        <div>
          <button className="primary" onClick={add} disabled={!url.trim()}>
            <Icon name="clock" /> Добавить
          </button>
        </div>
      </div>

      <div className="links">
        {list?.schedules.length === 0 && <div className="empty" style={{ height: 120 }}>Расписаний нет</div>}
        {list?.schedules.map((schedule) => {
          const last = schedule.last_crawl !== null ? byId.get(schedule.last_crawl) : undefined;
          return (
            <div key={schedule.id} className={`schedule-line ${schedule.enabled ? "" : "off"}`}>
              <span className="where">
                <span className="site">{hostOf(String(schedule.request.start_url ?? ""))}</span>
                <span className="note">
                  {presetTitle(presets, schedule.request.preset)}
                  {schedule.request.config ? ", свои настройки" : ""}
                </span>
              </span>
              <span className="where">
                <span>{cadence(schedule)}</span>
                <span className="note">{schedule.enabled ? `следующий ${unixWhen(schedule.next_run)}` : "выключено"}</span>
              </span>
              <span className="where">
                {last ? (
                  <button className="link" onClick={() => onOpen(last.id)}>
                    #{last.id} {formatWhen(last.started_at)}, {stateLabels[last.state].toLowerCase()}
                  </button>
                ) : (
                  <span className="note">ещё не запускался</span>
                )}
                <span className="note">{schedule.telegram_chat ? `Telegram ${schedule.telegram_chat}` : "без отчёта"}</span>
              </span>
              <span className="actions">
                <button onClick={() => act(api.runSchedule(schedule.id))} title="Запустить обход сейчас; расписание не сдвигается">
                  <Icon name="play" />
                </button>
                <button
                  onClick={() => act(api.saveSchedule({ ...schedule, enabled: !schedule.enabled }))}
                  title={schedule.enabled ? "Выключить" : "Включить"}
                >
                  <Icon name={schedule.enabled ? "pause" : "check"} />
                </button>
                <button onClick={() => act(api.deleteSchedule(schedule.id))} title="Удалить расписание">
                  <Icon name="cross" />
                </button>
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
