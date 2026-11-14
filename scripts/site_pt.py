"""Website texts in Brazilian Portuguese (see build-site.py; the keys are those of T["en"])."""

CODE, NAME, LOCALE = "pt", "Português", "pt_BR"

T = {
    "title": "BigText — editor gratuito para arquivos de texto e logs enormes (1–50+ GB)",
    "description": "Abra, pesquise e edite na hora arquivos de texto e logs de 1 GB, 10 GB ou 50 GB. Uma alternativa "
    "gratuita e de código aberto ao EmEditor e ao UltraEdit para Windows, Linux e macOS que nunca trava.",
    "og_title": "BigText — abra arquivos de texto de 50 GB na hora",
    "skip": "Ir para o conteúdo",
    "nav": ["Recursos", "Desempenho", "Download", "Perguntas"],
    "h1": "Abra arquivos de texto de 50 GB na hora",
    "sub": "Gratuito e de código aberto, para Windows, Linux e macOS. Sem travar e sem esperar, seja qual for o tamanho.",
    "lead": "O BigText é um editor para logs, exportações CSV, dumps de bancos de dados e qualquer arquivo grande demais "
    "para os editores comuns, e uma alternativa gratuita ao EmEditor e ao UltraEdit. Ele nunca carrega o arquivo inteiro "
    "na memória: um log de 10 GB abre em milissegundos e usa cerca de 100 MB de RAM.",
    "cta": "Baixar grátis",
    "cta2": "Código no GitHub",
    "note": "Windows · Linux · macOS · licença MIT · escrito em Rust",
    "demo_alt": "O BigText abre na hora um log de 50 GB, pula para o fim e para o meio, pesquisa com uma expressão "
                "regular enquanto os resultados chegam e acompanha um log que cresce.",
    "stats": [
        ("41 ms", "até a primeira tela de um arquivo de 50 GB"),
        ("2,5 GB/s", "velocidade da pesquisa de texto"),
        ("107 MB", "de RAM com um arquivo de 50 GB aberto"),
        ("< 3 ms", "por quadro da interface durante a indexação"),
    ],
    "features_h2": "Tudo o que você precisa para arquivos gigantes",
    "features_intro": "Feito para logs, dumps CSV e SQL, exportações JSON e qualquer arquivo de texto que trava outros editores.",
    "features": [
        ("Abre arquivos de qualquer tamanho", "Só a parte visível é lida: no Windows por janelas mapeadas na memória, no Linux e no macOS em blocos quando necessário. O trabalho em segundo plano processa o resto, e unidades de rede ou USB passam para leituras em blocos com cache, então um disco lento nunca trava a janela."),
        ("Vá para uma linha na hora", "Um índice de linhas esparso (8 bytes a cada 64 KB) é criado em segundo plano a "
         "2,5 GB/s. Ir para a linha e a barra de rolagem funcionam de imediato, mesmo antes de a indexação terminar."),
        ("Pesquisa rápida e regex", "Texto simples a 2,5 GB/s e expressões regulares em tempo linear, com opções de "
         "maiúsculas, palavra inteira e várias linhas. “Localizar todos” lista até um milhão de resultados; F3 funciona "
         "na hora."),
        ("Substituir em gigabytes", "Substitua com $1 e grupos nomeados. Até 50.000 alterações são uma única edição que "
         "pode ser desfeita; trabalhos maiores são gravados num arquivo novo: 22 milhões de substituições num arquivo de "
         "10 GB levaram 11 s e usaram 46 MB de RAM."),
        ("Edição de verdade", "Digitar, recortar, copiar, colar, desfazer e refazer, duplicar ou excluir linhas, recuo "
         "automático, sobre uma piece table, de modo que as edições não reescrevem o arquivo até você salvar."),
        ("Salvamento à prova de falhas", "Ao salvar, um arquivo temporário é gravado, enviado ao disco e substitui o "
         "original de forma atômica. Uma falha ou queda de energia nunca deixa um arquivo pela metade. Cópias .bak opcionais."),
        ("Codificações e visualização hex", "UTF-8, UTF-16 LE/BE, Windows-1251, Windows-1252 e ISO-8859-1 são "
         "detectados automaticamente; finais de linha LF, CRLF e CR são mantidos ou convertidos. Arquivos binários "
         "abrem numa visualização hexadecimal."),
        ("Ferramentas para logs", "Modo acompanhar (como tail -f) para logs que crescem, realce de sintaxe para logs, "
         "JSON, XML, CSV, SQL e INI, marcadores e uma visualização dividida de um arquivo."),
        ("Compare arquivos enormes", "Uma comparação de linhas em streaming de dois arquivos de vários gigabytes: 1 GB "
         "contra 1 GB em 0,6 s com cerca de 2 MB de memória. Clique numa diferença para ir até ela."),
        ("Filtrar e realçar", "Mostre só as linhas que correspondem, ou só as que não, como o grep sobre o arquivo "
         "inteiro, e salve-as num arquivo novo. Pinte padrões como ERROR com cores próprias e veja na barra de rolagem "
         "onde os resultados se concentram."),
        ("Pular para um horário", "Digite 14:32:05 e chegue à primeira linha do log com esse horário: uma busca binária "
         "lê algumas centenas de kilobytes de um log de 50 GB. Selecione linhas para ver o tempo entre elas."),
        ("Logs compactados e pipes", "Abra arquivos .gz e .zst diretamente, envie um comando ao BigText por pipe "
         "(journalctl -f | bigtext -) ou abra app.log:120 numa linha pela linha de comando."),
        ("Tabelas CSV", "Arquivos CSV e TSV abrem como uma tabela que rola tão rápido quanto texto. Ordene por uma coluna, veja estatísticas da coluna, filtre por uma coluna e renomeie, mova, exporte ou exclua colunas. Um registro com quebras de linha entre aspas é uma linha só."),
        ("Ferramentas de texto", "Ordene, remova duplicatas e inverta linhas, mude maiúsculas, apare espaços, formate ou "
         "minifique JSON e edite muitas linhas de uma vez com uma seleção de coluna por Alt+arrastar."),
        ("Do seu jeito", "Uma paleta de comandos (Ctrl+Shift+P), atalhos que você pode mudar, qualquer fonte monoespaçada "
         "e uma interface em inglês, russo, uzbeque, chinês, espanhol, português, alemão ou turco."),
        ("Análise de logs", "Uma barra de níveis com filtros ERROR, WARN, INFO e DEBUG e F8 para o próximo erro, um "
         "histograma de tempo, campos do log, padrões de mensagens, valores mais frequentes, lacunas de tempo e as "
         "linhas em volta de cada resultado ou com o mesmo id de requisição."),
        ("Logs lado a lado no tempo", "Mescle vários logs por data e hora numa visualização colorida, ou alinhe os logs "
         "abertos e role-os juntos pelo horário, com um desvio de relógio ajustável. O acompanhamento sobrevive à "
         "rotação de logs."),
        ("JSON e XML", "Um painel de estrutura, o caminho JSON do cursor na barra de status, dobramento, JSON Lines como "
         "tabela com consultas, uma verificação rigorosa que aponta o erro, conversão entre JSON, JSON Lines e logfmt e "
         "uma comparação estrutural."),
        ("Tabelas que calculam", "Agrupar com somas, tabelas dinâmicas, junção de dois arquivos por uma chave, colunas "
         "calculadas, gráficos de colunas, colunas fixas e ocultas, verificação de linhas e exportação para JSON, "
         "Markdown ou Excel."),
        ("Compare do seu jeito", "Lado a lado ou como diff unificado, diferença seguinte e anterior, ignorar espaços, "
         "maiúsculas ou datas e horas, linhas que só estão em um arquivo, CSV comparado por chave e patches aplicados."),
        ("Arquivos compactados e grandes edições", "Arquivos dentro de zip e tar, arquivos .xz e .bz2 exibidos enquanto "
         "descompactam, salvamento direto em .gz ou .zst, vários cursores, um editor de colunas e ordenar, remover "
         "duplicatas ou executar qualquer comando num arquivo de qualquer tamanho."),
        ("Localizar em arquivos e scripts", "Pesquise uma pasta inteira com resultados arquivo:linha e use o bigtext-cli "
         "para count, head, tail, slice, replace, ferramentas CSV e saída JSON em scripts. Sessões, modo portátil, e uma "
         "segunda execução abre na janela que já está aberta."),
        ("Confortável para todos", "Temas de alto contraste e seguros para daltonismo, escala da interface, suporte a "
         "leitores de tela, modo zen, quebra numa coluna com régua, uma barra de status clicável e configurações "
         "lembradas para cada arquivo."),
        ("Também um editor de código confortável", "Cores de sintaxe para Python, JavaScript, TypeScript, Java, C#, "
         "C/C++, Go, Rust, PHP, scripts de shell, YAML e mais, em One Dark, Monokai, GitHub ou Solarized; uma barra "
         "lateral de pasta como no Sublime Text; Ctrl+P para achar um arquivo, Ctrl+R para as funções e classes de um "
         "arquivo, Ctrl+/ para comentar linhas e colchetes que se fecham sozinhos. Só as linhas na tela são coloridas, "
         "então um arquivo enorme continua igualmente rápido."),
        ("Logs em servidores", "Abra user@server:/var/log/app.log por SSH com suas chaves, ou um arquivo https:// ou s3:// público, "
         "sem baixá-lo: só vem o que está na tela, e via SSH as buscas rodam no servidor. "
         "Ou acompanhe os últimos megabytes de um log como tail -F."),
        ("Logs de servidores web", "Logs de acesso do nginx e do Apache, logs dos balanceadores de carga da AWS e do IIS, e syslog abrem como uma tabela com uma coluna por campo. O relatório do log web traz em uma passada as URLs principais, os erros 5xx por minuto e as requisições mais lentas: um log de 1 GB em menos de 2 segundos."),
        ("Logs de contêineres e serviços", "Escolha em uma lista um contêiner Docker ou Podman, um pod do Kubernetes ou um serviço do systemd, neste computador ou em um servidor via SSH, e abra o log: as últimas linhas ou minutos, acompanhado enquanto cresce. Os reinícios de um pod aparecem, e a execução antes do último reinício, onde está o erro da queda, também abre."),
        ("Alertas de novas linhas", "Dê a um log acompanhado alguns padrões, como ERROR ou uma expressão regular: cada nova linha que corresponde é contada na aba e anunciada com uma notificação do sistema e um som, no máximo uma a cada dez segundos. No Windows, um clique na notificação vai até a linha."),
        ("Tabelas do dump SQL", "As tabelas de um dump do mysqldump, pg_dump ou SQLite com suas linhas e tamanhos: um dump de 1 GB é listado em cerca de um segundo. Salve uma tabela em um arquivo que é restaurado sozinho, ou divida o dump em um arquivo por tabela; na ordem dos nomes, os arquivos restauram o banco inteiro."),
    ],
    "gallery_h2": "Capturas de tela",
    "gallery": {
        "search": "“Localizar todos” com regex num log de 10 GB: 166 resultados em 4 s (2,5 GB/s).",
        "compare": "Comparando dois logs de 48 MB: diferenças encontradas em 0,04 s; clique numa para ir até ela.",
        "hex": "Visualização hexadecimal no meio de um arquivo de 10 GB, no deslocamento 0x140000000.",
        "json": "Tema claro com realce de JSON, uma visualização dividida e marcadores.",
    },
    "how_h2": "Como o BigText continua rápido",
    "how_intro": "Os editores comuns leem o arquivo inteiro antes de mostrar qualquer coisa. O BigText lê só o que está "
    "na tela e faz todo o resto em threads de segundo plano, então a janela responde em menos de 3 ms.",
    "how": [
        ("Janelas mapeadas na memória", "O arquivo é mapeado em janelas de 256 MB e só os bytes que você olha são tocados. Pesquisa, indexação e salvamento usam leituras em streaming separadas. No Linux e no macOS a tela lê blocos de 64 KB, então um arquivo truncado enquanto está aberto não derruba o programa."),
        ("Índice de linhas esparso", "Um ponto de controle de 8 bytes a cada 64 KB: 8 MB de índice para um arquivo de "
         "50 GB. As posições de linha entre pontos de controle são contadas sob demanda com SIMD."),
        ("Piece table", "As edições ficam guardadas como pequenos pedaços sobre o arquivo original, então inserir uma "
         "linha num arquivo de 50 GB custa poucos bytes e desfazer é instantâneo."),
        ("Salvamento atômico", "Um arquivo temporário ao lado do original, fsync e então uma substituição atômica "
         "(ReplaceFileW no Windows mantém atributos e permissões)."),
    ],
    "perf_h2": "Desempenho medido",
    "perf_intro": "Resultados dos benchmarks embutidos com arquivos de teste de 1 GB, 10 GB e 50 GB no Windows 11. O "
    "método e os resultados completos estão no README.",
    "perf_cols": ("Tarefa", "Resultado"),
    "perf": [
        ("Abrir → primeira tela de texto (1–50 GB)", "41–46 ms"),
        ("Pular para uma linha num arquivo de 50 GB", "menos de 1 ms"),
        ("Criar o índice de linhas", "2,2–2,7 GB/s (50 GB em cerca de 20 s)"),
        ("Pesquisa de texto simples", "2,4–2,5 GB/s"),
        ("Pesquisa com regex", "2,3–2,8 GB/s com parte literal, 672 MB/s sem"),
        ("Substituir tudo num arquivo de 10 GB", "11,3 s, 46 MB de pico de memória"),
        ("Memória com um arquivo de 50 GB aberto", "107 MB"),
        ("Início → primeiro quadro", "159–185 ms"),
    ],
    "dl_h2": "Baixar o BigText",
    "dl_intro": "Gratuito para uso pessoal e comercial. Sem conta, sem telemetria, um único executável.",
    "dl": [
        ("Windows 10 / 11", "Instalador com “Abrir com” e menu de contexto do Explorer, ou uma versão portátil. Os "
         "programas não são assinados: se o SmartScreen perguntar, escolha “Mais informações” → “Executar assim mesmo”.",
         [("Instalador (.exe)", "bigtext-setup-windows-x64.exe"), ("Portátil (.zip)", "bigtext-windows-x64.zip")]),
        ("Linux x64", "Um AppImage que roda na maioria das distribuições, um pacote .deb para Debian e Ubuntu ou um "
         ".tar.gz portátil.",
         [("AppImage", "bigtext-linux-x64.AppImage"), (".deb", "bigtext-linux-x64.deb"), (".tar.gz", "bigtext-linux-x64.tar.gz")]),
        ("macOS", "BigText.app para Apple Silicon e Intel. O app não é assinado: na primeira vez, clique com o botão "
         "direito → Abrir.",
         [("Apple Silicon", "bigtext-macos-arm64.app.zip"), ("Intel", "bigtext-macos-x64.app.zip")]),
    ],
    "dl_all": "Todas as versões e notas",
    "dl_pm": "Instalar com um gerenciador de pacotes",
    "dl_pm_scoop": "Windows, com Scoop:",
    "dl_pm_brew": "macOS e Linux, com Homebrew: a versão de linha de comando, ou o app com --cask:",
    "dl_source": "Instalar com Cargo",
    "dl_source_text": "Instale o Rust pelo rustup.rs e execute:",
    "dl_source_text2": "Ou compile a partir do código-fonte:",
    "faq_h2": "Perguntas frequentes",
    "faq": [
        ("Como abro um arquivo de texto de 50 GB?",
         "Instale o BigText e abra o arquivo com Arquivo → Abrir, arrastando-o para a janela ou com clique direito → "
         "“Open with BigText”. A primeira tela aparece em cerca de 40 ms, seja qual for o tamanho; a rolagem e ir para a "
         "linha funcionam enquanto o índice de linhas é criado em segundo plano."),
        ("O BigText é uma alternativa ao EmEditor ou ao UltraEdit?",
         "Para arquivos grandes, sim. Como eles, o BigText abre arquivos de vários gigabytes sem carregá-los na memória "
         "e os pesquisa, edita e salva; diferente deles, é gratuito e de código aberto e roda no Windows, no Linux e no "
         "macOS. Não tem macros nem plug-ins: o foco são arquivos enormes, logs e tabelas."),
        ("O BigText carrega o arquivo inteiro na memória?",
         "Não. Ele mapeia só a parte que está na tela e processa todo o resto em segundo plano. Com um arquivo de 50 GB "
         "aberto, usa cerca de 107 MB de RAM, a maior parte para a própria interface."),
        ("Posso editar e salvar arquivos muito grandes?",
         "Sim. As edições ficam numa piece table sobre o arquivo original, então digitar é instantâneo em qualquer "
         "tamanho. Ao salvar, o resultado é gravado num arquivo temporário que substitui o original de forma atômica, "
         "assim o original nunca é danificado se algo der errado."),
        ("Quão rápida é a pesquisa em arquivos enormes?",
         "Texto simples é pesquisado a 2,4–2,5 GB/s e expressões regulares comuns a 2,3–2,8 GB/s, então um log de 10 GB "
         "leva cerca de 4 segundos. F3 encontra o próximo resultado na hora, sem esperar uma pesquisa completa."),
        ("Ele acompanha um log que cresce, como o tail -f?",
         "Sim. Exibir → Acompanhar mantém a visualização no fim do arquivo e indexa as linhas novas conforme chegam. O "
         "BigText também percebe quando um arquivo muda no disco e oferece recarregá-lo."),
        ("Ele abre um arquivo que é uma única linha enorme, como um JSON minificado?",
         "Sim. Linhas com mais de 64 KB são mostradas quebradas em linhas visuais, então até uma única linha de 2 GB "
         "rola com suavidade."),
        ("Quais codificações e finais de linha são suportados?",
         "UTF-8, UTF-16 LE/BE (com ou sem BOM), Windows-1251, Windows-1252 e ISO-8859-1, detectados automaticamente e "
         "trocáveis em Arquivo → Reabrir com codificação. Finais LF, CRLF, CR e mistos são detectados e podem ser "
         "convertidos ao salvar."),
        ("O BigText é gratuito e em quais sistemas roda?",
         "O BigText é gratuito e de código aberto sob a licença MIT. Windows 10 e 11 é a plataforma principal, testada "
         "por completo; as versões para Linux e macOS (Apple Silicon e Intel) são compiladas e testadas na CI a cada mudança."),
    ],
    "privacy": "Privacidade",
    "privacy_items": [
        "Verificação de atualizações: só quando você pede (Ajuda → Verificar atualizações…), ou uma vez por dia se você "
        "aceitou as verificações automáticas na primeira vez que o BigText abriu. A requisição vai para a API do GitHub "
        "e leva a versão do BigText; nada sobre você ou seus arquivos.",
        "Arquivos remotos: o BigText se conecta apenas ao servidor ou endereço que você digita.",
        "Sem telemetria, estatísticas de uso ou relatórios de falhas.",
    ],
    "made_by": "Feito por",
    "license": "licença MIT",
    "lang_label": "Idioma",
    "nf_title": "Página não encontrada",
    "nf_text": "Esta página não existe.",
    "nf_home": "Ir para a página inicial do BigText",
}

