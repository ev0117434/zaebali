# Архитектура: Cross-Exchange Spread Scanner

# ЧАСТЬ A: БАЗОВАЯ АРХИТЕКТУРА

---

## A.1 Цель системы

Получать best bid/ask по уникальным торговым парам с 8 источников (4 биржи × spot + futures), вычислять спред по 12 направлениям, при обнаружении спреда отслеживать его движение каждые 200 мс в течение 3 часов и записывать в текстовый файл. Если спред сходится раньше — трекинг закрывается и пара немедленно доступна для нового сигнала.

Списки пар, маппинги и направления формируются АВТОМАТИЧЕСКИ модулем Pair Discovery: запрос REST API всех бирж → пересечение → WS-валидация → генерация конфигов.

---

## A.2 Идентификаторы — только числа

```
source_id:    u8   (0..7)    — источник данных
symbol_id:    u16  (0..N)    — глобальный ID пары (присваивается Discovery)
direction_id: u8   (0..11)   — направление спреда

Строковые имена — только при загрузке конфигов и записи в output файл.
На hot path — исключительно числовые ID.
```

---

## A.3 Модуль Pair Discovery

### Назначение

Автоматическое формирование всех списков пар, маппингов и направлений. Запускается ПЕРЕД основной системой и периодически (каждые 6–12 часов) для обнаружения новых пар и делистингов.

### Pipeline

```
ЭТАП 1: Сбор инструментов (REST API)
  ┌──────────────────────────────────────────────────────────────┐
  │  GET Binance Spot     /api/v3/exchangeInfo       → 460+ пар │
  │  GET Binance Futures  /fapi/v1/exchangeInfo      → 528+ пар │
  │  GET Bybit Spot       /v5/market/instruments     → 394+ пар │
  │  GET Bybit Futures    /v5/market/instruments     → 394+ пар │
  │  GET MEXC Spot        /api/v3/exchangeInfo       → 547+ пар │
  │  GET MEXC Futures     /contract/detail           → 579+ пар │
  │  GET OKX Spot         /api/v5/public/instruments → 278+ пар │
  │  GET OKX Futures      /api/v5/public/instruments → 252+ пар │
  └──────────────────────────────────────────────────────────────┘
  
  Для каждой пары извлекаем:
    - exchange_symbol: "BTCUSDT", "BTC-USDT-SWAP", "BTC_USDT"
    - base_asset: "BTC"
    - quote_asset: "USDT"
    - status: СТРОГО Trading/Online (отбрасываем Suspended/Delisted)
    - min_qty, max_qty, tick_size, min_notional (для Order Manager)

  СТРОГАЯ нормализация:
    - parse_base_quote(exchange, symbol, base, quote)
    - Проверка quote == "USDT" (жёсткое равенство, НЕ substring!)
    - exchange_symbol → normalized_name: "BTC-USDT"
    - Примеры: BTCUSDT→BTC-USDT, BTC-USDT-SWAP→BTC-USDT, BTC_USDT→BTC-USDT
  
  Retry logic:
    - 3 попытки с exponential backoff (100ms, 200ms, 400ms)
    - Graceful degradation: минимум 6/8 источников успешны


ЭТАП 2: Формирование глобального списка
  ┌──────────────────────────────────────────────────────────────┐
  │  Объединить все пары со всех 8 источников                    │
  │  Дедупликация по normalized_name через BTreeMap (НЕ HashMap!)│
  │  ДЕТЕРМИНИРОВАННОЕ присвоение symbol_id в sorted order       │
  │  Per-source параметры: [Option<f64>; 8] для min_qty/tick_size│
  │  Построить маппинг: (source_id, exchange_symbol) ↔ symbol_id │
  │  Результат: ~682 уникальных пар (может измениться!)          │
  └──────────────────────────────────────────────────────────────┘


ЭТАП 3: Формирование направлений
  ┌──────────────────────────────────────────────────────────────┐
  │  Для каждого из 12 направлений (spot_source, futures_source):│
  │    Пересечение: пары, присутствующие на ОБОИХ источниках     │
  │    Пример: okx_spot ∩ mexc_futures → 272 пары               │
  │  Результат: 12 списков symbol_id + direction_id 0..11       │
  └──────────────────────────────────────────────────────────────┘


ЭТАП 4: WS-валидация с батчами и таймаутами
  ┌──────────────────────────────────────────────────────────────┐
  │  Для каждого из 8 источников (параллельно):                  │
  │    1. Подключиться к WS (ПЕРЕИСПОЛЬЗУЯ парсеры из feeds)     │
  │    2. Разбивка на батчи: Binance=200, OKX=100, Bybit=50, MEXC=30│
  │    3. Для каждого батча с таймаутами:                        │
  │       - Batch timeout: 90 секунд                             │
  │       - Collect: 30 секунд, Idle: 10 секунд                  │
  │    4. Валидация данных: bid > 0 && ask > 0 && bid ≤ ask      │
  │    5. Логирование причин invalid (no_data vs invalid_prices) │
  │    6. Graceful degradation: минимум 6/8 источников валидны   │
  │                                                              │
  │  Типичные причины invalid:                                   │
  │    - Пара приостановлена (maintenance)                       │
  │    - Пара с нулевой ликвидностью                             │
  │    - Неправильный формат подписки                             │
  │    - Повреждённые данные (bid > ask)                         │
  │    - Превышение лимитов подписок                             │
  └──────────────────────────────────────────────────────────────┘


ЭТАП 5: Генерация конфигов с атомарной записью
  ┌──────────────────────────────────────────────────────────────┐
  │  АТОМАРНАЯ запись через temp files:                          │
  │    1. Записать в .tmp файл                                   │
  │    2. fsync() для гарантии на диск                           │
  │    3. Атомарный rename() (гарантия ОС)                       │
  │                                                              │
  │  Записать:                                                    │
  │    generated/symbols.bin      — бинарный: symbol_id, name,   │
  │                                  маппинги на 8 источников    │
  │    generated/directions.bin   — бинарный: 12 направлений     │
  │                                  с validated symbol_id       │
  │    generated/source_symbols/  — per-source списки symbol_id  │
  │    generated/metadata.json    — timestamp, counts, версия    │
  │                                                              │
  │  + текстовые копии для human-readable проверки:              │
  │    generated/symbols.txt                                     │
  │    generated/directions.txt                                  │
  │    generated/validation_report.txt                           │
  └──────────────────────────────────────────────────────────────┘


ЭТАП 6: Hot-reload (опционально)
  ┌──────────────────────────────────────────────────────────────┐
  │  Обновление без перезапуска основной системы:                 │
  │    1. Discovery генерирует новые конфиги                     │
  │    2. Записывает в generated/ с новым timestamp              │
  │    3. Устанавливает флаг в Control Flags: config_updated=true│
  │    4. Каждый процесс при следующем health-check:             │
  │       - Видит config_updated                                 │
  │       - Перечитывает конфиги                                 │
  │       - Переподписывается на новые пары / отписывается       │
  │  ИЛИ (проще):                                                │
  │    spread-ctl reload → graceful restart всех процессов        │
  └──────────────────────────────────────────────────────────────┘
```

