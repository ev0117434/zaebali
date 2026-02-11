# Техническое задание: спецификации каждого компонента

---

# 1. БИБЛИОТЕКИ (crates/)

---

## 1.1 crates/common

### types.rs

```
Константы:
  NUM_SOURCES:  u8  = 8
  MAX_SYMBOLS:  u16 = 1024     // Резерв в shm, реальное число из Discovery
  MAX_DIRECTIONS: u8 = 12

SourceId — enum (u8)
  BinanceSpot=0, BinanceFutures=1, BybitSpot=2, BybitFutures=3,
  MexcSpot=4, MexcFutures=5, OkxSpot=6, OkxFutures=7
  Методы: index(), is_spot(), is_futures(), name() → &'static str

PriceSeqEntry — #[repr(C, align(64))], 64B
  seq: AtomicU64, _pad: [u8;56]

PriceDataEntry — #[repr(C, align(64))], 64B
  best_bid: f64, best_ask: f64, updated_at: u64, _pad: [u8;40]

PriceSnapshot — обычная struct
  best_bid: f64, best_ask: f64, updated_at: u64

Event — #[repr(C)], 64B
  header: EventHeader, payload: [u8;40]

EventHeader — #[repr(C)]
  event_type: u16, source_proc: u8, timestamp: u64,
  sequence: u64, payload_len: u16, correlation_id: u64

EventType — enum u16
  SpreadSignal=1, TrackingSnapshot=2
  (зарезервировано: 10..15 ордера, 20..22 позиции, 90..99 control, 100..103 health)

SignalPayload — #[repr(C)], ≤40B
  symbol_id: u16, direction_id: u8,
  spot_source: u8, futures_source: u8,
  spot_ask: f64, futures_bid: f64, spread_pct: f64

DirectionEntry — 2B
  direction_id: u8, counterpart_source: u8
```

### config.rs

```
AppConfig — Deserialize из config/config.toml
  general:    { log_level, output_dir, generated_dir, shm_seqs, shm_data,
                shm_bitmap, shm_events, shm_health, shm_control }
  spread:     { min_spread_threshold_pct, staleness_max_ms, converge_threshold_pct }
  tracker:    { snapshot_interval_ms=200, tracking_duration_hours=3,
                delta_write_threshold_pct, heartbeat_write_sec, max_file_size_mb }
  ws:         { max_subscriptions_per_conn=200, ping_interval_sec=20,
                heartbeat_timeout_sec=30, reconnect_base_ms=100, reconnect_max_ms=30000 }
  engine:     { notification_mode="eventfd", eventfd_coalesce_us=200 }
  discovery:  { validation_timeout_sec=30, quote_filter=["USDT"],
                min_status="TRADING", cron_interval_hours=6 }
  monitoring: { prometheus_enabled, stats_log_interval_sec=10 }

ExchangeConfig — Deserialize из config/exchanges.toml
  [[exchange]]
  name = "binance"
  rest_spot = "https://api.binance.com"
  rest_futures = "https://fapi.binance.com"
  ws_spot = "wss://stream.binance.com:9443/stream"
  ws_futures = "wss://fstream.binance.com/stream"
  max_ws_subscriptions = 200
  instruments_path_spot = "/api/v3/exchangeInfo"
  instruments_path_futures = "/fapi/v1/exchangeInfo"

  (аналогично для bybit, okx, mexc)

DirectionConfig — Deserialize из config/directions.toml
  [[direction]]
  id = 0
  spot_source = 6       # OkxSpot
  futures_source = 5    # MexcFutures
  name = "okx_spot_mexc_futures"
  (12 записей, НЕ содержат списков символов — списки генерирует Discovery)
```

### symbols.rs