LABELS = {
    "guides_h2": "Guias",
    "guides_intro": "Passo a passo, para os arquivos sobre os quais mais se pergunta.",
    "steps": "Passo a passo",
    "tips": "Bom saber",
    "cta": "Baixar o BigText grátis",
    "more": "Mais guias",
    "home": "Início do BigText",
}

GUIDES = [
    {
        "slug": "open-csv-larger-than-excel",
        "title": "Abrir um CSV com mais de 1.048.576 linhas (o limite do Excel) — BigText",
        "description": "O Excel para em 1.048.576 linhas. Abra o CSV inteiro, de qualquer tamanho, como tabela no "
                       "BigText, grátis: ordene, filtre, agrupe ou divida em partes que o Excel consegue carregar.",
        "h1": "Abrir um arquivo CSV com mais linhas que as 1.048.576 do Excel",
        "card": "O Excel para em 1.048.576 linhas. Abra o arquivo inteiro como tabela, analise-o ou divida-o para o Excel.",
        "intro": [
            "A grade do Excel termina na linha 1.048.576. Com um CSV maior ele avisa que os dados são grandes demais, e "
            "as linhas além do limite se perdem se você salvar. Exportações de logs, pedidos ou tabelas de bancos de "
            "dados costumam ser maiores.",
            "O BigText abre um CSV de qualquer tamanho como tabela na hora e nunca o carrega na memória: um arquivo de "
            "50 milhões de linhas rola tão rápido quanto um de cinquenta. É gratuito e roda no Windows, no Linux e no macOS.",
        ],
        "steps": [
            ("Abra o arquivo", "Baixe o BigText e abra o .csv ou .tsv com Arquivo → Abrir, ou arraste-o para a janela. "
             "Ele abre como tabela: o separador (vírgula, ponto e vírgula, tabulação ou barra vertical) é detectado nas "
             "primeiras linhas e a primeira linha vira um cabeçalho fixo. {{Ctrl+T}} alterna entre tabela e texto."),
            ("Navegue pelo arquivo", "Role, pressione {{Ctrl+G}} para ir a uma linha ou {{Ctrl+F}} para pesquisar no "
             "arquivo inteiro; “Localizar todos” lista cada resultado com sua linha. Nada espera o arquivo carregar, "
             "porque ele nunca é carregado."),
            ("Analise sem o Excel", "Clique com o botão direito no cabeçalho de uma coluna: “Estatísticas…” (linhas, "
             "valores vazios e distintos, soma, média, mínimo, máximo e os valores mais frequentes), filtrar linhas por "
             "essa coluna, “Agrupar linhas por esta coluna…” com somas de outra coluna e “Tabela dinâmica por esta "
             "coluna…”. Ordenar funciona em arquivos de até 1 GB."),
            ("Ou ajuste para o Excel", "Arquivo → Dividir arquivo… corta o CSV em {{name.part001.csv}}, {{name.part002.csv}}… com um número de linhas definido, como 1.000.000, sempre entre registros (um campo entre aspas com quebras de linha fica inteiro), e “Repetir a linha de cabeçalho em cada parte” põe os nomes das colunas no início de cada uma. Para ficar só com algumas linhas, filtre-as (“Filtrar linhas por condições…”) e salve o resultado como um CSV novo e menor."),
        ],
        "tips": [
            "Exibir → Verificar linhas da tabela… lista os registros com o número errado de campos, uma aspa solta ou "
            "texto numa coluna numérica, com seus números de linha.",
            "As mesmas ferramentas funcionam em scripts com o {{bigtext-cli}}, por exemplo {{bigtext-cli pivot}}.",
        ],
    },
    {
        "slug": "open-large-log-file",
        "title": "Abrir um log de 10 GB sem travar — BigText",
        "description": "Abra na hora um log de qualquer tamanho e pesquise, filtre e acompanhe sem o editor travar: "
                       "pular para um horário, filtrar por nível, um filtro como o grep, tail -F. Grátis, para "
                       "Windows, Linux e macOS.",
        "h1": "Abrir um arquivo de log de 10 GB sem travar",
        "card": "Abra na hora, pule para um horário, filtre por nível, fique só com o que importa e acompanhe ao vivo.",
        "intro": [
            "O Notepad++ avisa que um arquivo com mais de 2 GB pode levar minutos para abrir, o VS Code mantém o arquivo "
            "inteiro na memória, e os dois podem travar com um log grande. O BigText lê só o que está na tela, então um "
            "log de 10 ou 50 GB abre na hora e usa cerca de 100 MB de memória.",
        ],
        "steps": [
            ("Abra", "Use Arquivo → Abrir, arraste o arquivo para a janela ou execute {{bigtext app.log}} num terminal "
             "({{bigtext app.log:120}} abre na linha 120). Logs em .gz e .zst abrem diretamente."),
            ("Navegue", "{{Ctrl+End}} vai para a última linha na hora. Ir para a linha ({{Ctrl+G}}) funciona antes de o "
             "índice de linhas terminar; até lá os números de linha levam um {{~}}. Ir para o horário "
             "({{Ctrl+Shift+T}}) aceita {{14:32:05}} e leva à primeira linha com esse horário."),
            ("Encontre os erros", "A barra de níveis acima de um log filtra as linhas ERROR, WARN, INFO e DEBUG, e "
             "{{F8}} pula para o próximo erro. {{Ctrl+F}} pesquisa o arquivo inteiro, com expressões regulares se "
             "quiser, e “Localizar todos” lista os resultados."),
            ("Fique só com o que importa", "A visualização de filtro mostra só as linhas que correspondem, ou só as que "
             "não, como o grep sobre o arquivo inteiro, e as salva num arquivo novo."),
            ("Acompanhe ao vivo", "O modo acompanhar ({{bigtext -f app.log}}, ou “Acompanhar alterações do arquivo (tail)” "
             "na paleta de comandos) adiciona as linhas novas como o {{tail -F}} e sobrevive à rotação de logs. Um log "
             "num servidor abre por SSH com suas chaves: {{bigtext -f user@server:/var/log/syslog}}."),
        ],
        "tips": [
            "Vários logs podem ser mesclados por horário numa só visualização, ou dois logs rolados juntos pelo horário.",
            "Pesquisa, filtro e salvamento rodam em segundo plano com barra de progresso e Cancelar, então a janela "
            "continua respondendo.",
        ],
    },
    {
        "slug": "extract-table-from-mysqldump",
        "title": "Extrair uma tabela de um arquivo mysqldump — BigText",
        "description": "Liste as tabelas de um dump SQL de vários gigabytes e salve a que você precisa em um "
                       "arquivo que é restaurado sozinho, sem carregar o dump na memória. Grátis, para Windows, "
                       "Linux e macOS.",
        "h1": "Extrair uma tabela de um arquivo mysqldump",
        "card": "Liste as tabelas de um dump SQL enorme e salve a que você precisa em um arquivo que é restaurado "
                "sozinho.",
        "intro": [
            "Um mysqldump completo de um banco de dados pode ter dezenas de gigabytes, e muitas vezes você precisa "
            "de uma tabela só: para restaurá-la, conferir os dados ou enviá-la a alguém. O BigText abre o dump na "
            "hora sem carregá-lo na memória, lista as tabelas em uma única passada e grava a que você precisa em "
            "um arquivo próprio, com o que ela precisa para ser restaurada.",
        ],
        "steps": [
            ("Abra o dump", "Abra o arquivo .sql com Arquivo → Abrir ou arrastando-o para a janela. Um dump "
             ".sql.gz também abre diretamente."),
            ("Liste as tabelas", "“Arquivo → Tabelas do dump SQL…” lê o dump uma vez, a cerca de um gigabyte por "
             "segundo, e lista as tabelas com suas linhas e tamanhos, na ordem do dump ou as maiores primeiro. "
             "Para achar uma tabela entre centenas, digite parte do nome no filtro."),
            ("Confira a tabela", "Clique em uma tabela para ir ao CREATE TABLE dela e ver as colunas."),
            ("Salve em um arquivo", "Clique com o botão direito na tabela e escolha “Salvar esta tabela como…”. O "
             "arquivo recebe os comandos DROP e CREATE da tabela, suas linhas, chaves, índices e gatilhos, e as "
             "configurações do início e do fim do dump, então é restaurado sozinho: {{mysql shop < orders.sql}}."),
        ],
        "tips": [
            "Várias tabelas: marque-as na lista e clique em “Salvar as tabelas marcadas em um arquivo…”. “Abrir "
            "esta tabela em uma nova aba” mostra uma tabela sem salvá-la.",
            "“Dividir em um arquivo por tabela…” grava um arquivo por tabela, um para o que vem antes das tabelas "
            "e outro para as views, rotinas e chaves estrangeiras que vêm depois. Na ordem dos nomes, eles "
            "restauram o dump inteiro: {{for f in shop.*.sql; do mysql shop < \"$f\"; done}}.",
            "Dumps do pg_dump em formato texto e arquivos {{.dump}} do SQLite funcionam do mesmo jeito. Em "
            "scripts: {{bigtext-cli sqldump shop.sql extract orders -o orders.sql}}.",
        ],
    },
]
