use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use core_affinity::CoreId;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use tracing::warn;

use crate::cores;
use crate::engine::contestant::Contestant;
use crate::engine::stats::{Histogram, StageStats};
use crate::net::tcp::ContestantSubmission;
use crate::protocol::feed::{FeedTick, PriceUpdate};
use crate::protocol::market::{Fee, Market, Token, TokenPair};
use crate::protocol::submission::{Direction, RouteSubmission, SubmissionResponse};

const SLEEP_SLACK_US: u64 = 50;
const LATE_THRESHOLD_NS: u64 = 10_000;
const SUBMISSION_DRAIN_LIMIT: usize = 64;
const USD_TOKEN_ID: u64 = 0;

#[derive(Debug, Clone)]
pub struct RunloopOutcome {
    pub dispatched: usize,
    pub udp_dispatch_errors: u64,
    pub submissions_received: u64,
    pub submissions_ok: u64,
    pub submissions_fail: u64,
    pub responses_dropped: u64,
    pub final_overshoot_ns: u64,
    pub late_ticks: u64,
    pub read_stats: StageStats,
    pub wait_stats: StageStats,
    pub send_stats: StageStats,
    pub iter_stats: StageStats,
    pub overshoot_stats: StageStats,
    pub udp_queue_depth_stats: StageStats,
}

