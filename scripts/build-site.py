#!/usr/bin/env python3
"""Builds the BigText website in docs/ (served by GitHub Pages).

    python scripts/build-site.py [--shots target/shots] [--no-images]

Writes docs/index.html (English), docs/ru/ and docs/uz/, sitemap.xml and 404.html. Unless
--no-images is given it also converts the app screenshots (made with
`bigtext --screenshots target/shots testdata/random-lines-10g.txt`) to WebP and draws the
logo, favicons, the social preview image and the app icons in assets/ (bigtext.ico for the
exe and installer, icon-256.png for the window). Needs Pillow; fonts from Windows if available.
"""

import argparse
import html
import json
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
BASE = "https://erkinbek.github.io/bigtext/"
REPO = "https://github.com/Erkinbek/bigtext"
RELEASES = REPO + "/releases/latest"
VERSION = "0.1.1"
UPDATED = "2026-10-24"
LANGS = ["en", "ru", "uz"]
LOCALE = {"en": "en_US", "ru": "ru_RU", "uz": "uz_UZ"}
LANG_NAME = {"en": "English", "ru": "Русский", "uz": "O'zbekcha"}
SHOTS = ["hero", "search", "compare", "hex", "json"]

# Logo: four "text lines" on a rounded square, on a 512 grid (shared by the SVG and PNGs).
LOGO_BG = "#0f172a"
LOGO_LINES = [(96, 118, 300, "#2dd4bf"), (96, 206, 416, "#e2e8f0"), (96, 294, 352, "#f59e0b"), (96, 382, 256, "#e2e8f0")]
# Pixel-snapped variants for small icon sizes (menus, title bars): x0 and (y0, y1, x1) per
# line, end-exclusive. 16 and 20 px keep three lines so each stays 2 px tall.
LOGO_HINTED = {
    16: (3, [(3, 5, 10), (7, 9, 13), (11, 13, 11)]),
    20: (4, [(4, 6, 12), (9, 11, 16), (14, 16, 13)]),
    24: (5, [(3, 6, 14), (8, 11, 20), (13, 16, 17), (18, 21, 12)]),
    32: (6, [(5, 8, 19), (11, 14, 26), (17, 20, 22), (23, 26, 16)]),
}
ICON_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def url(lang):
    return BASE if lang == "en" else f"{BASE}{lang}/"


# --- Text ----------------------------------------------------------------------------------

