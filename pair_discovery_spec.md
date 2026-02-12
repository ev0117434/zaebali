# Техническое задание
## Pair Discovery Module для Spread Scanner

---

## 1. Обзор

Модуль Pair Discovery автоматически получает списки торговых пар с 4 криптовалютных бирж (Binance, Bybit, MEXC, OKX), нормализует их, строит пересечения для 12 торговых направлений и валидирует через WebSocket подписки. Результаты сохраняются в бинарные файлы для использования фидами, движком и трекером.

Это критически важный компонент системы, который обеспечивает актуальность списков пар и предотвращает ошибки подписки на неактивные инструменты.

---

## 2. Архитектурный контекст

### 2.1. Место в системе

Discovery запускается ПЕРЕД основной системой и генерирует конфигурационные файлы, которые загружаются всеми остальными компонентами:

- **Feeds** читают `symbols.bin` для маппинга `exchange_symbol → symbol_id`
- **Engine** читает `directions.bin` для построения `SourceSymbolIndex`
- **Tracker** читает `symbols.bin` для ID→name маппинга при записи в файл

**Последовательность запуска:**
```
pair-discovery → shm-init → [feeds + engine + tracker параллельно]
```

### 2.2. Ключевые числа

| Параметр | Значение |
|----------|----------|
| Уникальных пар | ~682 (динамически) |
| Источников | 8 (4 биржи × 2 рынка) |
| Направлений | 12 |
| Время выполнения | ~60 секунд |
| Частота запуска | Каждые 6–12 часов (cron) |
| MAX_SYMBOLS | 1024 (с запасом) |

---

## 3. Детальные спецификации по биржам

### 3.1. Binance

#### 3.1.1. REST API - Получение списка инструментов

**Spot:**
- Endpoint: `GET https://api.binance.com/api/v3/exchangeInfo`
- Без параметров возвращает все пары
- Важные поля ответа:
  - `symbols[].symbol` (например, `"BTCUSDT"`)
  - `baseAsset`, `quoteAsset`
  - `status`
  - `filters` (для `min_qty`, `tick_size`)

**Futures (USDT-Margined Perpetual):**
- Endpoint: `GET https://fapi.binance.com/fapi/v1/exchangeInfo`
- Важные поля:
  - `symbols[].symbol`
  - `pair`
  - `contractType` (фильтр: только `"PERPETUAL"`)
  - `status`

**Фильтрация:**
- `status == "TRADING"` для обоих рынков
- Извлечь из `filters`:
  - `PRICE_FILTER` → `tickSize`
  - `LOT_SIZE` → `minQty`

**Пример ответа (Spot):**
```json
{
  "symbols": [
    {
      "symbol": "BTCUSDT",
      "status": "TRADING",
      "baseAsset": "BTC",
      "quoteAsset": "USDT",
      "filters": [
        {
          "filterType": "PRICE_FILTER",
          "tickSize": "0.01"
        },
        {
          "filterType": "LOT_SIZE",
          "minQty": "0.00001"
        }
      ]
    }
  ]
}
```

#### 3.1.2. WebSocket - Валидация

**Spot:**
- URL: `wss://stream.binance.com:9443/ws`
- Stream: `<symbol>@bookTicker` (например, `btcusdt@bookTicker`)
- Формат подписки:
```json
{
  "method": "SUBSCRIBE",
  "params": ["btcusdt@bookTicker", "ethusdt@bookTicker"],
  "id": 1
}
```

**Futures:**
- URL: `wss://fstream.binance.com/ws`
- Stream: `<symbol>@bookTicker` (формат аналогичен spot)

**Формат сообщения:**
```json
{
  "u": 400900217,
  "s": "BNBUSDT",
  "b": "25.35190000",
  "B": "31.21000000",
  "a": "25.36520000",
  "A": "40.66000000"
}
```

Поля:
- `s` — symbol
- `b` — best bid price
- `B` — best bid qty
- `a` — best ask price
- `A` — best ask qty

**Условие валидности:**
- Получено хотя бы одно сообщение с непустыми `b` и `a` в течение 30 секунд

**Ограничения:**
- Максимум 1024 streams в одном соединении
- Подписка батчами по 200-300 символов за раз

---

### 3.2. Bybit

#### 3.2.1. REST API - Получение списка инструментов

**Единый endpoint для обоих рынков:**
- Endpoint: `GET https://api.bybit.com/v5/market/instruments-info`
- Параметр `category`: `"spot"` или `"linear"` (для USDT perpetual)
- Важные поля:
  - `result.list[].symbol`
  - `baseCoin`, `quoteCoin`
  - `status`
  - `priceFilter.tickSize`
  - `lotSizeFilter.minOrderQty`

**Фильтрация:**
- `status == "Trading"`
- Для `linear`: только USDT-маржинальные пары (`quoteCoin == "USDT"`)

**Пагинация:**
- По умолчанию возвращает 500 записей
- Использовать `cursor` для получения всех пар (linear > 500)

**Пример запроса:**
```
GET /v5/market/instruments-info?category=spot
GET /v5/market/instruments-info?category=linear&limit=1000&cursor=nextPageCursor
```

**Пример ответа:**
```json
{
  "retCode": 0,
  "result": {
    "category": "spot",
    "list": [
      {
        "symbol": "BTCUSDT",
        "baseCoin": "BTC",
        "quoteCoin": "USDT",
        "status": "Trading",
        "priceFilter": {
          "tickSize": "0.01"
        },
        "lotSizeFilter": {
          "minOrderQty": "0.00001"
        }
      }
    ],
    "nextPageCursor": "..."
  }
}
```

#### 3.2.2. WebSocket - Валидация

- URL: `wss://stream.bybit.com/v5/public/spot` или `wss://stream.bybit.com/v5/public/linear`
- Channel: `tickers.{symbol}` (например, `tickers.BTCUSDT`)
- Формат подписки:
```json
{
  "op": "subscribe",
  "args": ["tickers.BTCUSDT", "tickers.ETHUSDT"]
}
```