### Валидация: детали

```
Для каждого source подключаем ОТДЕЛЬНЫЙ WS и подписываемся батчами.
Результат валидации:

  SourceValidation {
      source_id:     u8,
      total_pairs:   u16,           // Из REST API
      valid_pairs:   u16,           // Получили WS-ответ
      invalid_pairs: Vec<String>,   // Для отчёта
      timeout_sec:   u8,            // Сколько ждали (30)
  }

Пара считается VALID если:
  - Получили хотя бы 1 WS-update за 30 сек
  - bid > 0 и ask > 0
  - bid <= ask

Пара считается INVALID если:
  - 0 updates за 30 сек
  - Или ошибка подписки (rejected symbol)

Валидация занимает ~30–60 секунд (параллельно по всем 8 источникам).
Запускается ОДИН РАЗ перед стартом системы.
```

---

## A.4 Масштаб данных

Динамический — определяется Discovery при каждом запуске.

Ожидаемые числа (могут меняться):

| Источник | Ожидаемо пар | WS-канал | Conn |
|----------|-------------|----------|------|
| Binance Spot | ~460 | bookTicker | 3 |
| Binance Futures | ~528 | bookTicker | 3 |
| Bybit Spot | ~394 | tickers | 2 |
| Bybit Futures | ~394 | tickers | 2 |
| MEXC Spot | ~547 | mini.ticker | 3 |
| MEXC Futures | ~579 | sub.ticker | 3 |
| OKX Spot | ~278 | tickers | 2 |
| OKX Futures | ~252 | tickers | 2 |
| **Уникальных пар** | **~682** | | **~20** |

12 направлений (фиксированная структура, пары динамические):