```
SymbolRecord — Deserialize из generated/symbols.bin
  symbol_id: u16
  name: String                          // "BTC-USDT"
  source_names: [Option<String>; 8]     // [Some("BTCUSDT"), Some("BTCUSDT"), ..., Some("BTC-USDT-SWAP")]
  min_qty: [Option<f64>; 8]             // Для будущего Order Manager
  tick_size: [Option<f64>; 8]

SymbolTable
  records: Vec<SymbolRecord>                // Индекс = symbol_id
  num_symbols: u16
  // Lookup'ы для hot path:
  exchange_to_id: [HashMap<String, u16>; 8] // [source_id]["BTCUSDT"] → 0
  id_to_name: Vec<&'static str>             // Для записи в файл

  fn load(generated_dir: &Path) → Result<Self>
    Десериализует generated/symbols.bin
    Строит exchange_to_id HashMap'ы

  fn resolve(&self, source: SourceId, exchange_symbol: &str) → Option<u16>
  fn name(&self, symbol_id: u16) → &str
  fn num_symbols(&self) → u16

  fn subscription_list(&self, source: SourceId) → Vec<SymbolSub>
    Все symbol_id у которых source_names[source] != None
    SymbolSub { symbol_id: u16, exchange_name: String }
```

### directions.rs

```
DirectionRecord — Deserialize из generated/directions.bin
  direction_id: u8
  spot_source: u8
  futures_source: u8
  name: String
  symbols: Vec<u16>         // symbol_id принадлежащие этому направлению

DirectionTable
  records: Vec<DirectionRecord>    // Индекс = direction_id

  fn load(generated_dir: &Path) → Result<Self>

SourceSymbolIndex — flat array для Engine hot path
  lookup: Vec<SourceSymbolDirections>    // Размер: 8 × num_symbols
  num_symbols: u16

  SourceSymbolDirections
    entries: [DirectionEntry; 6]    // Макс 6 направлений на (source, symbol)
    count: u8

  fn build(directions: &DirectionTable, num_symbols: u16) → Self
    Для каждого direction:
      Для каждого symbol_id в direction.symbols:
        lookup[direction.spot_source * num_symbols + symbol_id]
          .push(DirectionEntry { direction_id, counterpart: futures_source })
        lookup[direction.futures_source * num_symbols + symbol_id]
          .push(DirectionEntry { direction_id, counterpart: spot_source })

  fn get(&self, source: u8, symbol_id: u16) → &SourceSymbolDirections
    &lookup[source as usize * num_symbols as usize + symbol_id as usize]
```

---

## 1.2 crates/shm

Без изменений от предыдущей версии:
- seqlock.rs: write/read/read_seq_only
- price_store.rs: split seq/data, MAX_SYMBOLS=1024, num_symbols в header
- bitmap.rs: per-source 128B aligned
- ring_buffer.rs: SPSC 64K
- health.rs: 16 slots
- control.rs: pause/kill/shutdown + config_version: AtomicU64

Добавление в control.rs:
```
config_version: AtomicU64    // Инкрементируется Discovery после генерации новых конфигов
                             // Процессы сравнивают с локальной версией → hot-reload (будущее)
```

---

## 1.3 crates/discovery

### rest_client.rs

