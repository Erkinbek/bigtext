"""Website texts in Spanish (see build-site.py; the keys are those of T["en"])."""

CODE, NAME, LOCALE = "es", "Español", "es_ES"

T = {
    "title": "BigText — editor gratuito para archivos de texto y logs enormes (1–50+ GB)",
    "description": "Abre, busca y edita al instante archivos de texto y logs de 1 GB, 10 GB o 50 GB. Una alternativa "
    "gratuita y de código abierto a EmEditor y UltraEdit para Windows, Linux y macOS que nunca se congela.",
    "og_title": "BigText — abre archivos de texto de 50 GB al instante",
    "skip": "Ir al contenido",
    "nav": ["Funciones", "Rendimiento", "Descarga", "Preguntas"],
    "h1": "Abre archivos de texto de 50 GB al instante",
    "sub": "Gratis y de código abierto, para Windows, Linux y macOS. Sin bloqueos ni esperas, sea cual sea el tamaño.",
    "lead": "BigText es un editor para logs, exportaciones CSV, volcados de bases de datos y cualquier archivo demasiado "
    "grande para los editores corrientes, y una alternativa gratuita a EmEditor y UltraEdit. Nunca carga el archivo "
    "entero en memoria: un log de 10 GB se abre en milisegundos y usa unos 100 MB de RAM.",
    "cta": "Descargar gratis",
    "cta2": "Código en GitHub",
    "note": "Windows · Linux · macOS · licencia MIT · escrito en Rust",
    "demo_alt": "BigText abre al instante un log de 50 GB, salta a su final y a su mitad, lo busca con una expresión "
                "regular mientras llegan las coincidencias y sigue un log que crece.",
    "stats": [
        ("41 ms", "hasta la primera pantalla de un archivo de 50 GB"),
        ("2,5 GB/s", "velocidad de búsqueda de texto"),
        ("107 MB", "de RAM con un archivo de 50 GB abierto"),
        ("< 3 ms", "por fotograma de la interfaz mientras indexa"),
    ],
    "features_h2": "Todo lo que necesitas para archivos gigantes",
    "features_intro": "Hecho para logs, volcados CSV y SQL, exportaciones JSON y cualquier archivo de texto que bloquea a otros editores.",
    "features": [
        ("Abre archivos de cualquier tamaño", "Solo se lee la parte visible, a través de ventanas mapeadas en memoria. "
         "El trabajo en segundo plano procesa el resto, y las unidades de red o USB pasan a lecturas por bloques con "
         "caché, así que un disco lento nunca bloquea la ventana."),
        ("Ir a una línea al instante", "Un índice de líneas disperso (8 bytes por cada 64 KB) se construye en segundo "
         "plano a 2,5 GB/s. Ir a la línea y la barra de desplazamiento funcionan de inmediato, incluso antes de que "
         "termine la indexación."),
        ("Búsqueda rápida y regex", "Texto simple a 2,5 GB/s y expresiones regulares en tiempo lineal, con opciones de "
         "mayúsculas, palabra completa y varias líneas. «Buscar todo» lista hasta un millón de resultados; F3 funciona "
         "en el acto."),
        ("Reemplazar en gigabytes", "Reemplaza con $1 y grupos con nombre. Hasta 50.000 cambios son una sola edición que "
         "se puede deshacer; los trabajos mayores se escriben en un archivo nuevo: 22 millones de reemplazos en un "
         "archivo de 10 GB tardaron 11 s y usaron 46 MB de RAM."),
        ("Edición de verdad", "Escribir, cortar, copiar, pegar, deshacer y rehacer, duplicar o borrar líneas, sangría "
         "automática, sobre una piece table, de modo que las ediciones no reescriben el archivo hasta que guardas."),
        ("Guardado a prueba de fallos", "Al guardar se escribe un archivo temporal, se vuelca al disco y sustituye al "
         "original de forma atómica. Un fallo o un corte de luz nunca deja un archivo a medias. Copias .bak opcionales."),
        ("Codificaciones y vista hex", "UTF-8, UTF-16 LE/BE, Windows-1251, Windows-1252 e ISO-8859-1 se detectan "
         "automáticamente; los finales de línea LF, CRLF y CR se mantienen o se convierten. Los archivos binarios se "
         "abren en una vista hexadecimal."),
        ("Herramientas para logs", "Modo seguimiento (como tail -f) para logs que crecen, resaltado de sintaxis para "
         "logs, JSON, XML, CSV, SQL e INI, marcadores y una vista dividida de un archivo."),
        ("Compara archivos enormes", "Una comparación de líneas en streaming de dos archivos de varios gigabytes: 1 GB "
         "contra 1 GB en 0,6 s con unos 2 MB de memoria. Haz clic en una diferencia para ir a ella."),
        ("Filtrar y resaltar", "Muestra solo las líneas que coinciden, o solo las que no, como grep sobre todo el "
         "archivo, y guárdalas en un archivo nuevo. Pinta patrones como ERROR con sus propios colores y mira en la "
         "barra de desplazamiento dónde se agrupan las coincidencias."),
        ("Saltar a una hora", "Escribe 14:32:05 y llega a la primera línea del log con esa hora: una búsqueda binaria "
         "lee unos cientos de kilobytes de un log de 50 GB. Selecciona líneas para ver el tiempo entre ellas."),
        ("Logs comprimidos y tuberías", "Abre archivos .gz y .zst directamente, envía un comando a BigText por una "
         "tubería (journalctl -f | bigtext -) o abre app.log:120 en una línea desde la línea de comandos."),
        ("Tablas CSV", "Los archivos CSV y TSV se abren como una tabla que se desplaza tan rápido como el texto. Ordena "
         "por una columna, obtén estadísticas de columna, filtra por una columna y renombra, mueve, exporta o elimina "
         "columnas."),
        ("Herramientas de texto", "Ordena, quita duplicados e invierte líneas, cambia mayúsculas, recorta espacios, "
         "formatea o minimiza JSON y edita muchas líneas a la vez con una selección de columna con Alt+arrastrar."),
        ("A tu manera", "Una paleta de comandos (Ctrl+Mayús+P), atajos que puedes cambiar, cualquier fuente monoespaciada "
         "y una interfaz en inglés, ruso, uzbeko, chino, español, portugués, alemán o turco."),
        ("Análisis de logs", "Una barra de niveles con filtros ERROR, WARN, INFO y DEBUG y F8 al siguiente error, un "
         "histograma de tiempo, campos del log, patrones de mensajes, valores más frecuentes, huecos de tiempo y las "
         "líneas alrededor de cada coincidencia o con el mismo id de petición."),
        ("Logs en paralelo por tiempo", "Combina varios logs por marca de tiempo en una vista coloreada, o alinea los "
         "logs abiertos y desplázalos juntos por tiempo, con un desfase de reloj ajustable. El seguimiento sobrevive a "
         "la rotación de logs."),
        ("JSON y XML", "Un panel de estructura, la ruta JSON del cursor en la barra de estado, plegado, JSON Lines como "
         "tabla con consultas, una comprobación estricta que señala el error, conversión entre JSON, JSON Lines y "
         "logfmt y una comparación estructural."),
        ("Tablas que calculan", "Agrupar con sumas, tablas dinámicas, unir dos archivos por una clave, columnas "
         "calculadas, gráficos de columnas, columnas fijas y ocultas, comprobación de filas y exportación a JSON, "
         "Markdown o Excel."),
        ("Compara a tu manera", "Lado a lado o como diff unificado, diferencia siguiente y anterior, ignorar espacios, "
         "mayúsculas o marcas de tiempo, líneas que están en un solo archivo, CSV comparado por clave y parches aplicados."),
        ("Archivos comprimidos y grandes ediciones", "Archivos dentro de zip y tar, archivos .xz y .bz2 mostrados "
         "mientras se descomprimen, guardado directo a .gz o .zst, varios cursores, un editor de columnas y ordenar, "
         "quitar duplicados o ejecutar cualquier comando sobre un archivo de cualquier tamaño."),
        ("Buscar en archivos y scripts", "Busca en una carpeta entera con resultados archivo:línea y usa bigtext-cli "
         "para count, head, tail, slice, replace, herramientas CSV y salida JSON en scripts. Sesiones, modo portátil, y "
         "un segundo arranque abre en la ventana que ya está en marcha."),
        ("Cómodo para todos", "Temas de alto contraste y seguros para daltonismo, escalado de la interfaz, compatibilidad "
         "con lectores de pantalla, modo zen, ajuste a una columna con regla, una barra de estado clicable y ajustes "
         "recordados para cada archivo."),
        ("También un editor de código cómodo", "Colores de sintaxis para Python, JavaScript, TypeScript, Java, C#, "
         "C/C++, Go, Rust, PHP, scripts de shell, YAML y más, en One Dark, Monokai, GitHub o Solarized; una barra "
         "lateral de carpeta como en Sublime Text; Ctrl+P para encontrar un archivo, Ctrl+R para las funciones y clases "
         "de un archivo, Ctrl+/ para comentar líneas y corchetes que se cierran solos. Solo se colorean las filas en "
         "pantalla, así que un archivo enorme sigue igual de rápido."),
        ("Logs en servidores", "Abre user@server:/var/log/app.log por SSH con tus claves, o un archivo https:// o s3:// público, "
         "sin descargarlo: solo llega lo que está en pantalla, y por SSH las búsquedas se ejecutan en el servidor. "
         "O sigue los últimos megabytes de un log como tail -F."),
    ],
    "gallery_h2": "Capturas",
    "gallery": {
        "search": "«Buscar todo» con regex en un log de 10 GB: 166 coincidencias en 4 s (2,5 GB/s).",
        "compare": "Comparando dos logs de 48 MB: diferencias encontradas en 0,04 s; haz clic en una para ir a ella.",
        "hex": "Vista hexadecimal en mitad de un archivo de 10 GB, en el desplazamiento 0x140000000.",
        "json": "Tema claro con resaltado de JSON, una vista dividida y marcadores.",
    },
    "how_h2": "Cómo BigText se mantiene rápido",
    "how_intro": "Los editores corrientes leen el archivo entero antes de mostrar nada. BigText lee solo lo que hay en "
    "pantalla y hace todo lo demás en hilos en segundo plano, así que la ventana responde en menos de 3 ms.",
    "how": [
        ("Ventanas mapeadas en memoria", "El archivo se mapea en ventanas de 256 MB y solo se tocan los bytes que miras. "
         "La búsqueda, la indexación y el guardado usan lecturas en streaming aparte."),
        ("Índice de líneas disperso", "Un punto de control de 8 bytes por cada 64 KB: 8 MB de índice para un archivo de "
         "50 GB. Las posiciones de línea entre puntos de control se cuentan bajo demanda con SIMD."),
        ("Piece table", "Las ediciones se guardan como pequeñas piezas sobre el archivo original, así que insertar una "
         "línea en un archivo de 50 GB cuesta unos pocos bytes y deshacer es instantáneo."),
        ("Guardado atómico", "Un archivo temporal junto al original, fsync y después un reemplazo atómico (ReplaceFileW "
         "en Windows conserva atributos y permisos)."),
    ],
    "perf_h2": "Rendimiento medido",
    "perf_intro": "Resultados de las pruebas de rendimiento integradas con archivos de prueba de 1 GB, 10 GB y 50 GB en "
    "Windows 11. El método y los resultados completos están en el README.",
    "perf_cols": ("Tarea", "Resultado"),
    "perf": [
        ("Abrir → primera pantalla de texto (1–50 GB)", "41–46 ms"),
        ("Saltar a una línea en un archivo de 50 GB", "menos de 1 ms"),
        ("Construir el índice de líneas", "2,2–2,7 GB/s (50 GB en unos 20 s)"),
        ("Búsqueda de texto simple", "2,4–2,5 GB/s"),
        ("Búsqueda con regex", "2,3–2,8 GB/s con parte literal, 672 MB/s sin ella"),
        ("Reemplazar todo en un archivo de 10 GB", "11,3 s, 46 MB de memoria máxima"),
        ("Memoria con un archivo de 50 GB abierto", "107 MB"),
        ("Arranque → primer fotograma", "159–185 ms"),
    ],
    "dl_h2": "Descargar BigText",
    "dl_intro": "Gratis para uso personal y comercial. Sin cuenta, sin telemetría, un solo ejecutable.",
    "dl": [
        ("Windows 10 / 11", "Instalador con «Abrir con» y menú contextual del Explorador, o una versión portátil. Los "
         "programas no están firmados: si SmartScreen pregunta, elige «Más información» → «Ejecutar de todas formas».",
         [("Instalador (.exe)", "bigtext-setup-windows-x64.exe"), ("Portátil (.zip)", "bigtext-windows-x64.zip")]),
        ("Linux x64", "Un AppImage que funciona en la mayoría de distribuciones, un paquete .deb para Debian y Ubuntu o "
         "un .tar.gz portátil.",
         [("AppImage", "bigtext-linux-x64.AppImage"), (".deb", "bigtext-linux-x64.deb"), (".tar.gz", "bigtext-linux-x64.tar.gz")]),
        ("macOS", "BigText.app para Apple Silicon e Intel. La app no está firmada: la primera vez, clic derecho → Abrir.",
         [("Apple Silicon", "bigtext-macos-arm64.app.zip"), ("Intel", "bigtext-macos-x64.app.zip")]),
    ],
    "dl_all": "Todas las versiones y sus notas",
    "dl_pm": "Instalar con un gestor de paquetes",
    "dl_pm_scoop": "Windows, con Scoop:",
    "dl_pm_brew": "macOS y Linux, con Homebrew: la versión de línea de comandos, o la app con --cask:",
    "dl_source": "Instalar con Cargo",
    "dl_source_text": "Instala Rust desde rustup.rs y ejecuta:",
    "dl_source_text2": "O compílalo desde el código fuente:",
    "faq_h2": "Preguntas frecuentes",
    "faq": [
        ("¿Cómo abro un archivo de texto de 50 GB?",
         "Instala BigText y abre el archivo con Archivo → Abrir, arrastrándolo a la ventana o con clic derecho → «Open "
         "with BigText». La primera pantalla aparece en unos 40 ms, sea cual sea el tamaño; el desplazamiento e ir a la "
         "línea funcionan mientras el índice de líneas se construye en segundo plano."),
        ("¿Es BigText una alternativa a EmEditor o UltraEdit?",
         "Para archivos grandes, sí. Como ellos, BigText abre archivos de varios gigabytes sin cargarlos en memoria y "
         "los busca, edita y guarda; a diferencia de ellos, es gratuito y de código abierto y funciona en Windows, Linux "
         "y macOS. No tiene macros ni plugins: su esfuerzo va a los archivos enormes, los logs y las tablas."),
        ("¿BigText carga el archivo entero en memoria?",
         "No. Mapea solo la parte que está en pantalla y procesa todo lo demás en segundo plano. Con un archivo de 50 GB "
         "abierto usa unos 107 MB de RAM, la mayor parte para la propia interfaz."),
        ("¿Puedo editar y guardar archivos muy grandes?",
         "Sí. Las ediciones se guardan en una piece table sobre el archivo original, así que escribir es instantáneo en "
         "cualquier tamaño. Al guardar, el resultado se escribe en un archivo temporal que sustituye al original de forma "
         "atómica, de modo que el original nunca se daña si algo sale mal."),
        ("¿Qué tan rápida es la búsqueda en archivos enormes?",
         "El texto simple se busca a 2,4–2,5 GB/s y las expresiones regulares habituales a 2,3–2,8 GB/s, así que un log "
         "de 10 GB tarda unos 4 segundos. F3 encuentra la siguiente coincidencia al instante, sin esperar a una búsqueda "
         "completa."),
        ("¿Puede seguir un log que crece, como tail -f?",
         "Sí. Ver → Seguir mantiene la vista al final del archivo e indexa las líneas nuevas a medida que llegan. BigText "
         "también nota cuando un archivo cambia en el disco y ofrece recargarlo."),
        ("¿Puede abrir un archivo que es una sola línea enorme, como un JSON minimizado?",
         "Sí. Las líneas de más de 64 KB se muestran partidas en filas, así que incluso una sola línea de 2 GB se "
         "desplaza con fluidez."),
        ("¿Qué codificaciones y finales de línea admite?",
         "UTF-8, UTF-16 LE/BE (con o sin BOM), Windows-1251, Windows-1252 e ISO-8859-1, detectadas automáticamente y "
         "cambiables con Archivo → Reabrir con codificación. Los finales LF, CRLF, CR y mezclados se detectan y pueden "
         "convertirse al guardar."),
        ("¿BigText es gratis y en qué sistemas funciona?",
         "BigText es gratuito y de código abierto con licencia MIT. Windows 10 y 11 es la plataforma principal, probada "
         "a fondo; las versiones para Linux y macOS (Apple Silicon e Intel) se compilan y prueban en CI con cada cambio."),
    ],
    "privacy": "Privacidad",
    "privacy_items": [
        "Búsqueda de actualizaciones: solo cuando la pides (Ayuda → Buscar actualizaciones…), o una vez al día si "
        "aceptaste las comprobaciones automáticas la primera vez que se inició BigText. La petición va a la API de "
        "GitHub y lleva la versión de BigText; nada sobre ti ni tus archivos.",
        "Archivos remotos: BigText se conecta solo al servidor o la dirección que escribes.",
        "Sin telemetría, estadísticas de uso ni informes de fallos.",
    ],
    "made_by": "Hecho por",
    "license": "licencia MIT",
    "lang_label": "Idioma",
    "nf_title": "Página no encontrada",
    "nf_text": "Esta página no existe.",
    "nf_home": "Ir a la página principal de BigText",
}

