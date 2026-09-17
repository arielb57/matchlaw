//! Seeded generator of adversarial order streams.
//!
//! Streams are deliberately hostile: prices sit in a band of a few ticks so single levels
//! build long queues, a handful of accounts trade against themselves constantly, and
//! icebergs have display sizes smaller than typical incoming orders.

use std::collections::HashMap;

use crate::types::*;

/// SplitMix64: tiny, fast and good enough for test-stream generation.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }
}

#[derive(Clone, Debug)]
pub struct GenConfig {
    pub events: usize,
    pub accounts: u32,
    /// Prices are drawn from `mid - band ..= mid + band`.
    pub band: i64,
    pub max_qty: u64,
    /// Percentage of events that cancel a previously submitted order.
    pub cancel_percent: u64,
    /// Percentage of events that amend a previously submitted order.
    pub amend_percent: u64,
}

impl GenConfig {
    pub fn adversarial(events: usize) -> GenConfig {
        GenConfig {
            events,
            accounts: 3,
            band: 2,
            max_qty: 12,
            cancel_percent: 14,
            amend_percent: 16,
        }
    }

    /// Wider prices and more accounts, closer to a realistic flow; used for throughput.
    pub fn benchmark(events: usize) -> GenConfig {
        GenConfig {
            events,
            accounts: 50,
            band: 20,
            max_qty: 100,
            cancel_percent: 36,
            amend_percent: 10,
        }
    }
}

pub fn generate(seed: u64, cfg: &GenConfig) -> Vec<Event> {
    let mut rng = Rng::new(seed);
    let mid: i64 = 1000;
    let mut events = Vec::with_capacity(cfg.events);
    let mut resting_candidates: Vec<OrderId> = Vec::new();
    let mut next_id: OrderId = 1;
    let mut last_price: HashMap<OrderId, Price> = HashMap::new();

    for _ in 0..cfg.events {
        let roll = rng.range(0, 99);
        if roll < cfg.cancel_percent && !resting_candidates.is_empty() {
            let idx = rng.range(0, resting_candidates.len() as u64 - 1) as usize;
            let id = resting_candidates.swap_remove(idx);
            events.push(Event::Cancel { id });
            continue;
        }
        if roll < cfg.cancel_percent + cfg.amend_percent && !resting_candidates.is_empty() {
            let idx = rng.range(0, resting_candidates.len() as u64 - 1) as usize;
            let id = resting_candidates[idx];
            // Half the amends keep the price so that in-place quantity decreases, the one
            // amend that keeps priority, are common rather than a 1-in-5 accident.
            let price = match last_price.get(&id) {
                Some(&p) if rng.chance(50) => p,
                _ => mid + rng.range(0, 2 * cfg.band as u64) as i64 - cfg.band,
            };
            let qty = rng.range(1, cfg.max_qty * 2);
            last_price.insert(id, price);
            events.push(Event::Amend { id, price, qty });
            continue;
        }

        let id = next_id;
        next_id += 1;
        let side = if rng.chance(50) { Side::Buy } else { Side::Sell };
        let account = rng.range(1, cfg.accounts as u64) as Account;
        let qty = rng.range(1, cfg.max_qty);
        let kind = rng.range(0, 99);
        let (price, tif) = match kind {
            0..=5 => (None, if rng.chance(70) { Tif::Ioc } else { Tif::Fok }),
            6..=15 => (Some(()), Tif::Ioc),
            16..=25 => (Some(()), Tif::Fok),
            _ => (Some(()), Tif::Gtc),
        };
        let price = price.map(|_| mid + rng.range(0, 2 * cfg.band as u64) as i64 - cfg.band);
        let display = if tif == Tif::Gtc && price.is_some() && rng.chance(30) {
            Some(rng.range(1, (qty / 3).max(1)))
        } else {
            None
        };
        if let (Tif::Gtc, Some(p)) = (tif, price) {
            last_price.insert(id, p);
            resting_candidates.push(id);
        }
        events.push(Event::New(NewOrder {
            id,
            account,
            side,
            price,
            qty,
            display,
            tif,
        }));
    }
    events
}
