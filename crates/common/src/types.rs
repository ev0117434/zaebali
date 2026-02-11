use std::sync::atomic::AtomicU64;

// === Constants ===

pub const NUM_SOURCES: u8 = 8;
pub const MAX_SYMBOLS: u16 = 1024;
pub const MAX_DIRECTIONS: u8 = 12;

// === Source ID ===

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum SourceId {
    BinanceSpot = 0,
    BinanceFutures = 1,
    BybitSpot = 2,
    BybitFutures = 3,
    MexcSpot = 4,
    MexcFutures = 5,
    OkxSpot = 6,
    OkxFutures = 7,
}

impl SourceId {
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn is_spot(self) -> bool {
        matches!(
            self,
            SourceId::BinanceSpot
                | SourceId::BybitSpot
                | SourceId::MexcSpot
                | SourceId::OkxSpot
        )
    }

    pub fn is_futures(self) -> bool {
        !self.is_spot()
    }

    pub fn name(self) -> &'static str {
        match self {
            SourceId::BinanceSpot => "binance_spot",
            SourceId::BinanceFutures => "binance_futures",
            SourceId::BybitSpot => "bybit_spot",
            SourceId::BybitFutures => "bybit_futures",
            SourceId::MexcSpot => "mexc_spot",
            SourceId::MexcFutures => "mexc_futures",
            SourceId::OkxSpot => "okx_spot",
            SourceId::OkxFutures => "okx_futures",
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(SourceId::BinanceSpot),
            1 => Some(SourceId::BinanceFutures),
            2 => Some(SourceId::BybitSpot),
            3 => Some(SourceId::BybitFutures),
            4 => Some(SourceId::MexcSpot),
            5 => Some(SourceId::MexcFutures),
            6 => Some(SourceId::OkxSpot),
            7 => Some(SourceId::OkxFutures),
            _ => None,
        }
    }
}

// === Price Store Entries (split seq/data) ===

/// Sequence entry — one per (symbol, source) slot.
/// Aligned to 64 bytes to avoid false sharing.
#[repr(C, align(64))]
pub struct PriceSeqEntry {
    pub seq: AtomicU64,
    pub _pad: [u8; 56],
}

impl PriceSeqEntry {
    pub const SIZE: usize = 64;
}

/// Data entry — one per (symbol, source) slot.
/// Aligned to 64 bytes to avoid false sharing.
#[repr(C, align(64))]
pub struct PriceDataEntry {
    pub best_bid: f64,
    pub best_ask: f64,
    pub updated_at: u64,
    pub _pad: [u8; 40],
}

impl PriceDataEntry {
    pub const SIZE: usize = 64;
}

/// Non-atomic snapshot returned from SeqLock reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct PriceSnapshot {
    pub best_bid: f64,
    pub best_ask: f64,
    pub updated_at: u64,
}

impl PriceSnapshot {
    pub fn is_valid(&self) -> bool {
        self.best_bid > 0.0 && self.best_ask > 0.0 && self.best_bid <= self.best_ask
    }
}

// === Events ===

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u16)]
pub enum EventType {
    SpreadSignal = 1,
    TrackingSnapshot = 2,
    // Reserved: 10..15 orders, 20..22 positions, 90..99 control, 100..103 health
}

impl EventType {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(EventType::SpreadSignal),
            2 => Some(EventType::TrackingSnapshot),
            _ => None,
        }
    }
}

/// Event header — fixed layout for all event types.
/// Carefully ordered so u64 fields are already 8-byte aligned.
/// Total: 24 bytes (no padding).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EventHeader {
    pub timestamp: u64,
    pub sequence: u64,
    pub event_type: u16,
    pub source_proc: u8,
    pub _reserved: u8,
    pub payload_len: u16,
    pub _reserved2: [u8; 2],
}

impl EventHeader {
    pub const SIZE: usize = std::mem::size_of::<Self>();
}

/// Fixed-size 64-byte event for the ring buffer.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Event {
    pub header: EventHeader,
    pub payload: [u8; 40],
}

impl Event {
    pub const SIZE: usize = 64;
}