LABELS = {
    "guides_h2": "Guías",
    "guides_intro": "Paso a paso, para los archivos por los que más se pregunta.",
    "steps": "Paso a paso",
    "tips": "Conviene saber",
    "cta": "Descargar BigText gratis",
    "more": "Más guías",
    "home": "Inicio de BigText",
}

GUIDES = [
    {
        "slug": "open-csv-larger-than-excel",
        "title": "Abrir un CSV de más de 1.048.576 filas (el límite de Excel) — BigText",
        "description": "Excel se detiene en 1.048.576 filas. Abre el CSV completo, de cualquier tamaño, como tabla en "
                       "BigText, gratis: ordénalo, fíltralo, agrúpalo o divídelo en partes que Excel pueda cargar.",
        "h1": "Abrir un archivo CSV con más filas que las 1.048.576 de Excel",
        "card": "Excel se detiene en 1.048.576 filas. Abre el archivo entero como tabla, analízalo o divídelo para Excel.",
        "intro": [
            "La cuadrícula de Excel termina en la fila 1.048.576. Con un CSV mayor avisa de que los datos son demasiado "
            "grandes, y las filas que pasan del límite se pierden si guardas. Las exportaciones de logs, pedidos o "
            "tablas de bases de datos suelen ser mayores.",
            "BigText abre un CSV de cualquier tamaño como tabla al instante y nunca lo carga en memoria: un archivo de 50 "
            "millones de filas se desplaza tan rápido como uno de cincuenta. Es gratuito y funciona en Windows, Linux y macOS.",
        ],
        "steps": [
            ("Abre el archivo", "Descarga BigText y abre el .csv o .tsv con Archivo → Abrir, o arrástralo a la ventana. "
             "Se abre como tabla: el separador (coma, punto y coma, tabulador o barra vertical) se detecta en las "
             "primeras líneas y la primera fila queda como encabezado fijo. {{Ctrl+T}} cambia entre tabla y texto."),
            ("Muévete por el archivo", "Desplázate, pulsa {{Ctrl+G}} para ir a una fila o {{Ctrl+F}} para buscar en todo "
             "el archivo; «Buscar todo» lista cada coincidencia con su línea. Nada espera a que el archivo cargue, porque "
             "nunca se carga."),
            ("Analízalo sin Excel", "Haz clic derecho en un encabezado de columna: «Estadísticas…» (filas, valores vacíos "
             "y distintos, suma, media, mínimo, máximo y los valores más frecuentes), filtrar filas por esa columna, "
             "«Agrupar filas por esta columna…» con sumas de otra columna y «Tabla dinámica por esta columna…». Ordenar "
             "funciona en archivos de hasta 1 GB."),
            ("O ajústalo a Excel", "Archivo → Dividir archivo… corta el CSV en {{name.part001.csv}}, {{name.part002.csv}}… "
             "con un número de líneas dado, por ejemplo 1.000.000, siempre en saltos de línea. El encabezado está solo en "
             "la primera parte; cópialo en las demás si tu herramienta lo necesita. Para quedarte solo con algunas "
             "filas, fíltralas («Filtrar filas por condiciones…») y guarda el resultado como un CSV nuevo y más pequeño."),
        ],
        "tips": [
            "Ver → Comprobar filas de la tabla… lista los registros con un número de campos incorrecto, una comilla "
            "suelta o texto en una columna numérica, con sus números de línea.",
            "Las mismas herramientas funcionan en scripts con {{bigtext-cli}}, por ejemplo {{bigtext-cli pivot}}.",
        ],
    },
    {
        "slug": "open-large-log-file",
        "title": "Abrir un log de 10 GB sin que se congele — BigText",
        "description": "Abre al instante un log de cualquier tamaño y búscalo, fíltralo y síguelo sin que el editor se "
                       "congele: saltar a una hora, filtrar por nivel, un filtro como grep, tail -F. Gratis, para "
                       "Windows, Linux y macOS.",
        "h1": "Abrir un archivo de log de 10 GB sin que se congele",
        "card": "Ábrelo al instante, salta a una hora, filtra por nivel, quédate con lo importante y síguelo en vivo.",
        "intro": [
            "Notepad++ avisa de que un archivo de más de 2 GB puede tardar minutos en abrirse, VS Code mantiene el "
            "archivo entero en memoria, y ambos pueden congelarse con un log grande. BigText lee solo lo que hay en "
            "pantalla, así que un log de 10 o 50 GB se abre al instante y usa unos 100 MB de memoria.",
        ],
        "steps": [
            ("Ábrelo", "Usa Archivo → Abrir, arrastra el archivo a la ventana o ejecuta {{bigtext app.log}} en una "
             "terminal ({{bigtext app.log:120}} lo abre en la línea 120). Los logs en .gz y .zst se abren directamente."),
            ("Muévete por él", "{{Ctrl+Fin}} va a la última línea al instante. Ir a la línea ({{Ctrl+G}}) funciona antes "
             "de que termine el índice de líneas; hasta entonces los números de línea llevan {{~}}. Ir a la hora "
             "({{Ctrl+Mayús+T}}) acepta {{14:32:05}} y te lleva a la primera línea con esa hora."),
            ("Encuentra los errores", "La barra de niveles sobre un log filtra las líneas ERROR, WARN, INFO y DEBUG, y "
             "{{F8}} salta al siguiente error. {{Ctrl+F}} busca en todo el archivo, con expresiones regulares si quieres, "
             "y «Buscar todo» lista las coincidencias."),
            ("Quédate con lo importante", "La vista de filtro muestra solo las líneas que coinciden, o solo las que no, "
             "como grep sobre todo el archivo, y las guarda en un archivo nuevo."),
            ("Síguelo en vivo", "El modo seguimiento ({{bigtext -f app.log}}, o «Seguir los cambios del archivo (tail)» en "
             "la paleta de comandos) añade las líneas nuevas como {{tail -F}} y sobrevive a la rotación de logs. Un log en "
             "un servidor se abre por SSH con tus claves: {{bigtext -f user@server:/var/log/syslog}}."),
        ],
        "tips": [
            "Se pueden combinar varios logs por tiempo en una sola vista, o desplazar dos logs juntos por tiempo.",
            "La búsqueda, el filtro y el guardado van en segundo plano con barra de progreso y Cancelar, así que la "
            "ventana sigue respondiendo.",
        ],
    },
    {
        "slug": "extract-table-from-mysqldump",
        "title": "Extraer una tabla de un archivo mysqldump — BigText",
        "description": "Encuentra una tabla en un volcado SQL de varios gigabytes y escríbela en su propio archivo en "
                       "pocos pasos, sin cargar el volcado en memoria. Gratis, para Windows, Linux y macOS.",
        "h1": "Extraer una tabla de un archivo mysqldump",
        "card": "Lista las tablas de un volcado SQL enorme y escribe la que necesitas en su propio archivo.",
        "intro": [
            "Un mysqldump completo de una base de datos puede ocupar decenas de gigabytes, y a menudo solo necesitas una "
            "tabla: para restaurarla, revisar sus datos o enviarla a alguien. BigText abre el volcado al instante sin "
            "cargarlo en memoria, y «Exportar líneas…» escribe cualquier rango de líneas en un archivo nuevo.",
        ],
        "steps": [
            ("Abre el volcado", "Abre el archivo .sql con Archivo → Abrir o arrastrándolo a la ventana. Un volcado "
             ".sql.gz también se abre directamente."),
            ("Lista las tablas", "mysqldump marca cada tabla con un comentario como "
             "{{-- Table structure for table `orders`}}. Busca {{-- Table structure for table}} y pulsa «Buscar todo»: "
             "los resultados listan todas las tablas del volcado con su número de línea."),
            ("Encuentra dónde empieza y termina la tabla", "Haz clic en la tabla que necesitas en los resultados; su "
             "número de línea aparece en la barra de estado. Termina donde empieza el comentario de la siguiente tabla: "
             "{{F3}} salta a él."),
            ("Escríbela en un archivo", "Archivo → Exportar líneas… escribe un rango de líneas en un archivo nuevo: desde "
             "la primera línea de la tabla hasta la línea anterior a la siguiente tabla. Obtienes un .sql pequeño con la "
             "sentencia CREATE TABLE y las líneas INSERT de la tabla."),
        ],
        "tips": [
            "El volcado empieza con líneas SET (juego de caracteres, comprobación de claves foráneas). Cópialas al "
            "principio del archivo nuevo si la tabla las necesita.",
            "Un volcado hecho con {{--skip-comments}} no tiene esos comentarios: busca {{CREATE TABLE `orders`}} y el "
            "siguiente {{DROP TABLE IF EXISTS}}.",
            "Solo los datos: la vista de filtro con {{INSERT INTO `orders`}} muestra solo las líneas INSERT de esa tabla y "
            "las guarda en un archivo nuevo.",
        ],
    },
]
