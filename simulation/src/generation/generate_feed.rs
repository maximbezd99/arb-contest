use anyhow::Result;
use rand::{Rng, SeedableRng};
use rand_pcg::Pcg64Mcg;

use super::config::GenerationConfig;
use super::math::{cross_pair_price, pow10};
use crate::protocol::feed::{FeedTick, PriceUpdate};
use crate::protocol::market::{Market, Token, TokenPair};

/// Generate a deterministic price-update feed on top of an initial [`Market`].
///
/// Generates exactly `cfg.arb_count` mispricings. Time advances in 1 ms
/// windows; each window contributes `cfg.updates_per_ms` mispricings
/// scattered at random offsets in `[0, 1000)` µs. Each mispricing perturbs
/// one token's USD price, emits a "main" update on a random adjacent pair
/// at the mispricing's instant, and schedules a rebalancing update on every
/// other adjacent pair at `mispricing_time + uniform(rebalance_delay_us)`.
///
/// Generation is four-pass:
///   1. Schedule all events (mispricings + rebalances) as `(t, kind)`.
///   2. Sort by absolute time; walk in time order maintaining live `pu`;
///      compute every emitted price from `pu` at emit time. This makes the
///      feed eventually consistent — the last event on each pair carries
///      the post-state equilibrium price.
///   3. Per-pair rebalance-cluster dedupe: a "duplicate rebalance cluster"
///      is a maximal run of consecutive *rebalance* events on a pair that
///      all emit the same price (no pu change for either of the pair's
///      tokens between them, no `main` interleaved). For each cluster of
///      length ≥ 2 we randomly pick one rebalance as the *price-change*
///      moment; earlier rebalances in the cluster revert to the pair's
///      prior distinct price, later rebalances keep the new price.
///   4. Per-tick volume random walk: each emitted tick perturbs the pair's
///      current volume by ± `volume_perturb_bps`, clamped to the pair's
///      `volume_usd`-derived range.
pub fn generate_feed(market: &Market, cfg: &GenerationConfig, seed: u64) -> Result<Vec<FeedTick>> {
    let mut rng = Pcg64Mcg::seed_from_u64(seed);

    let usd_scale = pow10(cfg.usd_decimals);
    let pu_min = cfg.price_usd.min * usd_scale;
    let pu_max = cfg.price_usd.max * usd_scale;

    // pu[token_id] = current USD price in atomic-USD per whole token.
    let mut pu: Vec<u64> = vec![usd_scale; market.tokens.len()];
    let usd_pair_count = market.tokens.len() - 1;
    for p in market.pairs.iter().take(usd_pair_count) {
        pu[p.base as usize] = p.price;
    }

    // adj[token_id] = indices into market.pairs of every pair that involves
    // this token. USD pairs land in adj[base] only (we never mispriceto USD).
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); market.tokens.len()];
    for (idx, p) in market.pairs.iter().enumerate() {
        adj[p.base as usize].push(idx);
        if p.quote != 0 {
            adj[p.quote as usize].push(idx);
        }
    }

    // Per-pair volume range (computed from initial pu so the bounds are
    // stable across the feed even as pu drifts).
    let mut volume_min = vec![0u64; market.pairs.len()];
    let mut volume_max = vec![0u64; market.pairs.len()];
    let mut current_volume = vec![0u64; market.pairs.len()];
    for (i, p) in market.pairs.iter().enumerate() {
        let base_decimals = market.tokens[p.base as usize].decimals;
        let base_factor = pow10(base_decimals);
        let pu_base = pu[p.base as usize];
        volume_min[i] = cfg.volume_usd.min * usd_scale * base_factor / pu_base;
        volume_max[i] = cfg.volume_usd.max * usd_scale * base_factor / pu_base;
        current_volume[i] = p.volume;
    }

    // --- Pass 1: schedule events. ---
    let mut events: Vec<Event> = Vec::with_capacity(cfg.arb_count as usize * 32);
    let arbs_per_window = cfg.updates_per_ms as usize;
    let total_arbs = cfg.arb_count as usize;
    let mut arbs_emitted = 0usize;
    let mut window_idx: u64 = 0;
    'outer: while arbs_emitted < total_arbs {
        let mut offsets: Vec<u64> = (0..arbs_per_window).map(|_| rng.gen_range(0..1_000)).collect();
        offsets.sort_unstable();

        for offset in offsets {
            if arbs_emitted >= total_arbs {
                break 'outer;
            }
            let t_m = window_idx * 1_000 + offset;
            let token = rng.gen_range(1..market.tokens.len() as u64) as usize;

            if adj[token].is_empty() {
                continue;
            }
            let main_pos = rng.gen_range(0..adj[token].len());
            let main_pair_idx = adj[token][main_pos];

            let delta_bps = rng.gen_range(cfg.price_perturb_bps.min..=cfg.price_perturb_bps.max);
            let sign_positive = rng.gen_bool(0.5);

            events.push(Event {
                t: t_m,
                kind: EventKind::Mispricing {
                    token,
                    delta_bps,
                    sign_positive,
                    main_pair_idx,
                },
            });
            for (i, &pair_idx) in adj[token].iter().enumerate() {
                if i == main_pos {
                    continue;
                }
                let t_emit = t_m + rng.gen_range(cfg.rebalance_delay_us.min..=cfg.rebalance_delay_us.max);
                events.push(Event {
                    t: t_emit,
                    kind: EventKind::Rebalance { pair_idx },
                });
            }
            arbs_emitted += 1;
        }
        window_idx += 1;
    }

    // --- Pass 2: sort and emit with live pu. ---
    events.sort_by_key(|e| e.t);

    let mut intermediate: Vec<Intermediate> = Vec::with_capacity(events.len());
    for ev in events {
        let (pair_idx, is_main) = match ev.kind {
            EventKind::Mispricing {
                token,
                delta_bps,
                sign_positive,
                main_pair_idx,
            } => {
                let factor = if sign_positive { 10_000 + delta_bps } else { 10_000 - delta_bps };
                let new_pu = (pu[token] * factor / 10_000).clamp(pu_min, pu_max);
                pu[token] = new_pu;
                (main_pair_idx, true)
            }
            EventKind::Rebalance { pair_idx } => (pair_idx, false),
        };
        let p = &market.pairs[pair_idx];
        let price = recompute_price(p, &pu, &market.tokens);
        intermediate.push(Intermediate {
            t: ev.t,
            pair_idx,
            price,
            is_main,
            volume: 0,
        });
    }

    // --- Pass 3: per-pair rebalance-cluster dedupe. ---
    let mut per_pair_indices: Vec<Vec<usize>> = vec![Vec::new(); market.pairs.len()];
    for (i, t) in intermediate.iter().enumerate() {
        per_pair_indices[t.pair_idx].push(i);
    }
    for pair_idx in 0..market.pairs.len() {
        let indices = &per_pair_indices[pair_idx];
        if indices.len() < 2 {
            continue;
        }
        let mut prev_distinct = market.pairs[pair_idx].price;
        let mut i = 0;
        while i < indices.len() {
            let cur = &intermediate[indices[i]];
            if cur.is_main {
                prev_distinct = cur.price;
                i += 1;
                continue;
            }
            // Rebalance — extend cluster to next main or different-price event.
            let cluster_price = cur.price;
            let mut j = i + 1;
            while j < indices.len() {
                let n = &intermediate[indices[j]];
                if n.is_main || n.price != cluster_price {
                    break;
                }
                j += 1;
            }
            let cluster_len = j - i;
            if cluster_len >= 2 && cluster_price != prev_distinct {
                let keep_offset = rng.gen_range(0..cluster_len);
                for k in 0..keep_offset {
                    intermediate[indices[i + k]].price = prev_distinct;
                }
            }
            prev_distinct = cluster_price;
            i = j;
        }
    }

    // --- Pass 4: per-tick volume random walk (in global tick order). ---
    for tick in intermediate.iter_mut() {
        let pair_idx = tick.pair_idx;
        let delta_bps = rng.gen_range(cfg.volume_perturb_bps.min..=cfg.volume_perturb_bps.max);
        let sign_positive = rng.gen_bool(0.5);
        let factor = if sign_positive { 10_000 + delta_bps } else { 10_000 - delta_bps };
        let new_vol = (current_volume[pair_idx] * factor / 10_000).clamp(volume_min[pair_idx], volume_max[pair_idx]);
        current_volume[pair_idx] = new_vol;
        tick.volume = new_vol;
    }

    // Convert to final FeedTicks with per-tick delays.
    let mut feed = Vec::with_capacity(intermediate.len());
    let mut prev_t: u64 = 0;
    for tick in intermediate {
        feed.push(FeedTick {
            delay_us: tick.t - prev_t,
            update: PriceUpdate {
                token_pair_id: market.pairs[tick.pair_idx].id,
                price: tick.price,
                volume: tick.volume,
            },
        });
        prev_t = tick.t;
    }

    Ok(feed)
}