T = {
    "en": {
        "title": "BigText — Fast Text Editor for Huge Files (1–50+ GB)",
        "description": "Open, search and edit 1 GB, 10 GB or 50 GB text and log files instantly. "
        "Free, open-source editor for Windows, Linux and macOS that never freezes.",
        "og_title": "BigText — open 50 GB text files instantly",
        "skip": "Skip to content",
        "nav": ["Features", "Performance", "Download", "FAQ"],
        "h1": "Open huge text files instantly",
        "sub": "1 GB, 10 GB, 50 GB and beyond — no freezing, no waiting.",
        "lead": "BigText is a free, open-source desktop editor for log files, CSV exports, database dumps "
        "and any other file that is too big for ordinary editors. It never loads the whole file into "
        "memory: a 10 GB log opens in milliseconds and uses about 100 MB of RAM.",
        "cta": "Download free",
        "cta2": "Source on GitHub",
        "note": "Windows · Linux · macOS · MIT license · Written in Rust",
        "hero_alt": "BigText showing line 45,357,933 of a 10 GB log file with log highlighting; the "
        "performance overlay reports 99 MB of private memory",
        "stats": [
            ("41 ms", "to the first screen of a 50 GB file"),
            ("2.5 GB/s", "full-text search speed"),
            ("107 MB", "of RAM with a 50 GB file open"),
            ("< 3 ms", "UI frame time while indexing"),
        ],
        "features_h2": "Everything you need for giant files",
        "features_intro": "Made for logs, CSV and SQL dumps, JSON exports and any text file that makes other editors hang.",
        "features": [
            ("Opens files of any size", "Only the visible part is read, through memory-mapped windows. Background work "
             "streams the rest, and network or USB drives fall back to cached chunked reads, so a slow disk never "
             "blocks the window."),
            ("Instant go to line", "A sparse line index (8 bytes per 64 KB) is built in the background at 2.5 GB/s. "
             "Go to line and the scrollbar work immediately, even before indexing finishes."),
            ("Fast search and regex", "Plain text at 2.5 GB/s and linear-time regular expressions with case, whole-word "
             "and multiline options. Find All lists up to a million results; F3 works right away."),
            ("Replace across gigabytes", "Replace with $1 and named groups. Up to 50,000 changes are one undoable edit; "
             "bigger jobs stream to a new file: 22 million replacements in a 10 GB file took 11 s and 46 MB of RAM."),
            ("Real editing", "Type, cut, copy, paste, undo and redo, duplicate or delete lines, auto-indent — on top of "
             "a piece table, so edits never rewrite the file until you save."),
            ("Crash-safe saving", "Saving writes a temporary file, flushes it to disk and atomically replaces the "
             "original. A crash or power cut never leaves a half-written file. Optional .bak backups."),
            ("Encodings and hex view", "UTF-8, UTF-16 LE/BE, Windows-1251, Windows-1252 and ISO-8859-1 are detected "
             "automatically; LF, CRLF and CR line endings are kept or converted. Binary files open in a hex view."),
            ("Tools for logs", "Follow mode (like tail -f) for growing logs, syntax highlighting for logs, JSON, XML, "
             "CSV, SQL and INI, bookmarks and a split view of one file."),
            ("Compare huge files", "A streaming line diff of two multi-gigabyte files: 1 GB against 1 GB in 0.6 s using "
             "about 2 MB of memory. Click a difference to jump to it."),
        ],
        "gallery_h2": "Screenshots",
        "gallery": {
            "search": "Regex Find All over a 10 GB log: 166 matches in 4 s (2.5 GB/s).",
            "compare": "Comparing two 48 MB logs: differences found in 0.04 s, click one to jump to it.",
            "hex": "Hex view in the middle of a 10 GB file, at offset 0x140000000.",
            "json": "Light theme with JSON highlighting, a split view and bookmarks.",
        },
        "how_h2": "How BigText stays fast",
        "how_intro": "Ordinary editors read the entire file before they show anything. BigText reads only what is on "
        "screen and does everything else on background threads, so the window answers in under 3 ms.",
        "how": [
            ("Memory-mapped windows", "The file is mapped in 256 MB windows and only the bytes you look at are touched. "
             "Search, indexing and saving use separate streaming reads."),
            ("Sparse line index", "One 8-byte checkpoint per 64 KB: 8 MB of index for a 50 GB file. Line positions "
             "between checkpoints are counted on demand with SIMD."),
            ("Piece table", "Edits are stored as small pieces over the original file, so inserting a line into a 50 GB "
             "file costs a few bytes, and undo is instant."),
            ("Atomic save", "A temporary file next to the original, fsync, then an atomic replace (ReplaceFileW on "
             "Windows keeps attributes and permissions)."),
        ],
        "perf_h2": "Measured performance",
        "perf_intro": "Results of the built-in benchmarks on 1 GB, 10 GB and 50 GB test files on Windows 11. The method "
        "and the full results are in the README.",
        "perf_cols": ("Task", "Result"),
        "perf": [
            ("Open → first screen of text (1–50 GB)", "41–46 ms"),
            ("Jump to a line in a 50 GB file", "under 1 ms"),
            ("Build the line index", "2.2–2.7 GB/s (50 GB in about 20 s)"),
            ("Plain-text search", "2.4–2.5 GB/s"),
            ("Regex search", "2.3–2.8 GB/s with a literal part, 672 MB/s without"),
            ("Replace All in a 10 GB file", "11.3 s, 46 MB peak memory"),
            ("Memory with a 50 GB file open", "107 MB"),
            ("Start → first frame", "159–185 ms"),
        ],
        "dl_h2": "Download BigText",
        "dl_intro": "Free for personal and commercial use. No account, no telemetry, one executable.",
        "dl": [
            ("Windows 10 / 11", "Installer with “Open with” and Explorer context-menu integration, or a portable build.",
             [("Installer (.exe)", "bigtext-setup-windows-x64.exe"), ("Portable (.zip)", "bigtext-windows-x64.zip")]),
            ("Linux x64", "Portable .tar.gz for X11 and Wayland desktops.",
             [("Download .tar.gz", "bigtext-linux-x64.tar.gz")]),
            ("macOS", "Builds for Apple Silicon and Intel. The app is not signed: right-click → Open on first launch.",
             [("Apple Silicon", "bigtext-macos-arm64.tar.gz"), ("Intel", "bigtext-macos-x64.tar.gz")]),
        ],
        "dl_all": "All releases and release notes",
        "dl_source": "Build from source",
        "dl_source_text": "Install Rust from rustup.rs, then run:",
        "faq_h2": "Frequently asked questions",
        "faq": [
            ("How do I open a 50 GB text file?",
             "Install BigText and open the file with File → Open, by dragging it onto the window, or with right-click → "
             "“Open with BigText”. The first screen appears in about 40 ms whatever the size; scrolling and go to line "
             "work while the line index is built in the background."),
            ("Does BigText load the whole file into memory?",
             "No. It maps only the part on screen and streams everything else in the background. With a 50 GB file "
             "open it uses about 107 MB of RAM, most of which is the user interface itself."),
            ("Can I edit and save very large files?",
             "Yes. Edits are kept in a piece table over the original file, so typing is instant at any size. Saving "
             "streams the result to a temporary file and replaces the original atomically, so the original is never "
             "damaged if something goes wrong."),
            ("How fast is search in huge files?",
             "Plain text is searched at 2.4–2.5 GB/s and typical regular expressions at 2.3–2.8 GB/s, so a 10 GB log takes "
             "about 4 seconds. F3 finds the next match immediately, without waiting for a full search."),
            ("Can it follow a growing log file like tail -f?",
             "Yes. View → Follow keeps the view at the end of the file and indexes new lines as they arrive. BigText "
             "also notices when a file changes on disk and offers to reload it."),
            ("Can it open a file that is one huge line, like minified JSON?",
             "Yes. Lines longer than 64 KB are shown as soft-broken rows, so even a single 2 GB line scrolls smoothly."),
            ("Which encodings and line endings are supported?",
             "UTF-8, UTF-16 LE/BE (with or without BOM), Windows-1251, Windows-1252 and ISO-8859-1, detected "
             "automatically and switchable with File → Reopen with encoding. LF, CRLF, CR and mixed line endings are "
             "detected and can be converted on save."),
            ("Is BigText free, and which systems does it run on?",
             "BigText is free and open source under the MIT license. Windows 10 and 11 is the main, fully tested "
             "platform; Linux and macOS (Apple Silicon and Intel) builds are made and tested by CI on every change."),
        ],
        "made_by": "Made by",
        "license": "MIT license",
        "lang_label": "Language",
        "nf_title": "Page not found",
        "nf_text": "This page does not exist.",
        "nf_home": "Go to the BigText home page",
    },
    "ru": {
        "title": "BigText — редактор больших текстовых файлов (1–50+ ГБ)",
        "description": "Мгновенно открывайте, ищите и редактируйте текстовые и лог-файлы на 1, 10 и 50 ГБ. "
        "Бесплатный редактор с открытым кодом для Windows, Linux и macOS.",
        "og_title": "BigText — открывайте файлы на 50 ГБ мгновенно",
        "skip": "Перейти к содержимому",
        "nav": ["Возможности", "Скорость", "Скачать", "Вопросы"],
        "h1": "Мгновенно открывайте огромные текстовые файлы",
        "sub": "1 ГБ, 10 ГБ, 50 ГБ и больше — без зависаний и ожидания.",
        "lead": "BigText — бесплатный редактор с открытым кодом для лог-файлов, выгрузок CSV, дампов баз данных и "
        "любых файлов, которые слишком велики для обычных редакторов. Он никогда не загружает файл в память "
        "целиком: лог на 10 ГБ открывается за миллисекунды и занимает около 100 МБ ОЗУ.",
        "cta": "Скачать бесплатно",
        "cta2": "Исходный код на GitHub",
        "note": "Windows · Linux · macOS · Лицензия MIT · Написан на Rust",
        "hero_alt": "BigText показывает строку 45 357 933 лог-файла на 10 ГБ с подсветкой; панель производительности "
        "показывает 99 МБ занятой памяти",
        "stats": [
            ("41 мс", "до первого экрана файла на 50 ГБ"),
            ("2,5 ГБ/с", "скорость полнотекстового поиска"),
            ("107 МБ", "ОЗУ с открытым файлом на 50 ГБ"),
            ("< 3 мс", "время кадра интерфейса во время индексации"),
        ],
        "features_h2": "Всё для работы с гигантскими файлами",
        "features_intro": "Для логов, дампов CSV и SQL, выгрузок JSON и любых текстовых файлов, на которых другие редакторы зависают.",
        "features": [
            ("Файлы любого размера", "Читается только видимая часть — через окна отображения файла в память. Фоновые "
             "задачи читают остальное потоком, а для сетевых и USB-дисков есть кэшированное чтение блоками, поэтому "
             "медленный диск не блокирует окно."),
            ("Мгновенный переход к строке", "Разреженный индекс строк (8 байт на 64 КБ) строится в фоне со скоростью "
             "2,5 ГБ/с. Переход к строке и полоса прокрутки работают сразу, ещё до конца индексации."),
            ("Быстрый поиск и регулярные выражения", "Обычный текст — 2,5 ГБ/с, регулярные выражения за линейное время, "
             "учёт регистра, целые слова, многострочный режим. «Найти все» выводит до миллиона результатов, F3 "
             "работает сразу."),
            ("Замена в гигабайтах", "Замена с $1 и именованными группами. До 50 000 замен — одна отменяемая правка; "
             "большие задачи пишутся в новый файл потоком: 22 млн замен в файле на 10 ГБ — 11 с и 46 МБ ОЗУ."),
            ("Полноценное редактирование", "Ввод, вырезание, копирование, вставка, отмена и повтор, дублирование и "
             "удаление строк, автоотступ — поверх таблицы фрагментов, так что файл не переписывается до сохранения."),
            ("Безопасное сохранение", "Сохранение пишет временный файл, сбрасывает его на диск и атомарно заменяет "
             "оригинал. Сбой или отключение питания не оставят полузаписанный файл. Резервная копия .bak по желанию."),
            ("Кодировки и hex-режим", "UTF-8, UTF-16 LE/BE, Windows-1251, Windows-1252 и ISO-8859-1 определяются "
             "автоматически; концы строк LF, CRLF и CR сохраняются или конвертируются. Двоичные файлы — в hex-режиме."),
            ("Инструменты для логов", "Режим слежения (как tail -f) за растущими логами, подсветка логов, JSON, XML, "
             "CSV, SQL и INI, закладки и разделённый вид одного файла."),
            ("Сравнение огромных файлов", "Потоковое построчное сравнение двух многогигабайтных файлов: 1 ГБ с 1 ГБ "
             "за 0,6 с и около 2 МБ памяти. Щелчок по отличию переходит к нему."),
        ],
        "gallery_h2": "Скриншоты",
        "gallery": {
            "search": "«Найти все» по регулярному выражению в логе на 10 ГБ: 166 совпадений за 4 с (2,5 ГБ/с).",
            "compare": "Сравнение двух логов по 48 МБ: отличия найдены за 0,04 с, щелчок переходит к строке.",
            "hex": "Hex-режим в середине файла на 10 ГБ, смещение 0x140000000.",
            "json": "Светлая тема, подсветка JSON, разделённый вид и закладки.",
        },
        "how_h2": "Почему BigText такой быстрый",
        "how_intro": "Обычные редакторы читают весь файл, прежде чем что-то показать. BigText читает только то, что "
        "на экране, а всё остальное делает в фоновых потоках, поэтому окно отвечает быстрее чем за 3 мс.",
        "how": [
            ("Окна отображения в память", "Файл отображается окнами по 256 МБ, и читаются только байты, которые вы "
             "видите. Поиск, индексация и сохранение используют отдельное потоковое чтение."),
            ("Разреженный индекс строк", "Одна 8-байтовая контрольная точка на 64 КБ: 8 МБ индекса для файла на 50 ГБ. "
             "Строки между точками считаются по запросу с помощью SIMD."),
            ("Таблица фрагментов", "Правки хранятся как небольшие фрагменты поверх исходного файла: вставка строки "
             "в файл на 50 ГБ стоит несколько байт, а отмена мгновенна."),
            ("Атомарное сохранение", "Временный файл рядом с оригиналом, fsync, затем атомарная замена (ReplaceFileW "
             "в Windows сохраняет атрибуты и права доступа)."),
        ],
        "perf_h2": "Измеренная производительность",
        "perf_intro": "Результаты встроенных бенчмарков на тестовых файлах 1, 10 и 50 ГБ в Windows 11. Методика и "
        "полные результаты — в README.",
        "perf_cols": ("Задача", "Результат"),
        "perf": [
            ("Открытие → первый экран текста (1–50 ГБ)", "41–46 мс"),
            ("Переход к строке в файле на 50 ГБ", "меньше 1 мс"),
            ("Построение индекса строк", "2,2–2,7 ГБ/с (50 ГБ примерно за 20 с)"),
            ("Поиск обычного текста", "2,4–2,5 ГБ/с"),
            ("Поиск по регулярному выражению", "2,3–2,8 ГБ/с с литеральной частью, 672 МБ/с без неё"),
            ("«Заменить все» в файле на 10 ГБ", "11,3 с, пик памяти 46 МБ"),
            ("Память с открытым файлом на 50 ГБ", "107 МБ"),
            ("Запуск → первый кадр", "159–185 мс"),
        ],
        "dl_h2": "Скачать BigText",
        "dl_intro": "Бесплатно для личного и коммерческого использования. Без аккаунта, без телеметрии, один исполняемый файл.",
        "dl": [
            ("Windows 10 / 11", "Установщик с пунктом «Открыть с помощью» и контекстным меню Проводника или портативная версия.",
             [("Установщик (.exe)", "bigtext-setup-windows-x64.exe"), ("Портативная (.zip)", "bigtext-windows-x64.zip")]),
            ("Linux x64", "Портативный архив .tar.gz для X11 и Wayland.",
             [("Скачать .tar.gz", "bigtext-linux-x64.tar.gz")]),
            ("macOS", "Сборки для Apple Silicon и Intel. Приложение не подписано: при первом запуске — правый щелчок → Открыть.",
             [("Apple Silicon", "bigtext-macos-arm64.tar.gz"), ("Intel", "bigtext-macos-x64.tar.gz")]),
        ],
        "dl_all": "Все релизы и примечания к ним",
        "dl_source": "Сборка из исходников",
        "dl_source_text": "Установите Rust с rustup.rs и выполните:",
        "faq_h2": "Частые вопросы",
        "faq": [
            ("Как открыть текстовый файл на 50 ГБ?",
             "Установите BigText и откройте файл через Файл → Открыть, перетащив его в окно или через правый щелчок → "
             "«Открыть в BigText». Первый экран появляется примерно за 40 мс при любом размере; прокрутка и переход к "
             "строке работают, пока индекс строится в фоне."),
            ("BigText загружает весь файл в память?",
             "Нет. Он отображает в память только видимую часть, а остальное читает потоком в фоне. С открытым файлом на "
             "50 ГБ он занимает около 107 МБ ОЗУ, и большая часть этого — сам интерфейс."),
            ("Можно ли редактировать и сохранять очень большие файлы?",
             "Да. Правки хранятся в таблице фрагментов поверх исходного файла, поэтому ввод мгновенный при любом размере. "
             "При сохранении результат пишется во временный файл и атомарно заменяет оригинал, так что оригинал не "
             "пострадает, если что-то пойдёт не так."),
            ("Насколько быстрый поиск в огромных файлах?",
             "Обычный текст ищется со скоростью 2,4–2,5 ГБ/с, типичные регулярные выражения — 2,3–2,8 ГБ/с, то есть лог "
             "на 10 ГБ просматривается примерно за 4 секунды. F3 находит следующее совпадение сразу, без полного поиска."),
            ("Можно ли следить за растущим логом, как tail -f?",
             "Да. Вид → Следить держит конец файла на экране и индексирует новые строки по мере появления. BigText "
             "также замечает изменения файла на диске и предлагает перезагрузить его."),
            ("Откроет ли он файл из одной огромной строки, например минифицированный JSON?",
             "Да. Строки длиннее 64 КБ показываются с мягкими переносами, поэтому даже одна строка на 2 ГБ прокручивается плавно."),
            ("Какие кодировки и концы строк поддерживаются?",
             "UTF-8, UTF-16 LE/BE (с BOM и без), Windows-1251, Windows-1252 и ISO-8859-1 определяются автоматически и "
             "переключаются через Файл → Открыть заново в кодировке. Концы строк LF, CRLF, CR и смешанные распознаются "
             "и могут быть преобразованы при сохранении."),
            ("BigText бесплатный? На каких системах он работает?",
             "BigText бесплатный, с открытым кодом под лицензией MIT. Основная, полностью протестированная платформа — "
             "Windows 10 и 11; сборки для Linux и macOS (Apple Silicon и Intel) собираются и тестируются в CI при каждом изменении."),
        ],
        "made_by": "Автор:",
        "license": "Лицензия MIT",
        "lang_label": "Язык",
        "nf_title": "Страница не найдена",
        "nf_text": "Такой страницы нет.",
        "nf_home": "На главную BigText",
    },
    "uz": {
        "title": "BigText — katta matn fayllari uchun tezkor muharrir (1–50+ GB)",
        "description": "1, 10 va 50 GB hajmdagi matn va log fayllarni bir zumda oching, qidiring va tahrirlang. "
        "Windows, Linux va macOS uchun bepul, ochiq kodli muharrir.",
        "og_title": "BigText — 50 GB matn fayllarini bir zumda oching",
        "skip": "Asosiy qismga o'tish",
        "nav": ["Imkoniyatlar", "Tezlik", "Yuklab olish", "Savollar"],
        "h1": "Ulkan matn fayllarini bir zumda oching",
        "sub": "1 GB, 10 GB, 50 GB va undan katta — qotishsiz va kutishsiz.",
        "lead": "BigText — log fayllar, CSV eksportlar, ma'lumotlar bazasi damplari va oddiy muharrirlar uchun juda "
        "katta bo'lgan har qanday fayl uchun bepul, ochiq kodli muharrir. U faylni hech qachon xotiraga to'liq "
        "yuklamaydi: 10 GB log millisekundlarda ochiladi va taxminan 100 MB operativ xotira ishlatadi.",
        "cta": "Bepul yuklab olish",
        "cta2": "GitHub'dagi manba kodi",
        "note": "Windows · Linux · macOS · MIT litsenziyasi · Rust tilida yozilgan",
        "hero_alt": "BigText 10 GB log faylning 45 357 933-satrini rangli ko'rinishda ko'rsatmoqda; unumdorlik "
        "paneli 99 MB xotira ishlatilganini ko'rsatadi",
        "stats": [
            ("41 ms", "50 GB faylning birinchi ekranigacha"),
            ("2,5 GB/s", "to'liq matnli qidiruv tezligi"),
            ("107 MB", "50 GB fayl ochiq turganda xotira"),
            ("< 3 ms", "indekslash paytida interfeys kadri"),
        ],
        "features_h2": "Ulkan fayllar bilan ishlash uchun hamma narsa",
        "features_intro": "Loglar, CSV va SQL damplar, JSON eksportlar va boshqa muharrirlarni qotiradigan har qanday matn fayli uchun.",
        "features": [
            ("Istalgan hajmdagi fayllar", "Faqat ko'rinib turgan qism xotiraga akslantirilgan oynalar orqali o'qiladi. "
             "Fon vazifalari qolganini oqim bilan o'qiydi, tarmoq va USB disklar uchun esa keshli bloklab o'qish bor — "
             "sekin disk oynani hech qachon to'xtatmaydi."),
            ("Satrga bir zumda o'tish", "Siyrak satr indeksi (har 64 KB ga 8 bayt) fonda 2,5 GB/s tezlikda quriladi. "
             "Satrga o'tish va aylantirish paneli indekslash tugashidan oldin ham darhol ishlaydi."),
            ("Tezkor qidiruv va regex", "Oddiy matn 2,5 GB/s, chiziqli vaqtli muntazam ifodalar, katta-kichik harf, "
             "butun so'z va ko'p satrli rejimlar. «Hammasini topish» milliongacha natijani ko'rsatadi, F3 darhol ishlaydi."),
            ("Gigabaytlab almashtirish", "$1 va nomlangan guruhlar bilan almashtirish. 50 000 tagacha o'zgarish — bitta "
             "bekor qilinadigan tahrir; kattaroq ishlar yangi faylga oqim bilan yoziladi: 10 GB faylda 22 million "
             "almashtirish 11 soniya va 46 MB xotira oldi."),
            ("To'laqonli tahrirlash", "Yozish, kesish, nusxalash, qo'yish, bekor qilish va qaytarish, satrni nusxalash "
             "yoki o'chirish, avtomatik chekinish — bo'laklar jadvali ustida, shuning uchun saqlamaguncha fayl qayta yozilmaydi."),
            ("Xavfsiz saqlash", "Saqlashda vaqtinchalik fayl yoziladi, diskka to'liq tushiriladi va asl fayl atomar "
             "almashtiriladi. Nosozlik yoki tok o'chishi chala yozilgan fayl qoldirmaydi. Ixtiyoriy .bak zaxira nusxasi."),
            ("Kodirovkalar va hex ko'rinish", "UTF-8, UTF-16 LE/BE, Windows-1251, Windows-1252 va ISO-8859-1 avtomatik "
             "aniqlanadi; LF, CRLF va CR satr oxirlari saqlanadi yoki o'zgartiriladi. Ikkilik fayllar hex ko'rinishda ochiladi."),
            ("Loglar uchun vositalar", "O'sib borayotgan loglarni kuzatish rejimi (tail -f kabi), log, JSON, XML, CSV, "
             "SQL va INI uchun rangli ko'rinish, xatcho'plar va bitta faylni ikki panelda ko'rish."),
            ("Ulkan fayllarni taqqoslash", "Ikki ko'p gigabaytli faylni oqim bilan satrma-satr taqqoslash: 1 GB ga "
             "1 GB — 0,6 soniya va taxminan 2 MB xotira. Farqni bosib, unga o'tasiz."),
        ],
        "gallery_h2": "Skrinshotlar",
        "gallery": {
            "search": "10 GB logda regex bo'yicha «Hammasini topish»: 4 soniyada 166 ta moslik (2,5 GB/s).",
            "compare": "Ikki 48 MB logni taqqoslash: farqlar 0,04 soniyada topildi, bosib satrga o'tish mumkin.",
            "hex": "10 GB faylning o'rtasida hex ko'rinish, 0x140000000 siljish.",
            "json": "Yorug' mavzu, JSON rang berish, ikki panelli ko'rinish va xatcho'plar.",
        },
        "how_h2": "BigText nega bunchalik tez",
        "how_intro": "Oddiy muharrirlar biror narsa ko'rsatishdan oldin butun faylni o'qiydi. BigText faqat ekrandagini "
        "o'qiydi, qolgan hamma ishni fon oqimlarida bajaradi, shuning uchun oyna 3 ms dan tez javob beradi.",
        "how": [
            ("Xotiraga akslantirilgan oynalar", "Fayl 256 MB lik oynalar bilan akslantiriladi va faqat siz ko'rayotgan "
             "baytlar o'qiladi. Qidiruv, indekslash va saqlash alohida oqimli o'qishdan foydalanadi."),
            ("Siyrak satr indeksi", "Har 64 KB ga bitta 8 baytli nazorat nuqtasi: 50 GB fayl uchun 8 MB indeks. "
             "Nuqtalar orasidagi satrlar kerak bo'lganda SIMD bilan sanaladi."),
            ("Bo'laklar jadvali", "Tahrirlar asl fayl ustidagi kichik bo'laklar sifatida saqlanadi: 50 GB faylga satr "
             "qo'shish bir necha baytga tushadi, bekor qilish esa bir zumda."),
            ("Atomar saqlash", "Asl fayl yonida vaqtinchalik fayl, fsync, so'ng atomar almashtirish (Windows'da "
             "ReplaceFileW atributlar va ruxsatlarni saqlaydi)."),
        ],
        "perf_h2": "O'lchangan unumdorlik",
        "perf_intro": "Windows 11 da 1, 10 va 50 GB test fayllarida ichki benchmarklar natijalari. Usul va to'liq "
        "natijalar README faylida.",
        "perf_cols": ("Vazifa", "Natija"),
        "perf": [
            ("Ochish → matnning birinchi ekrani (1–50 GB)", "41–46 ms"),
            ("50 GB faylda satrga o'tish", "1 ms dan kam"),
            ("Satr indeksini qurish", "2,2–2,7 GB/s (50 GB taxminan 20 soniyada)"),
            ("Oddiy matn qidiruvi", "2,4–2,5 GB/s"),
            ("Regex qidiruvi", "literal qismi bilan 2,3–2,8 GB/s, usiz 672 MB/s"),
            ("10 GB faylda «Hammasini almashtirish»", "11,3 soniya, eng ko'pi 46 MB xotira"),
            ("50 GB fayl ochiq turganda xotira", "107 MB"),
            ("Ishga tushish → birinchi kadr", "159–185 ms"),
        ],
        "dl_h2": "BigText'ni yuklab olish",
        "dl_intro": "Shaxsiy va tijorat maqsadlarida bepul. Akkaunt yo'q, telemetriya yo'q, bitta ishga tushiriladigan fayl.",
        "dl": [
            ("Windows 10 / 11", "«Open with» va Explorer kontekst menyusi bilan o'rnatuvchi yoki portativ versiya.",
             [("O'rnatuvchi (.exe)", "bigtext-setup-windows-x64.exe"), ("Portativ (.zip)", "bigtext-windows-x64.zip")]),
            ("Linux x64", "X11 va Wayland uchun portativ .tar.gz arxiv.",
             [(".tar.gz yuklab olish", "bigtext-linux-x64.tar.gz")]),
            ("macOS", "Apple Silicon va Intel uchun yig'malar. Ilova imzolanmagan: birinchi ishga tushirishda o'ng tugma → Open.",
             [("Apple Silicon", "bigtext-macos-arm64.tar.gz"), ("Intel", "bigtext-macos-x64.tar.gz")]),
        ],
        "dl_all": "Barcha relizlar va ular haqida",
        "dl_source": "Manba koddan yig'ish",
        "dl_source_text": "rustup.rs dan Rust'ni o'rnating, so'ng bajaring:",
        "faq_h2": "Ko'p beriladigan savollar",
        "faq": [
            ("50 GB matn faylini qanday ochaman?",
             "BigText'ni o'rnating va faylni File → Open orqali, oynaga sudrab tashlab yoki o'ng tugma → «Open with "
             "BigText» bilan oching. Hajmidan qat'i nazar birinchi ekran taxminan 40 ms da chiqadi; indeks fonda "
             "qurilayotganda ham aylantirish va satrga o'tish ishlaydi."),
            ("BigText butun faylni xotiraga yuklaydimi?",
             "Yo'q. U faqat ekrandagi qismni akslantiradi, qolganini fonda oqim bilan o'qiydi. 50 GB fayl ochiq "
             "turganda taxminan 107 MB operativ xotira ishlatadi, uning katta qismi interfeysning o'zi."),
            ("Juda katta fayllarni tahrirlab saqlash mumkinmi?",
             "Ha. Tahrirlar asl fayl ustidagi bo'laklar jadvalida saqlanadi, shuning uchun istalgan hajmda yozish bir "
             "zumda. Saqlashda natija vaqtinchalik faylga yoziladi va asl faylni atomar almashtiradi — biror narsa "
             "noto'g'ri ketsa ham asl fayl buzilmaydi."),
            ("Ulkan fayllarda qidiruv qanchalik tez?",
             "Oddiy matn 2,4–2,5 GB/s, odatiy muntazam ifodalar 2,3–2,8 GB/s tezlikda qidiriladi — 10 GB log taxminan "
             "4 soniyada ko'rib chiqiladi. F3 to'liq qidiruvni kutmasdan keyingi moslikni darhol topadi."),
            ("O'sib borayotgan logni tail -f kabi kuzatsa bo'ladimi?",
             "Ha. View → Follow faylning oxirini ekranda ushlab turadi va yangi satrlarni kelishi bilan indekslaydi. "
             "BigText fayl diskda o'zgarganini ham sezadi va qayta yuklashni taklif qiladi."),
            ("Bitta ulkan satrdan iborat faylni, masalan siqilgan JSON'ni ochadimi?",
             "Ha. 64 KB dan uzun satrlar yumshoq bo'lib ko'rsatiladi, shuning uchun 2 GB lik bitta satr ham silliq aylanadi."),
            ("Qaysi kodirovkalar va satr oxirlari qo'llab-quvvatlanadi?",
             "UTF-8, UTF-16 LE/BE (BOM bilan yoki usiz), Windows-1251, Windows-1252 va ISO-8859-1 avtomatik aniqlanadi "
             "va File → Reopen with encoding orqali almashtiriladi. LF, CRLF, CR va aralash satr oxirlari aniqlanadi "
             "va saqlashda o'zgartirilishi mumkin."),
            ("BigText bepulmi va qaysi tizimlarda ishlaydi?",
             "BigText bepul va MIT litsenziyasi ostida ochiq kodli. Asosiy, to'liq sinovdan o'tgan platforma — Windows 10 "
             "va 11; Linux va macOS (Apple Silicon va Intel) yig'malari har bir o'zgarishda CI tomonidan yig'iladi va sinanadi."),
        ],
        "made_by": "Muallif:",
        "license": "MIT litsenziyasi",
        "lang_label": "Til",
        "nf_title": "Sahifa topilmadi",
        "nf_text": "Bunday sahifa mavjud emas.",
        "nf_home": "BigText bosh sahifasiga o'tish",
    },
}