```
Для каждой биржи — отдельная fn:

async fn fetch_binance_spot(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/api/v3/exchangeInfo
  Парсить: symbols[].{symbol, baseAsset, quoteAsset, status}
  Фильтр: status == "TRADING", quoteAsset in quote_filter

async fn fetch_binance_futures(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/fapi/v1/exchangeInfo
  Парсить: symbols[].{symbol, baseAsset, quoteAsset, contractType, status}
  Фильтр: contractType == "PERPETUAL", status == "TRADING"

async fn fetch_bybit_spot(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/v5/market/instruments?category=spot
  Парсить: result.list[].{symbol, baseCoin, quoteCoin, status}
  Фильтр: status == "Trading"

async fn fetch_bybit_futures(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/v5/market/instruments?category=linear
  Парсить: result.list[].{symbol, baseCoin, quoteCoin, contractType, status}
  Фильтр: contractType == "LinearPerpetual", status == "Trading"

async fn fetch_okx_spot(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/api/v5/public/instruments?instType=SPOT
  Парсить: data[].{instId, baseCcy, quoteCcy, state}
  Фильтр: state == "live"

async fn fetch_okx_futures(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/api/v5/public/instruments?instType=SWAP
  Парсить: data[].{instId, ctValCcy, settleCcy, state}
  Фильтр: state == "live", settleCcy == "USDT"

async fn fetch_mexc_spot(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/api/v3/exchangeInfo
  Парсить: symbols[].{symbol, baseAsset, quoteAsset, status}
  Фильтр: status == "1" (enabled)

async fn fetch_mexc_futures(base_url: &str) → Result<Vec<RawInstrument>>
  GET {base_url}/api/v1/contract/detail
  Парсить: data[].{symbol, baseCoin, quoteCoin, state}
  Фильтр: state == 0 (enabled)

RawInstrument {
    source_id: u8,
    exchange_symbol: String,     // Как есть с биржи
    base_asset: String,
    quote_asset: String,
    status: String,
    min_qty: Option<f64>,
    tick_size: Option<f64>,
    min_notional: Option<f64>,
}

async fn fetch_all(exchanges: &ExchangeConfig) → Result<[Vec<RawInstrument>; 8]>
  tokio::join! все 8 запросов параллельно
  Retry: 3 попытки с backoff 1s на каждый endpoint
```

### normalizer.rs

```
fn normalize_name(base: &str, quote: &str) → String
  format!("{}-{}", base.to_uppercase(), quote.to_uppercase())
  "BTC" + "USDT" → "BTC-USDT"

fn build_symbol_registry(all_sources: &[Vec<RawInstrument>; 8]) → SymbolRegistry

SymbolRegistry {
    symbols: Vec<SymbolRecord>,          // Индекс = symbol_id
    name_to_id: HashMap<String, u16>,    // "BTC-USDT" → 0
    source_symbols: [HashSet<u16>; 8],   // Какие symbol_id есть на каком source
}

Алгоритм:
  1. Для каждого source, для каждого instrument:
       name = normalize_name(base, quote)
       if name not in name_to_id:
           symbol_id = symbols.len()
           name_to_id.insert(name, symbol_id)
           symbols.push(SymbolRecord { symbol_id, name, source_names: [None;8], ... })
       id = name_to_id[name]
       symbols[id].source_names[source] = Some(exchange_symbol)
       symbols[id].min_qty[source] = instrument.min_qty
       symbols[id].tick_size[source] = instrument.tick_size
       source_symbols[source].insert(id)

  2. Результат: глобальный список ~682 уникальных пар с маппингами
```

### direction_builder.rs

```
fn build_directions(
    registry: &SymbolRegistry,
    direction_configs: &[DirectionConfig],
) → Vec<DirectionRecord>

Алгоритм:
  Для каждого direction_config (12 штук):
    spot = direction_config.spot_source     // e.g. 6 (OKX Spot)
    fut  = direction_config.futures_source  // e.g. 5 (MEXC Futures)
    intersection = registry.source_symbols[spot] ∩ registry.source_symbols[fut]
    DirectionRecord {
        direction_id: direction_config.id,
        spot_source: spot,
        futures_source: fut,
        name: direction_config.name.clone(),
        symbols: intersection.into_sorted_vec(),
    }
```

### validator.rs

