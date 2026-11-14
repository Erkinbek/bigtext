"""Guides on the website: step-by-step pages for the files people search for help with (a CSV
past Excel's row limit, a 10 GB log, one table out of a mysqldump). Used by build-site.py.

Text marked {{like this}} is shown as code.
"""

LABELS = {
    "en": {
        "guides_h2": "Guides",
        "guides_intro": "Step by step, for the files people ask about most.",
        "steps": "Step by step",
        "tips": "Good to know",
        "cta": "Download BigText free",
        "more": "More guides",
        "home": "BigText home",
    },
    "ru": {
        "guides_h2": "Руководства",
        "guides_intro": "По шагам — для файлов, о которых спрашивают чаще всего.",
        "steps": "По шагам",
        "tips": "Полезно знать",
        "cta": "Скачать BigText бесплатно",
        "more": "Другие руководства",
        "home": "Главная BigText",
    },
    "uz": {
        "guides_h2": "Qo'llanmalar",
        "guides_intro": "Eng ko'p so'raladigan fayllar uchun qadamma-qadam.",
        "steps": "Qadamma-qadam",
        "tips": "Bilib qo'ying",
        "cta": "BigText'ni bepul yuklab olish",
        "more": "Boshqa qo'llanmalar",
        "home": "BigText bosh sahifasi",
    },
}