AUTHOR = "Erkin Pardayev"
AUTHOR_LINKS = [
    ("GitHub", "https://github.com/Erkinbek"),
    ("Telegram", "https://t.me/ErkinPardayev"),
    ("E-mail", "mailto:erkin.pardayev@gmail.com"),
]


# --- HTML ----------------------------------------------------------------------------------

def e(s):
    return html.escape(s, quote=True)


def head(lang, t, path_prefix, canonical, noindex=False):
    og_alt = "".join(f'  <meta property="og:locale:alternate" content="{LOCALE[l]}">\n' for l in LANGS if l != lang)
    if noindex:
        # Error pages: no canonical or language alternates, only a noindex.
        links = '  <meta name="robots" content="noindex">\n'
    else:
        links = f'  <link rel="canonical" href="{canonical}">\n'
        links += "".join(f'  <link rel="alternate" hreflang="{l}" href="{url(l)}">\n' for l in LANGS)
        links += f'  <link rel="alternate" hreflang="x-default" href="{BASE}">\n'
    return f"""<!doctype html>
<html lang="{lang}">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{e(t['title'])}</title>
  <meta name="description" content="{e(t['description'])}">
{links}  <meta name="theme-color" content="#0b0f17">
  <meta name="color-scheme" content="dark light">
  <meta name="author" content="{AUTHOR}">
  <meta property="og:type" content="website">
  <meta property="og:site_name" content="BigText">
  <meta property="og:title" content="{e(t['og_title'])}">
  <meta property="og:description" content="{e(t['description'])}">
  <meta property="og:url" content="{canonical}">
  <meta property="og:image" content="{BASE}img/og-image.png">
  <meta property="og:image:width" content="1280">
  <meta property="og:image:height" content="640">
  <meta property="og:image:alt" content="{e(t['og_title'])}">
  <meta property="og:locale" content="{LOCALE[lang]}">
{og_alt}  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="{e(t['og_title'])}">
  <meta name="twitter:description" content="{e(t['description'])}">
  <meta name="twitter:image" content="{BASE}img/og-image.png">
  <link rel="icon" href="{path_prefix}favicon.ico" sizes="48x48">
  <link rel="icon" href="{path_prefix}img/logo.svg" type="image/svg+xml">
  <link rel="apple-touch-icon" href="{path_prefix}img/apple-touch-icon.png">
  <link rel="manifest" href="{path_prefix}site.webmanifest">
  <link rel="stylesheet" href="{path_prefix}assets/style.css">
"""