```
async fn validate_source<P: ExchangeParser>(
    source: SourceId,
    symbols: &[SymbolSub],
    ws_url: &str,
    timeout: Duration,
) → ValidationResult

ValidationResult {
    source_id: u8,
    total: u16,
    valid: HashSet<u16>,      // symbol_id получившие хотя бы 1 update
    invalid: Vec<InvalidPair>,
}

InvalidPair {
    symbol_id: u16,
    exchange_name: String,
    reason: InvalidReason,     // NoResponse, ZeroBid, BidAboveAsk, SubscribeError
}

Алгоритм:
  1. Connect WS
  2. Subscribe батчами (по 100, пауза 100ms)
  3. received: HashSet<u16> = {}
  4. Цикл timeout секунд:
       msg = ws.next()
       if let Ok(Some(update)) = P::parse_message(msg, ...):
           if update.best_bid > 0 && update.best_ask > 0 && update.best_bid <= update.best_ask:
               received.insert(update.symbol_id)
  5. invalid = all_symbol_ids - received
  6. Return ValidationResult

async fn validate_all(
    registry: &SymbolRegistry,
    exchanges: &ExchangeConfig,
    timeout: Duration,
) → Vec<ValidationResult>
  8 валидаций параллельно через tokio::join!
  Каждая использует соответствующий ExchangeParser

fn apply_validation(
    registry: &mut SymbolRegistry,
    directions: &mut Vec<DirectionRecord>,
    results: &[ValidationResult],
)
  Для каждого source: убрать invalid symbol_id из source_symbols
  Пересчитать directions: symbols = symbols.filter(|s| valid на обоих source)
  Убрать symbol_id которые НЕ присутствуют ни в одном направлении (опционально)
```

### generator.rs

```
fn generate(
    registry: &SymbolRegistry,
    directions: &[DirectionRecord],
    validation_results: &[ValidationResult],
    output_dir: &Path,
) → Result<()>

Файлы:
  generated/symbols.bin         bincode::serialize(&registry.symbols)
  generated/directions.bin      bincode::serialize(&directions)
  generated/metadata.json       serde_json:
    {
      "timestamp": "2026-02-11T...",
      "num_symbols": 679,
      "sources": {
        "binance_spot": { "total": 462, "valid": 455, "invalid": 7 },
        ...
      },
      "directions": {
        "okx_spot_mexc_futures": { "pairs": 268, "direction_id": 0 },
        ...
      }
    }
  generated/symbols.txt         "0\tBTC-USDT\tBTCUSDT\tBTCUSDT\t..."
  generated/directions.txt      "0\tokx_spot_mexc_futures\t268 pairs"
  generated/validation_report.txt
    "binance_spot: 462 total, 455 valid (98.5%)
     invalid: XYZUSDT (NoResponse), ABCUSDT (ZeroBid), ..."
```

---

## 1.4 crates/feeds

Без изменений от предыдущей версии.
Feeds загружают `generated/symbols.bin` через `SymbolTable::load()`.

### binance.rs, bybit.rs, okx.rs, mexc.rs

Без изменений. Парсеры для каждой биржи.

Одно уточнение: Discovery использует ТЕ ЖЕ парсеры для WS-валидации.
`crates/feeds` — зависимость `crates/discovery`.

---

## 1.5 crates/engine

Без изменений.
Engine загружает `generated/directions.bin` → строит `SourceSymbolIndex`.

---

## 1.6 crates/tracker

Без изменений.
Tracker загружает `generated/symbols.bin` для ID→name при записи.

---

# 2. БИНАРНИКИ

---

## 2.1 bins/pair-discovery