struct Event {
    t: u64,
    kind: EventKind,
}

enum EventKind {
    Mispricing {
        token: usize,
        delta_bps: u64,
        sign_positive: bool,
        main_pair_idx: usize,
    },
    Rebalance {
        pair_idx: usize,
    },
}

struct Intermediate {
    t: u64,
    pair_idx: usize,
    price: u64,
    is_main: bool,
    volume: u64,
}

fn recompute_price(p: &TokenPair, pu: &[u64], tokens: &[Token]) -> u64 {
    if p.quote == 0 {
        pu[p.base as usize]
    } else {
        let quote_decimals = tokens[p.quote as usize].decimals;
        cross_pair_price(pu[p.base as usize], pu[p.quote as usize], quote_decimals)
    }
}

/// Aggregate statistics derived from a generated feed.
#[derive(Debug, Clone)]
pub struct FeedStats {
    pub total_ticks: usize,
    /// Absolute time of the last tick, in µs. Also the wall-clock the
    /// feed would take to replay.
    pub duration_us: u64,
    /// Mean ticks per 1 ms aligned window over the feed's duration.
    pub avg_ticks_per_ms: f64,
    /// Highest tick count observed in any aligned 1 ms window.
    pub peak_ticks_per_ms: u64,
    /// Distinct pair ids that received at least one tick.
    pub distinct_pairs_touched: usize,
    pub min_ticks_per_touched_pair: usize,
    pub avg_ticks_per_touched_pair: f64,
    pub max_ticks_per_touched_pair: usize,
}