def json_ld(lang, t):
    page = url(lang)
    graph = [
        {
            "@type": "SoftwareApplication",
            "@id": BASE + "#app",
            "name": "BigText",
            "alternateName": "BigText large file editor",
            "description": t["description"],
            "url": page,
            "inLanguage": lang,
            "applicationCategory": "DeveloperApplication",
            "applicationSubCategory": "Text editor",
            "operatingSystem": "Windows 10, Windows 11, Linux, macOS",
            "softwareVersion": VERSION,
            "downloadUrl": RELEASES,
            "installUrl": RELEASES,
            "license": "https://opensource.org/licenses/MIT",
            "isAccessibleForFree": True,
            "offers": {"@type": "Offer", "price": "0", "priceCurrency": "USD"},
            "image": BASE + "img/og-image.png",
            "screenshot": [BASE + f"img/{s}.webp" for s in SHOTS],
            "featureList": [f[0] for f in t["features"]],
            "codeRepository": REPO,
            "programmingLanguage": "Rust",
            "author": {"@id": BASE + "#author"},
        },
        {
            "@type": "Person",
            "@id": BASE + "#author",
            "name": AUTHOR,
            "url": "https://github.com/Erkinbek",
            "email": "mailto:erkin.pardayev@gmail.com",
            "sameAs": ["https://github.com/Erkinbek", "https://t.me/ErkinPardayev"],
        },
        {
            "@type": "WebPage",
            "@id": page + "#webpage",
            "url": page,
            "name": t["title"],
            "description": t["description"],
            "inLanguage": lang,
            "dateModified": UPDATED,
            "about": {"@id": BASE + "#app"},
            "primaryImageOfPage": BASE + "img/hero.webp",
        },
        {
            "@type": "FAQPage",
            "@id": page + "#faq",
            "inLanguage": lang,
            "mainEntity": [
                {"@type": "Question", "name": q, "acceptedAnswer": {"@type": "Answer", "text": a}} for q, a in t["faq"]
            ],
        },
    ]
    data = json.dumps({"@context": "https://schema.org", "@graph": graph}, ensure_ascii=False, indent=1)
    data = data.replace("</", "<\\/")
    return f'  <script type="application/ld+json">\n{data}\n  </script>\n'