**Формат сообщения:**
```json
{
  "topic": "tickers.BTCUSDT",
  "type": "snapshot",
  "data": {
    "symbol": "BTCUSDT",
    "bid1Price": "66666.60",
    "bid1Size": "23789.165",
    "ask1Price": "66666.70",
    "ask1Size": "23775.469"
  }
}
```

**Условие валидности:**
- `data.bid1Price` и `data.ask1Price` непустые

---

### 3.3. MEXC

#### 3.3.1. REST API - Получение списка инструментов

**Spot:**
- Endpoint: `GET https://api.mexc.com/api/v3/exchangeInfo`
- Важные поля:
  - `symbols[].symbol`
  - `baseAsset`, `quoteAsset`
  - `status` (`"1"` = активна)
  - `filters`

**Futures:**
- Endpoint: `GET https://api.mexc.com/api/v1/contract/detail`
- Важные поля:
  - `data.symbol` (например, `"BTC_USDT"`)
  - `baseCoin`, `quoteCoin`
  - `state` (`0` = активен)
  
⚠️ **ВНИМАНИЕ:** Futures API доступен только для institutional users

**Решение:** Попытаться запрос, если вернёт ошибку доступа — пропустить futures для MEXC с предупреждением в логе.

**Пример ответа (Spot):**
```json
{
  "symbols": [
    {
      "symbol": "BTCUSDT",
      "status": "1",
      "baseAsset": "BTC",
      "quoteAsset": "USDT",
      "baseSizePrecision": "0.000001"
    }
  ]
}
```

#### 3.3.2. WebSocket - Валидация

**Spot:**
- URL: `wss://wbs-api.mexc.com/ws`
- Channel: `spot@public.book_ticker.v3.api.pb@{symbol}` (символ ЗАГЛАВНЫМИ буквами)
- Формат подписки:
```json
{
  "method": "SUBSCRIPTION",
  "params": ["spot@public.book_ticker.v3.api.pb@BTCUSDT"]
}
```

**Futures:**
- URL: `wss://contract.mexc.com/ws` (если доступен)
- Формат аналогичен spot, символ с подчёркиванием: `BTC_USDT`

**Ограничения:**
- Максимум 30 подписок на одно соединение

---

### 3.4. OKX

#### 3.4.1. REST API - Получение списка инструментов

- Endpoint: `GET https://www.okx.com/api/v5/public/instruments`
- Параметр `instType`: `"SPOT"` или `"SWAP"` (для perpetual futures)
- Важные поля:
  - `data[].instId` (например, `"BTC-USDT"`)
  - `baseCcy`, `quoteCcy`
  - `state`
  - `tickSz`, `minSz`

**Фильтрация:**
- `state == "live"`

**Пример запроса:**
```
GET /api/v5/public/instruments?instType=SPOT
GET /api/v5/public/instruments?instType=SWAP
```

**Пример ответа:**
```json
{
  "code": "0",
  "data": [
    {
      "instId": "BTC-USDT",
      "baseCcy": "BTC",
      "quoteCcy": "USDT",
      "state": "live",
      "tickSz": "0.1",
      "minSz": "0.00001"
    }
  ]
}
```

#### 3.4.2. WebSocket - Валидация

- URL: `wss://ws.okx.com:8443/ws/v5/public`
- Channel: `tickers`
- Формат подписки:
```json
{
  "op": "subscribe",
  "args": [
    {"channel": "tickers", "instId": "BTC-USDT"},
    {"channel": "tickers", "instId": "ETH-USDT"}
  ]
}
```

**Формат сообщения:**
```json
{
  "arg": {
    "channel": "tickers",
    "instId": "BTC-USDT"
  },
  "data": [
    {
      "instId": "BTC-USDT",
      "bidPx": "56145.3",
      "bidSz": "538",
      "askPx": "56145.4",
      "askSz": "1475"
    }
  ]
}
```

**Условие валидности:**
- `data[0].bidPx` и `data[0].askPx` непустые

---

## 4. Алгоритм работы модуля

### 4.1. Шаг 1: REST — Сбор инструментов с retry

Для каждой биржи **параллельно** (`tokio::join!`) с **retry logic**:

```rust
async fn fetch_instruments_with_retry(
    exchange: Exchange,
    market: Market,
) -> Result<Vec<RawInstrument>, DiscoveryError> {
    let mut attempt = 0;
    let max_retries = 3;
    
    loop {
        match fetch_instruments_once(exchange, market).await {
            Ok(instruments) => return Ok(instruments),
            Err(e) if attempt < max_retries => {
                let backoff = Duration::from_millis(100 * 2u64.pow(attempt));
                warn!("Fetch failed, retry {}/{} after {:?}", attempt + 1, max_retries, backoff);
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
            Err(e) => {
                error!("Failed after {} retries", max_retries);
                return Err(DiscoveryError::RestFailed { exchange, market, source: e });
            }
        }
    }
}
```

**Биржи:**
- **Binance:** `GET /api/v3/exchangeInfo` + `GET /fapi/v1/exchangeInfo`
- **Bybit:** `GET /v5/market/instruments-info?category=spot` + `category=linear` (с пагинацией)
- **MEXC:** `GET /api/v3/exchangeInfo` + `try GET /api/v1/contract/detail` (если доступен)
- **OKX:** `GET /api/v5/public/instruments?instType=SPOT` + `instType=SWAP`

**Парсинг каждого ответа в структуру:**

```rust
struct RawInstrument {
    exchange_symbol: String,   // "BTCUSDT", "BTC-USDT-SWAP", "BTC_USDT"
    base_asset: String,        // "BTC"
    quote_asset: String,       // "USDT"
    status: InstrumentStatus,  // Trading, Suspended, Delisted, PreLaunch
    // Per-source параметры
    min_qty: Option<f64>,
    max_qty: Option<f64>,
    tick_size: Option<f64>,
    min_notional: Option<f64>,
}

enum InstrumentStatus {
    Trading,
    Suspended,
    Delisted,
    PreLaunch,
}
```