/// Payload for SpreadSignal events.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SignalPayload {
    pub symbol_id: u16,
    pub direction_id: u8,
    pub spot_source: u8,
    pub futures_source: u8,
    pub _pad: [u8; 3],
    pub spot_ask: f64,
    pub futures_bid: f64,
    pub spread_pct: f64,
}

impl SignalPayload {
    pub const SIZE: usize = std::mem::size_of::<Self>();

    pub fn from_event(event: &Event) -> Option<Self> {
        if event.header.event_type != EventType::SpreadSignal as u16 {
            return None;
        }
        if (event.header.payload_len as usize) < Self::SIZE {
            return None;
        }
        // Safety: SignalPayload is repr(C) and fits within 40 bytes
        let ptr = event.payload.as_ptr() as *const SignalPayload;
        Some(unsafe { ptr.read_unaligned() })
    }

    pub fn write_to_event(&self, event: &mut Event) {
        let src = self as *const SignalPayload as *const u8;
        let dst = event.payload.as_mut_ptr();
        unsafe {
            std::ptr::copy_nonoverlapping(src, dst, Self::SIZE);
        }
        event.header.payload_len = Self::SIZE as u16;
    }
}

/// Direction entry — maps (source, symbol) to a direction and counterpart.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct DirectionEntry {
    pub direction_id: u8,
    pub counterpart_source: u8,
}

// === Static asserts ===

const _: () = {
    assert!(std::mem::size_of::<PriceSeqEntry>() == 64);
    assert!(std::mem::align_of::<PriceSeqEntry>() == 64);
    assert!(std::mem::size_of::<PriceDataEntry>() == 64);
    assert!(std::mem::align_of::<PriceDataEntry>() == 64);
    assert!(std::mem::size_of::<Event>() == 64);
    assert!(SignalPayload::SIZE <= 40);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_id() {
        assert_eq!(SourceId::BinanceSpot.index(), 0);
        assert_eq!(SourceId::OkxFutures.index(), 7);
        assert!(SourceId::BinanceSpot.is_spot());
        assert!(SourceId::BinanceFutures.is_futures());
        assert_eq!(SourceId::from_u8(0), Some(SourceId::BinanceSpot));
        assert_eq!(SourceId::from_u8(8), None);
    }

    #[test]
    fn test_sizes_and_alignment() {
        assert_eq!(std::mem::size_of::<PriceSeqEntry>(), 64);
        assert_eq!(std::mem::align_of::<PriceSeqEntry>(), 64);
        assert_eq!(std::mem::size_of::<PriceDataEntry>(), 64);
        assert_eq!(std::mem::align_of::<PriceDataEntry>(), 64);
        assert_eq!(std::mem::size_of::<Event>(), 64);
    }

    #[test]
    fn test_signal_payload_roundtrip() {
        let signal = SignalPayload {
            symbol_id: 42,
            direction_id: 3,
            spot_source: 6,
            futures_source: 5,
            _pad: [0; 3],
            spot_ask: 50000.5,
            futures_bid: 50100.0,
            spread_pct: 0.199,
        };

        let mut event = Event {
            header: EventHeader {
                timestamp: 12345,
                sequence: 1,
                event_type: EventType::SpreadSignal as u16,
                source_proc: 0,
                _reserved: 0,
                payload_len: 0,
                _reserved2: [0; 2],
            },
            payload: [0u8; 40],
        };

        signal.write_to_event(&mut event);
        let decoded = SignalPayload::from_event(&event).unwrap();

        assert_eq!(decoded.symbol_id, 42);
        assert_eq!(decoded.direction_id, 3);
        assert!((decoded.spot_ask - 50000.5).abs() < f64::EPSILON);
        assert!((decoded.spread_pct - 0.199).abs() < f64::EPSILON);
    }

    #[test]
    fn test_price_snapshot_valid() {
        let snap = PriceSnapshot {
            best_bid: 100.0,
            best_ask: 101.0,
            updated_at: 1,
        };
        assert!(snap.is_valid());

        let invalid = PriceSnapshot {
            best_bid: 0.0,
            best_ask: 101.0,
            updated_at: 1,
        };
        assert!(!invalid.is_valid());

        let crossed = PriceSnapshot {
            best_bid: 102.0,
            best_ask: 101.0,
            updated_at: 1,
        };
        assert!(!crossed.is_valid());
    }
}
