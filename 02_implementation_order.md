# Порядок реализации

# ЧАСТЬ A: БАЗОВАЯ СИСТЕМА

---

## Фаза 0: Подготовка (1–2 дня)

### 0.1 Окружение
- Rust stable (1.75+), cargo-watch, git

### 0.2 Workspace
- Создать Cargo workspace: crates/ (common, shm, feeds, engine, tracker, discovery) + bins/ (12 бинарников)
- `cargo build` → 0 ошибок

### 0.3 Статическая конфигурация
- `config/config.toml` — параметры системы (thresholds, intervals, shm names)
- `config/exchanges.toml` — REST/WS URL'ы бирж, лимиты подписок
- `config/directions.toml` — 12 направлений (source_id пар, НЕ списки символов — списки генерирует Discovery)

**Checkpoint:** Workspace компилируется, статические конфиги на месте.

---

## Фаза 1: Shared Memory — фундамент (3–5 дней)

### 1.1 crates/common — типы и конфиги
```
types.rs: SourceId(u8), PriceSeqEntry(align 64), PriceDataEntry(align 64),
          Event/EventHeader, SignalPayload, DirectionEntry
          NUM_SOURCES=8, MAX_SYMBOLS=1024

config.rs: AppConfig (TOML), ExchangeConfig, DirectionConfig
symbols.rs: SymbolTable — загрузка из generated/symbols.bin
directions.rs: DirectionTable, SourceSymbolIndex — загрузка из generated/directions.bin

Тесты: sizeof/alignof, сериализация Event
```

### 1.2 crates/shm — SeqLock
```
seqlock.rs: write (Release), read (Acquire, 4 retries), read_seq_only

Тесты: однопоточный, многопоточный torn-read (10 сек), stress 1M+ ops
```

### 1.3 crates/shm — Price Store, Bitmap, Ring Buffer, Health, Control
```
price_store.rs:  split seq/data, MAX_SYMBOLS=1024, num_symbols в header
bitmap.rs:       per-source 128B aligned blocks
ring_buffer.rs:  SPSC 64K, padded producer/consumer
health.rs:       16 slots
control.rs:      pause/kill/shutdown + config_updated flag

Тесты: per-component + cross-process
```

**Checkpoint:** Все shm-структуры работают.

---

## Фаза 2: Pair Discovery (3–5 дней)

### 2.1 crates/discovery — REST клиенты

```
Для каждой биржи: fn fetch_instruments(market) → Vec<RawInstrument>

RawInstrument {
    exchange_symbol: String,   // "BTCUSDT", "BTC-USDT-SWAP"
    base_asset: String,        // "BTC"
    quote_asset: String,       // "USDT"
    status: InstrumentStatus,  // Trading, Suspended, ...
    min_qty: Option<f64>,      // Для будущего Order Manager
    tick_size: Option<f64>,
}

Реализация: reqwest + sonic-rs парсинг REST ответов.
8 запросов (4 биржи × 2 рынка), параллельно через tokio::join!

Тесты: реальные REST запросы, проверка парсинга, обработка ошибок
```

### 2.2 crates/discovery — нормализация и пересечение

```
fn normalize(exchange: Exchange, market: Market, raw: &RawInstrument) → NormalizedPair
  exchange_symbol → normalized_name: "{BASE}-{QUOTE}" upper case

fn build_global_list(all_sources: &[Vec<NormalizedPair>]) → SymbolRegistry
  Объединение, дедупликация по normalized_name
  Присвоение symbol_id: 0, 1, 2, ...
  Построение маппингов: (source_id, exchange_symbol) ↔ symbol_id

fn build_directions(registry: &SymbolRegistry, direction_configs: &[DirectionConfig])
    → Vec<DirectionData>
  Для каждого направления: пересечение source_A ∩ source_B
  Результат: direction_id + Vec<symbol_id>

Тесты:
  - normalize("BTCUSDT", Binance) == normalize("BTC-USDT", OKX) == "BTC-USDT"
  - Пересечение: если BTC на обоих → попадает, если SHIB только на одном → нет
  - Counts примерно совпадают с ожидаемыми (272, 174, 243, ...)
```

### 2.3 crates/discovery — WS-валидация