def logo_svg_inline(size):
    lines = "".join(
        f'<rect x="{x}" y="{y}" width="{w - x}" height="44" rx="22" fill="{c}"/>' for x, y, w, c in LOGO_LINES
    )
    return (f'<svg width="{size}" height="{size}" viewBox="0 0 512 512" aria-hidden="true" focusable="false">'
            f'<rect width="512" height="512" rx="112" fill="{LOGO_BG}"/>{lines}</svg>')


def lang_switch(lang, t, p):
    items = []
    for l in LANGS:
        href = p + ("" if l == "en" else f"{l}/")
        cur = ' aria-current="true"' if l == lang else ""
        items.append(f'<a href="{href}" hreflang="{l}" lang="{l}"{cur}>{l.upper()}</a>')
    return f'<nav class="langs" aria-label="{e(t["lang_label"])}">{"".join(items)}</nav>'


def header(lang, t, p, home=""):
    """Site header; `home` prefixes the section links on pages other than the home page."""
    n = t["nav"]
    links = "".join(f'<a href="{home}#{a}">{e(b)}</a>' for a, b in zip(["features", "performance", "download", "faq"], n))
    return f"""<a class="skip" href="#main">{e(t['skip'])}</a>
<header class="top">
  <div class="wrap bar">
    <a class="brand" href="{p}{'' if lang == 'en' else lang + '/'}">{logo_svg_inline(28)}<span>BigText</span></a>
    <nav class="links" aria-label="BigText">{links}<a href="{REPO}">GitHub</a></nav>
    {lang_switch(lang, t, p)}
  </div>
</header>
"""


