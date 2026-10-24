# BigText — bajarish ketma-ketligi va arxitektura rejasi

Manba: [prompt.md](prompt.md). Bu hujjat kod yozishdan oldin tuzilgan reja; har bir
bosqich alohida git commit(lar) bilan yakunlanadi.

## 1. Asosiy qarorlar (default'lar)

| Masala | Qaror | Sabab |
|---|---|---|
| Til / toolchain | Rust stable, faqat 64-bit | Promptdagi tavsiya; bitta mustaqil binar |
| GUI | `egui`/`eframe` 0.36, renderer: **glow** (OpenGL) | wgpu'ga nisbatan kichik binar va tez start; egui `TextEdit` ishlatilmaydi |
| Fayl kirish | `memmap2`; tarmoq/olinadigan disklar yoki mmap xatosida — bufferli chunk o'qish (fon yuklovchi oqim + LRU kesh) | Qoida 1 |
| Qidiruv | `memchr::memmem` (literal), `regex-automata` meta-regex (chiziqli vaqt) | Qoida: linear-time regex; `Input::range` orqali oynali qidiruv |
| Kodirovka | `encoding_rs` jadvallari + o'zimizning UTF-8/UTF-16 dekoderlari | Bayt ↔ belgi xaritasi to'liq nazoratda bo'lishi uchun |
| Fon ishlar | `std::thread` + kanallar + `AtomicBool` bekor qilish tokeni | Oddiy va sinalgan |
| Satr ajratuvchi | Bitta "nl bayti": LF (CRLF ham shu yerga) yoki faqat-CR fayllar uchun CR | `memchr` bilan tez; aralash holat status barda ko'rsatiladi |
| Undo saqlashdan keyin | Saqlash undo tarixini tozalaydi | Eski mmap yopiladi (Windows'da rename uchun shart) |

## 2. Modullar

```
src/
  lib.rs              — yadro kutubxona (GUI'siz, test va benchmarklar uchun)
  jobs.rs             — CancelToken, Progress, Waker (UI'ni uyg'otish)
  file.rs             — Source: Mmap | Chunked | Empty; tarmoq diskini aniqlash; bloklanmaydigan try_read
  encoding.rs         — Encoding, Codec, aniqlash (BOM/UTF-8/UTF-16/1251/1252/Latin-1), nl qidirish/sanash
  index.rs            — LineIndex: har 64 KB'da checkpoint (faqat u64 satr soni), fon indekslovchi
  text/
    buffer.rs         — TextBuf: piece table (Orig mmap + Add bufer), satr↔offset xaritasi, undo/redo
    rows.rs           — vizual "segment"lar (uzun satrlarni 64 KB'da yumshoq bo'lish), wrap
  search.rs           — Query, Matcher (literal/regex/dekodlangan yo'l), oynali oqimli qidiruv
  replace.rs          — shablon ($1, ${name}, \n, \t), kichik to'plam → piece edit, katta → oqimli
  save.rs             — temp faylga oqim, fsync, atomik almashtirish (Windows: ReplaceFileW), .bak
  watcher.rs          — tashqi o'zgarishlarni kuzatish (fon oqimi)
  main.rs             — CLI, eframe ishga tushirish, --bench-ui
  app/                — BigTextApp: tablar, menyular, dialoglar, status bar, qidiruv paneli
  view/               — maxsus virtual matn ko'rinishi va hex ko'rinish
  bin/gen_testfiles.rs— test fayllar generatori
  bin/bench.rs        — yadro benchmarklari (index, goto, search, replace, xotira)
```

## 3. Ma'lumot tuzilmalari

- **LineIndex**: `cp: Vec<u64>`, `cp[k]` = `[0, k*64KB)` oralig'idagi nl soni. 50 GB → ~800k yozuv → 6.4 MB
  (fayl hajmining 0.013%). `line_start(L)`: ikkilik qidiruv + ≤64 KB skan. Indeks tugamagan joylar uchun
  o'rtacha satr uzunligi bo'yicha taxmin ("~" belgisi bilan), keyin aniqlashtiriladi.
- **Piece table**: `Vec<Piece{buf: Orig|Add, start, len, nl, nl_exact}>` + kumulyativ bayt/satr massivlari.
  Undo yozuvi = `Swap{at, removed, inserted}` guruhlari (ketma-ket yozish birlashtiriladi).
- **Vizual qatorlar**: ko'rinish satr raqamiga emas, **bayt offset**ga bog'langan (`top`). Satr > 64 KB bo'lsa,
  u har 64 KB karralisida yumshoq bo'linadi ⇒ har qanday qator hisob-kitobi ≤128 KB o'qish bilan chegaralangan.
  2 GB'lik bitta satr ham muzlatmaydi.
- **Qidiruv natijalari**: `Vec<(u64,u64)>`, 1 000 000 bilan cheklangan, "yana bor" belgisi.

## 4. Oqimlar modeli (threading)

- **UI oqimi**: faqat ko'rinadigan qatorlarni o'qiydi (mmap'dan; chunked rejimda faqat keshdan, yo'q bo'lsa
  "Loading…" va fon yuklash). Har bir kadr ≤16 ms.
- **Ishchi oqimlar**: ochish+aniqlash, indekslash, qidiruv, find-next, replace-all, saqlash, nusxa olish, kuzatuvchi.
  Har birida `CancelToken` + progress; natijalar kanal orqali, UI `request_repaint` bilan uyg'otiladi.
- **Saqlash protokoli**: (1) ishchi snapshot'dan temp faylga yozadi + fsync; (2) UI o'z `Arc<Source>`ini tashlaydi,
  boshqa ishlarni bekor qiladi; (3) yakunlovchi ishchi barcha havolalar yo'qolishini kutadi → ReplaceFileW/rename →
  qayta ochadi. Xato bo'lsa original tegilmagan holda qayta ochiladi va tahrirlar saqlanib qoladi.

## 5. Bosqichlar (milestones) va commitlar

1. **M0 — Skelet**: git, Cargo, reja → `chore: init`.
2. **M1 — Ochish/ko'rish/scroll/go-to**: file, encoding (asosiy), index, TextBuf, rows, virtual ko'rinish,
   tablar, CLI/drag&drop/dialog, Ctrl+G, status bar, test-fayl generatori, bench → o'lchash.
3. **M2 — Qidiruv**: Matcher, oqimli qidiruv, F3/Shift+F3, Find All paneli (virtual ro'yxat), ko'rinishda highlight.
4. **M3 — Replace All**: shablonlar, kichik → piece edit (undo bilan), katta → oqimli temp+atomik almashtirish, preview.
5. **M4 — Tahrirlash + saqlash**: kiritish/o'chirish, clipboard (100 MB+ tasdiq), undo/redo, duplicate/delete line,
   auto-indent, Save/Save As, yopishda so'rash.
6. **M5 — Kodirovkalar, hex, sayqal**: kodirovka menyusi/qayta ochish, hex ko'rinish, EOL konvertatsiya, read-only,
   tashqi o'zgarish, mavzu, zoom, wrap, tail rejimi.
7. **M6 — Yetkazib berish**: GitHub Actions (Win/Linux/macOS), Inno Setup skripti, README (o'lchangan raqamlar bilan).

Har bosqichdan keyin: `cargo test`, `bigtext-bench` test fayllarida, natijalar README'ga.

## 6. Unumdorlik maqsadlari (o'lchanadi)

| Maqsad | Chegara |
|---|---|
| Birinchi ekran | < 200 ms |
| Go-to (indekslangan / indekslanmagan) | < 50 ms / < 500 ms |
| Kadr vaqti fon ishlar paytida | < 16 ms |
| Literal qidiruv | ≥ 1 GB/s |
| Oddiy regex | ≥ 200 MB/s |
| Replace All 10 GB, private xotira | < 300 MB |
| 50 GB fayl ochiq, bo'sh holat private xotira | < 200 MB |
| Startdan birinchi chizishgacha | < 300 ms |