**Фильтрация:**
- Только `status == Trading` (отбросить Suspended/Delisted/PreLaunch)
- Только `quote == "USDT"` (жёсткое равенство, НЕ substring matching!)

**Graceful degradation:**
- Минимум 6/8 источников должны быть успешны
- Если <6 успешных → return error
- Если ≥6 успешных → продолжаем с partial results

**Результат:**
- До 8 векторов `Vec<RawInstrument>` (частичные результаты допустимы)

---

### 4.2. Шаг 2: Нормализация и построение глобального списка

#### Функция `normalize_symbol` — СТРОГИЙ парсинг

**Вход:** `(Exchange, Market, &RawInstrument)`

**Выход:** `Result<NormalizedSymbol, NormalizationError>`

```rust
struct NormalizedSymbol {
    normalized_name: String,    // "BTC-USDT"
    exchange_symbol: String,    // "BTCUSDT"
    base: String,               // "BTC"
    quote: String,              // "USDT"
    source: SourceId,
    // Per-source параметры
    min_qty: Option<f64>,
    max_qty: Option<f64>,
    tick_size: Option<f64>,
    min_notional: Option<f64>,
}

fn normalize_symbol(exchange: Exchange, market: Market, raw: &RawInstrument) 
    -> Result<NormalizedSymbol, NormalizationError> {
    
    // 1. СТРОГИЙ парсинг base/quote (НЕ substring matching!)
    let (base, quote) = parse_base_quote(exchange, &raw.exchange_symbol, &raw.base_asset, &raw.quote_asset)?;
    
    // 2. Uppercase normalization
    let base_norm = base.to_uppercase();
    let quote_norm = quote.to_uppercase();
    
    // 3. Проверка quote = USDT (жёсткое равенство!)
    if quote_norm != "USDT" {
        return Err(NormalizationError::InvalidQuote { got: quote_norm });
    }
    
    // 4. Формат normalized_name
    let normalized_name = format!("{}-{}", base_norm, quote_norm);
    
    Ok(NormalizedSymbol {
        normalized_name,
        exchange_symbol: raw.exchange_symbol.clone(),
        base: base_norm,
        quote: quote_norm,
        source: SourceId::new(exchange, market),
        min_qty: raw.min_qty,
        max_qty: raw.max_qty,
        tick_size: raw.tick_size,
        min_notional: raw.min_notional,
    })
}

// СТРОГИЙ парсинг для каждой биржи
fn parse_base_quote(exchange: Exchange, symbol: &str, base: &str, quote: &str) 
    -> Result<(String, String), NormalizationError> {
    
    match exchange {
        Exchange::Binance => {
            // Binance: lowercase для WS, uppercase для REST
            if symbol.to_uppercase() != format!("{}{}", base, quote) {
                return Err(NormalizationError::SymbolMismatch);
            }
            Ok((base.to_string(), quote.to_string()))
        }
        Exchange::OKX => {
            // OKX: "BTC-USDT" для spot, "BTC-USDT-SWAP" для futures
            let parts: Vec<&str> = symbol.split('-').collect();
            match parts.as_slice() {
                [b, q] if market == Market::Spot => Ok((b.to_string(), q.to_string())),
                [b, q, "SWAP"] if market == Market::Futures => Ok((b.to_string(), q.to_string())),
                _ => Err(NormalizationError::InvalidFormat { exchange, symbol: symbol.to_string() })
            }
        }
        Exchange::Bybit => {
            // Bybit: "BTCUSDT" для обоих рынков
            Ok((base.to_string(), quote.to_string()))
        }
        Exchange::MEXC => {
            // MEXC: "BTC_USDT"
            let parts: Vec<&str> = symbol.split('_').collect();
            if parts.len() != 2 {
                return Err(NormalizationError::InvalidFormat { exchange, symbol: symbol.to_string() });
            }
            Ok((parts[0].to_string(), parts[1].to_string()))
        }
    }
}
```

**Примеры:**
```
Binance spot:    "BTCUSDT"         → "BTC-USDT"  ✓
Binance futures: "BTCUSDT"         → "BTC-USDT"  ✓
OKX spot:        "BTC-USDT"        → "BTC-USDT"  ✓
OKX futures:     "BTC-USDT-SWAP"   → "BTC-USDT"  ✓
Bybit spot:      "BTCUSDT"         → "BTC-USDT"  ✓
Bybit futures:   "BTCUSDT"         → "BTC-USDT"  ✓
MEXC spot:       "BTC_USDT"        → "BTC-USDT"  ✓
MEXC futures:    "BTC_USDT"        → "BTC-USDT"  ✓

Edge cases:
"USDTUSDT"       → INVALID (base не может быть USDT)
"BTCUSD"         → INVALID (quote != USDT)
"BTC-USDT-PERP"  → INVALID (неизвестный суффикс)
```

#### Функция `build_global_list` — ДЕТЕРМИНИРОВАННЫЙ порядок

```rust
use std::collections::BTreeMap;  // НЕ HashMap!

fn build_global_list(all_sources: &[Vec<NormalizedSymbol>]) -> SymbolRegistry {
    // 1. BTreeMap для детерминированного порядка
    let mut symbols: BTreeMap<String, SymbolBuilder> = BTreeMap::new();
    
    // 2. Собираем все источники для каждого символа
    for (source_idx, source_symbols) in all_sources.iter().enumerate() {
        for norm_sym in source_symbols {
            symbols.entry(norm_sym.normalized_name.clone())
                .or_insert_with(|| SymbolBuilder::new(&norm_sym.normalized_name))
                .add_source(source_idx as u8, norm_sym);
        }
    }
    
    // 3. Детерминированное присвоение ID (BTreeMap итерируется в sorted order)
    let mut registry = SymbolRegistry::new();
    for (symbol_id, (name, builder)) in symbols.into_iter().enumerate() {
        if symbol_id >= MAX_SYMBOLS {
            warn!("Reached MAX_SYMBOLS={}, truncating universe", MAX_SYMBOLS);
            break;
        }
        registry.add_symbol(symbol_id as u16, builder.build());
    }
    
    registry
}

struct SymbolBuilder {
    normalized_name: String,
    // Per-source данные (индекс = source_id)
    exchange_symbols: [Option<String>; 8],
    min_qty: [Option<f64>; 8],
    max_qty: [Option<f64>; 8],
    tick_size: [Option<f64>; 8],
    min_notional: [Option<f64>; 8],
}
```