GUIDES = {
    "en": [
        {
            "slug": "open-csv-larger-than-excel",
            "title": "Open a CSV With More Than 1,048,576 Rows (Excel's Limit) — BigText",
            "description": "Excel stops at 1,048,576 rows. Open the whole CSV, any size, as a table in BigText, free: "
                           "sort, filter, group and pivot it, or split it into parts Excel can load.",
            "h1": "Open a CSV file bigger than Excel's 1,048,576 rows",
            "card": "Excel stops at 1,048,576 rows. Open the whole file as a table, analyse it, or split it for Excel.",
            "intro": [
                "Excel's grid ends at 1,048,576 rows. With a bigger CSV it warns that the data set is too large for "
                "the grid, and the rows past the limit are lost if you save. Exports of logs, orders or database "
                "tables are often bigger than that.",
                "BigText opens a CSV of any size as a table at once and never loads it into memory, so a file with "
                "50 million rows scrolls as fast as one with fifty. It is free and runs on Windows, Linux and macOS.",
            ],
            "steps": [
                ("Open the file", "Download BigText and open the .csv or .tsv with File → Open, or drag it onto the "
                 "window. It opens as a table: the delimiter (comma, semicolon, tab or pipe) is found from the first "
                 "lines and the first row becomes a pinned header. {{Ctrl+T}} switches between the table and the text."),
                ("Find your way around", "Scroll, press {{Ctrl+G}} to go to a row, or {{Ctrl+F}} to search the whole "
                 "file; Find All lists every match with its line. Nothing waits for the file to load, because it is "
                 "never loaded."),
                ("Analyse it without Excel", "Right-click a column header: Statistics… (rows, empty and distinct "
                 "values, sum, mean, min, max and the most frequent values), Filter rows by this column, Group rows by "
                 "this column… with sums of another column, and Pivot by this column…. Sorting works on files up to 1 GB."),
                ("Or make it fit Excel", "File → Split file… cuts the CSV into {{name.part001.csv}}, {{name.part002.csv}}… of a given number of lines, say 1,000,000, always between records (a quoted field with line breaks stays whole), and Repeat the header row in every part puts the column names at the top of each one. To keep only some rows, filter them (Filter rows by conditions…) and save the result as a new, smaller CSV."),
            ],
            "tips": [
                "View → Check table rows… lists the records with the wrong number of fields, a stray quote or text in "
                "a number column, with their line numbers.",
                "The same tools work from scripts with {{bigtext-cli}}, for example {{bigtext-cli pivot}}.",
            ],
        },
        {
            "slug": "open-large-log-file",
            "title": "Open a 10 GB Log File Without Freezing — BigText",
            "description": "Open a log of any size at once and search, filter and follow it without the editor "
                           "freezing: jump to a time, filter by level, a grep-like filter view, tail -F. Free, for "
                           "Windows, Linux and macOS.",
            "h1": "Open a 10 GB log file without freezing",
            "card": "Open it at once, jump to a time, filter by level, keep the matching lines and follow it live.",
            "intro": [
                "Notepad++ warns that a file over 2 GB may take minutes to open, VS Code keeps the whole file in "
                "memory, and both can freeze on a big log. BigText reads only what is on screen, so a 10 GB or 50 GB "
                "log opens at once and uses about 100 MB of memory.",
            ],
            "steps": [
                ("Open it", "Use File → Open, drag the file onto the window, or run {{bigtext app.log}} in a terminal "
                 "({{bigtext app.log:120}} opens it at line 120). Logs compressed with .gz or .zst open directly."),
                ("Jump around", "{{Ctrl+End}} goes to the last line at once. Go to line ({{Ctrl+G}}) works before the "
                 "line index is finished; until then line numbers carry a {{~}}. Go to time ({{Ctrl+Shift+T}}) takes "
                 "{{14:32:05}} and lands on the first line at that time."),
                ("Find the errors", "The level bar above a log filters the ERROR, WARN, INFO and DEBUG lines, and "
                 "{{F8}} jumps to the next error. {{Ctrl+F}} searches the whole file, with regular expressions if you "
                 "like, and Find All lists the matches."),
                ("Keep only what matters", "The filter view shows only the lines that match, or only those that "
                 "don't, like grep over the whole file, and saves them to a new file."),
                ("Follow it live", "Follow mode ({{bigtext -f app.log}}, or Follow (tail) file changes in the command palette) keeps "
                 "adding new lines like {{tail -F}} and survives log rotation. A log on a server opens over SSH with "
                 "your keys: {{bigtext -f user@server:/var/log/syslog}}."),
            ],
            "tips": [
                "Several logs can be merged by time into one view, or two logs scrolled together by time.",
                "Search, the filter and saving run in the background with a progress bar and Cancel, so the window "
                "keeps answering.",
            ],
        },
        {
            "slug": "extract-table-from-mysqldump",
            "title": "Extract One Table From a mysqldump File — BigText",
            "description": "List the tables of a multi-gigabyte SQL dump and save the one you need as a file that "
                           "restores on its own, without loading the dump into memory. Free, for Windows, Linux "
                           "and macOS.",
            "h1": "Extract one table from a mysqldump file",
            "card": "List the tables of a huge SQL dump and save the one you need as a file that restores on its "
                    "own.",
            "intro": [
                "A full mysqldump of a database can be tens of gigabytes, and often you need one table from it: to "
                "restore it, to check its data or to send it to someone. BigText opens the dump at once without "
                "loading it into memory, lists its tables in one pass and writes the one you need to its own file, "
                "with what it needs to restore.",
            ],
            "steps": [
                ("Open the dump", "Open the .sql file with File → Open or by dragging it onto the window. A "
                 ".sql.gz dump opens directly too."),
                ("List the tables", "File → SQL dump tables… reads the dump once, about a gigabyte a second, and "
                 "lists its tables with their rows and sizes, in the dump's order or largest first. Type part of a "
                 "name in the filter to find a table among hundreds."),
                ("Check the table", "Click a table to go to its CREATE TABLE statement and see its columns."),
                ("Save it to a file", "Right-click the table and choose Save this table as… The file gets the "
                 "table's DROP and CREATE statements, its rows, keys, indexes and triggers, and the dump's opening "
                 "and closing settings, so it restores on its own: {{mysql shop < orders.sql}}."),
            ],
            "tips": [
                "Several tables: check them in the list and press Save the checked tables as one file… To look at "
                "a table without saving it, use Open this table in a new tab.",
                "Split into a file per table… writes a file for each table, one for what comes before the tables "
                "and one for the views, routines and foreign keys after them. In the order of their names they "
                "restore the whole dump: {{for f in shop.*.sql; do mysql shop < \"$f\"; done}}.",
                "pg_dump files in plain format and SQLite {{.dump}} files work the same way. In scripts: "
                "{{bigtext-cli sqldump shop.sql extract orders -o orders.sql}}.",
            ],
        },
    ],
    "ru": [
        {
            "slug": "open-csv-larger-than-excel",
            "title": "Как открыть CSV больше 1 048 576 строк (лимит Excel) — BigText",
            "description": "Excel останавливается на 1 048 576 строках. Откройте весь CSV любого размера как таблицу в "
                           "BigText бесплатно: сортируйте, фильтруйте, группируйте или разделите его на части для Excel.",
            "h1": "Как открыть CSV-файл больше 1 048 576 строк Excel",
            "card": "Excel останавливается на 1 048 576 строках. Откройте весь файл как таблицу, проанализируйте его "
                    "или разделите для Excel.",
            "intro": [
                "Сетка Excel заканчивается на 1 048 576 строке. С CSV большего размера он предупреждает, что набор "
                "данных слишком велик, и строки за пределом пропадут при сохранении. Выгрузки логов, заказов или "
                "таблиц баз данных часто больше.",
                "BigText сразу открывает CSV любого размера как таблицу и никогда не загружает его в память: файл на "
                "50 миллионов строк прокручивается так же быстро, как на пятьдесят. Он бесплатный и работает в "
                "Windows, Linux и macOS.",
            ],
            "steps": [
                ("Откройте файл", "Скачайте BigText и откройте .csv или .tsv через «Файл → Открыть» или "
                 "перетащите его в окно. Он откроется как таблица: разделитель (запятая, точка с запятой, табуляция "
                 "или вертикальная черта) определяется по первым строкам, первая строка становится закреплённым "
                 "заголовком. {{Ctrl+T}} переключает между таблицей и текстом."),
                ("Перемещайтесь по файлу", "Прокручивайте, нажмите {{Ctrl+G}}, чтобы перейти к строке, или "
                 "{{Ctrl+F}} для поиска по всему файлу; «Найти все» покажет каждое совпадение с номером строки. Ждать "
                 "загрузки не нужно: файл не загружается вовсе."),
                ("Анализируйте без Excel", "Щёлкните правой кнопкой по заголовку столбца: «Статистика…» (строки, "
                 "пустые и различные значения, сумма, среднее, минимум, максимум и частые значения), фильтр строк по "
                 "столбцу, «Сгруппировать строки по этому столбцу…» с суммами другого столбца и сводная таблица. Сортировка "
                 "работает для файлов до 1 ГБ."),
                ("Или подгоните под Excel", "«Файл → Разделить файл…» режет CSV на {{name.part001.csv}}, {{name.part002.csv}}… по заданному числу строк, например 1 000 000, всегда между записями (поле в кавычках с переводами строк не разрывается), а «Повторять строку заголовка в каждой части» ставит названия столбцов в начало каждой. Чтобы оставить только часть строк, отфильтруйте их («Фильтр строк по условиям…») и сохраните результат как новый, меньший CSV."),
            ],
            "tips": [
                "«Вид → Проверить строки таблицы…» находит записи с неверным числом полей, лишней кавычкой или "
                "текстом в числовом столбце — с номерами строк.",
                "Те же инструменты есть для скриптов в {{bigtext-cli}}, например {{bigtext-cli pivot}}.",
            ],
        },
        {
            "slug": "open-large-log-file",
            "title": "Как открыть лог-файл на 10 ГБ без зависаний — BigText",
            "description": "Откройте лог любого размера сразу, ищите, фильтруйте и следите за ним без зависаний: "
                           "переход ко времени, фильтр по уровням, фильтр как grep, tail -F. Бесплатно, для Windows, "
                           "Linux и macOS.",
            "h1": "Как открыть лог-файл на 10 ГБ без зависаний",
            "card": "Откройте сразу, перейдите ко времени, отфильтруйте по уровню, оставьте нужные строки и "
                    "следите вживую.",
            "intro": [
                "Notepad++ предупреждает, что файл больше 2 ГБ может открываться несколько минут, VS Code держит весь "
                "файл в памяти, и оба могут зависнуть на большом логе. BigText читает только то, что на экране, "
                "поэтому лог на 10 или 50 ГБ открывается сразу и занимает около 100 МБ памяти.",
            ],
            "steps": [
                ("Откройте его", "Через «Файл → Открыть», перетаскиванием в окно или командой {{bigtext app.log}} в "
                 "терминале ({{bigtext app.log:120}} откроет на 120-й строке). Логи в .gz и .zst открываются напрямую."),
                ("Перемещайтесь", "{{Ctrl+End}} сразу переходит к последней строке. Переход к строке ({{Ctrl+G}}) "
                 "работает ещё до окончания индексации; до тех пор номера строк идут со знаком {{~}}. Переход ко "
                 "времени ({{Ctrl+Shift+T}}) принимает {{14:32:05}} и ставит на первую строку с этим временем."),
                ("Найдите ошибки", "Панель уровней над логом фильтрует строки ERROR, WARN, INFO и DEBUG, а {{F8}} "
                 "переходит к следующей ошибке. {{Ctrl+F}} ищет по всему файлу, в том числе по регулярным выражениям, "
                 "«Найти все» показывает список совпадений."),
                ("Оставьте только нужное", "Режим фильтра показывает только совпадающие строки или, наоборот, "
                 "только несовпадающие — как grep по всему файлу — и сохраняет их в новый файл."),
                ("Следите вживую", "Режим слежения ({{bigtext -f app.log}} или «Следить за файлом (tail)» в палитре команд) добавляет "
                 "новые строки как {{tail -F}} и переживает ротацию логов. Лог на сервере открывается по SSH с вашими "
                 "ключами: {{bigtext -f user@server:/var/log/syslog}}."),
            ],
            "tips": [
                "Несколько логов можно объединить по времени в одно окно или прокручивать два лога вместе по времени.",
                "Поиск, фильтр и сохранение идут в фоне с индикатором и кнопкой отмены, окно не перестаёт отвечать.",
            ],
        },
        {
            "slug": "extract-table-from-mysqldump",
            "title": "Как извлечь одну таблицу из дампа mysqldump — BigText",
            "description": "Получите список таблиц SQL-дампа на несколько гигабайт и сохраните нужную в файл, "
                           "который восстанавливается сам по себе, не загружая дамп в память. Бесплатно, для "
                           "Windows, Linux и macOS.",
            "h1": "Как извлечь одну таблицу из дампа mysqldump",
            "card": "Получите список таблиц огромного SQL-дампа и сохраните нужную в файл, который "
                    "восстанавливается сам по себе.",
            "intro": [
                "Полный mysqldump базы может весить десятки гигабайт, а нужна из него часто одна таблица: "
                "восстановить её, проверить данные или отправить коллеге. BigText открывает дамп сразу, не "
                "загружая его в память, за один проход составляет список таблиц и записывает нужную в отдельный "
                "файл со всем, что нужно для восстановления.",
            ],
            "steps": [
                ("Откройте дамп", "Откройте .sql через «Файл → Открыть» или перетащите в окно. Дамп в .sql.gz тоже "
                 "открывается напрямую."),
                ("Получите список таблиц", "«Файл → Таблицы дампа SQL…» читает дамп один раз, примерно гигабайт в "
                 "секунду, и выводит его таблицы с числом строк и размером — в порядке дампа или по убыванию "
                 "размера. Чтобы найти таблицу среди сотен, введите часть имени в фильтр."),
                ("Проверьте таблицу", "Щёлкните таблицу, чтобы перейти к её CREATE TABLE и увидеть столбцы."),
                ("Сохраните её в файл", "Щёлкните таблицу правой кнопкой и выберите «Сохранить эту таблицу как…». "
                 "В файл попадут DROP и CREATE таблицы, её строки, ключи, индексы и триггеры, а также начальные и "
                 "конечные настройки дампа, поэтому он восстанавливается сам по себе: {{mysql shop < orders.sql}}."),
            ],
            "tips": [
                "Несколько таблиц: отметьте их в списке и нажмите «Сохранить отмеченные таблицы в один файл…». "
                "«Открыть эту таблицу в новой вкладке» показывает таблицу, не сохраняя её.",
                "«Разбить на файл для каждой таблицы…» записывает файл на каждую таблицу, один файл для того, что "
                "идёт до таблиц, и один для представлений, процедур и внешних ключей после них. В порядке имён они "
                "восстанавливают весь дамп: {{for f in shop.*.sql; do mysql shop < \"$f\"; done}}.",
                "Так же работают дампы pg_dump в текстовом формате и файлы {{.dump}} из SQLite. В скриптах: "
                "{{bigtext-cli sqldump shop.sql extract orders -o orders.sql}}.",
            ],
        },
    ],
    "uz": [
        {
            "slug": "open-csv-larger-than-excel",
            "title": "Excel'ning 1 048 576 qatoridan katta CSV'ni ochish — BigText",
            "description": "Excel 1 048 576 qatorda to'xtaydi. Har qanday hajmdagi CSV'ni BigText'da jadval sifatida "
                           "bepul oching: saralang, filtrlang, guruhlang yoki Excel uchun bo'laklarga bo'ling.",
            "h1": "Excel'ning 1 048 576 qatoridan katta CSV faylni ochish",
            "card": "Excel 1 048 576 qatorda to'xtaydi. Butun faylni jadval sifatida oching, tahlil qiling yoki "
                    "Excel uchun bo'ling.",
            "intro": [
                "Excel jadvali 1 048 576-qatorda tugaydi. Undan katta CSV'da u ma'lumotlar juda kattaligini "
                "ogohlantiradi, chegaradan keyingi qatorlar esa saqlashda yo'qoladi. Log, buyurtma yoki ma'lumotlar "
                "bazasi jadvallarining eksportlari ko'pincha bundan katta bo'ladi.",
                "BigText har qanday hajmdagi CSV'ni darhol jadval sifatida ochadi va uni hech qachon xotiraga "
                "yuklamaydi: 50 million qatorli fayl ham ellik qatorli fayldek tez aylanadi. U bepul va Windows, "
                "Linux, macOS'da ishlaydi.",
            ],
            "steps": [
                ("Faylni oching", "BigText'ni yuklab oling va .csv yoki .tsv faylni «Fayl → Ochish» orqali yoki "
                 "oynaga tortib oching. U jadval bo'lib ochiladi: ajratuvchi (vergul, nuqtali vergul, tab yoki "
                 "vertikal chiziq) birinchi qatorlardan aniqlanadi, birinchi qator esa qotirilgan sarlavha bo'ladi. "
                 "{{Ctrl+T}} jadval va matn o'rtasida almashtiradi."),
                ("Fayl bo'ylab yuring", "Aylantiring, qatorga o'tish uchun {{Ctrl+G}}, butun fayl bo'yicha qidirish "
                 "uchun {{Ctrl+F}} bosing; «Hammasini topish» har bir moslikni qator raqami bilan ko'rsatadi. "
                 "Yuklanishini kutish shart emas: fayl umuman yuklanmaydi."),
                ("Excel'siz tahlil qiling", "Ustun sarlavhasini o'ng tugma bilan bosing: «Statistika…» (qatorlar, bo'sh "
                 "va turli qiymatlar, yig'indi, o'rtacha, eng kichik, eng katta va eng ko'p uchraydigan qiymatlar), "
                 "ustun bo'yicha filtr, boshqa ustun yig'indisi bilan guruhlash va pivot jadval. Saralash 1 GB "
                 "gacha bo'lgan fayllarda ishlaydi."),
                ("Yoki Excel'ga moslang", "«Fayl → Faylni bo'lish…» CSV'ni {{name.part001.csv}}, {{name.part002.csv}}… bo'laklarga, masalan har biri 1 000 000 qatordan, doim yozuvlar orasida bo'ladi (ichida satr oxiri bor qo'shtirnoqli maydon bo'linmaydi), «Sarlavha qatorini har bir qismda takrorlash» esa ustun nomlarini har bir bo'lak boshiga qo'yadi. Faqat ayrim qatorlar kerak bo'lsa, ularni filtrlang («Qatorlarni shartlar bo'yicha filtrlash…») va natijani yangi, kichikroq CSV qilib saqlang."),
            ],
            "tips": [
                "«Ko'rinish → Jadval qatorlarini tekshirish…» maydonlar soni noto'g'ri, ortiqcha qo'shtirnoqli yoki "
                "son ustunida matn bor yozuvlarni qator raqamlari bilan ko'rsatadi.",
                "Xuddi shu vositalar skriptlar uchun {{bigtext-cli}} da bor, masalan {{bigtext-cli pivot}}.",
            ],
        },
        {
            "slug": "open-large-log-file",
            "title": "10 GB log faylni qotmasdan ochish — BigText",
            "description": "Har qanday hajmdagi logni darhol oching, muharrir qotmasdan qidiring, filtrlang va kuzating: "
                           "vaqtga o'tish, daraja bo'yicha filtr, grep kabi filtr, tail -F. Bepul, Windows, Linux va "
                           "macOS uchun.",
            "h1": "10 GB log faylni qotmasdan ochish",
            "card": "Darhol oching, vaqtga o'ting, darajasi bo'yicha filtrlang, keraklisini qoldiring va jonli kuzating.",
            "intro": [
                "Notepad++ 2 GB dan katta fayl ochilishi bir necha daqiqa olishi mumkinligini ogohlantiradi, VS Code "
                "butun faylni xotirada ushlaydi, ikkalasi ham katta logda qotib qolishi mumkin. BigText faqat "
                "ekrandagini o'qiydi, shuning uchun 10 yoki 50 GB lik log darhol ochiladi va taxminan 100 MB xotira "
                "oladi.",
            ],
            "steps": [
                ("Oching", "«Fayl → Ochish», oynaga tortish yoki terminalda {{bigtext app.log}} buyrug'i bilan "
                 "({{bigtext app.log:120}} 120-qatorda ochadi). .gz va .zst dagi loglar to'g'ridan-to'g'ri ochiladi."),
                ("Fayl bo'ylab yuring", "{{Ctrl+End}} darhol oxirgi qatorga o'tadi. Qatorga o'tish ({{Ctrl+G}}) "
                 "indekslash tugamasdan ishlaydi; ungacha qator raqamlari oldida {{~}} turadi. Vaqtga o'tish "
                 "({{Ctrl+Shift+T}}) {{14:32:05}} ni qabul qiladi va o'sha vaqtdagi birinchi qatorga qo'yadi."),
                ("Xatolarni toping", "Log ustidagi darajalar paneli ERROR, WARN, INFO va DEBUG qatorlarini filtrlaydi, "
                 "{{F8}} esa keyingi xatoga o'tadi. {{Ctrl+F}} butun fayl bo'yicha qidiradi, xohlasangiz regex bilan, "
                 "«Hammasini topish» mosliklar ro'yxatini beradi."),
                ("Faqat keraklisini qoldiring", "Filtr rejimi faqat mos keladigan yoki aksincha, mos kelmaydigan "
                 "qatorlarni ko'rsatadi, xuddi butun faylda grep kabi, va ularni yangi faylga saqlaydi."),
                ("Jonli kuzating", "Kuzatish rejimi ({{bigtext -f app.log}} yoki buyruqlar palitrasidagi «Faylni kuzatish (tail)») "
                 "yangi qatorlarni {{tail -F}} kabi qo'shib boradi va log almashtirilganda ham davom etadi. "
                 "Serverdagi log SSH orqali kalitlaringiz bilan ochiladi: {{bigtext -f user@server:/var/log/syslog}}."),
            ],
            "tips": [
                "Bir nechta logni vaqt bo'yicha bitta oynaga birlashtirish yoki ikki logni vaqt bo'yicha birga "
                "aylantirish mumkin.",
                "Qidiruv, filtr va saqlash fonda, progress va bekor qilish tugmasi bilan ishlaydi, oyna javob berishdan "
                "to'xtamaydi.",
            ],
        },
        {
            "slug": "extract-table-from-mysqldump",
            "title": "mysqldump faylidan bitta jadvalni ajratib olish — BigText",
            "description": "Bir necha gigabaytli SQL dampdagi jadvallar ro'yxatini oling va keraklisini o'zi "
                           "alohida tiklanadigan faylga saqlang, dampni xotiraga yuklamasdan. Bepul, Windows, "
                           "Linux va macOS uchun.",
            "h1": "mysqldump faylidan bitta jadvalni ajratib olish",
            "card": "Ulkan SQL dampdagi jadvallar ro'yxatini oling va keraklisini o'zi alohida tiklanadigan faylga "
                    "saqlang.",
            "intro": [
                "Ma'lumotlar bazasining to'liq mysqldump'i o'nlab gigabayt bo'lishi mumkin, undan esa ko'pincha "
                "bitta jadval kerak: tiklash, ma'lumotini tekshirish yoki hamkasbga yuborish uchun. BigText dampni "
                "xotiraga yuklamasdan darhol ochadi, bir o'tishda jadvallar ro'yxatini tuzadi va keraklisini "
                "tiklash uchun zarur hamma narsasi bilan alohida faylga yozadi.",
            ],
            "steps": [
                ("Dampni oching", ".sql faylni «Fayl → Ochish» orqali yoki oynaga tortib oching. .sql.gz damp ham "
                 "to'g'ridan-to'g'ri ochiladi."),
                ("Jadvallar ro'yxatini oling", "«Fayl → SQL damp jadvallari…» dampni bir marta, soniyasiga "
                 "taxminan bir gigabayt tezlikda o'qiydi va jadvallarini qatorlar soni hamda hajmi bilan "
                 "ko'rsatadi: damp tartibida yoki avval eng kattalari. Yuzlab jadval orasidan keraklisini topish "
                 "uchun filtrga nomining bir qismini yozing."),
                ("Jadvalni tekshiring", "Jadvalni bossangiz, uning CREATE TABLE qatoriga o'tasiz va ustunlarini "
                 "ko'rasiz."),
                ("Uni faylga saqlang", "Jadvalni sichqonchaning o'ng tugmasi bilan bosing va «Bu jadvalni "
                 "saqlash…» ni tanlang. Faylga jadvalning DROP va CREATE buyruqlari, qatorlari, kalitlari, "
                 "indekslari va triggerlari hamda dampning boshidagi va oxiridagi sozlamalar yoziladi, shuning "
                 "uchun u o'zi alohida tiklanadi: {{mysql shop < orders.sql}}."),
            ],
            "tips": [
                "Bir nechta jadval: ularni ro'yxatda belgilang va «Belgilangan jadvallarni bitta faylga saqlash…» "
                "ni bosing. «Bu jadvalni yangi varaqda ochish» jadvalni saqlamasdan ko'rsatadi.",
                "«Har jadvalni alohida faylga bo'lish…» har bir jadval uchun bitta fayl, jadvallardan oldingi qism "
                "uchun bitta va ulardan keyingi view, protsedura va tashqi kalitlar uchun bitta fayl yozadi. "
                "Nomlari tartibida ular butun dampni tiklaydi: {{for f in shop.*.sql; do mysql shop < \"$f\"; "
                "done}}.",
                "Matn formatidagi pg_dump va SQLite'ning {{.dump}} fayllari ham xuddi shunday ishlaydi. "
                "Skriptlarda: {{bigtext-cli sqldump shop.sql extract orders -o orders.sql}}.",
            ],
        },
    ],
}