| direction_id | Направление | Ожидаемо пар |
|-------------|-------------|--------------|
| 0 | okx_spot → mexc_futures | ~272 |
| 1 | okx_spot → bybit_futures | ~174 |
| 2 | okx_spot → binance_futures | ~243 |
| 3 | mexc_spot → okx_futures | ~243 |
| 4 | mexc_spot → bybit_futures | ~363 |
| 5 | mexc_spot → binance_futures | ~500 |
| 6 | bybit_spot → okx_futures | ~190 |
| 7 | bybit_spot → mexc_futures | ~385 |
| 8 | bybit_spot → binance_futures | ~306 |
| 9 | binance_spot → okx_futures | ~199 |
| 10 | binance_spot → mexc_futures | ~447 |
| 11 | binance_spot → bybit_futures | ~268 |

---

## A.5 Железо

```
CPU:    Intel Core i9-13900 — 8 P-cores (HT) + 16 E-cores, L3 36 MB
RAM:    64 GB DDR5 dual channel
Disk:   2× Samsung NVMe RAID — Seq Write 2700 MB/s, Random Write 130K IOPS
OS:     Linux (Ubuntu 22.04)
```

---

## A.6 Технологический стек

| Компонент | Технология |
|-----------|-----------|
| Язык | Rust |
| Async runtime | tokio |
| WebSocket | tokio-tungstenite |
| JSON-парсинг | sonic-rs (SIMD) |
| HTTP (Discovery) | reqwest |
| IPC | Shared memory (mmap, memmap2) |
| Синхронизация | SeqLock (custom) |
| Файловый вывод | BufWriter + tokio::fs |
| Конфигурация | TOML (статичная) + бинарные конфиги (Discovery) |
| Сериализация конфигов | bincode (бинарные) + serde_json (отчёты) |
| Метрики | tracing + prometheus (опц.) |

---

## A.7 Процессная архитектура

### 11 процессов

```
Процесс                   Роль                              Когда работает
──────────────────────────────────────────────────────────────────────────────
pair-discovery             REST API → валидация → конфиги    При старте + cron
feed-binance-spot          WS → Price Store (region 0)       24/7
feed-binance-futures       WS → Price Store (region 1)       24/7
feed-bybit-spot            WS → Price Store (region 2)       24/7
feed-bybit-futures         WS → Price Store (region 3)       24/7
feed-mexc-spot             WS → Price Store (region 4)       24/7
feed-mexc-futures          WS → Price Store (region 5)       24/7
feed-okx-spot              WS → Price Store (region 6)       24/7
feed-okx-futures           WS → Price Store (region 7)       24/7
spread-engine              Price Store → Event Bus           24/7
spread-tracker             Event Bus + Price Store → файл    24/7
```

Утилиты:
```
shm-init                   Создание shared memory            Oneshot при старте
spread-ctl                 CLI управление                    По требованию
```

### Схема

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                  СИСТЕМА                                    │
│                                                                             │
│  ┌───────────────────────────────────────────────────────────┐             │
│  │  PAIR DISCOVERY (при старте + периодически)                │             │
│  │                                                           │             │
│  │  REST API ──→ Нормализация ──→ Пересечение ──→ WS-валидация             │
│  │      (8 бирж)                    (12 направлений)    │             │
│  │                                                       │             │
│  │  Результат: generated/symbols.bin + directions.bin    │             │
│  └───────────────────────────────────────────┬───────────┘             │
│                                              │ read                        │
│                                              ▼                             │
│  ┌───────────────────────────────────────────────────────────┐             │
│  │  8× Feed Processes                                         │             │
│  │  Загружают generated/ → формируют WS-подписки              │             │
│  │  WS connect → parse → write Price Store → bitmap → eventfd │             │
│  └──────────────────────────┬────────────────────────────────┘             │
│                             │                                              │
│                             ▼                                              │
│  ┌──────────────────────────────────────────────────────────────┐          │
│  │  SHARED MEMORY                                                │          │
│  │  Price Store (682KB) │ Bitmap (1KB) │ Event Bus (4MB)         │          │
│  │  Health (1KB)        │ Control (256B)                         │          │
│  └───────┬──────────────────┬──────────────┬────────────────────┘          │
│          ▼                  ▼              ▼                                │
│  ┌──────────────┐   ┌─────────────────┐   ┌──────────────────┐            │
│  │Spread Engine │   │ Spread Tracker  │   │Supervisor / CLI  │            │
│  │              │   │                 │   │                  │            │
│  │ Загружает    │   │ 200ms snapshots │   │ health monitor   │            │
│  │ generated/   │   │ → файл          │   │ pause/kill       │            │
│  │ directions   │   │ converge/expire │   │                  │            │
│  └──────────────┘   └─────────────────┘   └──────────────────┘            │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Startup Sequence