```
main.rs:
  1. config = AppConfig::load("config/config.toml")
  2. exchanges = ExchangeConfig::load("config/exchanges.toml")
  3. direction_defs = load DirectionConfig из config/directions.toml
  4. init_tracing("pair-discovery", config.log_level)

  5. // ЭТАП 1: REST API
     info!("Fetching instruments from all exchanges...");
     all_instruments = fetch_all(&exchanges).await
     // Параллельно 8 запросов. При ошибке одного → retry 3×, потом skip source.
     info!("Fetched: binance_spot={}, binance_fut={}, ...", counts...);

  6. // ЭТАП 2: Нормализация + глобальный список
     registry = build_symbol_registry(&all_instruments)
     info!("Unique symbols: {}", registry.symbols.len());

  7. // ЭТАП 3: Направления (пересечение)
     directions = build_directions(&registry, &direction_defs)
     for d in &directions:
         info!("Direction {}: {} → {} = {} pairs",
               d.direction_id, d.name, d.symbols.len());

  8. // ЭТАП 4: WS-валидация
     info!("Starting WS validation ({} sec timeout)...", config.discovery.validation_timeout_sec);
     results = validate_all(&registry, &exchanges, timeout).await
     apply_validation(&mut registry, &mut directions, &results)
     for r in &results:
         info!("Validated {}: {}/{} valid ({}%)",
               SourceId::from(r.source_id).name(), r.valid.len(), r.total, pct);

  9. // ЭТАП 5: Генерация
     generate(&registry, &directions, &results, &config.general.generated_dir)
     info!("Generated configs in {}", config.general.generated_dir);

  10. // Summary
      info!("Discovery complete: {} symbols, {} directions, {} total pair-directions",
            registry.symbols.len(), directions.len(),
            directions.iter().map(|d| d.symbols.len()).sum::<usize>());

Время работы: ~60–90 секунд (REST: ~5 сек, WS-валидация: ~30 сек, генерация: <1 сек)
```

---

## 2.2–2.9 Feed бинарники (8 штук)

Шаблон main.rs (единственное отличие — SourceId и Parser):

```
fn main():
  1. config = AppConfig::load("config/config.toml")
  2. symbols = SymbolTable::load(&config.general.generated_dir)   // ← из generated/
  3. price_store = PriceStore::open(config.shm_seqs, config.shm_data)
  4. bitmap = UpdateBitmap::open(config.shm_bitmap)
  5. health = HealthTable::open(config.shm_health)
  6. control = ControlStore::open(config.shm_control)
  7. eventfd = open_eventfd(...)
  8. subs = symbols.subscription_list(MY_SOURCE_ID)     // ← динамический список
  9. feed_config = FeedConfig { source: MY_SOURCE_ID, ... }
  10. init_tracing(MY_NAME, ...)
  11. tokio runtime → feed_loop::<MY_PARSER>(...)
```

| Бинарник | SourceId | Parser |
|----------|----------|--------|
| feed-binance-spot | 0 | BinanceParser |
| feed-binance-futures | 1 | BinanceParser |
| feed-bybit-spot | 2 | BybitParser |
| feed-bybit-futures | 3 | BybitParser |
| feed-mexc-spot | 4 | MexcSpotParser |
| feed-mexc-futures | 5 | MexcFuturesParser |
| feed-okx-spot | 6 | OkxParser |
| feed-okx-futures | 7 | OkxParser |

---

## 2.10 bins/spread-engine

```
main.rs:
  1. config = AppConfig::load(...)
  2. symbols = SymbolTable::load(&config.general.generated_dir)
  3. directions = DirectionTable::load(&config.general.generated_dir)
  4. index = SourceSymbolIndex::build(&directions, symbols.num_symbols())
  5. Open shm: price_store, bitmap, ring_buffer, health, control
  6. engine = SpreadEngine::new(price_store, bitmap, ring_buffer, index, config.spread)
  7. Main loop: epoll(eventfd) → engine.process_cycle()
```

---

## 2.11 bins/spread-tracker

```
main.rs:
  1. config, symbols, price_store, ring_buffer, health, control
  2. writer = FileWriter::new(...)
  3. tracker = SpreadTracker::new(...)
  4. tracker.recover_from_file()
  5. Main loop: 200ms interval → process_new_signals → snapshot_all
```

---

## 2.12 bins/spread-ctl

```
Команды:
  spread-ctl status        — Health Table + Control Flags
  spread-ctl pause         — global_pause = true
  spread-ctl resume        — global_pause = false
  spread-ctl kill-switch   — kill_switch = true
  spread-ctl reset-kill    — kill_switch = false
  spread-ctl shutdown      — shutdown = true
  spread-ctl reload        — systemctl restart spread-scanner.target
  spread-ctl discovery     — запустить pair-discovery + reload
```