```
async fn validate_source(source: SourceId, symbols: &[SymbolSub]) → ValidationResult
  1. Подключиться к WS (используя парсер из crates/feeds)
  2. Подписаться на ВСЕ пары батчами
  3. Ждать 30 секунд, собирать: HashSet<symbol_id> которые прислали update
  4. invalid = all - received
  5. Return ValidationResult { valid: Vec<u16>, invalid: Vec<u16>, stats }

async fn validate_all(registry, feeds_config) → ValidatedRegistry
  Параллельно по 8 источникам через tokio::join!
  Удалить invalid пары из registry и directions
  Пересчитать direction lists

Тесты:
  - Мок WS-сервер: 90% пар отвечают, 10% нет → invalid корректно определены
  - Реальный тест: подключение к каждой бирже, 1 минута
```

### 2.4 crates/discovery — генерация конфигов

```
fn generate_configs(registry: &ValidatedRegistry, output_dir: &Path)

Выходные файлы:
  generated/symbols.bin       bincode: Vec<SymbolRecord>
    SymbolRecord { symbol_id: u16, name: String, source_names: [Option<String>; 8] }

  generated/directions.bin    bincode: Vec<DirectionRecord>
    DirectionRecord { direction_id: u8, spot_source: u8, futures_source: u8,
                      symbols: Vec<u16> }

  generated/metadata.json     { timestamp, num_symbols, per_source_counts,
                                per_direction_counts, validation_stats }

  generated/symbols.txt       human-readable: "0\tBTC-USDT\tBTCUSDT\tBTCUSDT\t..."
  generated/directions.txt    human-readable: "0\tokx_spot_mexc_futures\t272 pairs"
  generated/validation_report.txt   "binance_spot: 460 total, 455 valid, 5 invalid: ..."
```

### 2.5 bins/pair-discovery

```
main.rs:
  1. config = AppConfig::load("config/config.toml")
  2. exchanges = ExchangeConfig::load("config/exchanges.toml")
  3. direction_defs = DirectionConfig::load("config/directions.toml")
  4. Fetch instruments (REST, параллельно)
  5. Normalize + build global list + build directions
  6. WS-validate (параллельно)
  7. Generate configs → generated/
  8. Print summary + exit

Запуск:
  ./pair-discovery --config config/config.toml --output generated/
  Или через systemd oneshot перед spread-scanner.target

Переодический запуск:
  cron: 0 */6 * * * /opt/spread-scanner/pair-discovery && systemctl restart spread-scanner.target
  Или: spread-ctl reload (если реализован hot-reload)
```

**Checkpoint:** pair-discovery запускается, генерирует корректные конфиги, validation report показывает реальные числа.

---

## Фаза 3: Один feed — proof of concept (2–3 дня)

### 3.1 crates/feeds — инфраструктура
```
ws.rs, lib.rs: ExchangeParser trait, feed_loop, connection_loop
Feeds теперь загружают generated/symbols.bin вместо статических txt
```

### 3.2 Binance parser + bins/feed-binance-spot
```
binance.rs: parse bookTicker, build combined stream URL
E2E: pair-discovery → shm-init → feed-binance-spot → Price Store обновляется
```

**Checkpoint:** Discovery → Feed → Price Store pipeline работает.

---

## Фаза 4: Spread Engine (2–3 дня)

```
Engine загружает generated/directions.bin → строит SourceSymbolIndex
Cached reads, event-driven через bitmap + eventfd

Checkpoint: Feed → Price Store → Engine → Ring Buffer
```

---

## Фаза 5: Spread Tracker (2–3 дня)

```
Tracker загружает generated/symbols.bin (для ID→name при записи в файл)
200ms snapshots, converge/expire, re-signal, crash recovery

Checkpoint: полный pipeline для одной биржи
```

---

## Фаза 6: Все feeds (3–5 дней)

```
bybit.rs, okx.rs, mexc.rs → 6 оставшихся бинарников
Каждый загружает generated/ для своего source_id

Checkpoint: 10 процессов, 4 биржи, 12 направлений
```

---

## Фаза 7: Стабилизация (3–5 дней)

```
Failover тесты, 24-часовой тест, мониторинг, spread-ctl

Добавить: тест pair-discovery → full restart → система подхватывает новые конфиги
```

---

## Фаза 8: Deployment (1–2 дня)

```
systemd units (12 штук: shm-init, pair-discovery, 8 feeds, engine, tracker)
deploy/setup.sh, OS tuning, CPU pinning
cron для periodic discovery
```

**Итого базовая система: 20–35 дней**

---
---

# ЧАСТЬ B: РАСШИРЕНИЕ ДО ТОРГОВОГО БОТА (20–35 дней)

Без изменений: фазы 9–14 (Event Bus, Exchange API, Order/Position/Risk Manager, интеграционное тестирование).

Discovery предоставляет Order Manager дополнительные данные: min_qty, tick_size, min_notional — уже сохранены в symbols.bin.
