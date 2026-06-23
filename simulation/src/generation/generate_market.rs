use std::collections::HashSet;

use anyhow::{ensure, Result};
use rand::{Rng, SeedableRng};
use rand_pcg::Pcg64Mcg;

use super::config::GenerationConfig;
use super::math::{cross_pair_price, pow10, volume_atomic};
use crate::protocol::market::{Fee, Market, Token, TokenPair};

/// Generate a deterministic market from the given config and seed.
///
/// Layout:
///   - Token id 0 = USD (uses `usd_decimals`).
///   - Token ids 1..token_count = non-USD, decimals sampled from `decimals`.
///   - First (token_count - 1) pairs are USD pairs (quote = USD).
///   - Remaining pairs are random non-USD/non-USD cross pairs, deduped by
///     unordered token-pair so we don't emit both T1/T2 and T2/T1.
///
/// Prices for non-USD tokens are sampled in **atomic-USD** across the range
/// `[price_usd.min × 10^usd_decimals, price_usd.max × 10^usd_decimals]`, so
/// fractional-cent prices are possible. USD pair prices are those sampled
/// values directly; cross pair prices are derived from the two underlying
/// USD prices — so the market starts in equilibrium (no arbs at t=0).
/// Volumes are independent samples from `volume_usd` per pair.
pub fn generate_market(cfg: &GenerationConfig, seed: u64) -> Result<Market> {
    ensure!(
        cfg.token_count >= 2,
        "token_count must be at least 2 (USD + 1)"
    );
    let non_usd = cfg.token_count - 1;
    let max_cross = non_usd.saturating_mul(non_usd - 1) / 2;
    ensure!(
        cfg.pair_count >= non_usd,
        "pair_count={} must be at least token_count-1={} (room for all USD pairs)",
        cfg.pair_count,
        non_usd
    );
    let cross_needed = cfg.pair_count - non_usd;
    ensure!(
        cross_needed <= max_cross,
        "pair_count={} exceeds available pairs (USD={} + max cross={})",
        cfg.pair_count,
        non_usd,
        max_cross
    );

    let mut rng = Pcg64Mcg::seed_from_u64(seed);

    // 1. Tokens
    let tokens = {
        let mut tokens = Vec::with_capacity(cfg.token_count as usize);
        tokens.push(Token {
            id: 0,
            decimals: cfg.usd_decimals,
        });
        for id in 1..cfg.token_count {
            let decimals = rng.gen_range(cfg.decimals.min..=cfg.decimals.max);
            tokens.push(Token { id, decimals });
        }
        tokens
    };

    // USD prices in **atomic-USD per whole-token**, indexed by token id.
    // Token 0 is USD itself, priced at 1 USD = 10^usd_decimals atomic-USD.
    let usd_scale = pow10(cfg.usd_decimals);
    let usd_price_atomic = {
        let mut v = vec![usd_scale; cfg.token_count as usize];
        let lo = cfg.price_usd.min * usd_scale;
        let hi = cfg.price_usd.max * usd_scale;
        for i in 1..cfg.token_count as usize {
            v[i] = rng.gen_range(lo..=hi);
        }
        v
    };

    // 3. Pairs
    let mut pairs = Vec::with_capacity(cfg.pair_count as usize);
    let mut next_id: u64 = 0;

    // USD pairs: base = T, quote = USD (id 0)
    for t in 1..cfg.token_count {
        let base_decimals = tokens[t as usize].decimals;
        let pu = usd_price_atomic[t as usize];

        // USD pair: price (atomic-quote per whole-base) equals the token's
        // atomic-USD price directly, since quote IS USD.
        let price = pu;
        let vol_usd_atomic = rng.gen_range(cfg.volume_usd.min..=cfg.volume_usd.max) * usd_scale;
        let volume = volume_atomic(vol_usd_atomic, base_decimals, pu);

        pairs.push(TokenPair {
            id: next_id,
            base: t,
            quote: 0,
            price,
            volume,
        });
        next_id += 1;
    }

    // Cross pairs: random non-USD/non-USD, dedup by unordered tuple
    let mut seen: HashSet<(u64, u64)> = HashSet::with_capacity(cross_needed as usize);
    while pairs.len() < cfg.pair_count as usize {
        let a = rng.gen_range(1..cfg.token_count);
        let b = rng.gen_range(1..cfg.token_count);
        if a == b {
            continue;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        if !seen.insert(key) {
            continue;
        }

        // Random base/quote direction so cross pairs aren't all "lower-id base"
        let (base, quote) = if rng.gen_bool(0.5) { (a, b) } else { (b, a) };

        let base_decimals = tokens[base as usize].decimals;
        let quote_decimals = tokens[quote as usize].decimals;
        let pu_base = usd_price_atomic[base as usize];
        let pu_quote = usd_price_atomic[quote as usize];

        let price = cross_pair_price(pu_base, pu_quote, quote_decimals);
        let vol_usd_atomic = rng.gen_range(cfg.volume_usd.min..=cfg.volume_usd.max) * usd_scale;
        let volume = volume_atomic(vol_usd_atomic, base_decimals, pu_base);

        pairs.push(TokenPair {
            id: next_id,
            base,
            quote,
            price,
            volume,
        });
        next_id += 1;
    }

    let fee = Fee {
        static_atomic_usd: cfg.static_fee_atomic_usd,
        variable_bps: cfg.variable_fee_bps,
    };

    Ok(Market { fee, tokens, pairs })
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::config::load_config;

    #[test]
    fn real_config_generates_expected_counts() {
        let cfg = load_config().unwrap();
        let market = generate_market(&cfg, 42).unwrap();
        assert_eq!(market.tokens.len() as u64, cfg.token_count);
        assert_eq!(market.pairs.len() as u64, cfg.pair_count);
    }

    #[test]
    fn token_0_is_usd() {
        let cfg = load_config().unwrap();
        let market = generate_market(&cfg, 42).unwrap();
        assert_eq!(market.tokens[0].id, 0);
        assert_eq!(market.tokens[0].decimals, cfg.usd_decimals);
    }

    #[test]
    fn all_non_usd_tokens_have_usd_pair_first() {
        let cfg = load_config().unwrap();
        let market = generate_market(&cfg, 42).unwrap();
        // first (token_count - 1) pairs are USD pairs (quote = 0)
        for i in 0..(cfg.token_count - 1) as usize {
            assert_eq!(market.pairs[i].quote, 0, "pair {i} should be USD quote");
            assert_eq!(
                market.pairs[i].base,
                (i as u64) + 1,
                "pair {i} base mismatch"
            );
        }
        // every non-USD token is covered
        let usd_bases: std::collections::HashSet<u64> = market
            .pairs
            .iter()
            .take((cfg.token_count - 1) as usize)
            .map(|p| p.base)
            .collect();
        assert_eq!(usd_bases.len() as u64, cfg.token_count - 1);
    }

    #[test]
    fn cross_pair_price_derived_from_usd_prices() {
        let cfg = load_config().unwrap();
        let market = generate_market(&cfg, 42).unwrap();

        // USD pair prices ARE the token's atomic-USD price.
        let mut pu_atomic = vec![pow10(cfg.usd_decimals); cfg.token_count as usize];
        let usd_pair_count = (cfg.token_count - 1) as usize;
        for p in market.pairs.iter().take(usd_pair_count) {
            pu_atomic[p.base as usize] = p.price;
        }
        for p in market.pairs.iter().skip(usd_pair_count) {
            let quote_d = market.tokens[p.quote as usize].decimals;
            let expected = cross_pair_price(
                pu_atomic[p.base as usize],
                pu_atomic[p.quote as usize],
                quote_d,
            );
            assert_eq!(p.price, expected, "cross pair {:?} price mismatch", p.id);
        }
    }

    #[test]
    fn usd_pair_price_is_within_atomic_usd_range() {
        let cfg = load_config().unwrap();
        let market = generate_market(&cfg, 42).unwrap();
        let scale = pow10(cfg.usd_decimals);
        let lo = cfg.price_usd.min * scale;
        let hi = cfg.price_usd.max * scale;
        for p in market.pairs.iter().take((cfg.token_count - 1) as usize) {
            assert!(
                p.price >= lo && p.price <= hi,
                "USD pair price {} out of [{lo}, {hi}]",
                p.price,
            );
        }
    }

    #[test]
    fn deterministic_for_same_seed() {
        let cfg = load_config().unwrap();
        let a = generate_market(&cfg, 7).unwrap();
        let b = generate_market(&cfg, 7).unwrap();
        assert_eq!(a.tokens.len(), b.tokens.len());
        assert_eq!(a.pairs.len(), b.pairs.len());
        for (p1, p2) in a.pairs.iter().zip(b.pairs.iter()) {
            assert_eq!(
                (p1.id, p1.base, p1.quote, p1.price, p1.volume),
                (p2.id, p2.base, p2.quote, p2.price, p2.volume)
            );
        }
    }

    #[test]
    fn pair_count_too_low_rejected() {
        let mut cfg = load_config().unwrap();
        cfg.token_count = 10;
        cfg.pair_count = 5; // less than 9 (USD pairs needed)
        let err = generate_market(&cfg, 0).unwrap_err();
        assert!(err.to_string().contains("at least"), "{err}");
    }

    #[test]
    fn pair_count_too_high_rejected() {
        let mut cfg = load_config().unwrap();
        cfg.token_count = 5;
        // max = 4 USD pairs + C(4,2)=6 cross = 10. request 11.
        cfg.pair_count = 11;
        let err = generate_market(&cfg, 0).unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }
}
