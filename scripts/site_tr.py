"""Website texts in Turkish (see build-site.py; the keys are those of T["en"])."""

CODE, NAME, LOCALE = "tr", "Türkçe", "tr_TR"

T = {
    "title": "BigText — devasa metin ve log dosyaları için ücretsiz düzenleyici (1–50+ GB)",
    "description": "1 GB, 10 GB veya 50 GB metin ve log dosyalarını anında açın, arayın ve düzenleyin. Windows, Linux "
    "ve macOS için EmEditor ve UltraEdit'e ücretsiz, açık kaynaklı ve asla donmayan bir alternatif.",
    "og_title": "BigText — 50 GB metin dosyalarını anında açın",
    "skip": "İçeriğe geç",
    "nav": ["Özellikler", "Performans", "İndir", "SSS"],
    "h1": "50 GB metin dosyalarını anında açın",
    "sub": "Ücretsiz ve açık kaynaklı; Windows, Linux ve macOS için. Boyutu ne olursa olsun donma yok, bekleme yok.",
    "lead": "BigText; log dosyaları, CSV dışa aktarımları, veritabanı dökümleri ve sıradan düzenleyiciler için fazla büyük "
    "her dosya için bir düzenleyici ve EmEditor ile UltraEdit'e ücretsiz bir alternatif. Dosyayı asla tamamen belleğe "
    "yüklemez: 10 GB'lık bir log milisaniyeler içinde açılır ve yaklaşık 100 MB RAM kullanır.",
    "cta": "Ücretsiz indir",
    "cta2": "GitHub'da kaynak kod",
    "note": "Windows · Linux · macOS · MIT lisansı · Rust ile yazıldı",
    "demo_alt": "BigText 50 GB'lık bir logu anında açar, sonuna ve ortasına atlar, eşleşmeler gelirken düzenli ifadeyle "
                "arar ve büyüyen bir logu izler.",
    "stats": [
        ("41 ms", "50 GB'lık bir dosyanın ilk ekranına kadar"),
        ("2,5 GB/s", "metin arama hızı"),
        ("107 MB", "50 GB'lık dosya açıkken RAM"),
        ("< 3 ms", "indekslenirken arayüz kare süresi"),
    ],
    "features_h2": "Dev dosyalar için ihtiyacınız olan her şey",
    "features_intro": "Loglar, CSV ve SQL dökümleri, JSON dışa aktarımları ve diğer düzenleyicileri kilitleyen her metin dosyası için yapıldı.",
    "features": [
        ("Her boyutta dosyayı açar", "Yalnızca görünen kısım, belleğe eşlenmiş pencereler üzerinden okunur. Arka plandaki "
         "iş geri kalanını akış olarak işler; ağ veya USB sürücüler önbellekli parça okumalara geçer, böylece yavaş bir "
         "disk pencereyi asla kilitlemez."),
        ("Anında satıra git", "Seyrek bir satır indeksi (her 64 KB için 8 bayt) arka planda 2,5 GB/s hızla oluşturulur. "
         "Satıra git ve kaydırma çubuğu, indeksleme bitmeden bile hemen çalışır."),
        ("Hızlı arama ve regex", "Düz metin 2,5 GB/s hızla, düzenli ifadeler doğrusal sürede; büyük/küçük harf, tam "
         "sözcük ve çok satır seçenekleriyle. “Tümünü bul” bir milyona kadar sonucu listeler; F3 hemen çalışır."),
        ("Gigabaytlar boyunca değiştir", "$1 ve adlandırılmış gruplarla değiştirin. 50.000'e kadar değişiklik geri "
         "alınabilir tek bir düzenlemedir; daha büyük işler yeni bir dosyaya akış olarak yazılır: 10 GB'lık bir dosyada "
         "22 milyon değiştirme 11 s sürdü ve 46 MB RAM kullandı."),
        ("Gerçek düzenleme", "Yazma, kesme, kopyalama, yapıştırma, geri alma ve yineleme, satır çoğaltma veya silme, "
         "otomatik girinti; bir piece table üzerinde, böylece düzenlemeler siz kaydedene kadar dosyayı yeniden yazmaz."),
        ("Çökmeye dayanıklı kaydetme", "Kaydetme geçici bir dosya yazar, diske boşaltır ve orijinalin yerine atomik "
         "olarak koyar. Bir çökme veya elektrik kesintisi asla yarım yazılmış bir dosya bırakmaz. İsteğe bağlı .bak yedekleri."),
        ("Kodlamalar ve hex görünümü", "UTF-8, UTF-16 LE/BE, Windows-1251, Windows-1252 ve ISO-8859-1 otomatik algılanır; "
         "LF, CRLF ve CR satır sonları korunur ya da dönüştürülür. İkili dosyalar hex görünümünde açılır."),
        ("Loglar için araçlar", "Büyüyen loglar için izleme modu (tail -f gibi); loglar, JSON, XML, CSV, SQL ve INI için "
         "sözdizimi vurgulama, yer imleri ve bir dosyanın bölünmüş görünümü."),
        ("Devasa dosyaları karşılaştırın", "Çok gigabaytlık iki dosyanın akış hâlinde satır karşılaştırması: 1 GB'a karşı "
         "1 GB 0,6 s'de, yaklaşık 2 MB bellekle. Bir farka tıklayınca oraya gidilir."),
        ("Filtrele ve vurgula", "Yalnızca eşleşen satırları ya da yalnızca eşleşmeyenleri, tüm dosyada grep gibi gösterin "
         "ve yeni bir dosyaya kaydedin. ERROR gibi kalıpları kendi renkleriyle boyayın ve eşleşmelerin kaydırma "
         "çubuğunda nerede yoğunlaştığını görün."),
        ("Bir zamana atla", "14:32:05 yazın ve o zamandaki ilk log satırına gidin: ikili arama 50 GB'lık bir logun yalnızca "
         "birkaç yüz kilobaytını okur. Aralarındaki süreyi görmek için satırları seçin."),
        ("Sıkıştırılmış loglar ve borular", ".gz ve .zst dosyalarını doğrudan açın, bir komutu BigText'e borulayın "
         "(journalctl -f | bigtext -) ya da komut satırından app.log:120 ile bir satırda açın."),
        ("CSV tabloları", "CSV ve TSV dosyaları metin kadar hızlı kayan bir tablo olarak açılır. Bir sütuna göre sıralayın, "
         "sütun istatistiklerini görün, bir sütuna göre filtreleyin; sütunları yeniden adlandırın, taşıyın, dışa aktarın "
         "veya silin."),
        ("Metin araçları", "Satırları sıralayın, tekrarları kaldırın ve ters çevirin, büyük/küçük harfi değiştirin, boşlukları "
         "kırpın, JSON'u biçimlendirin veya küçültün ve Alt+sürükle sütun seçimiyle birçok satırı aynı anda düzenleyin."),
        ("Kendi tarzınızda", "Bir komut paleti (Ctrl+Shift+P), değiştirilebilir kısayollar, herhangi bir eş aralıklı yazı "
         "tipi ve İngilizce, Rusça, Özbekçe, Çince, İspanyolca, Portekizce, Almanca veya Türkçe arayüz."),
        ("Log analizi", "ERROR, WARN, INFO ve DEBUG filtreli bir seviye çubuğu ve sonraki hataya F8, bir zaman histogramı, "
         "log alanları, mesaj kalıpları, en sık değerler, zaman boşlukları ve her eşleşmenin çevresindeki ya da aynı "
         "istek kimliğini taşıyan satırlar."),
        ("Zamana göre yan yana loglar", "Birkaç logu zaman damgasına göre tek bir renkli görünümde birleştirin ya da açık "
         "logları hizalayıp zamana göre birlikte kaydırın; saat kayması ayarlanabilir. İzleme, log döndürmeyi atlatır."),
        ("JSON ve XML", "Bir yapı paneli, imlecin JSON yolu durum çubuğunda, katlama, sorgulu tablo olarak JSON Lines, hatayı "
         "gösteren katı bir denetim, JSON, JSON Lines ve logfmt arasında dönüştürme ve yapısal karşılaştırma."),
        ("Hesap yapan tablolar", "Toplamlarla gruplama, pivot tablolar, iki dosyanın bir anahtarla birleştirilmesi, "
         "hesaplanan sütunlar, sütun grafikleri, sabitlenmiş ve gizli sütunlar, satır denetimleri ve JSON, Markdown veya "
         "Excel'e dışa aktarma."),
        ("İstediğiniz gibi karşılaştırın", "Yan yana ya da birleşik diff olarak, sonraki ve önceki fark; boşlukları, "
         "büyük/küçük harfi veya zaman damgalarını yok sayma; yalnızca bir dosyada olan satırlar, anahtara göre CSV "
         "karşılaştırma ve yamaların uygulanması."),
        ("Arşivler ve büyük düzenlemeler", "zip ve tar arşivlerindeki dosyalar, açılırken gösterilen .xz ve .bz2 dosyaları, "
         "doğrudan .gz veya .zst olarak kaydetme, çoklu imleç, bir sütun düzenleyici ve her boyutta dosyada sıralama, "
         "tekrar kaldırma veya herhangi bir komutu çalıştırma."),
        ("Dosyalarda bul ve betikler", "Bütün bir klasörü dosya:satır sonuçlarıyla arayın; betiklerde count, head, tail, "
         "slice, replace, CSV araçları ve JSON çıktısı için bigtext-cli kullanın. Oturumlar, taşınabilir mod ve ikinci "
         "bir başlatma çalışan pencerede açılır."),
        ("Herkes için rahat", "Yüksek karşıtlıklı ve renk körlüğüne uygun temalar, arayüz ölçekleme, ekran okuyucu desteği, "
         "zen modu, cetvelli bir sütunda kaydırma, tıklanabilir bir durum çubuğu ve her dosya için hatırlanan ayarlar."),
        ("Rahat bir kod düzenleyici de", "Python, JavaScript, TypeScript, Java, C#, C/C++, Go, Rust, PHP, kabuk betikleri, "
         "YAML ve daha fazlası için One Dark, Monokai, GitHub veya Solarized renklerinde sözdizimi renkleri; Sublime "
         "Text'teki gibi bir klasör kenar çubuğu; dosya bulmak için Ctrl+P, bir dosyanın işlev ve sınıfları için Ctrl+R, "
         "satırları yorum yapmak için Ctrl+/ ve kendiliğinden kapanan parantezler. Yalnızca ekrandaki satırlar renklenir, "
         "böylece devasa bir dosya da aynı hızda kalır."),
        ("Sunuculardaki loglar", "user@server:/var/log/app.log'u anahtarlarınızla SSH üzerinden ya da bir https:// veya herkese açık s3:// "
         "dosyasını indirmeden açın: yalnızca ekrandaki kısım gelir, SSH üzerinden aramalar sunucuda çalışır. "
         "Ya da bir logun son megabaytlarını tail -F gibi izleyin."),
        ("Web sunucusu logları", "nginx ve Apache erişim logları, AWS yük dengeleyici ve IIS logları ile syslog, her alan için bir sütunu olan bir tablo olarak açılır. Web log raporu tek geçişte en çok istenen URL'leri, dakika başına 5xx hatalarını ve en yavaş istekleri verir: 1 GB'lık bir log 2 saniyeden kısa sürede."),
    ],
    "gallery_h2": "Ekran görüntüleri",
    "gallery": {
        "search": "10 GB'lık bir logda regex ile “Tümünü bul”: 4 s'de 166 eşleşme (2,5 GB/s).",
        "compare": "48 MB'lık iki logun karşılaştırılması: farklar 0,04 s'de bulundu; birine tıklayınca oraya gidilir.",
        "hex": "10 GB'lık bir dosyanın ortasında, 0x140000000 konumunda hex görünümü.",
        "json": "JSON vurgulamalı açık tema, bölünmüş görünüm ve yer imleri.",
    },
    "how_h2": "BigText nasıl hızlı kalıyor",
    "how_intro": "Sıradan düzenleyiciler bir şey göstermeden önce dosyanın tamamını okur. BigText yalnızca ekrandakini okur "
    "ve geri kalan her şeyi arka plan iş parçacıklarında yapar, bu yüzden pencere 3 ms'den kısa sürede yanıt verir.",
    "how": [
        ("Belleğe eşlenmiş pencereler", "Dosya 256 MB'lık pencerelerle eşlenir ve yalnızca baktığınız baytlara dokunulur. "
         "Arama, indeksleme ve kaydetme ayrı akış okumaları kullanır."),
        ("Seyrek satır indeksi", "Her 64 KB için 8 baytlık bir kontrol noktası: 50 GB'lık bir dosya için 8 MB indeks. "
         "Kontrol noktaları arasındaki satır konumları gerektiğinde SIMD ile sayılır."),
        ("Piece table", "Düzenlemeler orijinal dosyanın üzerinde küçük parçalar olarak saklanır; 50 GB'lık bir dosyaya bir "
         "satır eklemek birkaç bayta mal olur ve geri alma anındadır."),
        ("Atomik kaydetme", "Orijinalin yanında geçici bir dosya, fsync, ardından atomik bir değiştirme (Windows'ta "
         "ReplaceFileW öznitelikleri ve izinleri korur)."),
    ],
    "perf_h2": "Ölçülen performans",
    "perf_intro": "Windows 11'de 1 GB, 10 GB ve 50 GB'lık test dosyalarıyla yerleşik kıyaslama sonuçları. Yöntem ve "
    "sonuçların tamamı README'de.",
    "perf_cols": ("Görev", "Sonuç"),
    "perf": [
        ("Aç → ilk metin ekranı (1–50 GB)", "41–46 ms"),
        ("50 GB'lık dosyada bir satıra atlama", "1 ms'nin altında"),
        ("Satır indeksini oluşturma", "2,2–2,7 GB/s (50 GB yaklaşık 20 s'de)"),
        ("Düz metin arama", "2,4–2,5 GB/s"),
        ("Regex ile arama", "sabit kısımlı 2,3–2,8 GB/s, kısımsız 672 MB/s"),
        ("10 GB'lık dosyada tümünü değiştir", "11,3 s, en fazla 46 MB bellek"),
        ("50 GB'lık dosya açıkken bellek", "107 MB"),
        ("Başlatma → ilk kare", "159–185 ms"),
    ],
    "dl_h2": "BigText'i indirin",
    "dl_intro": "Kişisel ve ticari kullanım için ücretsiz. Hesap yok, telemetri yok, tek bir çalıştırılabilir dosya.",
    "dl": [
        ("Windows 10 / 11", "“Birlikte aç” ve Gezgin bağlam menüsü desteğiyle yükleyici ya da taşınabilir sürüm. Programlar "
         "imzalı değil: SmartScreen sorarsa “Ek bilgi” → “Yine de çalıştır”ı seçin.",
         [("Yükleyici (.exe)", "bigtext-setup-windows-x64.exe"), ("Taşınabilir (.zip)", "bigtext-windows-x64.zip")]),
        ("Linux x64", "Çoğu dağıtımda çalışan bir AppImage, Debian ve Ubuntu için bir .deb paketi ya da taşınabilir bir .tar.gz.",
         [("AppImage", "bigtext-linux-x64.AppImage"), (".deb", "bigtext-linux-x64.deb"), (".tar.gz", "bigtext-linux-x64.tar.gz")]),
        ("macOS", "Apple Silicon ve Intel için BigText.app. Uygulama imzalı değil: ilk açılışta sağ tık → Aç.",
         [("Apple Silicon", "bigtext-macos-arm64.app.zip"), ("Intel", "bigtext-macos-x64.app.zip")]),
    ],
    "dl_all": "Tüm sürümler ve sürüm notları",
    "dl_pm": "Paket yöneticisiyle kurun",
    "dl_pm_scoop": "Windows, Scoop ile:",
    "dl_pm_brew": "macOS ve Linux, Homebrew ile: komut satırı sürümü ya da --cask ile uygulama:",
    "dl_source": "Cargo ile kurun",
    "dl_source_text": "Rust'ı rustup.rs'den kurun, ardından çalıştırın:",
    "dl_source_text2": "Ya da kaynaktan derleyin:",
    "faq_h2": "Sık sorulan sorular",
    "faq": [
        ("50 GB'lık bir metin dosyasını nasıl açarım?",
         "BigText'i kurun ve dosyayı Dosya → Aç ile, pencereye sürükleyerek ya da sağ tık → “Open with BigText” ile açın. "
         "İlk ekran, boyut ne olursa olsun yaklaşık 40 ms'de görünür; satır indeksi arka planda oluşturulurken kaydırma ve "
         "satıra git çalışır."),
        ("BigText, EmEditor veya UltraEdit'e bir alternatif mi?",
         "Büyük dosyalar için evet. Onlar gibi BigText de çok gigabaytlık dosyaları belleğe yüklemeden açar, arar, düzenler "
         "ve kaydeder; onlardan farklı olarak ücretsiz ve açık kaynaklıdır ve Windows, Linux ve macOS'ta çalışır. Makro "
         "veya eklentisi yok: emeğini devasa dosyalara, loglara ve tablolara veriyor."),
        ("BigText dosyanın tamamını belleğe yükler mi?",
         "Hayır. Yalnızca ekrandaki kısmı eşler ve geri kalan her şeyi arka planda akış olarak işler. 50 GB'lık bir dosya "
         "açıkken yaklaşık 107 MB RAM kullanır; bunun çoğu arayüzün kendisidir."),
        ("Çok büyük dosyaları düzenleyip kaydedebilir miyim?",
         "Evet. Düzenlemeler orijinal dosyanın üzerindeki bir piece table'da tutulur, bu yüzden yazmak her boyutta anında "
         "olur. Kaydetme, sonucu geçici bir dosyaya yazar ve orijinalin yerine atomik olarak koyar; bir şeyler ters "
         "giderse orijinal asla zarar görmez."),
        ("Devasa dosyalarda arama ne kadar hızlı?",
         "Düz metin 2,4–2,5 GB/s, tipik düzenli ifadeler 2,3–2,8 GB/s hızla aranır; yani 10 GB'lık bir log yaklaşık "
         "4 saniye sürer. F3, tam bir aramayı beklemeden sonraki eşleşmeyi hemen bulur."),
        ("Büyüyen bir logu tail -f gibi izleyebilir mi?",
         "Evet. Görünüm → İzle, görünümü dosyanın sonunda tutar ve yeni satırları geldikçe indeksler. BigText bir dosyanın "
         "diskte değiştiğini de fark eder ve yeniden yüklemeyi önerir."),
        ("Küçültülmüş JSON gibi tek bir devasa satırdan oluşan dosyayı açabilir mi?",
         "Evet. 64 KB'tan uzun satırlar yumuşak bölünmüş satırlar olarak gösterilir, böylece tek bir 2 GB'lık satır bile "
         "akıcı kayar."),
        ("Hangi kodlamalar ve satır sonları destekleniyor?",
         "UTF-8, UTF-16 LE/BE (BOM'lu veya BOM'suz), Windows-1251, Windows-1252 ve ISO-8859-1; otomatik algılanır ve "
         "Dosya → Kodlamayla yeniden aç ile değiştirilebilir. LF, CRLF, CR ve karışık satır sonları algılanır ve "
         "kaydederken dönüştürülebilir."),
        ("BigText ücretsiz mi ve hangi sistemlerde çalışır?",
         "BigText, MIT lisansıyla ücretsiz ve açık kaynaklıdır. Windows 10 ve 11 ana ve tam test edilen platformdur; "
         "Linux ve macOS (Apple Silicon ve Intel) sürümleri her değişiklikte CI tarafından derlenir ve test edilir."),
    ],
    "privacy": "Gizlilik",
    "privacy_items": [
        "Güncelleme denetimi: yalnızca siz istediğinizde (Yardım → Güncellemeleri denetle…) ya da BigText ilk açıldığında "
        "otomatik denetimi kabul ettiyseniz günde bir kez. İstek GitHub API'sine gider ve BigText'in sürümünü taşır; "
        "sizin ya da dosyalarınız hakkında hiçbir şey içermez.",
        "Uzak dosyalar: BigText yalnızca yazdığınız sunucuya veya adrese bağlanır.",
        "Telemetri, kullanım istatistiği veya çökme raporu yok.",
    ],
    "made_by": "Geliştiren:",
    "license": "MIT lisansı",
    "lang_label": "Dil",
    "nf_title": "Sayfa bulunamadı",
    "nf_text": "Bu sayfa mevcut değil.",
    "nf_home": "BigText ana sayfasına git",
}

