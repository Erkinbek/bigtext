"""Website texts in German (see build-site.py; the keys are those of T["en"])."""

CODE, NAME, LOCALE = "de", "Deutsch", "de_DE"

T = {
    "title": "BigText — kostenloser Editor für riesige Text- und Logdateien (1–50+ GB)",
    "description": "Text- und Logdateien mit 1 GB, 10 GB oder 50 GB sofort öffnen, durchsuchen und bearbeiten. Eine "
    "kostenlose Open-Source-Alternative zu EmEditor und UltraEdit für Windows, Linux und macOS, die nie einfriert.",
    "og_title": "BigText — 50-GB-Textdateien sofort öffnen",
    "skip": "Zum Inhalt springen",
    "nav": ["Funktionen", "Leistung", "Download", "FAQ"],
    "h1": "50-GB-Textdateien sofort öffnen",
    "sub": "Kostenlos und Open Source, für Windows, Linux und macOS. Kein Einfrieren und kein Warten, egal wie groß die Datei ist.",
    "lead": "BigText ist ein Editor für Logdateien, CSV-Exporte, Datenbank-Dumps und jede andere Datei, die für "
    "gewöhnliche Editoren zu groß ist – und eine kostenlose Alternative zu EmEditor und UltraEdit. Er lädt die Datei "
    "nie vollständig in den Speicher: Ein 10-GB-Log öffnet sich in Millisekunden und braucht etwa 100 MB RAM.",
    "cta": "Kostenlos herunterladen",
    "cta2": "Quellcode auf GitHub",
    "note": "Windows · Linux · macOS · MIT-Lizenz · in Rust geschrieben",
    "demo_alt": "BigText öffnet eine 50-GB-Logdatei sofort, springt an ihr Ende und in ihre Mitte, durchsucht sie mit "
                "einem regulären Ausdruck, während die Treffer eintreffen, und verfolgt ein wachsendes Log.",
    "stats": [
        ("41 ms", "bis zum ersten Bildschirm einer 50-GB-Datei"),
        ("2,5 GB/s", "Volltextsuche"),
        ("107 MB", "RAM mit einer geöffneten 50-GB-Datei"),
        ("< 3 ms", "Frame-Zeit der Oberfläche beim Indizieren"),
    ],
    "features_h2": "Alles, was Sie für riesige Dateien brauchen",
    "features_intro": "Gemacht für Logs, CSV- und SQL-Dumps, JSON-Exporte und jede Textdatei, an der andere Editoren hängen bleiben.",
    "features": [
        ("Öffnet Dateien jeder Größe", "Gelesen wird nur der sichtbare Teil, über speichergemappte Fenster. Arbeit im "
         "Hintergrund streamt den Rest, und Netzwerk- oder USB-Laufwerke weichen auf zwischengespeicherte, blockweise "
         "Lesevorgänge aus, damit eine langsame Platte das Fenster nie blockiert."),
        ("Sofort zu jeder Zeile", "Ein dünn besetzter Zeilenindex (8 Byte pro 64 KB) entsteht im Hintergrund mit "
         "2,5 GB/s. Gehe zu Zeile und die Bildlaufleiste funktionieren sofort, schon bevor die Indizierung fertig ist."),
        ("Schnelle Suche und reguläre Ausdrücke", "Klartext mit 2,5 GB/s und reguläre Ausdrücke in linearer Zeit, mit "
         "Optionen für Groß-/Kleinschreibung, ganze Wörter und mehrere Zeilen. „Alle suchen“ listet bis zu eine Million "
         "Treffer; F3 funktioniert sofort."),
        ("Ersetzen über Gigabytes", "Ersetzen mit $1 und benannten Gruppen. Bis zu 50.000 Änderungen sind ein Schritt, "
         "der sich rückgängig machen lässt; größere Aufgaben werden in eine neue Datei gestreamt: 22 Millionen "
         "Ersetzungen in einer 10-GB-Datei dauerten 11 s und brauchten 46 MB RAM."),
        ("Echtes Bearbeiten", "Tippen, Ausschneiden, Kopieren, Einfügen, Rückgängig und Wiederholen, Zeilen duplizieren "
         "oder löschen, automatisches Einrücken – auf einer Piece Table, sodass Änderungen die Datei erst beim Speichern "
         "neu schreiben."),
        ("Absturzsicheres Speichern", "Beim Speichern entsteht eine temporäre Datei, die auf die Platte geschrieben wird "
         "und das Original atomar ersetzt. Ein Absturz oder Stromausfall hinterlässt nie eine halb geschriebene Datei. "
         "Auf Wunsch mit .bak-Sicherungen."),
        ("Kodierungen und Hex-Ansicht", "UTF-8, UTF-16 LE/BE, Windows-1251, Windows-1252 und ISO-8859-1 werden "
         "automatisch erkannt; LF-, CRLF- und CR-Zeilenenden bleiben erhalten oder werden umgewandelt. Binärdateien "
         "öffnen sich in einer Hex-Ansicht."),
        ("Werkzeuge für Logs", "Verfolgen-Modus (wie tail -f) für wachsende Logs, Syntaxhervorhebung für Logs, JSON, "
         "XML, CSV, SQL und INI, Lesezeichen und eine geteilte Ansicht einer Datei."),
        ("Riesige Dateien vergleichen", "Ein streamender Zeilenvergleich zweier Dateien mit mehreren Gigabyte: 1 GB "
         "gegen 1 GB in 0,6 s mit etwa 2 MB Speicher. Ein Klick auf einen Unterschied springt dorthin."),
        ("Filtern und hervorheben", "Nur die passenden Zeilen zeigen oder nur die nicht passenden, wie grep über die "
         "ganze Datei, und sie in eine neue Datei speichern. Muster wie ERROR in eigenen Farben markieren und auf der "
         "Bildlaufleiste sehen, wo sich die Treffer häufen."),
        ("Zu einer Uhrzeit springen", "14:32:05 eingeben und auf der ersten Logzeile mit dieser Zeit landen: Eine "
         "binäre Suche liest wenige hundert Kilobyte eines 50-GB-Logs. Zeilen markieren, um die Zeit zwischen ihnen zu sehen."),
        ("Komprimierte Logs und Pipes", ".gz- und .zst-Dateien direkt öffnen, einen Befehl in BigText leiten "
         "(journalctl -f | bigtext -) oder app.log:120 von der Kommandozeile an einer Zeile öffnen."),
        ("CSV-Tabellen", "CSV- und TSV-Dateien öffnen sich als Tabelle, die so schnell scrollt wie Text. Nach einer "
         "Spalte sortieren, Spaltenstatistiken, nach einer Spalte filtern und Spalten umbenennen, verschieben, "
         "exportieren oder löschen."),
        ("Textwerkzeuge", "Zeilen sortieren, Duplikate entfernen und umkehren, Groß-/Kleinschreibung ändern, Leerraum "
         "entfernen, JSON formatieren oder minimieren und viele Zeilen auf einmal mit einer Spaltenauswahl per Alt+Ziehen "
         "bearbeiten."),
        ("So, wie Sie es mögen", "Eine Befehlspalette (Strg+Umschalt+P), änderbare Tastenkürzel, jede "
         "Festbreitenschrift und eine Oberfläche auf Englisch, Russisch, Usbekisch, Chinesisch, Spanisch, Portugiesisch, "
         "Deutsch oder Türkisch."),
        ("Log-Analyse", "Eine Level-Leiste mit Filtern für ERROR, WARN, INFO und DEBUG und F8 zum nächsten Fehler, ein "
         "Zeit-Histogramm, Log-Felder, Nachrichtenmuster, häufigste Werte, Zeitlücken und die Zeilen um jeden Treffer "
         "oder mit derselben Request-ID."),
        ("Logs zeitlich nebeneinander", "Mehrere Logs nach Zeitstempel zu einer farbigen Ansicht zusammenführen oder "
         "die offenen Logs ausrichten und gemeinsam nach Zeit scrollen, mit einstellbarer Uhrabweichung. Das Verfolgen "
         "übersteht die Log-Rotation."),
        ("JSON und XML", "Ein Strukturbereich, der JSON-Pfad der Schreibmarke in der Statusleiste, Falten, JSON Lines "
         "als Tabelle mit Abfragen, eine strenge Prüfung, die auf den Fehler zeigt, Umwandlung zwischen JSON, JSON Lines "
         "und logfmt und ein struktureller Vergleich."),
        ("Tabellen, die rechnen", "Gruppieren mit Summen, Pivot-Tabellen, ein Join zweier Dateien über einen "
         "Schlüssel, berechnete Spalten, Spaltendiagramme, fixierte und ausgeblendete Spalten, Zeilenprüfungen und "
         "Export nach JSON, Markdown oder Excel."),
        ("Vergleichen, wie Sie wollen", "Nebeneinander oder als Unified Diff, nächster und vorheriger Unterschied, "
         "Leerraum, Groß-/Kleinschreibung oder Zeitstempel ignorieren, Zeilen, die nur in einer Datei vorkommen, CSV "
         "nach Schlüssel verglichen und Patches angewendet."),
        ("Archive und große Änderungen", "Dateien in zip- und tar-Archiven, .xz- und .bz2-Dateien, die schon beim "
         "Entpacken angezeigt werden, direktes Speichern als .gz oder .zst, mehrere Cursor, ein Spalteneditor und "
         "Sortieren, Entdoppeln oder ein beliebiger Befehl für eine Datei jeder Größe."),
        ("In Dateien suchen und Skripte", "Einen ganzen Ordner mit Ergebnissen im Format Datei:Zeile durchsuchen und "
         "bigtext-cli für count, head, tail, slice, replace, CSV-Werkzeuge und JSON-Ausgabe in Skripten nutzen. "
         "Sitzungen, portabler Modus, und ein zweiter Start öffnet im laufenden Fenster."),
        ("Bequem für alle", "Kontrastreiche und farbenblindsichere Themen, Skalierung der Oberfläche, Unterstützung für "
         "Bildschirmleser, Zen-Modus, Umbruch an einer Spalte mit Lineal, eine anklickbare Statusleiste und "
         "Einstellungen, die für jede Datei gespeichert werden."),
        ("Auch ein angenehmer Code-Editor", "Syntaxfarben für Python, JavaScript, TypeScript, Java, C#, C/C++, Go, "
         "Rust, PHP, Shell-Skripte, YAML und mehr, in One Dark, Monokai, GitHub oder Solarized; eine Ordner-Seitenleiste "
         "wie in Sublime Text; Strg+P findet eine Datei, Strg+R die Funktionen und Klassen einer Datei, Strg+/ "
         "kommentiert Zeilen aus, und Klammern schließen sich selbst. Eingefärbt werden nur die Zeilen auf dem "
         "Bildschirm, darum bleibt eine riesige Datei genauso schnell."),
        ("Logs auf Servern", "user@server:/var/log/app.log über SSH mit Ihren Schlüsseln öffnen, oder eine Datei unter https:// oder "
         "öffentlichem s3://, ohne sie herunterzuladen: Nur das Angezeigte wird übertragen, und über SSH laufen Suchen "
         "auf dem Server. Oder die letzten Megabytes eines Logs wie tail -F verfolgen."),
    ],
    "gallery_h2": "Screenshots",
    "gallery": {
        "search": "„Alle suchen“ mit regulärem Ausdruck in einem 10-GB-Log: 166 Treffer in 4 s (2,5 GB/s).",
        "compare": "Vergleich zweier 48-MB-Logs: Unterschiede in 0,04 s gefunden, ein Klick springt hin.",
        "hex": "Hex-Ansicht mitten in einer 10-GB-Datei, bei Offset 0x140000000.",
        "json": "Helles Thema mit JSON-Hervorhebung, geteilter Ansicht und Lesezeichen.",
    },
    "how_h2": "Warum BigText schnell bleibt",
    "how_intro": "Gewöhnliche Editoren lesen die ganze Datei, bevor sie etwas zeigen. BigText liest nur, was auf dem "
    "Bildschirm ist, und erledigt alles andere in Hintergrund-Threads, darum antwortet das Fenster in unter 3 ms.",
    "how": [
        ("Speichergemappte Fenster", "Die Datei wird in 256-MB-Fenstern gemappt, und nur die Bytes, die Sie ansehen, "
         "werden berührt. Suche, Indizierung und Speichern lesen getrennt als Stream."),
        ("Dünn besetzter Zeilenindex", "Ein 8-Byte-Kontrollpunkt pro 64 KB: 8 MB Index für eine 50-GB-Datei. "
         "Zeilenpositionen zwischen den Kontrollpunkten werden bei Bedarf mit SIMD gezählt."),
        ("Piece Table", "Änderungen werden als kleine Stücke über der Originaldatei gespeichert, sodass das Einfügen "
         "einer Zeile in eine 50-GB-Datei wenige Bytes kostet und Rückgängig sofort geht."),
        ("Atomares Speichern", "Eine temporäre Datei neben dem Original, fsync, dann ein atomares Ersetzen "
         "(ReplaceFileW unter Windows behält Attribute und Berechtigungen)."),
    ],
    "perf_h2": "Gemessene Leistung",
    "perf_intro": "Ergebnisse der eingebauten Benchmarks mit Testdateien von 1 GB, 10 GB und 50 GB unter Windows 11. "
    "Methode und vollständige Ergebnisse stehen in der README.",
    "perf_cols": ("Aufgabe", "Ergebnis"),
    "perf": [
        ("Öffnen → erster Bildschirm mit Text (1–50 GB)", "41–46 ms"),
        ("Sprung zu einer Zeile in einer 50-GB-Datei", "unter 1 ms"),
        ("Zeilenindex aufbauen", "2,2–2,7 GB/s (50 GB in etwa 20 s)"),
        ("Klartextsuche", "2,4–2,5 GB/s"),
        ("Suche mit regulärem Ausdruck", "2,3–2,8 GB/s mit literalem Anteil, 672 MB/s ohne"),
        ("Alle ersetzen in einer 10-GB-Datei", "11,3 s, 46 MB Spitzenspeicher"),
        ("Speicher mit geöffneter 50-GB-Datei", "107 MB"),
        ("Start → erster Frame", "159–185 ms"),
    ],
    "dl_h2": "BigText herunterladen",
    "dl_intro": "Kostenlos für private und kommerzielle Nutzung. Kein Konto, keine Telemetrie, eine ausführbare Datei.",
    "dl": [
        ("Windows 10 / 11", "Installer mit „Öffnen mit“ und Kontextmenü im Explorer oder eine portable Version. Die "
         "Programme sind nicht signiert: Wenn SmartScreen fragt, wählen Sie „Weitere Informationen“ → „Trotzdem ausführen“.",
         [("Installer (.exe)", "bigtext-setup-windows-x64.exe"), ("Portabel (.zip)", "bigtext-windows-x64.zip")]),
        ("Linux x64", "Ein AppImage, das auf den meisten Distributionen läuft, ein .deb-Paket für Debian und Ubuntu "
         "oder ein portables .tar.gz.",
         [("AppImage", "bigtext-linux-x64.AppImage"), (".deb", "bigtext-linux-x64.deb"), (".tar.gz", "bigtext-linux-x64.tar.gz")]),
        ("macOS", "BigText.app für Apple Silicon und Intel. Die App ist nicht signiert: beim ersten Start Rechtsklick → Öffnen.",
         [("Apple Silicon", "bigtext-macos-arm64.app.zip"), ("Intel", "bigtext-macos-x64.app.zip")]),
    ],
    "dl_all": "Alle Releases und Versionshinweise",
    "dl_pm": "Mit einem Paketmanager installieren",
    "dl_pm_scoop": "Windows, mit Scoop:",
    "dl_pm_brew": "macOS und Linux, mit Homebrew: die Kommandozeilen-Version oder die App mit --cask:",
    "dl_source": "Mit Cargo installieren",
    "dl_source_text": "Rust von rustup.rs installieren, dann ausführen:",
    "dl_source_text2": "Oder aus dem Quellcode bauen:",
    "faq_h2": "Häufige Fragen",
    "faq": [
        ("Wie öffne ich eine 50-GB-Textdatei?",
         "Installieren Sie BigText und öffnen Sie die Datei mit Datei → Öffnen, per Ziehen auf das Fenster oder per "
         "Rechtsklick → „Open with BigText“. Der erste Bildschirm erscheint unabhängig von der Größe nach etwa 40 ms; "
         "Scrollen und Gehe zu Zeile funktionieren, während der Zeilenindex im Hintergrund entsteht."),
        ("Ist BigText eine Alternative zu EmEditor oder UltraEdit?",
         "Für große Dateien ja. Wie diese öffnet BigText Dateien mit mehreren Gigabyte, ohne sie in den Speicher zu "
         "laden, und durchsucht, bearbeitet und speichert sie; anders als diese ist es kostenlos und Open Source und "
         "läuft unter Windows, Linux und macOS. Makros oder Plug-ins hat es nicht: Seine Stärke sind riesige Dateien, "
         "Logs und Tabellen."),
        ("Lädt BigText die ganze Datei in den Speicher?",
         "Nein. Es mappt nur den Teil auf dem Bildschirm und streamt alles andere im Hintergrund. Mit einer geöffneten "
         "50-GB-Datei braucht es etwa 107 MB RAM, das meiste davon für die Oberfläche selbst."),
        ("Kann ich sehr große Dateien bearbeiten und speichern?",
         "Ja. Änderungen liegen in einer Piece Table über der Originaldatei, sodass Tippen bei jeder Größe sofort geht. "
         "Beim Speichern wird das Ergebnis in eine temporäre Datei gestreamt, die das Original atomar ersetzt; das "
         "Original wird also nie beschädigt, wenn etwas schiefgeht."),
        ("Wie schnell ist die Suche in riesigen Dateien?",
         "Klartext wird mit 2,4–2,5 GB/s durchsucht und typische reguläre Ausdrücke mit 2,3–2,8 GB/s, ein 10-GB-Log "
         "dauert also etwa 4 Sekunden. F3 findet den nächsten Treffer sofort, ohne auf eine vollständige Suche zu warten."),
        ("Kann es ein wachsendes Log wie tail -f verfolgen?",
         "Ja. Ansicht → Verfolgen hält die Ansicht am Dateiende und indiziert neue Zeilen, sobald sie ankommen. "
         "BigText bemerkt auch, wenn sich eine Datei auf der Platte ändert, und bietet an, sie neu zu laden."),
        ("Kann es eine Datei öffnen, die aus einer einzigen riesigen Zeile besteht, etwa minimiertes JSON?",
         "Ja. Zeilen über 64 KB werden weich umbrochen angezeigt, sodass selbst eine einzige 2-GB-Zeile flüssig scrollt."),
        ("Welche Kodierungen und Zeilenenden werden unterstützt?",
         "UTF-8, UTF-16 LE/BE (mit oder ohne BOM), Windows-1251, Windows-1252 und ISO-8859-1, automatisch erkannt und "
         "über Datei → Mit Kodierung neu öffnen umschaltbar. LF, CRLF, CR und gemischte Zeilenenden werden erkannt und "
         "können beim Speichern umgewandelt werden."),
        ("Ist BigText kostenlos, und auf welchen Systemen läuft es?",
         "BigText ist kostenlos und Open Source unter der MIT-Lizenz. Windows 10 und 11 ist die wichtigste, vollständig "
         "getestete Plattform; Builds für Linux und macOS (Apple Silicon und Intel) werden bei jeder Änderung von der "
         "CI erstellt und getestet."),
    ],
    "privacy": "Datenschutz",
    "privacy_items": [
        "Update-Prüfung: nur wenn Sie darum bitten (Hilfe → Nach Updates suchen…) oder einmal täglich, wenn Sie beim "
        "ersten Start automatischen Prüfungen zugestimmt haben. Die Anfrage geht an die GitHub-API und enthält die "
        "Version von BigText; nichts über Sie oder Ihre Dateien.",
        "Entfernte Dateien: BigText verbindet sich nur mit dem Server oder der Adresse, die Sie eingeben.",
        "Keine Telemetrie, keine Nutzungsstatistiken, keine Absturzberichte.",
    ],
    "made_by": "Erstellt von",
    "license": "MIT-Lizenz",
    "lang_label": "Sprache",
    "nf_title": "Seite nicht gefunden",
    "nf_text": "Diese Seite existiert nicht.",
    "nf_home": "Zur Startseite von BigText",
}

