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

### 2.1 crates/discovery — REST клиенты с retry

```
async fn fetch_instruments_with_retry(exchange, market) → Result<Vec<RawInstrument>>
  Retry logic:
    - Max retries: 3
    - Backoff: 100ms * 2^attempt (100ms, 200ms, 400ms)
    - Timeout: 10 секунд на запрос
  
  Graceful degradation:
    - Если fetch failed → log error, return Err
    - Минимум 6/8 источников должны быть успешны
    - Если <6 успешных → main() returns error

RawInstrument {
    exchange_symbol: String,
    base_asset: String,
    quote_asset: String,
    status: InstrumentStatus,  // Trading, Suspended, Delisted, PreLaunch
    min_qty: Option<f64>,
    max_qty: Option<f64>,
    tick_size: Option<f64>,
    min_notional: Option<f64>,
}

Фильтрация:
  - Только status == Trading (отбросить Suspended/Delisted/PreLaunch)
  - Только quote == "USDT" (жёсткое равенство, НЕ substring!)

Реализация: reqwest + sonic-rs парсинг REST ответов
8 запросов (4 биржи × 2 рынка), параллельно через tokio::join!

Тесты:
  - Реальные REST запросы
  - Mock retry scenarios (429, 503, timeout)
  - Проверка фильтрации по status
  - Graceful degradation (7/8 успешных → OK, 5/8 → ERROR)
```

### 2.2 crates/discovery — нормализация и построение глобального списка

```
fn normalize_symbol(exchange, market, raw) → Result<NormalizedSymbol, NormalizationError>
  СТРОГИЙ парсинг:
    - parse_base_quote(exchange, symbol, base, quote)
    - Проверка quote == "USDT" (жёсткое равенство!)
    - Case-sensitive handling (Binance lowercase для WS)
    - Edge cases: "USDTUSDT" → ERROR, "BTCUSD" → ERROR

  Примеры:
    Binance "BTCUSDT" → "BTC-USDT"
    OKX "BTC-USDT-SWAP" → "BTC-USDT"
    MEXC "BTC_USDT" → "BTC-USDT"

fn build_global_list(all_sources) → SymbolRegistry
  ДЕТЕРМИНИРОВАННЫЙ порядок:
    - BTreeMap<String, SymbolBuilder> (НЕ HashMap!)
    - symbol_id присваивается в sorted order по normalized_name
    - Per-source параметры: [Option<f64>; 8] для min_qty/max_qty/tick_size/min_notional
    - Построение маппингов: (source_id, exchange_symbol) ↔ symbol_id

fn build_directions(registry, direction_configs) → Vec<DirectionData>
  Для каждого направления: пересечение source_A ∩ source_B
  Результат: direction_id + Vec<symbol_id>

Тесты:
  - normalize("BTCUSDT", Binance) == "BTC-USDT"
  - normalize("BTC-USDT-SWAP", OKX) == "BTC-USDT"
  - Детерминированность: 10 запусков → одинаковые symbol_id
  - Per-source данные: BTC на binance min_qty=0.001, на okx=0.0001
  - Пересечение: если BTC на обоих → попадает, если SHIB только на одном → нет
```

### 2.3 crates/discovery — WS-валидация с батчами и таймаутами

```
async fn validate_source(source, symbols, config) → ValidationResult
  Разбивка на батчи:
    - Binance: 200 symbols/batch (max 1024 streams/connection)
    - OKX: 100 symbols/batch (max 240 subs/connection)
    - Bybit: 50 symbols/batch (max 100 topics/connection)
    - MEXC: 30 symbols/batch (max 30 subs/connection)
  
  validate_batch_with_timeout() для каждого batch:
    - Общий таймаут: 90 секунд на batch
    - Collect duration: 30 секунд сбора данных
    - Idle timeout: 10 секунд без новых updates
    - Ранний выход если все символы получены
  
  ПЕРЕИСПОЛЬЗОВАНИЕ feeds парсеров:
    - create_parser(exchange, market) из crates/feeds
    - build_subscription_message() → тот же формат что в продакшене
    - parse_update() → та же логика парсинга
  
  Валидация данных:
    - bid > 0 && ask > 0
    - bid <= ask
    - Логирование причин invalid (no_data vs invalid_prices)
  
  Graceful degradation:
    - Если batch failed → помечаем как invalid, но продолжаем
    - Если 7/8 источников успешны → OK
    - Итоговый ValidationResult содержит partial results

async fn validate_all(registry, feeds_config) → ValidatedRegistry
  Параллельно по 8 источникам через tokio::join!
  Минимум 6/8 источников должны быть валидны
  Удалить invalid пары из registry и directions
  Пересчитать direction lists

Тесты:
  - Mock WS-сервер: 90% пар корректно, 5% invalid bid/ask, 5% no data
  - Batch timeout: сервер не отвечает → batch invalid после 90 сек
  - Connection drop: reconnect в следующем batch
  - Реальный smoke test: 1 минута на каждую биржу
```

