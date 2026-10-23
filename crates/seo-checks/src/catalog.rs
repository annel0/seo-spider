//! The catalog of every issue the crate can report. 79 entries come from
//! SEOnaut (same severities: its priority 1/2/3 is High/Medium/Low), five are
//! additions the Screaming Frog tabs call for, twelve belong to the JavaScript
//! tab and five to structured data.

use crate::{CheckDef, Severity};
use Severity::{High, Low, Medium};

const fn def(
    code: &'static str,
    severity: Severity,
    category: &'static str,
    title: &'static str,
    description: &'static str,
) -> CheckDef {
    CheckDef { code, severity, category, title, description }
}

/// The definition of a check by code.
pub fn find(code: &str) -> Option<&'static CheckDef> {
    CATALOG.iter().find(|check| check.code == code)
}

pub static CATALOG: &[CheckDef] = &[
    // ---- response_codes ----
    def(
        "redirect_3xx",
        High,
        "response_codes",
        "Редирект (3xx)",
        "URL отвечает редиректом: пользователи и роботы уходят на другой адрес, а каждый переход тратит время и краулинговый бюджет.",
    ),
    def(
        "client_error_4xx",
        High,
        "response_codes",
        "Ошибка клиента (4xx)",
        "Страница не найдена или недоступна. Пользователи и роботы попадают на страницу ошибки: исправьте ссылки или настройте редирект.",
    ),
    def(
        "server_error_5xx",
        High,
        "response_codes",
        "Ошибка сервера (5xx)",
        "Сервер не смог обработать запрос. Постоянные 5xx замедляют обход сайта и могут выбить страницы из индекса.",
    ),
    def(
        "redirect_chain",
        High,
        "response_codes",
        "Цепочка редиректов",
        "URL перенаправляет на адрес, который сам перенаправляет дальше. Каждый шаг замедляет загрузку: ведите сразу на конечный URL.",
    ),
    def(
        "redirect_loop",
        High,
        "response_codes",
        "Петля редиректов",
        "Редиректы замыкаются в кольцо: страница недоступна ни пользователям, ни роботам.",
    ),
    def(
        "timeout",
        High,
        "response_codes",
        "Таймаут ответа",
        "Сервер не ответил за отведённое время.",
    ),
    def(
        "no_response",
        High,
        "response_codes",
        "Нет ответа",
        "Запрос завершился ошибкой до получения ответа: DNS, соединение, TLS и т. п.",
    ),
    def(
        "meta_refresh_redirect",
        Medium,
        "response_codes",
        "Редирект через meta refresh",
        "Страница перенаправляет браузер тегом meta refresh. Роботы обрабатывают его хуже серверного 301: используйте HTTP-редирект.",
    ),
    def(
        "slow_ttfb",
        Medium,
        "response_codes",
        "Медленный ответ сервера",
        "Время до первого байта больше 800 мс. Медленный сервер ухудшает опыт пользователей и замедляет обход.",
    ),
    // ---- page_titles ----
    def(
        "title_missing",
        Medium,
        "page_titles",
        "Нет title",
        "Title говорит, о чём страница, и часто показывается в выдаче. Без него страница хуже ранжируется и выглядит неряшливо.",
    ),
    def(
        "title_too_short",
        Medium,
        "page_titles",
        "Короткий title",
        "Title короче 20 символов: поисковикам не хватает контекста, возможности для запросов не используются.",
    ),
    def(
        "title_too_long",
        Medium,
        "page_titles",
        "Длинный title",
        "Title длиннее 60 символов: в выдаче он обрежется, а важное слово может остаться за границей.",
    ),
    def(
        "title_multiple",
        Medium,
        "page_titles",
        "Несколько title в head",
        "В head больше одного тега title; поисковик выберет любой из них.",
    ),
    def(
        "title_duplicate",
        Medium,
        "page_titles",
        "Дубль title",
        "Одинаковый title у нескольких страниц одного языка: они конкурируют друг с другом в выдаче.",
    ),
    // ---- meta_description ----
    def(
        "description_missing",
        Medium,
        "meta_description",
        "Нет meta description",
        "Описание не влияет на ранжирование напрямую, но формирует сниппет и кликабельность.",
    ),
    def(
        "description_too_short",
        Medium,
        "meta_description",
        "Короткое meta description",
        "Описание короче 80 символов: сниппет получается невыразительным.",
    ),
    def(
        "description_too_long",
        Medium,
        "meta_description",
        "Длинное meta description",
        "Описание длиннее 160 символов: в выдаче оно обрежется.",
    ),
    def(
        "description_multiple",
        Medium,
        "meta_description",
        "Несколько meta description",
        "В head больше одного тега meta description; поисковик выберет любой из них.",
    ),
    def(
        "description_duplicate",
        Medium,
        "meta_description",
        "Дубль meta description",
        "Одинаковое описание у нескольких страниц одного языка: сниппеты неотличимы.",
    ),
    // ---- headings ----
    def(
        "h1_missing",
        Medium,
        "headings",
        "Нет H1",
        "H1 задаёт главную тему страницы; без него роботам и пользователям сложнее её понять.",
    ),
    def(
        "headings_order_invalid",
        Medium,
        "headings",
        "Нарушен порядок заголовков",
        "Уровни заголовков пропускаются (например, H1, затем H3): структура документа нарушена.",
    ),
    def(
        "h1_duplicate",
        Low,
        "headings",
        "Дубль H1",
        "Одинаковый H1 у нескольких страниц одного языка.",
    ),
    // ---- content ----
    def(
        "little_content",
        Low,
        "content",
        "Мало контента",
        "Меньше 200 слов видимого текста: такая страница малополезна пользователям и роботам.",
    ),
    def(
        "duplicate_content",
        Medium,
        "content",
        "Дубль контента",
        "Точно такой же видимый текст на нескольких страницах: поисковик выберет одну, остальные потеряют трафик.",
    ),
    def(
        "near_duplicate_content",
        Medium,
        "content",
        "Почти дубль контента",
        "Основной текст страницы (без шапки, меню, подвала и сайдбара) совпадает с другой страницей на 90% и больше: поисковик может счесть их дублями и показывать только одну.",
    ),
    def(
        "duplicate_ids",
        Low,
        "content",
        "Повторяющиеся id",
        "В документе несколько элементов с одним id: ломаются якоря, скрипты и доступность.",
    ),
    def(
        "dom_size",
        Low,
        "content",
        "Слишком большой DOM",
        "Больше 1500 элементов в документе: страница тяжелее рендерится и медленнее работает.",
    ),
    def(
        "incorrect_media_type",
        Low,
        "content",
        "Неверный тип содержимого",
        "Content-Type отсутствует или не соответствует расширению в URL.",
    ),
    def(
        "metas_in_body",
        Medium,
        "content",
        "Теги meta в body",
        "Браузеры и роботы игнорируют большинство meta-тегов вне head: они не сработают.",
    ),
    // ---- indexability ----
    def(
        "noindex",
        Low,
        "indexability",
        "Закрыта от индексации",
        "Страница закрыта директивой noindex (meta robots или X-Robots-Tag) и не попадёт в индекс.",
    ),
    def(
        "blocked_by_robots",
        Low,
        "indexability",
        "Закрыта в robots.txt",
        "Робот не может запросить страницу из-за robots.txt: её содержимое и директивы остаются невидимыми.",
    ),
    def(
        "nosnippet",
        Low,
        "indexability",
        "Запрещён сниппет",
        "Директива nosnippet или max-snippet:0 запрещает показывать текст страницы в выдаче.",
    ),
    def(
        "noimageindex",
        Low,
        "indexability",
        "Запрещена индексация картинок",
        "Директива noimageindex не даёт поисковикам индексировать картинки страницы.",
    ),
    def(
        "sitemap_noindex",
        Medium,
        "indexability",
        "Noindex-страница в sitemap",
        "Sitemap зовёт робота на страницу, которую сама же запрещает индексировать.",
    ),
    def(
        "sitemap_blocked_by_robots",
        Medium,
        "indexability",
        "Закрытая в robots.txt страница в sitemap",
        "Sitemap содержит URL, который запрещён в robots.txt.",
    ),
    def(
        "sitemap_non_canonical",
        Medium,
        "indexability",
        "Неканоническая страница в sitemap",
        "В sitemap должны быть только канонические URL; здесь canonical указывает на другой адрес.",
    ),
    // ---- canonicals ----
    def(
        "canonical_multiple",
        Medium,
        "canonicals",
        "Несколько canonical",
        "В head больше одного тега canonical: поисковик может проигнорировать все.",
    ),
    def(
        "canonical_relative",
        Medium,
        "canonicals",
        "Относительный canonical",
        "Canonical задан относительным адресом; безопаснее указывать абсолютный URL.",
    ),
    def(
        "canonical_mismatch",
        Medium,
        "canonicals",
        "Canonical в HTML и в заголовке различаются",
        "Тег canonical и заголовок Link: rel=canonical указывают на разные адреса.",
    ),
    def(
        "canonical_to_non_canonical",
        Medium,
        "canonicals",
        "Canonical на неканоническую страницу",
        "Canonical ведёт на страницу, у которой canonical указывает ещё куда-то: цепочка canonical.",
    ),
    def(
        "canonical_to_non_indexable",
        Low,
        "canonicals",
        "Canonical на неиндексируемую страницу",
        "Canonical ведёт на страницу с noindex или закрытую в robots.txt.",
    ),
    def(
        "canonical_to_redirect",
        Medium,
        "canonicals",
        "Canonical на редирект",
        "Canonical ведёт на URL, который перенаправляет дальше; укажите конечный адрес.",
    ),
    def(
        "canonical_to_error",
        Medium,
        "canonicals",
        "Canonical на страницу с ошибкой",
        "Canonical ведёт на недоступный URL (4xx, 5xx или нет ответа).",
    ),
    // ---- hreflang ----
    def(
        "hreflang_missing_x_default",
        Medium,
        "hreflang",
        "Нет hreflang x-default",
        "У страницы есть hreflang, но нет значения x-default для остальных языков.",
    ),
    def(
        "hreflang_missing_self",
        Medium,
        "hreflang",
        "Нет hreflang на саму себя",
        "Среди hreflang страницы нет ссылки на неё саму: аннотации могут быть проигнорированы.",
    ),
    def(
        "hreflang_lang_mismatch",
        Medium,
        "hreflang",
        "Язык hreflang не совпадает с lang страницы",
        "Hreflang страницы на саму себя называет другой язык, чем атрибут lang в html.",
    ),
    def(
        "hreflang_relative_url",
        Medium,
        "hreflang",
        "Относительные адреса в hreflang",
        "Hreflang должен содержать абсолютные URL.",
    ),
    def(
        "hreflang_no_return_link",
        Medium,
        "hreflang",
        "Нет обратной ссылки hreflang",
        "Страница, на которую указывает hreflang, не ссылается обратно; без взаимных ссылок аннотации игнорируются.",
    ),
    def(
        "hreflang_to_non_canonical",
        Medium,
        "hreflang",
        "Hreflang на неканоническую страницу",
        "Hreflang ведёт на страницу, у которой canonical указывает на другой URL.",
    ),
    def(
        "hreflang_to_noindex",
        Low,
        "hreflang",
        "Hreflang на неиндексируемую страницу",
        "Hreflang ведёт на страницу с noindex или закрытую в robots.txt.",
    ),
    def(
        "hreflang_to_redirect",
        Medium,
        "hreflang",
        "Hreflang на редирект",
        "Hreflang ведёт на URL, который перенаправляет дальше; укажите конечный адрес.",
    ),
    def(
        "hreflang_to_error",
        Medium,
        "hreflang",
        "Hreflang на страницу с ошибкой",
        "Hreflang ведёт на недоступный URL (4xx, 5xx или нет ответа).",
    ),
    def(
        "hreflang_multiple_langs",
        Medium,
        "hreflang",
        "Страница указана в hreflang с разными языками",
        "На один URL ведут hreflang с разными языками: поисковик не поймёт, на каком языке страница.",
    ),
    def(
        "lang_missing",
        Low,
        "hreflang",
        "Нет атрибута lang",
        "Атрибут lang помогает скринридерам и части поисковиков определить язык, на рейтинг не влияет.",
    ),
    def(
        "lang_invalid",
        Medium,
        "hreflang",
        "Некорректный атрибут lang",
        "Значение lang не является допустимым языковым тегом BCP 47 (например, ru, en-US, zh-Hans).",
    ),
    // ---- links ----
    def(
        "too_many_links",
        Low,
        "links",
        "Слишком много ссылок",
        "Больше 100 внутренних ссылок: вес делится между ними, важные ссылки теряются.",
    ),
    def(
        "internal_nofollow_links",
        Low,
        "links",
        "Внутренние nofollow-ссылки",
        "Внутренние ссылки обычно не стоит закрывать nofollow, кроме страниц, которые не нужны в индексе.",
    ),
    def(
        "external_follow_links",
        Low,
        "links",
        "Внешние ссылки без nofollow",
        "Страница передаёт вес на внешние сайты; для рекламных и пользовательских ссылок нужны nofollow, sponsored или ugc.",
    ),
    def(
        "links_to_http",
        Medium,
        "links",
        "Ссылки с HTTPS на HTTP",
        "Защищённая страница ссылается на внутренние адреса по небезопасному HTTP.",
    ),
    def(
        "dead_end",
        Low,
        "links",
        "Тупиковая страница",
        "На странице нет ни одной ссылки: роботы и пользователи не могут идти дальше.",
    ),
    def(
        "localhost_links",
        Medium,
        "links",
        "Ссылки на localhost",
        "Страница ссылается на localhost или 127.0.0.1: такие ссылки не работают у посетителей.",
    ),
    def(
        "link_with_credentials",
        Medium,
        "links",
        "Ссылки с логином в адресе",
        "В адресе ссылки логин или пароль (https://имя@сайт/): чаще всего это испорченная mailto-ссылка. Посетитель попадёт не туда или увидит запрос пароля; обход читает такую ссылку без логина.",
    ),
    def(
        "external_link_redirect",
        Low,
        "links",
        "Внешние ссылки на редиректы",
        "Страница ссылается на внешний URL, который перенаправляет дальше.",
    ),
    def(
        "external_link_broken",
        Low,
        "links",
        "Битые внешние ссылки",
        "Страница ссылается на внешний URL, который отвечает ошибкой или не отвечает.",
    ),
    def(
        "nofollow_to_indexable",
        Medium,
        "links",
        "Nofollow-ссылки на индексируемые страницы",
        "Страница закрывает nofollow внутренние ссылки на страницы, которые должны быть в индексе.",
    ),
    def(
        "incoming_follow_and_nofollow",
        Medium,
        "links",
        "Входящие ссылки и с nofollow, и без",
        "На страницу ведут внутренние ссылки и с nofollow, и без него: сигнал противоречив.",
    ),
    def(
        "internal_link_to_redirect",
        Medium,
        "links",
        "Внутренние ссылки на редиректы",
        "Страница ссылается на внутренние URL, которые перенаправляют дальше; ссылайтесь сразу на конечный адрес.",
    ),
    def(
        "internal_link_to_error",
        Medium,
        "links",
        "Битые внутренние ссылки",
        "Страница ссылается на внутренние URL, которые отвечают ошибкой или не отвечают.",
    ),
    def(
        "orphan_page",
        Low,
        "links",
        "Страница-сирота",
        "Страница есть в sitemap, но на неё не ведёт ни одна внутренняя ссылка.",
    ),
    def(
        "depth_too_high",
        Low,
        "links",
        "Большая глубина",
        "До страницы больше 4 кликов от стартовой: роботы добираются реже, вес доходит слабее.",
    ),
    def(
        "pagination_link_missing",
        Medium,
        "links",
        "Нет ссылки на соседнюю страницу пагинации",
        "Адрес из link rel=next/prev не встречается среди ссылок страницы.",
    ),
    // ---- images ----
    def(
        "image_alt_missing",
        Medium,
        "images",
        "Картинки без alt",
        "У картинок нет текста alt: он нужен доступности и помогает поисковикам понять картинку.",
    ),
    def(
        "image_alt_too_long",
        Medium,
        "images",
        "Слишком длинный alt",
        "Alt длиннее 100 символов: описание картинки превращается в текст для поисковиков.",
    ),
    def(
        "image_too_large",
        Low,
        "images",
        "Тяжёлая картинка",
        "Файл картинки больше 500 КБ: страница грузится медленнее.",
    ),
    def(
        "picture_missing_img",
        Medium,
        "images",
        "Picture без img",
        "Внутри picture нет обязательного запасного тега img: картинка не покажется в части браузеров и роботов.",
    ),
    def(
        "image_without_size",
        Low,
        "images",
        "Картинки без width и height",
        "Без размеров браузер не резервирует место, и вёрстка прыгает при загрузке.",
    ),
    // ---- url ----
    def(
        "url_underscore",
        Low,
        "url",
        "Подчёркивания в URL",
        "Поисковики считают дефис разделителем слов, а подчёркивание нет.",
    ),
    def(
        "url_space",
        Medium,
        "url",
        "Пробелы в URL",
        "Пробел в пути превращается в %20: адрес плохо читается и легко ломается при копировании.",
    ),
    def(
        "url_multiple_slashes",
        Medium,
        "url",
        "Двойные слэши в URL",
        "В пути URL повторяются слэши: один и тот же контент может открываться по нескольким адресам.",
    ),
    // ---- security ----
    def(
        "http_scheme",
        High,
        "security",
        "Страница по HTTP",
        "Страница отдаётся по HTTP вместо HTTPS: данные не защищены, браузеры показывают предупреждение.",
    ),
    def(
        "hsts_missing",
        Low,
        "security",
        "Нет заголовка HSTS",
        "Без Strict-Transport-Security браузер может открыть сайт по небезопасному HTTP.",
    ),
    def(
        "csp_missing",
        Low,
        "security",
        "Нет Content-Security-Policy",
        "Политика безопасности контента защищает от внедрения скриптов; не найдена ни в заголовке, ни в meta.",
    ),
    def(
        "x_content_type_options_missing",
        Low,
        "security",
        "Нет X-Content-Type-Options",
        "Без заголовка X-Content-Type-Options: nosniff браузер может неверно определять тип файлов.",
    ),
    def(
        "form_on_http",
        Medium,
        "security",
        "Форма на HTTP-странице",
        "Данные формы на странице без HTTPS уходят открытым текстом.",
    ),
    def(
        "insecure_form",
        Medium,
        "security",
        "Форма отправляется на HTTP",
        "Action формы ведёт на HTTP-адрес: данные можно перехватить.",
    ),
    // ---- mobile ----
    def(
        "viewport_missing",
        Low,
        "mobile",
        "Нет meta viewport",
        "Без meta viewport мобильные браузеры масштабируют страницу как десктопную.",
    ),
    // ---- structured_data ----
    def(
        "structured_data_parse_error",
        High,
        "structured_data",
        "Структурированные данные не читаются",
        "Блок JSON-LD не является корректным JSON или в нём нет @context schema.org: поисковики его не прочтут.",
    ),
    def(
        "schema_type_unknown",
        Medium,
        "structured_data",
        "Неизвестный тип Schema.org",
        "Такого @type нет в словаре Schema.org (опечатка или устаревший словарь вроде data-vocabulary.org): поисковики не поймут, что описано.",
    ),
    def(
        "schema_property_invalid",
        Medium,
        "structured_data",
        "Свойство не по Schema.org",
        "Свойства нет в Schema.org, оно не относится к указанному типу или его значение не того типа: поисковики его проигнорируют.",
    ),
    def(
        "rich_result_required_missing",
        High,
        "structured_data",
        "Нет обязательного свойства расширенного результата",
        "Google требует это свойство для расширенного результата (товар, рецепт, вакансия, хлебные крошки и т. п.): без него сниппет не расширится.",
    ),
    def(
        "rich_result_recommended_missing",
        Low,
        "structured_data",
        "Нет рекомендуемого свойства расширенного результата",
        "Google рекомендует это свойство: с ним расширенный результат полнее и заметнее в выдаче.",
    ),
    // ---- javascript ----
    def(
        "js_title_only_in_rendered",
        Medium,
        "javascript",
        "Title только после рендера",
        "В исходном HTML нет title, он появляется скриптом. Не все роботы выполняют JavaScript.",
    ),
    def(
        "js_title_changed",
        Medium,
        "javascript",
        "Title изменён скриптом",
        "Title в отрендеренной странице отличается от исходного HTML.",
    ),
    def(
        "js_description_only_in_rendered",
        Medium,
        "javascript",
        "Description только после рендера",
        "В исходном HTML нет meta description, он появляется скриптом.",
    ),
    def(
        "js_description_changed",
        Medium,
        "javascript",
        "Description изменён скриптом",
        "Meta description в отрендеренной странице отличается от исходного HTML.",
    ),
    def(
        "js_h1_only_in_rendered",
        Medium,
        "javascript",
        "H1 только после рендера",
        "В исходном HTML нет H1, он появляется скриптом.",
    ),
    def(
        "js_h1_changed",
        Medium,
        "javascript",
        "H1 изменён скриптом",
        "H1 в отрендеренной странице отличается от исходного HTML.",
    ),
    def(
        "js_canonical_only_in_rendered",
        Medium,
        "javascript",
        "Canonical только после рендера",
        "В исходном HTML нет canonical, он появляется скриптом; робот может не дождаться рендера.",
    ),
    def(
        "js_canonical_mismatch",
        Medium,
        "javascript",
        "Canonical изменён скриптом",
        "Canonical в отрендеренной странице отличается от исходного HTML: сигналы противоречат друг другу.",
    ),
    def(
        "js_noindex_only_in_raw",
        High,
        "javascript",
        "Noindex в исходном HTML, скрипт его убирает",
        "Исходный HTML закрыт noindex, а скрипт снимает директиву. Google видит noindex до рендера и может не индексировать страницу.",
    ),
    def(
        "js_nofollow_only_in_raw",
        Medium,
        "javascript",
        "Nofollow в исходном HTML, скрипт его убирает",
        "Исходный HTML закрыт nofollow, а скрипт снимает директиву: поведение для роботов зависит от рендера.",
    ),
    def(
        "js_links_only_in_rendered",
        Low,
        "javascript",
        "Ссылки только после рендера",
        "Часть ссылок появляется только после выполнения JavaScript; роботы без рендера их не увидят.",
    ),
    def(
        "js_content_only_in_rendered",
        Medium,
        "javascript",
        "Значительная часть контента только после рендера",
        "Заметная доля текста страницы появляется только после выполнения JavaScript.",
    ),
    def(
        "js_render_error",
        High,
        "javascript",
        "Страница не отрисовалась",
        "Рендер страницы не удался (ошибка загрузки или таймаут): поисковик, выполняющий JavaScript, может увидеть пустую или неполную страницу.",
    ),
    def(
        "js_render_timeout",
        Low,
        "javascript",
        "Рендер не успел устояться",
        "Страница продолжала меняться до конца времени ожидания: снимок мог попасть на середину загрузки, контент может быть неполным.",
    ),
    def(
        "js_console_errors",
        Medium,
        "javascript",
        "Ошибки JavaScript при рендере",
        "Скрипты страницы выбрасывали ошибки: часть контента или функций может не работать ни у пользователей, ни у роботов.",
    ),
    def(
        "js_failed_requests",
        Low,
        "javascript",
        "Не загрузились ресурсы при рендере",
        "Скрипты, стили или запросы данных страницы вернули ошибку или не ответили: отрисованная страница может отличаться от задуманной.",
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use super::*;

    const CATEGORIES: [&str; 15] = [
        "response_codes",
        "page_titles",
        "meta_description",
        "headings",
        "content",
        "indexability",
        "canonicals",
        "hreflang",
        "links",
        "images",
        "url",
        "security",
        "mobile",
        "structured_data",
        "javascript",
    ];

    #[test]
    fn codes_are_unique_snake_case() {
        let mut seen = HashSet::new();
        for check in CATALOG {
            assert!(seen.insert(check.code), "duplicate code {}", check.code);
            assert!(
                !check.code.is_empty()
                    && check.code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && !check.code.starts_with('_')
                    && !check.code.ends_with('_'),
                "bad code {}",
                check.code
            );
        }
    }

    #[test]
    fn every_entry_is_filled_in_and_in_a_known_tab() {
        for check in CATALOG {
            assert!(CATEGORIES.contains(&check.category), "{}: category {}", check.code, check.category);
            assert!(!check.title.is_empty() && !check.description.is_empty(), "{}", check.code);
            // Titles and descriptions are Russian.
            assert!(check.title.chars().any(|c| ('а'..='я').contains(&c.to_ascii_lowercase())), "{}", check.code);
            assert!(check.description.chars().any(|c| ('а'..='я').contains(&c.to_ascii_lowercase())), "{}", check.code);
        }
    }

    #[test]
    fn size_of_each_tab() {
        let mut per_category: BTreeMap<&str, usize> = BTreeMap::new();
        for check in CATALOG {
            *per_category.entry(check.category).or_default() += 1;
        }
        let expected = [
            ("canonicals", 7),
            ("content", 7),
            ("headings", 3),
            ("hreflang", 12),
            ("images", 5),
            ("indexability", 7),
            ("javascript", 16),
            ("links", 16),
            ("meta_description", 5),
            ("mobile", 1),
            ("page_titles", 5),
            ("response_codes", 9),
            ("security", 6),
            ("structured_data", 5),
            ("url", 3),
        ];
        assert_eq!(per_category.into_iter().collect::<Vec<_>>(), expected);
        assert_eq!(CATALOG.len(), 107);
    }

    #[test]
    fn severities_follow_seonaut_priorities() {
        let severity = |code: &str| find(code).unwrap().severity;
        // SEOnaut priority 1.
        for code in ["redirect_3xx", "client_error_4xx", "server_error_5xx", "redirect_chain", "redirect_loop", "timeout", "http_scheme"] {
            assert_eq!(severity(code), High, "{code}");
        }
        // Priority 2.
        for code in ["title_missing", "title_duplicate", "description_too_long", "hreflang_no_return_link", "slow_ttfb", "metas_in_body", "url_space"] {
            assert_eq!(severity(code), Medium, "{code}");
        }
        // Priority 3.
        for code in ["noindex", "little_content", "hsts_missing", "image_too_large", "dead_end", "orphan_page", "canonical_to_non_indexable", "viewport_missing"] {
            assert_eq!(severity(code), Low, "{code}");
        }
        assert!(High < Medium && Medium < Low, "sorting puts the most important first");
    }

    #[test]
    fn find_looks_a_code_up() {
        assert_eq!(find("title_too_long").unwrap().category, "page_titles");
        assert!(find("no_such_check").is_none());
    }

    /// A catalog entry no check ever reports would be a silent hole in a
    /// report. The sources of the checks (without their tests) must name every
    /// code.
    #[test]
    fn every_code_is_reported_by_some_check() {
        let sources = [
            include_str!("page/response.rs"),
            include_str!("page/meta.rs"),
            include_str!("page/indexing.rs"),
            include_str!("page/links.rs"),
            include_str!("page/security.rs"),
            include_str!("javascript.rs"),
            include_str!("structured.rs"),
            include_str!("site/redirects.rs"),
            include_str!("site/duplicates.rs"),
            include_str!("site/near_duplicates.rs"),
            include_str!("site/canonicals.rs"),
            include_str!("site/linking.rs"),
        ];
        let code_only: String = sources
            .iter()
            .map(|source| source.split("#[cfg(test)]").next().unwrap_or(source))
            .collect();
        for check in CATALOG {
            assert!(code_only.contains(&format!("\"{}\"", check.code)), "nothing reports {}", check.code);
        }
    }
}