def picture(p, name, alt, w=1200, h=800, eager=False):
    load = 'fetchpriority="high" decoding="async"' if eager else 'loading="lazy" decoding="async"'
    return f'<img src="{p}img/{name}.webp" width="{w}" height="{h}" alt="{e(alt)}" {load}>'


def page(lang):
    t = T[lang]
    p = "" if lang == "en" else "../"
    out = [head(lang, t, p, url(lang)), json_ld(lang, t), "</head>\n<body>\n", header(lang, t, p)]
    stats = "".join(f'<li><strong>{e(v)}</strong><span>{e(l)}</span></li>' for v, l in t["stats"])
    out.append(f"""<main id="main">
<section class="hero">
  <div class="wrap">
    <h1>{e(t['h1'])}</h1>
    <p class="sub">{e(t['sub'])}</p>
    <p class="lead">{e(t['lead'])}</p>
    <div class="cta">
      <a class="btn primary" href="#download">{e(t['cta'])}</a>
      <a class="btn" href="{REPO}">{e(t['cta2'])}</a>
    </div>
    <p class="note">{e(t['note'])}</p>
    <figure class="shot hero-shot">{picture(p, 'hero', t['hero_alt'], eager=True)}</figure>
    <ul class="stats">{stats}</ul>
  </div>
</section>
""")
    cards = "".join(f'<article class="card"><h3>{e(a)}</h3><p>{e(b)}</p></article>' for a, b in t["features"])
    out.append(f"""<section id="features" class="section">
  <div class="wrap">
    <h2>{e(t['features_h2'])}</h2>
    <p class="intro">{e(t['features_intro'])}</p>
    <div class="grid cards">{cards}</div>
  </div>
</section>
""")
    figs = "".join(
        f'<figure class="shot">{picture(p, k, v)}<figcaption>{e(v)}</figcaption></figure>' for k, v in t["gallery"].items()
    )
    out.append(f"""<section id="screenshots" class="section alt">
  <div class="wrap">
    <h2>{e(t['gallery_h2'])}</h2>
    <div class="grid gallery">{figs}</div>
  </div>
</section>
""")
    how = "".join(f'<article class="card"><h3>{e(a)}</h3><p>{e(b)}</p></article>' for a, b in t["how"])
    out.append(f"""<section id="how" class="section">
  <div class="wrap">
    <h2>{e(t['how_h2'])}</h2>
    <p class="intro">{e(t['how_intro'])}</p>
    <div class="grid how">{how}</div>
  </div>
</section>
""")
    rows = "".join(f"<tr><th scope=\"row\">{e(a)}</th><td>{e(b)}</td></tr>" for a, b in t["perf"])
    c1, c2 = t["perf_cols"]
    out.append(f"""<section id="performance" class="section alt">
  <div class="wrap">
    <h2>{e(t['perf_h2'])}</h2>
    <p class="intro">{e(t['perf_intro'])} <a href="{REPO}#measured-performance">README</a></p>
    <div class="table"><table>
      <thead><tr><th scope="col">{e(c1)}</th><th scope="col">{e(c2)}</th></tr></thead>
      <tbody>{rows}</tbody>
    </table></div>
  </div>
</section>
""")
    def buttons(items):
        return "".join(
            f'<a class="btn{" primary" if i == 0 else ""}" href="{REPO}/releases/latest/download/{f}">{e(label)}</a>'
            for i, (label, f) in enumerate(items)
        )

    dl = "".join(
        f'<article class="card dl"><h3>{e(a)}</h3><p>{e(b)}</p><div class="btns">{buttons(items)}</div></article>'
        for a, b, items in t["dl"]
    )
    out.append(f"""<section id="download" class="section">
  <div class="wrap">
    <h2>{e(t['dl_h2'])}</h2>
    <p class="intro">{e(t['dl_intro'])}</p>
    <div class="grid downloads">{dl}</div>
    <p class="all"><a href="{REPO}/releases">{e(t['dl_all'])}</a></p>
    <h3 class="src">{e(t['dl_source'])}</h3>
    <p>{e(t['dl_source_text'])}</p>
    <pre><code>git clone {REPO}.git
cd bigtext
cargo build --release</code></pre>
  </div>
</section>
""")
    faq = "".join(f"<details><summary>{e(q)}</summary><p>{e(a)}</p></details>" for q, a in t["faq"])
    out.append(f"""<section id="faq" class="section alt">
  <div class="wrap narrow">
    <h2>{e(t['faq_h2'])}</h2>
    <div class="faq">{faq}</div>
  </div>
</section>
</main>
""")
    out.append(footer(lang, t, p))
    out.append("</body>\n</html>\n")
    return "".join(out)