**Результат:**

```rust
struct SymbolRegistry {
    symbols: Vec<SymbolRecord>,
    source_mappings: HashMap<(u8, String), u16>, // (source_id, exchange_symbol) → symbol_id
}

struct SymbolRecord {
    symbol_id: u16,
    normalized_name: String,
    exchange_symbols: [Option<String>; 8],
    // Per-source параметры
    min_qty: [Option<f64>; 8],
    max_qty: [Option<f64>; 8],
    tick_size: [Option<f64>; 8],
    min_notional: [Option<f64>; 8],
}
```

---

### 4.3. Шаг 3: Построение направлений

**Загрузить** `config/directions.toml`:
- 12 записей, каждая: `{ direction_id, spot_source, futures_source }`
- Например, direction 0: `okx_spot=6, mexc_futures=1`

#### Функция `build_directions`

Для каждого направления:
1. Взять множество `normalized_name` для `spot_source`
2. Взять множество `normalized_name` для `futures_source`
3. `Intersection = spot ∩ futures`
4. Преобразовать `normalized_name → symbol_id` через `SymbolRegistry`

**Результат:**

```rust
struct DirectionData {
    direction_id: u8,
    spot_source: u8,
    futures_source: u8,
    symbols: Vec<u16>,  // symbol_ids в этом направлении
}
```

---

### 4.4. Шаг 4: WebSocket валидация с батчами и таймаутами

#### Функция `validate_source` для каждого источника

```rust
use crate::feeds::{ExchangeParser, create_parser};  // ПЕРЕИСПОЛЬЗУЕМ feeds парсеры!

async fn validate_source(
    source: SourceId,
    symbols: &[SymbolSub],
    config: &WsValidationConfig
) -> ValidationResult {
    let batch_size = get_batch_size(source.exchange());  // Per-exchange лимиты!
    let batches = symbols.chunks(batch_size);
    
    let mut all_valid = Vec::new();
    let mut all_invalid = Vec::new();
    let mut stats = ValidationStats::default();
    
    for (batch_idx, batch) in batches.enumerate() {
        info!("Validating batch {}/{} ({} symbols) for {:?}", 
              batch_idx + 1, batches.len(), batch.len(), source);
        
        match validate_batch_with_timeout(source, batch, config).await {
            Ok(result) => {
                all_valid.extend(result.valid);
                all_invalid.extend(result.invalid);
                stats.merge(result.stats);
            }
            Err(e) => {
                error!("Batch {} failed for {:?}: {}", batch_idx, source, e);
                // Graceful degradation: помечаем весь batch как invalid, но продолжаем
                all_invalid.extend(batch.iter().map(|s| s.symbol_id));
                stats.failed_batches += 1;
            }
        }
        
        // Rate limiting между батчами
        if batch_idx < batches.len() - 1 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    
    ValidationResult { source, valid: all_valid, invalid: all_invalid, stats }
}

// Per-exchange лимиты подписок
fn get_batch_size(exchange: Exchange) -> usize {
    match exchange {
        Exchange::Binance => 200,   // max 1024 streams per connection
        Exchange::OKX => 100,       // max 240 subscriptions per connection
        Exchange::Bybit => 50,      // max 100 topics per connection
        Exchange::MEXC => 30,       // max 30 subscriptions per connection
    }
}

async fn validate_batch_with_timeout(
    source: SourceId,
    batch: &[SymbolSub],
    config: &WsValidationConfig
) -> Result<BatchValidationResult, ValidationError> {
    // Таймаут на весь batch!
    tokio::time::timeout(
        Duration::from_secs(config.batch_timeout_secs),
        validate_batch_inner(source, batch, config)
    )
    .await
    .map_err(|_| ValidationError::Timeout { source, batch_size: batch.len() })?
}

async fn validate_batch_inner(
    source: SourceId,
    batch: &[SymbolSub],
    config: &WsValidationConfig
) -> Result<BatchValidationResult, ValidationError> {
    // ПЕРЕИСПОЛЬЗУЕМ парсер из crates/feeds!
    let parser = create_parser(source.exchange(), source.market());
    let ws_url = get_ws_url(source);
    
    // 1. Подключаемся
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(ws_url).await?;
    
    // 2. Подписываемся (используя формат из feeds!)
    let sub_msg = parser.build_subscription_message(batch);
    ws_stream.send(tungstenite::Message::Text(sub_msg)).await?;
    
    // 3. Собираем updates с таймаутами
    let mut received = HashSet::new();
    let mut last_update = Instant::now();
    let collect_duration = Duration::from_secs(config.collect_duration_secs);
    let idle_timeout = Duration::from_secs(config.idle_timeout_secs);
    
    let start = Instant::now();
    loop {
        // Проверяем общий таймаут
        if start.elapsed() > collect_duration {
            break;
        }
        
        // Проверяем idle таймаут
        if last_update.elapsed() > idle_timeout {
            warn!("Idle timeout for {:?}, stopping collection", source);
            break;
        }
        
        // Читаем сообщение с таймаутом
        match tokio::time::timeout(Duration::from_secs(1), ws_stream.next()).await {
            Ok(Some(Ok(msg))) => {
                if let Some(symbol_id) = parser.parse_update(&msg)? {
                    received.insert(symbol_id);
                    last_update = Instant::now();
                    
                    // Ранний выход если все получены
                    if received.len() == batch.len() {
                        info!("All {} symbols validated for {:?}", batch.len(), source);
                        break;
                    }
                }
            }
            Ok(Some(Err(e))) => return Err(ValidationError::WebSocketError { source, error: e }),
            Ok(None) => return Err(ValidationError::ConnectionClosed { source }),
            Err(_) => continue, // Timeout на чтение - норма, продолжаем
        }
    }
    
    // 4. Валидация bid ≤ ask для полученных символов
    let mut valid = Vec::new();
    let mut invalid_data = Vec::new();
    
    for &symbol_id in &received {
        if let Some(price) = parser.get_last_price(symbol_id) {
            if price.bid <= price.ask && price.bid > 0.0 && price.ask > 0.0 {
                valid.push(symbol_id);
            } else {
                warn!("Invalid price for symbol_id={}: bid={}, ask={}", symbol_id, price.bid, price.ask);
                invalid_data.push(symbol_id);
            }
        }
    }
    
    // 5. Символы которые не прислали данные
    let all_ids: HashSet<_> = batch.iter().map(|s| s.symbol_id).collect();
    let not_received: Vec<_> = all_ids.difference(&received).copied().collect();
    
    // Логирование причин invalid
    if !not_received.is_empty() {
        warn!("No data received for {} symbols in {:?}: {:?}", not_received.len(), source, not_received);
    }
    if !invalid_data.is_empty() {
        warn!("Invalid bid/ask for {} symbols in {:?}: {:?}", invalid_data.len(), source, invalid_data);
    }
    
    Ok(BatchValidationResult {
        valid,
        invalid: [not_received, invalid_data].concat(),
        stats: ValidationStats {
            total: batch.len(),
            received: received.len(),
            valid: valid.len(),
            no_data: not_received.len(),
            invalid_prices: invalid_data.len(),
            duration_secs: start.elapsed().as_secs(),
        },
    })
}

struct WsValidationConfig {
    batch_timeout_secs: u64,      // Максимум на весь batch (default: 90)
    collect_duration_secs: u64,   // Сколько собираем данные (default: 30)
    idle_timeout_secs: u64,       // Максимум без новых updates (default: 10)
}
```