pub fn spawn(
    core_id: CoreId,
    feed: Vec<FeedTick>,
    market: Market,
    contestants: HashMap<u64, Contestant>,
    send_tx: Sender<PriceUpdate>,
    sub_rx: Receiver<ContestantSubmission>,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<RunloopOutcome> {
    thread::Builder::new()
        .name("engine-runloop".into())
        .spawn(move || {
            cores::pin_and_verify(core_id);
            run(feed, market, contestants, send_tx, sub_rx, shutdown)
        })
        .expect("spawn engine-runloop thread")
}

fn run(
    feed: Vec<FeedTick>,
    market: Market,
    mut contestants: HashMap<u64, Contestant>,
    udp_tx: Sender<PriceUpdate>,
    sub_rx: Receiver<ContestantSubmission>,
    shutdown: Arc<AtomicBool>,
) -> RunloopOutcome {
    let Market { fee, tokens, mut pairs } = market;

    // For each non-USD token t, holds the index in `pairs` of the t/USD pair.
    // i.e. this array maps token t with t_id -> idx in pairs of t/USD. pairs[usd_pair_of_token[t_id]] = t/USD.
    let usd_pair_of_token: Vec<usize> = {
        let mut usd_pair_of_token: Vec<usize> = vec![usize::MAX; tokens.len()];
        for (idx, p) in pairs.iter().enumerate() {
            if p.quote == USD_TOKEN_ID {
                usd_pair_of_token[p.base as usize] = idx;
            }
        }
        usd_pair_of_token
    };

    let mut overshoot_ns = 0u64;
    let mut late_ticks = 0u64;
    let mut dispatched = 0usize;
    let mut udp_dispatch_errors = 0u64;
    let mut submissions_received = 0u64;
    let mut submissions_ok = 0u64;
    let mut submissions_fail = 0u64;
    let mut responses_dropped = 0u64;

    let feed_len = feed.len();
    let mut idx = 0usize;
    let mut target = Instant::now();
    if feed_len > 0 {
        target += Duration::from_micros(feed[0].delay_us);
    }

    let mut read_hist = Histogram::new();
    let mut wait_hist = Histogram::new();
    let mut send_hist = Histogram::new();
    let mut iter_hist = Histogram::new();
    let mut overshoot_hist = Histogram::new();
    let mut udp_queue_depth_hist = Histogram::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        let iter_start = Instant::now();

        // 1. Read & evaluate submissions.
        let read_start = iter_start;
        for _ in 0..SUBMISSION_DRAIN_LIMIT {
            match sub_rx.try_recv() {
                Ok(msg) => {
                    submissions_received += 1;

                    let (ok, balance) = evaluate(
                        &tokens,
                        &mut pairs,
                        &usd_pair_of_token,
                        fee,
                        &mut contestants,
                        msg.contestant_id,
                        &msg.submission,
                    );
                    if ok == 1 {
                        submissions_ok += 1;
                    } else {
                        submissions_fail += 1;
                    }

                    let resp = SubmissionResponse {
                        sub_id: msg.submission.sub_id,
                        ok,
                        balance,
                    };

                    if msg.response_tx.send(resp).is_err() {
                        responses_dropped += 1;
                    }
                }
                Err(_) => break,
            }
        }
        let after_read = Instant::now();
        read_hist.record((after_read - read_start).as_nanos() as u64);

        if idx >= feed_len {
            iter_hist.record((after_read - iter_start).as_nanos() as u64);
            break;
        }

        // 2. Check if can send tick.
        if after_read < target {
            let wait_start = after_read;
            if target > wait_start + Duration::from_micros(SLEEP_SLACK_US) {
                thread::sleep(target - wait_start - Duration::from_micros(SLEEP_SLACK_US));
            } else {
                std::hint::spin_loop();
            }
            let after_wait = Instant::now();
            wait_hist.record((after_wait - wait_start).as_nanos() as u64);
            iter_hist.record((after_wait - iter_start).as_nanos() as u64);
            continue;
        }

        // 3. Dispatch the tick to the UDP send thread and update local state.
        overshoot_ns = after_read.saturating_duration_since(target).as_nanos() as u64;
        if overshoot_ns > LATE_THRESHOLD_NS {
            late_ticks += 1;
        }
        overshoot_hist.record(overshoot_ns);

        let update = feed[idx].update;
        let pair_idx = update.token_pair_id as usize;
        pairs[pair_idx].price = update.price;
        pairs[pair_idx].volume = update.volume;

        let send_start = after_read;
        match udp_tx.try_send(update) {
            Ok(()) => {
                dispatched += 1;
                udp_queue_depth_hist.record(udp_tx.len() as u64);
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                udp_dispatch_errors += 1;
            }
        }
        let after_send = Instant::now();
        send_hist.record((after_send - send_start).as_nanos() as u64);
        iter_hist.record((after_send - iter_start).as_nanos() as u64);

        idx += 1;
        if idx < feed_len {
            target += Duration::from_micros(feed[idx].delay_us);
        }
    }

    RunloopOutcome {
        dispatched,
        udp_dispatch_errors,
        submissions_received,
        submissions_ok,
        submissions_fail,
        responses_dropped,
        final_overshoot_ns: overshoot_ns,
        late_ticks,
        read_stats: read_hist.stats("read", "ns"),
        wait_stats: wait_hist.stats("wait", "ns"),
        send_stats: send_hist.stats("send", "ns"),
        iter_stats: iter_hist.stats("iter", "ns"),
        overshoot_stats: overshoot_hist.stats("overshoot", "ns"),
        udp_queue_depth_stats: udp_queue_depth_hist.stats("udp_queue_depth", "items"),
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate(
    tokens: &[Token],
    pairs: &mut [TokenPair],
    usd_pair_of_token: &[usize],
    fee: Fee,
    contestants: &mut HashMap<u64, Contestant>,
    contestant_id: u64,
    submission: &RouteSubmission,
) -> (u8, i64) {
    fn bail(contestant: &mut Contestant, fees: u64) -> (u8, i64) {
        contestant.balance -= fees as i64;
        (0, contestant.balance)
    }

    let Some(contestant) = contestants.get_mut(&contestant_id) else {
        warn!(contestant_id, "submission from unknown contestant");
        return (0, 0);
    };

    // Underwater contestants get an immediate no-op rejection — no fees, no
    // processing. They have to wait until profitable trades pull them back
    // above zero before any further submission is honored.
    if contestant.balance < 0 {
        return (0, contestant.balance);
    }

    let legs = &submission.legs;

    // Pre-loop structural rejects each cost one static fee.
    let n = legs.len();
    if n < 2 {
        return bail(contestant, fee.static_atomic_usd);
    }
    let Some(first_pair) = pairs.get(legs[0].pair_id as usize) else {
        return bail(contestant, fee.static_atomic_usd);
    };
    if first_pair.quote != USD_TOKEN_ID || legs[0].direction != Direction::Buy {
        return bail(contestant, fee.static_atomic_usd);
    }
    let Some(last_pair) = pairs.get(legs[n - 1].pair_id as usize) else {
        return bail(contestant, fee.static_atomic_usd);
    };
    if last_pair.quote != USD_TOKEN_ID || legs[n - 1].direction != Direction::Sell {
        return bail(contestant, fee.static_atomic_usd);
    }

    // All multiplications and sums are assumed to stay within u64 by the caps instroduced (and checked) by generation config.
    let mut prev_output_token: u64 = 0;
    let mut prev_output_amount: u64 = 0;
    let mut initial_usd: u64 = 0;
    let mut final_usd: u64 = 0;
    let mut total_fees: u64 = 0;
    let mut pool_consumption: HashMap<u64, u64> = HashMap::with_capacity(legs.len() * 2);

    for (i, leg) in legs.iter().enumerate() {
        let pair_idx = leg.pair_id as usize;
        let Some(pair) = pairs.get(pair_idx) else {
            return bail(contestant, total_fees + fee.static_atomic_usd);
        };
        let Some(base_token) = tokens.get(pair.base as usize) else {
            return bail(contestant, total_fees + fee.static_atomic_usd);
        };

        let pow_base = 10u64.pow(base_token.decimals as u32);
        let base_usd_price = pairs[usd_pair_of_token[pair.base as usize]].price;
        let usd_value = leg.volume * base_usd_price / pow_base;
        let variable_fee = fee.variable_bps * usd_value / 10_000;
        let leg_fee = fee.static_atomic_usd + variable_fee;
        total_fees += leg_fee;

        // Price must match current state exactly.
        if leg.price != pair.price {
            return bail(contestant, total_fees);
        }

        // Per-pair volume cap, with intra-submission accumulation.
        let consumed_so_far = *pool_consumption.get(&leg.pair_id).unwrap_or(&0);
        let new_consumed = consumed_so_far + leg.volume;
        if new_consumed > pair.volume {
            return bail(contestant, total_fees);
        }
        pool_consumption.insert(leg.pair_id, new_consumed);

        let quote_atomic = leg.volume * leg.price / pow_base;

        let (input_token, input_amount, output_token, output_amount) = match leg.direction {
            Direction::Buy => (pair.quote, quote_atomic, pair.base, leg.volume),
            Direction::Sell => (pair.base, leg.volume, pair.quote, quote_atomic),
        };

        // Chain continuity (token + exact amount).
        if i == 0 {
            initial_usd = input_amount;
        } else if input_token != prev_output_token || input_amount != prev_output_amount {
            return bail(contestant, total_fees);
        }

        if i == n - 1 {
            final_usd = output_amount;
        }

        prev_output_token = output_token;
        prev_output_amount = output_amount;
    }

    for (pair_id, consumed) in pool_consumption {
        let p = &mut pairs[pair_id as usize];
        p.volume = p.volume.saturating_sub(consumed);
    }

    let delta = (final_usd as i64) - (initial_usd as i64) - (total_fees as i64);
    let new_balance = contestant.balance + delta;
    contestant.balance = new_balance;

    (1, new_balance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::submission::{Direction, RouteSubmission, RouteSubmissionLeg};

    fn fee() -> Fee {
        Fee {
            static_atomic_usd: 1_000, // 0.001 USD (with usd_decimals=6)
            variable_bps: 10,         // 0.1%
        }
    }

    // Three-token market with usd_decimals=6:
    //   token 0 = USD (decimals 6)
    //   token 1 = A   (decimals 6, $2 / A)
    //   token 2 = B   (decimals 6, $3 / B)
    // Pairs:
    //   pair 0: A/USD (base=1, quote=0), price = 2_000_000 (atomic-USD per 1 whole A)
    //   pair 1: B/USD (base=2, quote=0), price = 3_000_000
    //   pair 2: A/B  (base=1, quote=2), price computed so A and B agree on USD value:
    //           1 A = $2 = 2/3 B, so price = (2/3) * 10^6 = 666_666 atomic-B per 1 whole A
    fn setup() -> (Vec<TokenPair>, Vec<Token>, Vec<usize>, HashMap<u64, Contestant>) {
        let pairs = vec![
            TokenPair {
                id: 0,
                base: 1,
                quote: 0,
                price: 2_000_000,
                volume: 1_000_000_000,
            },
            TokenPair {
                id: 1,
                base: 2,
                quote: 0,
                price: 3_000_000,
                volume: 1_000_000_000,
            },
            TokenPair {
                id: 2,
                base: 1,
                quote: 2,
                price: 666_666,
                volume: 1_000_000_000,
            },
        ];
        let tokens = vec![
            Token { id: 0, decimals: 6 },
            Token { id: 1, decimals: 6 },
            Token { id: 2, decimals: 6 },
        ];
        // token 0 (USD) is unused as base, so the sentinel never gets read.
        // token 1 (A) → pair 0 (A/USD); token 2 (B) → pair 1 (B/USD).
        let usd_pair_of_token = vec![usize::MAX, 0, 1];
        let mut contestants = HashMap::new();
        contestants.insert(42, Contestant::new(42, 1_000_000_000));
        (pairs, tokens, usd_pair_of_token, contestants)
    }

    fn leg(pair_id: u64, direction: Direction, price: u64, volume: u64) -> RouteSubmissionLeg {
        RouteSubmissionLeg {
            pair_id,
            direction,
            price,
            volume,
        }
    }

    fn submission(legs: Vec<RouteSubmissionLeg>) -> RouteSubmission {
        RouteSubmission { sub_id: 1, legs }
    }

    #[test]
    fn rejects_too_few_legs() {
        let (mut s, m, u, mut c) = setup();
        let sub = submission(vec![leg(0, Direction::Buy, 2_000_000, 1_000_000)]);
        let (ok, _) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 0);
        // Malformed (< 2 legs) submissions still pay the static fee.
        assert_eq!(c[&42].balance, 1_000_000_000_i64 - fee().static_atomic_usd as i64);
    }

    #[test]
    fn rejects_unknown_contestant() {
        let (mut s, m, u, mut c) = setup();
        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 1_000_000),
        ]);
        let (ok, bal) = evaluate(&m, &mut s, &u, fee(), &mut c, 99, &sub);
        assert_eq!(ok, 0);
        assert_eq!(bal, 0);
    }

    #[test]
    fn rejects_price_mismatch() {
        let (mut s, m, u, mut c) = setup();
        let start_balance = c[&42].balance;
        let sub = submission(vec![
            leg(0, Direction::Buy, 1_999_999, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 1_000_000),
        ]);
        let (ok, _) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 0);
        // Leg 0 fee was computed before the price check failed: static (1_000)
        // + 10 bps × $2 atomic (= 2_000) = 3_000.
        assert_eq!(c[&42].balance, start_balance - 3_000);
    }

    #[test]
    fn rejects_first_leg_not_usd() {
        let (mut s, m, u, mut c) = setup();
        let start_balance = c[&42].balance;
        let sub = submission(vec![
            leg(2, Direction::Buy, 666_666, 1_000_000),
            leg(1, Direction::Sell, 3_000_000, 666_666),
        ]);
        let (ok, _) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 0);
        // Structural pre-loop reject: one static fee.
        assert_eq!(c[&42].balance, start_balance - fee().static_atomic_usd as i64);
    }

    #[test]
    fn rejects_chain_amount_mismatch() {
        let (mut s, m, u, mut c) = setup();
        let start_balance = c[&42].balance;
        // leg 0 outputs 1_000_000 atomic-A; leg 1 tries to sell 2_000_000 atomic-A.
        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 2_000_000),
        ]);
        let (ok, _) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 0);
        // Both legs had their fees computed before the chain mismatch fired.
        // leg 0: 3_000 (static + 10 bps × $2). leg 1: 1_000 + 10 bps × $4 = 5_000.
        assert_eq!(c[&42].balance, start_balance - (3_000 + 5_000));
    }

    #[test]
    fn rejects_volume_over_pool() {
        let (mut s, m, u, mut c) = setup();
        let start_balance = c[&42].balance;
        s[0].volume = 500_000; // less than what we try to fill
        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 1_000_000),
        ]);
        let (ok, _) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 0);
        assert_eq!(s[0].volume, 500_000); // unchanged
                                          // Leg 0 fee was charged before the volume cap fired.
        assert_eq!(c[&42].balance, start_balance - 3_000);
    }

    #[test]
    fn intra_submission_pool_depletion_enforced() {
        let (mut s, m, u, mut c) = setup();
        let start_balance = c[&42].balance;
        // Pool has 1.5M; both legs together want 2M.
        s[0].volume = 1_500_000;
        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 1_000_000),
        ]);
        let (ok, _) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 0);
        assert_eq!(s[0].volume, 1_500_000); // unchanged
                                            // Leg 0 fee + leg 1 fee charged before leg 1's volume cap fired.
        assert_eq!(c[&42].balance, start_balance - (3_000 + 3_000));
    }

    #[test]
    fn round_trip_two_legs_loses_fees() {
        // Buy 1 whole A for $2, immediately Sell 1 whole A for $2.
        // Net USD: 0. Two legs of fees: each = static + variable_bps * $2.
        let (mut s, m, u, mut c) = setup();
        let start_balance = c[&42].balance;
        let start_pool = s[0].volume;

        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 1_000_000),
        ]);
        let (ok, bal) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 1);
        // Expected fee per leg: static (1_000) + 10 bps of $2 atomic = 10 * 2_000_000 / 10_000 = 2_000.
        // Total = 2 * 3_000 = 6_000.
        let expected = start_balance - 6_000;
        assert_eq!(bal, expected);
        assert_eq!(c[&42].balance, expected);
        // Pool was depleted by 2_000_000 (sum of both leg volumes).
        assert_eq!(s[0].volume, start_pool - 2_000_000);
    }

    #[test]
    fn submission_pushes_balance_negative() {
        // Same round-trip as `round_trip_two_legs_loses_fees`, but the
        // contestant starts with less than the 6_000 in fees. The trade
        // still executes; the balance just dips underwater.
        let (mut s, m, u, mut c) = setup();
        c.get_mut(&42).unwrap().balance = 1_000;
        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 1_000_000),
        ]);
        let (ok, bal) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 1);
        // Net delta = -6_000 (round-trip on same pair, just fees); 1_000 - 6_000 = -5_000.
        assert_eq!(bal, -5_000);
        assert_eq!(c[&42].balance, -5_000);
    }

    #[test]
    fn rejects_when_balance_already_negative() {
        // Underwater contestants get an immediate no-op rejection — no fees,
        // no state mutation, no processing.
        let (mut s, m, u, mut c) = setup();
        c.get_mut(&42).unwrap().balance = -42;
        let start_pool = s[0].volume;
        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(0, Direction::Sell, 2_000_000, 1_000_000),
        ]);
        let (ok, bal) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 0);
        assert_eq!(bal, -42);
        assert_eq!(c[&42].balance, -42); // unchanged
        assert_eq!(s[0].volume, start_pool); // pool untouched
    }

    #[test]
    fn three_leg_round_trip_succeeds() {
        // A -> B -> A round trip via cross pair, then back to USD.
        // 1 whole A -> 0.666666 B (price 666_666 atomic-B per 1 whole A)
        // 0.666666 B -> ? A at the same price (Buy A on A/B pair)
        // For an exact return, leg 2: Buy on A/B pair: input atomic-B = volume * price / 10^base_dec
        //   leg2.volume must round-trip back to 1 whole A; with integer math we have to engineer this.
        // Simpler: do USD -> A -> USD using two USD pairs A/USD and B/USD via cross.
        // Use Buy USD->A, Sell A on A/B for B, Sell B->USD.
        let (mut s, m, u, mut c) = setup();
        let start_balance = c[&42].balance;
        // leg 0: Buy on A/USD: spend 1 A * 2_000_000 = 2_000_000 atomic-USD, receive 1_000_000 atomic-A.
        // leg 1: Sell on A/B: spend 1_000_000 atomic-A, receive 1_000_000 * 666_666 / 10^6 = 666_666 atomic-B.
        // leg 2: Sell on B/USD: spend 666_666 atomic-B, receive 666_666 * 3_000_000 / 10^6 = 1_999_998 atomic-USD.
        // Net USD: 1_999_998 - 2_000_000 = -2 atomic-USD (rounding).
        // Fees: 3 legs, each = static + variable_bps * (leg.volume * usd_price[base] / 10^base.dec) / 10000.
        //   leg0 base=A, usd_value = 10^6 * 2_000_000 / 10^6 = 2_000_000.
        //        fee = 1000 + 10*2_000_000/10_000 = 3000.
        //   leg1 base=A, usd_value = 2_000_000. fee = 3000.
        //   leg2 base=B, usd_value = 666_666 * 3_000_000 / 10^6 = 1_999_998. fee = 1000 + 1999 = 2999.
        // Total fees = 8999.
        // Net delta = -2 - 8999 = -9001.
        let sub = submission(vec![
            leg(0, Direction::Buy, 2_000_000, 1_000_000),
            leg(2, Direction::Sell, 666_666, 1_000_000),
            leg(1, Direction::Sell, 3_000_000, 666_666),
        ]);
        let (ok, bal) = evaluate(&m, &mut s, &u, fee(), &mut c, 42, &sub);
        assert_eq!(ok, 1, "should succeed");
        assert_eq!(bal, start_balance - 9001);
    }
}