---

## 2.13 bins/shm-init

```
Создаёт все shm segments с MAX_SYMBOLS=1024.
Oneshot перед всеми остальными.
```

---

# 3. КОНФИГУРАЦИОННЫЕ ФАЙЛЫ

## Статические (пишутся вручную, не меняются)

```
config/
├── config.toml           # Параметры системы
├── exchanges.toml        # REST/WS URL'ы бирж
└── directions.toml       # 12 направлений (source_id пары, БЕЗ символов)
```

## Генерируемые (создаются pair-discovery)

```
generated/
├── symbols.bin           # Бинарный: все пары + маппинги
├── directions.bin        # Бинарный: 12 направлений с symbol_id
├── metadata.json         # Timestamp, counts, stats
├── symbols.txt           # Human-readable
├── directions.txt        # Human-readable
└── validation_report.txt # Отчёт валидации
```

---

# 4. SYSTEMD UNITS

```ini
# pair-discovery.service (oneshot, ПЕРЕД всеми)
[Unit]
Description=Pair Discovery
Before=spread-scanner.target
After=spread-scanner-init.service

[Service]
Type=oneshot
ExecStart=/opt/spread-scanner/pair-discovery --config /etc/spread-scanner/config.toml
TimeoutStartSec=120
```

```ini
# spread-scanner-init.service (oneshot, самый первый)
[Unit]
Description=SHM Init
Before=pair-discovery.service

[Service]
Type=oneshot
ExecStart=/opt/spread-scanner/shm-init
RemainAfterExit=yes
```

```ini
# spread-scanner.target
[Unit]
After=pair-discovery.service
Wants=feed-binance-spot.service ...
      spread-engine.service spread-tracker.service
```

Startup: shm-init → pair-discovery → feeds + engine + tracker (параллельно)

---

# 5. СТРУКТУРА ПРОЕКТА

```
spread-scanner/
├── Cargo.toml
├── config/
│   ├── config.toml
│   ├── exchanges.toml
│   └── directions.toml
├── generated/                        # Создаётся pair-discovery (gitignored)
│   ├── symbols.bin
│   ├── directions.bin
│   ├── metadata.json
│   ├── symbols.txt
│   ├── directions.txt
│   └── validation_report.txt
├── crates/
│   ├── common/src/ { lib.rs, types.rs, config.rs, symbols.rs, directions.rs }
│   ├── shm/src/    { lib.rs, seqlock.rs, price_store.rs, bitmap.rs,
│   │                 ring_buffer.rs, health.rs, control.rs }
│   ├── discovery/src/ { lib.rs, rest_client.rs, normalizer.rs,
│   │                    direction_builder.rs, validator.rs, generator.rs }
│   ├── feeds/src/  { lib.rs, ws.rs, binance.rs, bybit.rs, okx.rs, mexc.rs }
│   ├── engine/src/ { lib.rs }
│   └── tracker/src/{ lib.rs, writer.rs }
├── bins/
│   ├── pair-discovery/src/main.rs
│   ├── shm-init/src/main.rs
│   ├── spread-ctl/src/main.rs
│   ├── feed-binance-spot/src/main.rs
│   ├── feed-binance-futures/src/main.rs
│   ├── feed-bybit-spot/src/main.rs
│   ├── feed-bybit-futures/src/main.rs
│   ├── feed-mexc-spot/src/main.rs
│   ├── feed-mexc-futures/src/main.rs
│   ├── feed-okx-spot/src/main.rs
│   ├── feed-okx-futures/src/main.rs
│   ├── spread-engine/src/main.rs
│   └── spread-tracker/src/main.rs
├── deploy/
│   ├── systemd/ { *.service, spread-scanner.target }
│   └── setup.sh
└── output/
    └── spreads_YYYY-MM-DD.txt
```