**Результат:**

```rust
struct ValidationResult {
    source: SourceId,
    valid: Vec<u16>,
    invalid: Vec<u16>,
    stats: ValidationStats,
}

struct ValidationStats {
    total: usize,
    received: usize,
    valid: usize,
    no_data: usize,
    invalid_prices: usize,
    failed_batches: usize,
    duration_secs: u64,
}
```

#### Функция `validate_all` с graceful degradation

1. Запустить `validate_source` **параллельно** для всех 8 источников
2. Минимум 6/8 источников должны быть успешны
3. Собрать `invalid` символы со всех источников
4. **Удалить** `invalid` из `SymbolRegistry`
5. **Пересчитать** direction lists

---

### 4.5. Шаг 5: Генерация конфигов с атомарной записью

#### АТОМАРНАЯ запись через temp files

```rust
// КРИТИЧНО: гарантирует что читатели не видят частично записанный файл
fn write_atomic(path: &Path, data: &[u8]) -> Result<(), std::io::Error> {
    let temp_path = path.with_extension("tmp");
    
    // 1. Пишем во временный файл
    std::fs::write(&temp_path, data)?;
    
    // 2. fsync для гарантии записи на диск
    let file = std::fs::File::open(&temp_path)?;
    file.sync_all()?;
    drop(file);
    
    // 3. Атомарный rename (гарантируется ОС)
    std::fs::rename(&temp_path, path)?;
    
    Ok(())
}

fn generate_configs(registry: &ValidatedRegistry, output_dir: &Path) -> Result<()> {
    // Сериализуем
    let symbols_data = bincode::serialize(&registry.symbols)?;
    let directions_data = bincode::serialize(&registry.directions)?;
    let metadata_json = serde_json::to_string_pretty(&registry.metadata)?;
    
    // Атомарно записываем
    write_atomic(&output_dir.join("symbols.bin"), &symbols_data)?;
    write_atomic(&output_dir.join("directions.bin"), &directions_data)?;
    write_atomic(&output_dir.join("metadata.json"), metadata_json.as_bytes())?;
    
    // Human-readable файлы (не критично если частично записаны)
    std::fs::write(output_dir.join("symbols.txt"), generate_symbols_txt(registry))?;
    std::fs::write(output_dir.join("directions.txt"), generate_directions_txt(registry))?;
    std::fs::write(output_dir.join("validation_report.txt"), generate_validation_report(registry))?;
    
    Ok(())
}
```

#### Выходные файлы в `generated/`:

**1. `symbols.bin` (bincode):**
```rust
Vec<SymbolRecord>

struct SymbolRecord {
    symbol_id: u16,
    normalized_name: String,
    exchange_symbols: [Option<String>; 8],
    // Per-source параметры
    min_qty: [Option<f64>; 8],
    max_qty: [Option<f64>; 8],
    tick_size: [Option<f64>; 8],
    min_notional: [Option<f64>; 8],
}
```
- Используется **feeds** для маппинга и **tracker** для ID→name

**2. `directions.bin` (bincode):**
```rust
Vec<DirectionRecord>

struct DirectionRecord {
    direction_id: u8,
    spot_source: u8,
    futures_source: u8,
    symbols: Vec<u16>,
}
```
- Используется **engine** для построения `SourceSymbolIndex`

**3. `metadata.json` (human-readable):**
```json
{
  "timestamp": "2026-02-12T10:30:00Z",
  "num_symbols": 682,
  "per_source_counts": {
    "binance_spot": 460,
    "binance_futures": 305,
    "bybit_spot": 387,
    "bybit_linear": 342,
    "mexc_spot": 1250,
    "mexc_futures": 0,
    "okx_spot": 505,
    "okx_swap": 398
  },
  "per_direction_counts": {
    "direction_0": 272,
    "direction_1": 174,
    ...
  },
  "validation_stats": {
    "total_validated": 3590,
    "total_invalid": 108
  }
}
```

**4. `symbols.txt` (human-readable):**
```
symbol_id	normalized_name	binance_spot	binance_futures	bybit_spot	...
0	BTC-USDT	BTCUSDT	BTCUSDT	BTCUSDT	...
1	ETH-USDT	ETHUSDT	ETHUSDT	ETHUSDT	...
```