### 2.4 crates/discovery — генерация конфигов с атомарной записью

```
fn write_atomic(path, data) → Result<()>
  1. Запись в .tmp файл
  2. fsync()
  3. Атомарный rename()
  
  Гарантии:
    - Читатели никогда не видят частично записанный файл
    - Crash во время записи → старая версия остаётся валидной
    - No race conditions при concurrent read/write

fn generate_configs(registry: &ValidatedRegistry, output_dir: &Path)

Выходные файлы:
  generated/symbols.bin       bincode: Vec<SymbolRecord>
    SymbolRecord { 
      symbol_id: u16, 
      name: String, 
      exchange_symbols: [Option<String>; 8],
      min_qty: [Option<f64>; 8],
      max_qty: [Option<f64>; 8],
      tick_size: [Option<f64>; 8],
      min_notional: [Option<f64>; 8],
    }

  generated/directions.bin    bincode: Vec<DirectionRecord>
    DirectionRecord { direction_id: u8, spot_source: u8, futures_source: u8,
                      symbols: Vec<u16> }

  generated/metadata.json     { timestamp, num_symbols, per_source_counts,
                                per_direction_counts, validation_stats }

  generated/symbols.txt       human-readable: "0\tBTC-USDT\tBTCUSDT\tBTCUSDT\t..."
  generated/directions.txt    human-readable: "0\tokx_spot_mexc_futures\t272 pairs"
  generated/validation_report.txt   "binance_spot: 460 total, 455 valid, 5 invalid: ..."

Тесты:
  - Симуляция crash во время записи → файл либо старый, либо новый, не corrupt
  - Concurrent read/write → читатели всегда получают валидные данные
```

### 2.5 bins/pair-discovery — main с graceful degradation

```
main.rs:
  1. config = AppConfig::load("config/config.toml")
  2. exchanges = ExchangeConfig::load("config/exchanges.toml")
  3. direction_defs = DirectionConfig::load("config/directions.toml")
  
  4. Fetch instruments (REST, параллельно с retry)
     - Проверка: ≥6/8 источников успешны?
     - Если <6 → return Err(InsufficientSources)
  
  5. Фильтрация только Trading инструментов
  
  6. Нормализация (СТРОГАЯ)
  
  7. Построение глобального списка (ДЕТЕРМИНИРОВАННОЕ)
  
  8. Построение directions
  
  9. WS-валидация (параллельно с graceful degradation)
     - Проверка: ≥6/8 источников валидны?
     - Если <6 → return Err(ValidationFailed)
  
  10. Обновление registry на основе validation
  
  11. Генерация конфигов (АТОМАРНАЯ запись)
  
  12. Вывод статистики

Запуск:
  ./pair-discovery --config config/config.toml --output generated/

Переодический запуск:
  systemd timer: pair-discovery.timer (раз в 6 часов)
  После успешного завершения: systemctl restart spread-scanner.target

Exit codes:
  0 - Успех (≥6 источников)
  1 - Критическая ошибка (config не найден, write failed)
  2 - Недостаточно источников (<6 успешных fetch)
  3 - Недостаточно валидных данных (<6 успешных validation)
```

**Checkpoint:** 
- pair-discovery запускается и завершается успешно при ≥6/8 источниках
- generated/ содержит корректные файлы (атомарно записанные)
- symbols.bin содержит per-source параметры (min_qty, tick_size, ...)
- validation_report.txt показывает детальную статистику по каждому источнику
- Детерминированность: 10 запусков → одинаковые symbol_id
- Graceful degradation: если binance недоступен → остальные 7 работают
- Повторный запуск с теми же данными → побитово идентичные .bin файлы

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