LABELS = {
    "guides_h2": "Rehberler",
    "guides_intro": "En çok sorulan dosyalar için adım adım.",
    "steps": "Adım adım",
    "tips": "Bilmekte fayda var",
    "cta": "BigText'i ücretsiz indirin",
    "more": "Diğer rehberler",
    "home": "BigText ana sayfası",
}

GUIDES = [
    {
        "slug": "open-csv-larger-than-excel",
        "title": "1.048.576 satırdan büyük CSV açma (Excel sınırı) — BigText",
        "description": "Excel 1.048.576 satırda durur. CSV'nin tamamını, boyutu ne olursa olsun, BigText'te ücretsiz "
                       "olarak tablo şeklinde açın: sıralayın, filtreleyin, gruplayın veya Excel'in yükleyebileceği "
                       "parçalara bölün.",
        "h1": "Excel'in 1.048.576 satırından büyük bir CSV dosyasını açma",
        "card": "Excel 1.048.576 satırda durur. Dosyanın tamamını tablo olarak açın, analiz edin ya da Excel için bölün.",
        "intro": [
            "Excel'in ızgarası 1.048.576. satırda biter. Daha büyük bir CSV'de verinin fazla büyük olduğunu bildirir ve "
            "sınırı aşan satırlar kaydederseniz kaybolur. Log, sipariş veya veritabanı tablosu dışa aktarımları çoğu "
            "zaman bundan büyüktür.",
            "BigText her boyutta CSV'yi anında tablo olarak açar ve asla belleğe yüklemez: 50 milyon satırlık bir dosya, "
            "elli satırlık bir dosya kadar hızlı kayar. Ücretsizdir ve Windows, Linux ve macOS'ta çalışır.",
        ],
        "steps": [
            ("Dosyayı açın", "BigText'i indirin ve .csv veya .tsv dosyasını Dosya → Aç ile açın ya da pencereye sürükleyin. "
             "Tablo olarak açılır: ayırıcı (virgül, noktalı virgül, sekme veya dikey çizgi) ilk satırlardan bulunur ve ilk "
             "satır sabitlenmiş başlık olur. {{Ctrl+T}} tablo ile metin arasında geçiş yapar."),
            ("Dosyada gezinin", "Kaydırın, bir satıra gitmek için {{Ctrl+G}}'ye ya da tüm dosyada aramak için {{Ctrl+F}}'ye "
             "basın; “Tümünü bul” her eşleşmeyi satırıyla listeler. Hiçbir şey dosyanın yüklenmesini beklemez, çünkü dosya "
             "hiç yüklenmez."),
            ("Excel olmadan analiz edin", "Bir sütun başlığına sağ tıklayın: “İstatistikler…” (satırlar, boş ve farklı "
             "değerler, toplam, ortalama, en küçük, en büyük ve en sık değerler), bu sütuna göre satır filtresi, başka bir "
             "sütunun toplamlarıyla “Satırları bu sütuna göre grupla…” ve “Bu sütuna göre pivotla…”. Sıralama 1 GB'a kadar "
             "dosyalarda çalışır."),
            ("Ya da Excel'e uydurun", "Dosya → Dosyayı böl… CSV'yi {{name.part001.csv}}, {{name.part002.csv}}… gibi, "
             "örneğin 1.000.000'er satırlık parçalara, her zaman satır sonlarında böler. Başlık satırı yalnızca ilk "
             "parçadadır; aracınız gerektiriyorsa diğerlerine kopyalayın. Yalnızca bazı satırlar gerekiyorsa onları "
             "filtreleyin (“Satırları koşullara göre filtrele…”) ve sonucu yeni, daha küçük bir CSV olarak kaydedin."),
        ],
        "tips": [
            "Görünüm → Tablo satırlarını denetle…, alan sayısı yanlış, fazladan tırnaklı veya sayı sütununda metin olan "
            "kayıtları satır numaralarıyla listeler.",
            "Aynı araçlar betiklerde {{bigtext-cli}} ile de çalışır, örneğin {{bigtext-cli pivot}}.",
        ],
    },
    {
        "slug": "open-large-log-file",
        "title": "10 GB'lık log dosyasını donmadan açma — BigText",
        "description": "Her boyutta logu anında açın; düzenleyici donmadan arayın, filtreleyin ve izleyin: bir zamana atlama, "
                       "seviyeye göre filtre, grep benzeri filtre, tail -F. Ücretsiz; Windows, Linux ve macOS için.",
        "h1": "10 GB'lık bir log dosyasını donmadan açma",
        "card": "Anında açın, bir zamana atlayın, seviyeye göre filtreleyin, önemli olanı tutun ve canlı izleyin.",
        "intro": [
            "Notepad++, 2 GB'tan büyük bir dosyanın açılmasının dakikalar sürebileceği uyarısını verir, VS Code dosyanın "
            "tamamını bellekte tutar ve ikisi de büyük bir logda donabilir. BigText yalnızca ekrandakini okur; bu yüzden "
            "10 veya 50 GB'lık bir log anında açılır ve yaklaşık 100 MB bellek kullanır.",
        ],
        "steps": [
            ("Açın", "Dosya → Aç'ı kullanın, dosyayı pencereye sürükleyin ya da terminalde {{bigtext app.log}} çalıştırın "
             "({{bigtext app.log:120}} 120. satırda açar). .gz ve .zst ile sıkıştırılmış loglar doğrudan açılır."),
            ("Gezinin", "{{Ctrl+End}} anında son satıra gider. Satıra git ({{Ctrl+G}}) satır indeksi bitmeden çalışır; o "
             "zamana kadar satır numaraları {{~}} taşır. Zamana git ({{Ctrl+Shift+T}}) {{14:32:05}} alır ve o zamandaki ilk "
             "satıra götürür."),
            ("Hataları bulun", "Logun üstündeki seviye çubuğu ERROR, WARN, INFO ve DEBUG satırlarını filtreler; {{F8}} "
             "sonraki hataya atlar. {{Ctrl+F}} tüm dosyada, isterseniz düzenli ifadelerle arar; “Tümünü bul” eşleşmeleri "
             "listeler."),
            ("Yalnızca önemli olanı tutun", "Filtre görünümü yalnızca eşleşen satırları ya da yalnızca eşleşmeyenleri, tüm "
             "dosyada grep gibi gösterir ve yeni bir dosyaya kaydeder."),
            ("Canlı izleyin", "İzleme modu ({{bigtext -f app.log}} ya da komut paletinde “Dosya değişikliklerini izle "
             "(tail)”) yeni satırları {{tail -F}} gibi ekler ve log döndürmeyi atlatır. Sunucudaki bir log, "
             "anahtarlarınızla SSH üzerinden açılır: {{bigtext -f user@server:/var/log/syslog}}."),
        ],
        "tips": [
            "Birkaç log zamana göre tek bir görünümde birleştirilebilir ya da iki log zamana göre birlikte kaydırılabilir.",
            "Arama, filtre ve kaydetme arka planda ilerleme çubuğu ve İptal düğmesiyle çalışır; pencere yanıt vermeye devam eder.",
        ],
    },
    {
        "slug": "extract-table-from-mysqldump",
        "title": "mysqldump dosyasından tek bir tabloyu çıkarma — BigText",
        "description": "Çok gigabaytlık bir SQL dökümünde bir tabloyu bulun ve birkaç adımda kendi dosyasına yazın; dökümü "
                       "belleğe yüklemeden. Ücretsiz; Windows, Linux ve macOS için.",
        "h1": "mysqldump dosyasından tek bir tabloyu çıkarma",
        "card": "Devasa bir SQL dökümünün tablolarını listeleyin ve ihtiyacınız olanı kendi dosyasına yazın.",
        "intro": [
            "Bir veritabanının tam mysqldump'ı onlarca gigabayt olabilir ve çoğu zaman içinden yalnızca bir tablo gerekir: "
            "geri yüklemek, verisini kontrol etmek ya da birine göndermek için. BigText dökümü belleğe yüklemeden anında "
            "açar ve “Satırları dışa aktar…” herhangi bir satır aralığını yeni bir dosyaya yazar.",
        ],
        "steps": [
            ("Dökümü açın", ".sql dosyasını Dosya → Aç ile ya da pencereye sürükleyerek açın. .sql.gz dökümü de doğrudan açılır."),
            ("Tabloları listeleyin", "mysqldump her tabloyu {{-- Table structure for table `orders`}} gibi bir yorumla "
             "işaretler. {{-- Table structure for table}} arayın ve “Tümünü bul”a basın: sonuçlar dökümdeki tüm tabloları "
             "satır numaralarıyla listeler."),
            ("Tablonun başını ve sonunu bulun", "Sonuçlarda ihtiyacınız olan tabloya tıklayın; satır numarası durum "
             "çubuğundadır. Tablo, sonraki tablonun yorumunun başladığı yerde biter: {{F3}} oraya atlar."),
            ("Bir dosyaya yazın", "Dosya → Satırları dışa aktar… bir satır aralığını yeni bir dosyaya yazar: tablonun ilk "
             "satırından sonraki tablodan önceki satıra kadar. CREATE TABLE ifadesini ve tablonun INSERT satırlarını "
             "içeren küçük bir .sql dosyası elde edersiniz."),
        ],
        "tips": [
            "Döküm SET satırlarıyla (karakter kümesi, yabancı anahtar denetimi) başlar. Tablo gerektiriyorsa bunları yeni "
            "dosyanın başına kopyalayın.",
            "{{--skip-comments}} ile alınmış bir dökümde bu yorumlar yoktur: bunun yerine {{CREATE TABLE `orders`}} ve "
            "sonraki {{DROP TABLE IF EXISTS}} satırını arayın.",
            "Yalnızca veri: {{INSERT INTO `orders`}} ile filtre görünümü yalnızca o tablonun INSERT satırlarını gösterir ve "
            "yeni bir dosyaya kaydeder.",
        ],
    },
]