def footer(lang, t, p):
    links = " · ".join(f'<a href="{h}">{e(n)}</a>' for n, h in AUTHOR_LINKS)
    langs = " · ".join(
        f'<a href="{p}{"" if l == "en" else l + "/"}" hreflang="{l}" lang="{l}">{LANG_NAME[l]}</a>' for l in LANGS
    )
    return f"""<footer class="foot">
  <div class="wrap">
    <p>{e(t['made_by'])} <strong>{AUTHOR}</strong> · {links}</p>
    <p><a href="{REPO}">github.com/Erkinbek/bigtext</a> · <a href="{REPO}/blob/main/LICENSE">{e(t['license'])}</a> · v{VERSION}</p>
    <p class="langs-foot">{langs}</p>
  </div>
</footer>
"""


def not_found():
    t = T["en"]
    p = "/bigtext/"
    body = f"""<main id="main" class="section">
  <div class="wrap narrow">
    <h1>{e(t['nf_title'])}</h1>
    <p>{e(t['nf_text'])} <a href="{p}">{e(t['nf_home'])}</a> ·
      <a href="{p}ru/">{e(T['ru']['nf_home'])}</a> · <a href="{p}uz/">{e(T['uz']['nf_home'])}</a></p>
  </div>
</main>
"""
    t404 = dict(t, title=t["nf_title"] + " — BigText")
    return head("en", t404, p, BASE, noindex=True) + "</head>\n<body>\n" + header("en", t, p, home=p) + body + footer("en", t, p) + "</body>\n</html>\n"