LABELS = {
    "guides_h2": "Anleitungen",
    "guides_intro": "Schritt für Schritt, für die Dateien, nach denen am häufigsten gefragt wird.",
    "steps": "Schritt für Schritt",
    "tips": "Gut zu wissen",
    "cta": "BigText kostenlos herunterladen",
    "more": "Weitere Anleitungen",
    "home": "BigText-Startseite",
}

GUIDES = [
    {
        "slug": "open-csv-larger-than-excel",
        "title": "CSV mit mehr als 1.048.576 Zeilen öffnen (Excel-Grenze) — BigText",
        "description": "Excel hört bei 1.048.576 Zeilen auf. Öffnen Sie die ganze CSV-Datei, egal wie groß, kostenlos "
                       "als Tabelle in BigText: sortieren, filtern, gruppieren, pivotieren oder in Teile für Excel aufteilen.",
        "h1": "Eine CSV-Datei mit mehr als 1.048.576 Excel-Zeilen öffnen",
        "card": "Excel hört bei 1.048.576 Zeilen auf. Öffnen Sie die ganze Datei als Tabelle, werten Sie sie aus oder "
                "teilen Sie sie für Excel auf.",
        "intro": [
            "Das Raster von Excel endet bei Zeile 1.048.576. Bei einer größeren CSV-Datei warnt Excel, dass die Daten zu "
            "groß sind, und die Zeilen jenseits der Grenze gehen beim Speichern verloren. Exporte von Logs, Bestellungen "
            "oder Datenbanktabellen sind oft größer.",
            "BigText öffnet eine CSV-Datei jeder Größe sofort als Tabelle und lädt sie nie in den Speicher: Eine Datei mit "
            "50 Millionen Zeilen scrollt so schnell wie eine mit fünfzig. Es ist kostenlos und läuft unter Windows, Linux "
            "und macOS.",
        ],
        "steps": [
            ("Datei öffnen", "Laden Sie BigText herunter und öffnen Sie die .csv- oder .tsv-Datei mit Datei → Öffnen "
             "oder ziehen Sie sie auf das Fenster. Sie öffnet sich als Tabelle: Das Trennzeichen (Komma, Semikolon, "
             "Tabulator oder senkrechter Strich) wird aus den ersten Zeilen erkannt, die erste Zeile wird zur fixierten "
             "Kopfzeile. {{Strg+T}} wechselt zwischen Tabelle und Text."),
            ("Sich zurechtfinden", "Scrollen Sie, drücken Sie {{Strg+G}} für eine Zeile oder {{Strg+F}} für die Suche "
             "in der ganzen Datei; „Alle suchen“ listet jeden Treffer mit seiner Zeile. Auf das Laden muss nichts warten, "
             "denn die Datei wird nie geladen."),
            ("Ohne Excel auswerten", "Rechtsklick auf eine Spaltenüberschrift: „Statistik…“ (Zeilen, leere und "
             "verschiedene Werte, Summe, Mittelwert, Minimum, Maximum und die häufigsten Werte), Filter nach dieser "
             "Spalte, „Zeilen nach dieser Spalte gruppieren…“ mit Summen einer anderen Spalte und „Pivot nach dieser "
             "Spalte…“. Sortieren funktioniert bei Dateien bis 1 GB."),
            ("Oder passend für Excel machen", "Datei → Datei aufteilen… schneidet die CSV-Datei in {{name.part001.csv}}, "
             "{{name.part002.csv}}… mit einer festen Zeilenzahl, etwa 1.000.000, immer an Zeilengrenzen. Die Kopfzeile "
             "steht nur im ersten Teil; kopieren Sie sie in die anderen, wenn Ihr Programm sie braucht. Für nur einige "
             "Zeilen filtern Sie sie („Zeilen nach Bedingungen filtern…“) und speichern das Ergebnis als neue, kleinere "
             "CSV-Datei."),
        ],
        "tips": [
            "Ansicht → Tabellenzeilen prüfen… listet Datensätze mit falscher Feldanzahl, einem verirrten "
            "Anführungszeichen oder Text in einer Zahlenspalte, mit ihren Zeilennummern.",
            "Dieselben Werkzeuge gibt es für Skripte in {{bigtext-cli}}, zum Beispiel {{bigtext-cli pivot}}.",
        ],
    },
    {
        "slug": "open-large-log-file",
        "title": "Eine 10-GB-Logdatei ohne Einfrieren öffnen — BigText",
        "description": "Ein Log jeder Größe sofort öffnen, durchsuchen, filtern und verfolgen, ohne dass der Editor "
                       "einfriert: zu einer Uhrzeit springen, nach Level filtern, Filter wie grep, tail -F. Kostenlos, "
                       "für Windows, Linux und macOS.",
        "h1": "Eine 10-GB-Logdatei ohne Einfrieren öffnen",
        "card": "Sofort öffnen, zu einer Uhrzeit springen, nach Level filtern, das Wichtige behalten und live verfolgen.",
        "intro": [
            "Notepad++ warnt, dass das Öffnen einer Datei über 2 GB Minuten dauern kann, VS Code hält die ganze Datei "
            "im Speicher, und beide können bei einem großen Log einfrieren. BigText liest nur, was auf dem Bildschirm ist, "
            "darum öffnet sich ein 10- oder 50-GB-Log sofort und braucht etwa 100 MB Speicher.",
        ],
        "steps": [
            ("Öffnen", "Mit Datei → Öffnen, per Ziehen auf das Fenster oder mit {{bigtext app.log}} im Terminal "
             "({{bigtext app.log:120}} öffnet bei Zeile 120). Logs in .gz und .zst öffnen sich direkt."),
            ("Herumspringen", "{{Strg+Ende}} geht sofort zur letzten Zeile. Gehe zu Zeile ({{Strg+G}}) funktioniert, "
             "bevor der Zeilenindex fertig ist; bis dahin tragen die Zeilennummern ein {{~}}. Gehe zu Zeit "
             "({{Strg+Umschalt+T}}) nimmt {{14:32:05}} und landet auf der ersten Zeile mit dieser Zeit."),
            ("Die Fehler finden", "Die Level-Leiste über einem Log filtert die Zeilen ERROR, WARN, INFO und DEBUG, und "
             "{{F8}} springt zum nächsten Fehler. {{Strg+F}} durchsucht die ganze Datei, auf Wunsch mit regulären "
             "Ausdrücken, und „Alle suchen“ listet die Treffer."),
            ("Nur das Wichtige behalten", "Die Filteransicht zeigt nur die passenden Zeilen oder nur die nicht "
             "passenden, wie grep über die ganze Datei, und speichert sie in eine neue Datei."),
            ("Live verfolgen", "Der Verfolgen-Modus ({{bigtext -f app.log}} oder „Dateiänderungen verfolgen (tail)“ in "
             "der Befehlspalette) fügt neue Zeilen wie {{tail -F}} hinzu und übersteht die Log-Rotation. Ein Log auf "
             "einem Server öffnet sich über SSH mit Ihren Schlüsseln: {{bigtext -f user@server:/var/log/syslog}}."),
        ],
        "tips": [
            "Mehrere Logs lassen sich nach Zeit zu einer Ansicht zusammenführen oder zwei Logs gemeinsam nach Zeit scrollen.",
            "Suche, Filter und Speichern laufen im Hintergrund mit Fortschrittsanzeige und Abbrechen, das Fenster "
            "reagiert weiter.",
        ],
    },
    {
        "slug": "extract-table-from-mysqldump",
        "title": "Eine Tabelle aus einer mysqldump-Datei herauslösen — BigText",
        "description": "Eine Tabelle in einem SQL-Dump mit mehreren Gigabyte finden und in wenigen Schritten in eine "
                       "eigene Datei schreiben, ohne den Dump in den Speicher zu laden. Kostenlos, für Windows, Linux und macOS.",
        "h1": "Eine Tabelle aus einer mysqldump-Datei herauslösen",
        "card": "Die Tabellen eines riesigen SQL-Dumps auflisten und die gesuchte in eine eigene Datei schreiben.",
        "intro": [
            "Ein vollständiger mysqldump einer Datenbank kann Dutzende Gigabyte groß sein, und oft brauchen Sie nur eine "
            "Tabelle daraus: um sie wiederherzustellen, ihre Daten zu prüfen oder sie weiterzugeben. BigText öffnet den "
            "Dump sofort, ohne ihn in den Speicher zu laden, und „Zeilen exportieren…“ schreibt einen beliebigen "
            "Zeilenbereich in eine neue Datei.",
        ],
        "steps": [
            ("Den Dump öffnen", "Öffnen Sie die .sql-Datei mit Datei → Öffnen oder ziehen Sie sie auf das Fenster. Ein "
             ".sql.gz-Dump öffnet sich ebenfalls direkt."),
            ("Die Tabellen auflisten", "mysqldump markiert jede Tabelle mit einem Kommentar wie "
             "{{-- Table structure for table `orders`}}. Suchen Sie nach {{-- Table structure for table}} und drücken "
             "Sie „Alle suchen“: Die Ergebnisse listen jede Tabelle des Dumps mit ihrer Zeilennummer."),
            ("Anfang und Ende der Tabelle finden", "Klicken Sie in den Ergebnissen auf die gesuchte Tabelle; ihre "
             "Zeilennummer steht in der Statusleiste. Sie endet dort, wo der Kommentar der nächsten Tabelle beginnt: "
             "{{F3}} springt dorthin."),
            ("In eine Datei schreiben", "Datei → Zeilen exportieren… schreibt einen Zeilenbereich in eine neue Datei: "
             "von der ersten Zeile der Tabelle bis zur Zeile vor der nächsten Tabelle. Sie erhalten eine kleine "
             ".sql-Datei mit der CREATE-TABLE-Anweisung und den INSERT-Zeilen der Tabelle."),
        ],
        "tips": [
            "Der Dump beginnt mit SET-Zeilen (Zeichensatz, Fremdschlüsselprüfung). Kopieren Sie sie an den Anfang der "
            "neuen Datei, wenn die Tabelle sie braucht.",
            "Ein Dump mit {{--skip-comments}} hat diese Kommentare nicht: Suchen Sie stattdessen nach "
            "{{CREATE TABLE `orders`}} und dem nächsten {{DROP TABLE IF EXISTS}}.",
            "Nur die Daten: Die Filteransicht mit {{INSERT INTO `orders`}} zeigt nur die INSERT-Zeilen dieser Tabelle "
            "und speichert sie in eine neue Datei.",
        ],
    },
]