/// Walk the feed once and aggregate the stats. O(N) over ticks.
pub fn feed_stats(feed: &[FeedTick]) -> FeedStats {
    let total_ticks = feed.len();
    if total_ticks == 0 {
        return FeedStats {
            total_ticks: 0,
            duration_us: 0,
            avg_ticks_per_ms: 0.0,
            peak_ticks_per_ms: 0,
            distinct_pairs_touched: 0,
            min_ticks_per_touched_pair: 0,
            avg_ticks_per_touched_pair: 0.0,
            max_ticks_per_touched_pair: 0,
        };
    }

    let mut absolute: u64 = 0;
    let mut per_ms_counts: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut per_pair_counts: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for tick in feed {
        absolute += tick.delay_us;
        *per_ms_counts.entry(absolute / 1_000).or_default() += 1;
        *per_pair_counts.entry(tick.update.token_pair_id).or_default() += 1;
    }

    let duration_us = absolute;
    let windows = (duration_us / 1_000).max(1);
    let avg_ticks_per_ms = total_ticks as f64 / windows as f64;
    let peak_ticks_per_ms = per_ms_counts.values().copied().max().unwrap_or(0);

    let pair_counts: Vec<usize> = per_pair_counts.values().copied().collect();
    let min_per_pair = pair_counts.iter().copied().min().unwrap_or(0);
    let max_per_pair = pair_counts.iter().copied().max().unwrap_or(0);
    let avg_per_pair = pair_counts.iter().sum::<usize>() as f64 / pair_counts.len() as f64;

    FeedStats {
        total_ticks,
        duration_us,
        avg_ticks_per_ms,
        peak_ticks_per_ms,
        distinct_pairs_touched: pair_counts.len(),
        min_ticks_per_touched_pair: min_per_pair,
        avg_ticks_per_touched_pair: avg_per_pair,
        max_ticks_per_touched_pair: max_per_pair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::config::load_config;
    use crate::generation::generate_market::generate_market;

    /// Cheaper config for full-feed tests: same shape, fewer arbs.
    fn test_config(arb_count: u64) -> GenerationConfig {
        let mut c = load_config().unwrap();
        c.arb_count = arb_count;
        c
    }

    #[test]
    fn deterministic_for_same_seed() {
        let cfg = test_config(500);
        let market = generate_market(&cfg, 0).unwrap();
        let a = generate_feed(&market, &cfg, 42).unwrap();
        let b = generate_feed(&market, &cfg, 42).unwrap();
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.delay_us, y.delay_us);
            assert_eq!(x.update.token_pair_id, y.update.token_pair_id);
            assert_eq!(x.update.price, y.update.price);
            assert_eq!(x.update.volume, y.update.volume);
        }
    }

    #[test]
    fn absolute_times_non_decreasing() {
        let cfg = test_config(500);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 7).unwrap();
        let mut t: u64 = 0;
        for tick in &feed {
            t = t.checked_add(tick.delay_us).expect("absolute time overflow");
            let _ = t;
        }
    }

    #[test]
    fn replay_yields_equilibrium() {
        // After replaying every tick, each cross pair's `price` should equal
        // the recomputed price from the final pu vector. Dedupe preserves
        // this because the last event in any rebalance cluster is at or
        // after the keep, so it always carries the equilibrium price.
        let cfg = test_config(2_000);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 11).unwrap();

        let mut live: Vec<u64> = market.pairs.iter().map(|p| p.price).collect();
        let id_to_idx: std::collections::HashMap<u64, usize> = market.pairs.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        for tick in &feed {
            let idx = id_to_idx[&tick.update.token_pair_id];
            live[idx] = tick.update.price;
        }

        let usd_scale = pow10(cfg.usd_decimals);
        let mut pu: Vec<u64> = vec![usd_scale; market.tokens.len()];
        let usd_pair_count = market.tokens.len() - 1;
        for (i, p) in market.pairs.iter().take(usd_pair_count).enumerate() {
            pu[p.base as usize] = live[i];
        }

        for (i, p) in market.pairs.iter().enumerate().skip(usd_pair_count) {
            let expected = cross_pair_price(pu[p.base as usize], pu[p.quote as usize], market.tokens[p.quote as usize].decimals);
            assert_eq!(
                live[i], expected,
                "cross pair {} not in equilibrium: live={} expected={}",
                p.id, live[i], expected
            );
        }
    }

    #[test]
    fn clamping_keeps_usd_prices_in_range() {
        let mut cfg = test_config(500);
        cfg.price_usd.min = 1;
        cfg.price_usd.max = 2;
        cfg.price_perturb_bps.min = 5_000;
        cfg.price_perturb_bps.max = 9_999;
        cfg.sanity_check().unwrap();
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 13).unwrap();

        let scale = pow10(cfg.usd_decimals);
        let usd_pair_ids: std::collections::HashSet<u64> = market.pairs.iter().filter(|p| p.quote == 0).map(|p| p.id).collect();
        let lo = cfg.price_usd.min * scale;
        let hi = cfg.price_usd.max * scale;
        for tick in &feed {
            if usd_pair_ids.contains(&tick.update.token_pair_id) {
                assert!(
                    tick.update.price >= lo && tick.update.price <= hi,
                    "USD pair price {} outside clamp [{lo}, {hi}]",
                    tick.update.price,
                );
            }
        }
    }

    #[test]
    fn empty_feed_for_zero_arbs() {
        let cfg = test_config(0);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 1).unwrap();
        assert!(feed.is_empty());
    }

    #[test]
    fn different_seeds_produce_different_feeds() {
        let cfg = test_config(500);
        let market = generate_market(&cfg, 0).unwrap();
        let a = generate_feed(&market, &cfg, 1).unwrap();
        let b = generate_feed(&market, &cfg, 2).unwrap();
        // At least some tick must differ in price or pair id.
        let any_diff = a
            .iter()
            .zip(b.iter())
            .any(|(x, y)| x.update.price != y.update.price || x.update.token_pair_id != y.update.token_pair_id);
        assert!(any_diff, "feeds for distinct seeds should differ in content");
    }

    #[test]
    fn different_market_seeds_produce_different_feeds() {
        let cfg = test_config(500);
        let m1 = generate_market(&cfg, 0).unwrap();
        let m2 = generate_market(&cfg, 1).unwrap();
        let a = generate_feed(&m1, &cfg, 99).unwrap();
        let b = generate_feed(&m2, &cfg, 99).unwrap();
        let any_diff = a
            .iter()
            .zip(b.iter())
            .any(|(x, y)| x.update.price != y.update.price || x.update.token_pair_id != y.update.token_pair_id);
        assert!(any_diff, "feeds for distinct markets should differ in content");
    }

    #[test]
    fn all_pair_ids_in_feed_are_valid() {
        let cfg = test_config(500);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 5).unwrap();
        let valid: std::collections::HashSet<u64> = market.pairs.iter().map(|p| p.id).collect();
        for tick in &feed {
            assert!(
                valid.contains(&tick.update.token_pair_id),
                "feed references unknown pair id {}",
                tick.update.token_pair_id
            );
        }
    }

    #[test]
    fn single_arb_touches_only_pairs_of_one_token() {
        // With arb_count = 1, all emitted ticks should land on pairs that
        // share at least one token (the mispriced one).
        let cfg = test_config(1);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 7).unwrap();
        assert!(!feed.is_empty(), "single arb should still emit ticks");

        let by_id: std::collections::HashMap<u64, (u64, u64)> = market.pairs.iter().map(|p| (p.id, (p.base, p.quote))).collect();

        let mut common: Option<std::collections::HashSet<u64>> = None;
        for tick in &feed {
            let (b, q) = by_id[&tick.update.token_pair_id];
            let mut tokens = std::collections::HashSet::new();
            // We never mispriceto USD (id 0), so exclude it from the candidate
            // intersection set.
            if b != 0 {
                tokens.insert(b);
            }
            if q != 0 {
                tokens.insert(q);
            }
            common = Some(match common {
                None => tokens,
                Some(c) => c.intersection(&tokens).copied().collect(),
            });
        }
        let intersection = common.unwrap();
        assert!(
            !intersection.is_empty(),
            "all single-arb ticks should share at least one (non-USD) token"
        );
    }

    #[test]
    fn single_arb_rebalance_delays_within_bounds() {
        // First emitted tick is the main (fired at the mispricing's offset).
        // Every subsequent tick is a rebalance whose offset relative to the
        // main must lie in [rebalance_delay_us.min, rebalance_delay_us.max].
        let cfg = test_config(1);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 13).unwrap();
        assert!(feed.len() >= 2, "single arb on a connected token should have ≥2 ticks");

        // Reconstruct absolute timestamps.
        let mut abs: Vec<u64> = Vec::with_capacity(feed.len());
        let mut t = 0u64;
        for tick in &feed {
            t += tick.delay_us;
            abs.push(t);
        }

        let t_main = abs[0];
        assert!(t_main < 1_000, "main should fire within the first 1 ms window, got {t_main} µs");
        for &t_rebalance in &abs[1..] {
            let off = t_rebalance - t_main;
            assert!(
                off >= cfg.rebalance_delay_us.min,
                "rebalance fired {off} µs after main, below min {}",
                cfg.rebalance_delay_us.min
            );
            assert!(
                off <= cfg.rebalance_delay_us.max,
                "rebalance fired {off} µs after main, above max {}",
                cfg.rebalance_delay_us.max
            );
        }
    }

    #[test]
    fn mispricings_distributed_across_many_tokens() {
        // Over a few hundred arbs, we should be touching well more than a
        // handful of distinct tokens — otherwise the random token selection
        // is broken.
        let cfg = test_config(500);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 23).unwrap();

        // For each emitted pair, the set of tokens it involves is a
        // superset of the mispriced tokens that touched it. Collecting all
        // non-USD tokens across the feed underestimates "distinct mispriced
        // tokens" but bounds it from above. With 500 arbs and uniform
        // sampling over ~999 tokens, expect well over 100 distinct.
        let by_id: std::collections::HashMap<u64, (u64, u64)> = market.pairs.iter().map(|p| (p.id, (p.base, p.quote))).collect();
        let mut tokens = std::collections::HashSet::new();
        for tick in &feed {
            let (b, q) = by_id[&tick.update.token_pair_id];
            if b != 0 {
                tokens.insert(b);
            }
            if q != 0 {
                tokens.insert(q);
            }
        }
        assert!(
            tokens.len() > 100,
            "expected >100 distinct non-USD tokens touched; got {}",
            tokens.len()
        );
    }

    #[test]
    fn volumes_evolve_per_pair() {
        // For any pair that gets touched many times, its emitted volumes
        // should not all be identical — the random walk must move things.
        let cfg = test_config(5_000);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 17).unwrap();

        let mut by_pair: std::collections::HashMap<u64, Vec<u64>> = std::collections::HashMap::new();
        for tick in &feed {
            by_pair.entry(tick.update.token_pair_id).or_default().push(tick.update.volume);
        }

        let busiest = by_pair.values().max_by_key(|v| v.len()).expect("at least one pair must be touched");
        assert!(busiest.len() >= 10, "expected a pair with ≥10 ticks; got max {}", busiest.len());
        let min = busiest.iter().min().unwrap();
        let max = busiest.iter().max().unwrap();
        assert!(min != max, "volumes for the busiest pair should vary; min == max == {min}");
    }

    #[test]
    fn feed_length_matches_event_counts() {
        // total ticks = 1 main + adj_size - 1 rebalances summed over each
        // chosen mispricing's token. Since we can't easily reconstruct
        // which tokens were chosen without re-running the RNG, just bound
        // total length: arb_count ≤ len ≤ arb_count × max_adj.
        let cfg = test_config(200);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 31).unwrap();

        let max_adj = {
            let mut adj_counts = vec![0usize; market.tokens.len()];
            for p in &market.pairs {
                adj_counts[p.base as usize] += 1;
                if p.quote != 0 {
                    adj_counts[p.quote as usize] += 1;
                }
            }
            *adj_counts.iter().max().unwrap()
        };
        assert!(feed.len() as u64 >= cfg.arb_count);
        assert!(feed.len() <= cfg.arb_count as usize * max_adj);
    }

    #[test]
    fn volumes_stay_in_range() {
        let cfg = test_config(2_000);
        let market = generate_market(&cfg, 0).unwrap();
        let feed = generate_feed(&market, &cfg, 19).unwrap();

        let usd_scale = pow10(cfg.usd_decimals);
        // Recompute the same per-pair range and compare.
        let mut pu: Vec<u64> = vec![usd_scale; market.tokens.len()];
        let usd_pair_count = market.tokens.len() - 1;
        for p in market.pairs.iter().take(usd_pair_count) {
            pu[p.base as usize] = p.price;
        }
        let id_to_idx: std::collections::HashMap<u64, usize> = market.pairs.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
        for tick in &feed {
            let idx = id_to_idx[&tick.update.token_pair_id];
            let p = &market.pairs[idx];
            let base_factor = pow10(market.tokens[p.base as usize].decimals);
            let pu_base = pu[p.base as usize];
            let lo = cfg.volume_usd.min * usd_scale * base_factor / pu_base;
            let hi = cfg.volume_usd.max * usd_scale * base_factor / pu_base;
            assert!(
                tick.update.volume >= lo && tick.update.volume <= hi,
                "pair {} volume {} outside range [{lo}, {hi}]",
                p.id,
                tick.update.volume,
            );
        }
    }
}