def sitemap():
    alts = "".join(f'\n    <xhtml:link rel="alternate" hreflang="{l}" href="{url(l)}"/>' for l in LANGS)
    alts += f'\n    <xhtml:link rel="alternate" hreflang="x-default" href="{BASE}"/>'
    urls = "".join(
        f"""  <url>
    <loc>{url(l)}</loc>
    <lastmod>{UPDATED}</lastmod>{alts}
  </url>
""" for l in LANGS)
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">
{urls}</urlset>
"""


def manifest():
    return json.dumps({
        "name": "BigText",
        "short_name": "BigText",
        "description": T["en"]["description"],
        "start_url": "/bigtext/",
        "display": "browser",
        "background_color": "#0b0f17",
        "theme_color": "#0b0f17",
        "icons": [
            {"src": "img/icon-192.png", "sizes": "192x192", "type": "image/png"},
            {"src": "img/icon-512.png", "sizes": "512x512", "type": "image/png"},
        ],
    }, indent=2) + "\n"


# --- Images --------------------------------------------------------------------------------

def font(names, size):
    from PIL import ImageFont
    for n in names:
        for d in [os.environ.get("WINDIR", "C:/Windows") + "/Fonts", "/usr/share/fonts/truetype/dejavu"]:
            f = Path(d) / n
            if f.exists():
                return ImageFont.truetype(str(f), size)
    return ImageFont.load_default(size)


def draw_logo(size):
    from PIL import Image, ImageDraw
    k = 4  # supersample for smooth edges
    s = size * k / 512
    im = Image.new("RGBA", (size * k, size * k), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.rounded_rectangle([0, 0, size * k - 1, size * k - 1], radius=112 * s, fill=LOGO_BG)
    hinted = LOGO_HINTED.get(size)
    if not hinted:
        for x, y, w, c in LOGO_LINES:
            d.rounded_rectangle([x * s, y * s, w * s, (y + 44) * s], radius=22 * s, fill=c)
    im = im.resize((size, size), Image.LANCZOS)
    if hinted:
        # Lines on whole pixels stay sharp at 16-32 px, where anti-aliased ones blur.
        x0, rows = hinted
        d = ImageDraw.Draw(im)
        for (y0, y1, x1), (_, _, _, c) in zip(rows, LOGO_LINES):
            d.rectangle([x0, y0, x1 - 1, y1 - 1], fill=c)
    return im


def write_ico(path, sizes):
    """Multi-size .ico with an individually drawn image per size (BMP entries for old APIs)."""
    imgs = [draw_logo(n) for n in sizes]
    imgs[-1].save(path, format="ICO", sizes=[(n, n) for n in sizes], append_images=imgs[:-1], bitmap_format="bmp")


def build_app_icons():
    """Icon embedded in bigtext.exe (build.rs) and the installer, plus the window icon PNG."""
    assets = ROOT / "assets"
    assets.mkdir(exist_ok=True)
    write_ico(assets / "bigtext.ico", ICON_SIZES)
    draw_logo(256).save(assets / "icon-256.png", optimize=True)


def build_images(shots):
    from PIL import Image, ImageDraw, ImageFilter
    img = DOCS / "img"
    img.mkdir(parents=True, exist_ok=True)
    for name in SHOTS:
        src = shots / f"{name}.png"
        if not src.exists():
            raise SystemExit(f"missing {src}; run: bigtext --screenshots {shots} testdata/random-lines-10g.txt")
        im = Image.open(src).convert("RGB")
        # Lossless is both smaller and sharper than lossy WebP for flat UI screenshots.
        im.save(img / f"{name}.webp", "WEBP", lossless=True, method=6)
    (img / "logo.svg").write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">'
        f'<rect width="512" height="512" rx="112" fill="{LOGO_BG}"/>'
        + "".join(f'<rect x="{x}" y="{y}" width="{w - x}" height="44" rx="22" fill="{c}"/>' for x, y, w, c in LOGO_LINES)
        + "</svg>\n", encoding="utf-8")
    draw_logo(180).save(img / "apple-touch-icon.png", optimize=True)
    draw_logo(192).save(img / "icon-192.png", optimize=True)
    draw_logo(512).save(img / "icon-512.png", optimize=True)
    write_ico(DOCS / "favicon.ico", [16, 32, 48])
    build_app_icons()

    # Social preview (Open Graph / Twitter / GitHub): 1280x640.
    W, H = 1280, 640
    og = Image.new("RGB", (W, H), "#0b0f17")
    d = ImageDraw.Draw(og)
    for y in range(H):  # subtle vertical gradient
        c = int(11 + 10 * y / H)
        d.line([(0, y), (W, y)], fill=(c, c + 4, c + 12))
    hero = Image.open(shots / "hero.png").convert("RGB")
    shot = hero.resize((760, 507), Image.LANCZOS)
    x0, y0 = 560, 96
    shadow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle([x0 - 4, y0 + 10, x0 + 760, y0 + 530], radius=18, fill=(0, 0, 0, 170))
    og.paste(shadow.filter(ImageFilter.GaussianBlur(18)), (0, 0), shadow.filter(ImageFilter.GaussianBlur(18)))
    mask = Image.new("L", shot.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, shot.size[0] - 1, shot.size[1] - 1], radius=14, fill=255)
    og.paste(shot, (x0, y0), mask)
    d = ImageDraw.Draw(og)
    d.rounded_rectangle([x0, y0, x0 + 759, y0 + 506], radius=14, outline="#334155", width=2)
    og.paste(draw_logo(96), (64, 72), draw_logo(96))
    d.text((176, 80), "BigText", font=font(["segoeuib.ttf", "DejaVuSans-Bold.ttf"], 64), fill="#f8fafc")
    big = font(["segoeuib.ttf", "DejaVuSans-Bold.ttf"], 46)
    for i, line in enumerate(["Open 50 GB text", "files instantly"]):
        d.text((64, 214 + i * 58), line, font=big, fill="#f8fafc")
    small = font(["segoeui.ttf", "DejaVuSans.ttf"], 26)
    for i, line in enumerate(["Search at 2.5 GB/s · edit · atomic save", "~100 MB of RAM for a 10 GB file"]):
        d.text((64, 350 + i * 38), line, font=small, fill="#94a3b8")
    chip = font(["segoeuib.ttf", "DejaVuSans-Bold.ttf"], 22)
    x = 64
    for label in ["Free · MIT", "Windows · Linux · macOS", "Rust"]:
        w = d.textlength(label, font=chip) + 32
        if x + w > x0 - 24:  # keep clear of the screenshot
            break
        d.rounded_rectangle([x, 470, x + w, 512], radius=21, fill="#132a2a", outline="#2dd4bf", width=2)
        d.text((x + 16, 476), label, font=chip, fill="#5eead4")
        x += w + 12
    d.text((64, 560), "erkinbek.github.io/bigtext", font=small, fill="#64748b")
    og.save(img / "og-image.png", optimize=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shots", default=str(ROOT / "target" / "shots"))
    ap.add_argument("--no-images", action="store_true")
    a = ap.parse_args()
    if not a.no_images:
        build_images(Path(a.shots))
    for lang in LANGS:
        d = DOCS if lang == "en" else DOCS / lang
        d.mkdir(parents=True, exist_ok=True)
        (d / "index.html").write_text(page(lang), encoding="utf-8", newline="\n")
    (DOCS / "404.html").write_text(not_found(), encoding="utf-8", newline="\n")
    (DOCS / "sitemap.xml").write_text(sitemap(), encoding="utf-8", newline="\n")
    (DOCS / "site.webmanifest").write_text(manifest(), encoding="utf-8", newline="\n")
    (DOCS / ".nojekyll").write_text("", encoding="utf-8")
    print("site written to", DOCS)


if __name__ == "__main__":
    main()