```
1. shm-init                     # Создать shared memory (oneshot)
2. pair-discovery               # REST → validate → generated/ (oneshot)
3. feed-* (все 8, параллельно)  # Читают generated/, подключаются к WS
4. spread-engine                # Читает generated/, начинает обработку
5. spread-tracker               # Читает generated/, ждёт сигналов
```

---

## A.8 Shared Memory Layout

### Price Store — split seq/data, symbol-major

```
NUM_SOURCES = 8 (фиксировано)
NUM_SYMBOLS = N (определяется Discovery, записывается в header)
MAX_SYMBOLS = 1024 (зарезервировано в shm для роста без пересоздания)

Seqs: /dev/shm/spread-scanner-seqs
  Header (64B): { magic, version, num_symbols: u16 }
  Entries: MAX_SYMBOLS × 8 × 64B = 512 KB
  Index: symbol_id × 8 + source_id

Data: /dev/shm/spread-scanner-data
  Header (64B): { magic, version, num_symbols: u16 }
  Entries: MAX_SYMBOLS × 8 × 64B = 512 KB
  Index: symbol_id × 8 + source_id

PriceSeqEntry — #[repr(C, align(64))]
  seq: AtomicU64 (8B), _pad: [u8;56]

PriceDataEntry — #[repr(C, align(64))]
  best_bid: f64, best_ask: f64, updated_at: u64, _pad: [u8;40]
```

MAX_SYMBOLS = 1024 позволяет Discovery добавлять новые пары без пересоздания shm. При текущих ~682 парах — запас ~50%.

### Остальное без изменений

```
Bitmap:    1 KB (8 sources × 128B padded)
Event Bus: 4 MB (SPSC ring buffer, 64K entries)
Health:    1 KB (16 slots × 64B)
Control:   256 B
```

---

## A.9 Синхронизация

Без изменений от предыдущей версии:
- SeqLock v2 (split seq/data), ~5 нс cached read
- Bitmap: atomic fetch_or / swap
- Ring Buffer: SPSC, padded producer/consumer state
- CPU pinning: feeds → E-cores, engine → P-core 0

---

## A.10 Engine: SourceSymbolIndex

```
Engine загружает generated/directions.bin и строит:

  SourceSymbolIndex:
    lookup: [SourceSymbolDirections; 8 × MAX_SYMBOLS]
    
    SourceSymbolDirections:
      entries: [DirectionEntry; 6]   // Макс 6 направлений на (source, symbol)
      count: u8
    
    DirectionEntry:
      direction_id:      u8
      counterpart_source: u8    // source_id контрпартии
      is_spot_side:      bool   // true = я spot в этом направлении

  Lookup при обновлении цены:
    idx = source_id × MAX_SYMBOLS + symbol_id
    dirs = lookup[idx]
    for i in 0..dirs.count:
        dir = dirs.entries[i]
        counterpart = cached_read(dir.counterpart_source, symbol_id)
        spread = calc(...)

  Размер: 8 × 1024 × ~50B ≈ 400 KB → L2 кеш
  Доступ: ~1–2 нс (array index)
```

---

## A.11 Spread Tracker

Без изменений: 200 мс интервал, CONVERGED → re-signal, EXPIRED после 3ч. Crash recovery из файла.

---

## A.12 Логирование

Без изменений: hot path → counters, warm path → data file, редкие события → non-blocking tracing.

---

## A.13 Стабильность 24/7

Добавлено:

```
Новые/делистинг пар   → pair-discovery по cron (каждые 6–12ч)
                        → spread-ctl reload (restart feeds/engine/tracker)
Пара перестала отвечать → feed логирует warning
                        → engine видит stale → игнорирует
                        → следующий discovery уберёт из списка
```

---

## A.14 Ожидаемые ресурсы

```
RAM:    ~60 MB (все процессы + shm, с запасом MAX_SYMBOLS=1024)
CPU:    < 5% суммарно
Disk:   ~270 MB/час при 100 трекингов
Net:    ~20 WS + 8 REST (при discovery)
Discovery: ~60 сек на полный цикл (REST + WS-валидация)
```

---
---

# ЧАСТЬ B: БУДУЩАЯ АРХИТЕКТУРА (торговый бот)

Без изменений от предыдущей версии:
- Order Manager, Position Manager, Risk Manager
- Event Bus upgrade (SPMC)
- WAL для crash recovery
- Kill Switch
- Сценарии отказов

Discovery становится ещё важнее: Order Manager использует min_qty / tick_size из REST API для расчёта размеров ордеров. Discovery уже сохраняет эти данные.