**5. `directions.txt` (human-readable):**
```
direction_id	name	num_pairs
0	okx_spot_mexc_futures	272
1	okx_spot_bybit_linear	174
...
```

**6. `validation_report.txt` (подробный отчёт):**
```
=== Validation Report ===

binance_spot: 460 total, 455 valid, 5 invalid
Invalid symbols: SHIB1USDT, TEST1USDT, ...

binance_futures: 305 total, 302 valid, 3 invalid
Invalid symbols: ...

...

Total: 3590 pairs validated, 108 invalid (3.0%)
```

---

## 5. Структура кода

### 5.1. `crates/discovery/`

```
discovery/
├── lib.rs              # Публичный API модуля
├── rest.rs             # REST клиенты для всех бирж
├── normalize.rs        # Функции normalize, build_global_list
├── directions.rs       # Функция build_directions
├── validate.rs         # Функции validate_source, validate_all
└── generate.rs         # Функция generate_configs
```

**Ключевые функции:**

```rust
// rest.rs
pub async fn fetch_binance_spot() -> Result<Vec<RawInstrument>>;
pub async fn fetch_binance_futures() -> Result<Vec<RawInstrument>>;
pub async fn fetch_bybit_spot() -> Result<Vec<RawInstrument>>;
pub async fn fetch_bybit_linear() -> Result<Vec<RawInstrument>>;
pub async fn fetch_mexc_spot() -> Result<Vec<RawInstrument>>;
pub async fn fetch_mexc_futures() -> Result<Vec<RawInstrument>>;
pub async fn fetch_okx_spot() -> Result<Vec<RawInstrument>>;
pub async fn fetch_okx_swap() -> Result<Vec<RawInstrument>>;

// normalize.rs
pub fn normalize(exchange: Exchange, market: Market, raw: &RawInstrument) -> NormalizedPair;
pub fn build_global_list(sources: &[Vec<NormalizedPair>]) -> SymbolRegistry;

// directions.rs
pub fn build_directions(
    registry: &SymbolRegistry,
    configs: &[DirectionConfig]
) -> Vec<DirectionData>;

// validate.rs
pub async fn validate_source(
    source_id: u8,
    symbols: &[SymbolSub],
    ws_url: &str
) -> Result<ValidationResult>;

pub async fn validate_all(
    registry: &mut SymbolRegistry,
    directions: &mut Vec<DirectionData>,
    config: &ExchangeConfig
) -> Result<ValidationSummary>;

// generate.rs
pub fn generate_configs(
    registry: &SymbolRegistry,
    directions: &[DirectionData],
    output_dir: &Path
) -> Result<()>;
```

### 5.2. `bins/pair-discovery/`

```
pair-discovery/
└── main.rs             # Точка входа
```

**Псевдокод `main.rs`:**

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // 1. Загрузка конфигов
    let app_config = AppConfig::load("config/config.toml")?;
    let exchanges = ExchangeConfig::load("config/exchanges.toml")?;
    let direction_defs = DirectionConfig::load("config/directions.toml")?;
    
    // 2. Fetch instruments (REST, параллельно)
    info!("Fetching instruments from exchanges...");
    let (binance_spot, binance_fut, bybit_spot, bybit_lin, 
         mexc_spot, mexc_fut, okx_spot, okx_swap) = tokio::join!(
        fetch_binance_spot(),
        fetch_binance_futures(),
        fetch_bybit_spot(),
        fetch_bybit_linear(),
        fetch_mexc_spot(),
        fetch_mexc_futures(),
        fetch_okx_spot(),
        fetch_okx_swap(),
    );
    
    // 3. Normalize + build global list
    info!("Normalizing and building global symbol list...");
    let mut registry = build_global_list(&all_sources);
    
    // 4. Build directions
    info!("Building direction lists...");
    let mut directions = build_directions(&registry, &direction_defs);
    
    // 5. WS validate (параллельно)
    info!("Validating symbols via WebSocket...");
    let validation = validate_all(&mut registry, &mut directions, &exchanges).await?;
    
    // 6. Generate configs
    info!("Generating configuration files...");
    generate_configs(&registry, &directions, Path::new("generated/"))?;
    
    // 7. Print summary
    info!("=== Discovery Complete ===");
    info!("Total symbols: {}", registry.symbols.len());
    info!("Invalid symbols: {}", validation.total_invalid);
    
    Ok(())
}
```

---

## 6. Зависимости

**Cargo.toml:**

```toml
[dependencies]
tokio = { version = "1.40", features = ["full"] }
reqwest = { version = "0.12", features = ["json"] }
sonic-rs = "0.3"
tokio-tungstenite = "0.24"
bincode = "1.3"
toml = "0.8"
serde = { version = "1.0", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = "0.3"
anyhow = "1.0"
```

---

## 7. Обработка ошибок

**Стратегии:**

1. **REST запросы:**
   - Повтор 3 раза с exponential backoff при таймауте/сетевых ошибках
   - Начальная задержка: 1 секунда
   - Множитель: 2x (1s, 2s, 4s)

2. **WebSocket:**
   - Переподключение до 5 попыток
   - Если не удалось — пометить весь источник как invalid
   - Логировать подробности

3. **Парсинг JSON:**
   - Логировать невалидные ответы с WARNING
   - Продолжать обработку остальных

4. **MEXC futures недоступен:**
   - Предупреждение в лог: `WARN: MEXC futures API unavailable (institutional only)`
   - Продолжить без futures для MEXC

**Примеры обработки:**

```rust
// REST retry
let response = retry_with_backoff(3, Duration::from_secs(1), || {
    client.get(url).send()
}).await?;

// WS validation error
if let Err(e) = validate_source(source_id, ...).await {
    warn!("Failed to validate source {}: {}", source_id, e);
    // Mark all symbols from this source as invalid
}
```

---

## 8. Логирование

Использовать `tracing` с уровнями:

**INFO:**
- Начало/конец каждого шага
- Итоговые цифры (количество пар, направлений, invalid)
- Прогресс выполнения

```rust
info!("Fetching instruments from Binance...");
info!("Found {} spot pairs, {} futures pairs", spot_count, fut_count);
```

**WARN:**
- Недоступные источники
- Большое количество invalid пар (>5%)
- MEXC futures недоступен

```rust
warn!("MEXC futures API unavailable");
warn!("High invalid rate for {}: {}%", source, rate);
```

**ERROR:**
- Критические ошибки (невозможность записать файлы)
- Полный отказ источника

```rust
error!("Failed to write symbols.bin: {}", e);
```

**DEBUG:**
- Подробности REST ответов
- WS сообщений
- Промежуточные результаты

```rust
debug!("Normalized {} -> {}", original, normalized);
debug!("Received WS message: {:?}", msg);
```

---

## 9. Тестирование

### 9.1. Unit-тесты

**`tests/normalize_tests.rs`:**
```rust
#[test]
fn test_normalize_binance_spot() {
    let raw = RawInstrument {
        exchange_symbol: "BTCUSDT".to_string(),
        base_asset: "BTC".to_string(),
        quote_asset: "USDT".to_string(),
        ...
    };
    let normalized = normalize(Exchange::Binance, Market::Spot, &raw);
    assert_eq!(normalized.normalized_name, "BTC-USDT");
}

#[test]
fn test_normalize_okx_swap() {
    let raw = RawInstrument {
        exchange_symbol: "BTC-USDT-SWAP".to_string(),
        ...
    };
    let normalized = normalize(Exchange::OKX, Market::Swap, &raw);
    assert_eq!(normalized.normalized_name, "BTC-USDT");
}

#[test]
fn test_normalize_mexc_futures() {
    let raw = RawInstrument {
        exchange_symbol: "BTC_USDT".to_string(),
        ...
    };
    let normalized = normalize(Exchange::MEXC, Market::Futures, &raw);
    assert_eq!(normalized.normalized_name, "BTC-USDT");
}
```

**`tests/build_global_list_tests.rs`:**
```rust
#[test]
fn test_deduplication() {
    let sources = vec![
        vec![
            NormalizedPair { normalized_name: "BTC-USDT", ... },
            NormalizedPair { normalized_name: "ETH-USDT", ... },
        ],
        vec![
            NormalizedPair { normalized_name: "BTC-USDT", ... }, // дубликат
            NormalizedPair { normalized_name: "SOL-USDT", ... },
        ],
    ];
    
    let registry = build_global_list(&sources);
    
    assert_eq!(registry.symbols.len(), 3); // BTC, ETH, SOL
    assert!(registry.source_mappings.contains_key(&(0, "BTCUSDT".to_string())));
    assert!(registry.source_mappings.contains_key(&(1, "BTC-USDT".to_string())));
}
```

**`tests/directions_tests.rs`:**
```rust
#[test]
fn test_intersection() {
    let registry = create_test_registry();
    let configs = vec![
        DirectionConfig {
            direction_id: 0,
            spot_source: 6,     // OKX spot
            futures_source: 1,  // MEXC futures
        },
    ];
    
    let directions = build_directions(&registry, &configs);
    
    // Проверить, что в пересечении только пары, присутствующие в обоих источниках
    for symbol_id in &directions[0].symbols {
        let record = &registry.symbols[*symbol_id as usize];
        assert!(record.source_names[6].is_some()); // есть на OKX spot
        assert!(record.source_names[1].is_some()); // есть на MEXC futures
    }
}
```

### 9.2. Интеграционные тесты

**`tests/integration_rest.rs`:**
```rust
#[tokio::test]
async fn test_real_binance_spot() {
    let instruments = fetch_binance_spot().await.unwrap();
    
    assert!(instruments.len() > 400);
    assert!(instruments.iter().any(|i| i.exchange_symbol == "BTCUSDT"));
    
    // Проверить парсинг полей
    let btc = instruments.iter().find(|i| i.exchange_symbol == "BTCUSDT").unwrap();
    assert_eq!(btc.base_asset, "BTC");
    assert_eq!(btc.quote_asset, "USDT");
    assert!(btc.tick_size.is_some());
}
```

**`tests/integration_ws.rs`:**
```rust
#[tokio::test]
async fn test_binance_ws_validation() {
    let symbols = vec![
        SymbolSub { symbol_id: 0, exchange_symbol: "BTCUSDT".to_string() },
        SymbolSub { symbol_id: 1, exchange_symbol: "ETHUSDT".to_string() },
    ];
    
    let result = validate_source(
        0,
        &symbols,
        "wss://stream.binance.com:9443/ws"
    ).await.unwrap();
    
    assert!(result.valid.contains(&0)); // BTC должен быть валидным
    assert!(result.valid.contains(&1)); // ETH должен быть валидным
}
```

**`tests/e2e.rs`:**
```rust
#[tokio::test]
async fn test_full_discovery_pipeline() {
    // 1. Запустить Discovery
    run_discovery().await.unwrap();
    
    // 2. Проверить наличие файлов
    assert!(Path::new("generated/symbols.bin").exists());
    assert!(Path::new("generated/directions.bin").exists());
    
    // 3. Загрузить и проверить
    let symbols: Vec<SymbolRecord> = bincode::deserialize(
        &fs::read("generated/symbols.bin").unwrap()
    ).unwrap();
    
    assert!(symbols.len() > 600);
    
    // 4. Проверить, что feeds могут загрузить
    let feed_registry = SymbolTable::load("generated/symbols.bin").unwrap();
    assert_eq!(feed_registry.len(), symbols.len());
}
```

---

## 10. Критичные требования

### 10.1. ТОЧНОСТЬ нормализации
- Одна и та же пара на разных биржах должна получить **ОДИНАКОВЫЙ** `symbol_id`
- Проверка: BTC-USDT на Binance, Bybit, OKX должен иметь один ID

### 10.2. ПОЛНОТА валидации
- Пропуск WS-валидации приведёт к краху фидов при подписке на мёртвые пары
- **ОБЯЗАТЕЛЬНО** проверять наличие непустых bid/ask

### 10.3. АТОМАРНОСТЬ генерации
- Файлы должны быть записаны **атомарно** (сначала в tmp, потом rename)
- Избежать ситуации, когда feeds читают частично записанный файл

```rust
// Правильно:
let tmp_path = output_dir.join(".symbols.bin.tmp");
fs::write(&tmp_path, &data)?;
fs::rename(&tmp_path, output_dir.join("symbols.bin"))?;
```

### 10.4. ДЕТЕРМИНИЗМ
- При одинаковых входных данных генерировать **одинаковые** `symbol_id`
- Решение: сортировать `normalized_name` перед присвоением ID

```rust
let mut names: Vec<_> = name_set.into_iter().collect();
names.sort(); // ВАЖНО!
for (id, name) in names.iter().enumerate() {
    // присвоить symbol_id = id
}
```

### 10.5. ПРОИЗВОДИТЕЛЬНОСТЬ
- Весь процесс не должен занимать более **90 секунд**
- Использовать параллелизм везде, где возможно
- Батчинг WS подписок

---

## 11. Дополнительные замечания

1. **Case sensitivity:**
   - Символы должны быть в **UPPER CASE** в нормализованном виде
   - При WS подписке учитывать case-sensitivity каждой биржи:
     - MEXC требует UPPERCASE
     - Binance — lowercase
     - OKX, Bybit — как в REST API

2. **MAX_SYMBOLS=1024:**
   - На будущее, сейчас ~682
   - Систему проектируем с запасом
   - Не хардкодить лимиты

3. **Сохранение min_qty и tick_size:**
   - Записывать в `symbols.bin`
   - Для будущего Order Manager

4. **Права доступа:**
   - Generated файлы должны иметь права чтения для всех процессов
   - `chmod 644` или создавать с правильными permissions

5. **Graceful degradation:**
   - Если один источник недоступен — продолжить с остальными
   - Логировать проблему, но не падать

6. **Версионирование:**
   - Добавить версию в `metadata.json`
   - Feeds/engine/tracker могут проверять совместимость

---

## 12. Конфигурационные файлы

### 12.1. `config/exchanges.toml`

```toml
[binance]
spot_rest = "https://api.binance.com"
futures_rest = "https://fapi.binance.com"
spot_ws = "wss://stream.binance.com:9443/ws"
futures_ws = "wss://fstream.binance.com/ws"

[bybit]
rest = "https://api.bybit.com"
spot_ws = "wss://stream.bybit.com/v5/public/spot"
linear_ws = "wss://stream.bybit.com/v5/public/linear"

[mexc]
spot_rest = "https://api.mexc.com"
futures_rest = "https://api.mexc.com"
spot_ws = "wss://wbs-api.mexc.com/ws"
futures_ws = "wss://contract.mexc.com/ws"

[okx]
rest = "https://www.okx.com"
ws = "wss://ws.okx.com:8443/ws/v5/public"
```

### 12.2. `config/directions.toml`

```toml
[[direction]]
id = 0
name = "okx_spot_mexc_futures"
spot_source = 6
futures_source = 1

[[direction]]
id = 1
name = "okx_spot_bybit_linear"
spot_source = 6
futures_source = 3

# ... ещё 10 направлений
```

---

## 13. Примеры использования

### 13.1. Запуск Discovery

```bash
# Из корня проекта
./pair-discovery --config config/config.toml --output generated/

# Или через cargo
cargo run --bin pair-discovery -- --config config/config.toml --output generated/
```

### 13.2. Проверка результатов

```bash
# Просмотр metadata
cat generated/metadata.json | jq

# Просмотр symbols
head -20 generated/symbols.txt

# Просмотр validation report
cat generated/validation_report.txt
```

### 13.3. Интеграция с systemd

**`/etc/systemd/system/pair-discovery.service`:**

```ini
[Unit]
Description=Pair Discovery Service
Before=spread-scanner.target

[Service]
Type=oneshot
User=spread-scanner
WorkingDirectory=/opt/spread-scanner
ExecStart=/opt/spread-scanner/bin/pair-discovery --config config/config.toml --output generated/
RemainAfterExit=yes

[Install]
WantedBy=spread-scanner.target
```

**Cron для периодического обновления:**

```cron
# Каждые 6 часов
0 */6 * * * systemctl start pair-discovery && systemctl restart spread-scanner.target
```

---

## 14. Метрики и мониторинг

**Логировать в structured format:**

```rust
info!(
    num_symbols = symbols.len(),
    num_directions = directions.len(),
    invalid_count = validation.total_invalid,
    duration_ms = start.elapsed().as_millis(),
    "Discovery completed"
);
```

**Ожидаемые значения:**
- `num_symbols`: 600-750
- `invalid_count`: <5% от total
- `duration_ms`: <90000

**Алерты:**
- `invalid_count > 10%` → WARNING
- `duration_ms > 120000` → WARNING
- `num_symbols < 500` → ERROR

---

## 15. Roadmap

### Phase 1 (MVP):
- ✅ REST сбор для всех 4 бирж
- ✅ Нормализация и глобальный список
- ✅ Построение направлений
- ✅ Базовая WS валидация

### Phase 2 (Production):
- ⬜ Продвинутая обработка ошибок
- ⬜ Метрики и мониторинг
- ⬜ Полное тестовое покрытие
- ⬜ Документация

### Phase 3 (Optimization):
- ⬜ Кэширование REST ответов
- ⬜ Инкрементальные обновления
- ⬜ WebSocket keepalive и reconnect

---

## 16. Заключение

После реализации модуля Pair Discovery система получит:

✅ **Автоматизацию:** Никаких ручных обновлений списков пар

✅ **Надёжность:** WS-валидация предотвращает подписку на мёртвые пары

✅ **Масштабируемость:** Легко добавить новые биржи или рынки

✅ **Согласованность:** Единый формат данных для всех компонентов

Модуль является **критически важным фундаментом** для всей системы Spread Scanner и должен быть реализован с максимальным вниманием к деталям и надёжности.

---

**Версия документа:** 1.0  
**Дата:** 2026-02-12  
**Автор:** Technical Architecture Team
