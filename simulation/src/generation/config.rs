use anyhow::{ensure, Context, Result};
use serde::Deserialize;

const CONFIG_JSON: &str = include_str!("../../config.json");

pub fn load_config() -> Result<GenerationConfig> {
    serde_json::from_str(CONFIG_JSON).context("parsing embedded simulation/config.json")
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Range {
    pub min: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerationConfig {
    /// Inclusive range of decimals each non-USD token may use during market generation.
    pub decimals: Range,
    /// Decimals for token id 0 (USD).
    pub usd_decimals: u64,
    /// Inclusive range, in **whole USD**, for price of any non-USD token at any given moment.
    pub price_usd: Range,
    /// Inclusive range, in **whole USD**, for volume of any pair (both USD and non-USD) at any given moment.
    pub volume_usd: Range,
    /// Total number of tokens, including USD at id 0.
    pub token_count: u64,
    /// Total number of pairs. The first `token_count - 1` are USD pairs (every non-USD token paired against USD).
    /// There are no mirror pairs (e.g. T1/T2 and T2/T1).
    pub pair_count: u64,
    /// Flat fee charged per leg, in atomic-USD. Volume-independent.
    /// `10_000` with `usd_decimals = 6` means $0.01 per leg.
    pub static_fee_atomic_usd: u64,
    /// Volume-proportional fee per leg, in basis points (1 bps = 0.01%).
    /// Applied to the leg's filled quote-volume.
    pub variable_fee_bps: u64,
    /// Number of original mispricings (arbs) to generate over the simulation.
    pub arb_count: u64,
    /// Number of new mispricings per 1 ms window.
    pub updates_per_ms: u64,
    /// Per-pair delay range (µs) between a mispricing and its rebalancing update on a connected pair.
    pub rebalance_delay_us: Range,
    /// Magnitude of per-mispricing perturbation applied to a token's USD price, in basis points (1 bps = 0.01%). Sign is random.
    pub price_perturb_bps: Range,
    /// Per-tick volume perturbation magnitude, in basis points. Each emitted tick perturbs the pair's
    /// current volume by a random delta in this range with random sign (clamped to the pair's `volume_usd`-derived range).
    pub volume_perturb_bps: Range,
}

impl GenerationConfig {
    /// Validates that:
    ///   - Range fields are well-formed (min ≤ max, min > 0 where required).
    ///   - The validator's worst-case `base_atomic × price` fits in i64:
    ///       volume_usd.max × 10^(base_decimals + quote_decimals) / min_quote_price_usd
    ///     where:
    ///       - base_decimals, quote_decimals ≤ max(decimals.max, usd_decimals)
    ///       - min_quote_price_usd = min(price_usd.min, 1)  // USD is always $1
    ///   - Feed-generation params are inside the ranges the generator assumes.
    pub fn sanity_check(&self) -> Result<()> {
        ensure!(self.price_usd.min > 0, "price_usd.min must be > 0");
        ensure!(
            self.decimals.min <= self.decimals.max,
            "decimals: min > max"
        );
        ensure!(
            self.price_usd.min <= self.price_usd.max,
            "price_usd: min > max"
        );
        ensure!(
            self.volume_usd.min <= self.volume_usd.max,
            "volume_usd: min > max"
        );

        let max_decimals = self.decimals.max.max(self.usd_decimals);
        let min_quote_price_usd = self.price_usd.min.min(1); // USD always $1

        let exp: u32 = (2 * max_decimals)
            .try_into()
            .context("2 × max_decimals overflows u32")?;
        let factor = 10u128
            .checked_pow(exp)
            .with_context(|| format!("10^{exp} overflows u128"))?;
        let numerator = u128::from(self.volume_usd.max)
            .checked_mul(factor)
            .with_context(|| format!("volume_usd.max × 10^{exp} overflows u128"))?;
        let bound = numerator / u128::from(min_quote_price_usd);
        let i64_max = i64::MAX as u128;

        ensure!(
            bound <= i64_max,
            "worst-case `base_atomic × price` = {bound} exceeds i64::MAX = {i64_max} \
             (lower volume_usd.max, raise price_usd.min, or reduce decimals)"
        );

        ensure!(self.updates_per_ms >= 1, "updates_per_ms must be >= 1");
        ensure!(
            self.rebalance_delay_us.min >= 1,
            "rebalance_delay_us.min must be >= 1"
        );
        ensure!(
            self.rebalance_delay_us.min <= self.rebalance_delay_us.max,
            "rebalance_delay_us: min > max"
        );
        ensure!(
            self.rebalance_delay_us.max < 1_000,
            "rebalance_delay_us.max must be < 1000 (tail must close within 1 ms of mispricing)"
        );
        ensure!(
            self.price_perturb_bps.min >= 1,
            "price_perturb_bps.min must be >= 1"
        );
        ensure!(
            self.price_perturb_bps.min <= self.price_perturb_bps.max,
            "price_perturb_bps: min > max"
        );
        ensure!(
            self.price_perturb_bps.max < 10_000,
            "price_perturb_bps.max must be < 10000 (delta < 100%)"
        );
        ensure!(
            self.volume_perturb_bps.min >= 1,
            "volume_perturb_bps.min must be >= 1"
        );
        ensure!(
            self.volume_perturb_bps.min <= self.volume_perturb_bps.max,
            "volume_perturb_bps: min > max"
        );
        ensure!(
            self.volume_perturb_bps.max < 10_000,
            "volume_perturb_bps.max must be < 10000 (delta < 100%)"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_config_passes() {
        load_config().unwrap().sanity_check().unwrap();
    }

    #[test]
    fn zero_price_min_rejected() {
        let mut c = load_config().unwrap();
        c.price_usd.min = 0;
        let err = c.sanity_check().unwrap_err();
        assert!(
            err.to_string().contains("price_usd.min must be > 0"),
            "{err}"
        );
    }

    #[test]
    fn huge_volume_overflows() {
        // 10^16 fits, 10^20 doesn't (i64::MAX ≈ 9.2e18)
        let mut c = load_config().unwrap();
        c.volume_usd.max = 10u64.pow(8);
        let err = c.sanity_check().unwrap_err();
        assert!(err.to_string().contains("exceeds i64::MAX"), "{err}");
    }

    #[test]
    fn too_many_decimals_overflows() {
        let mut c = load_config().unwrap();
        c.decimals.max = 12;
        c.usd_decimals = 12;
        let err = c.sanity_check().unwrap_err();
        assert!(err.to_string().contains("exceeds i64::MAX"), "{err}");
    }

    #[test]
    fn zero_updates_per_ms_rejected() {
        let mut c = load_config().unwrap();
        c.updates_per_ms = 0;
        let err = c.sanity_check().unwrap_err();
        assert!(err.to_string().contains("updates_per_ms"), "{err}");
    }

    #[test]
    fn rebalance_delay_max_too_large_rejected() {
        let mut c = load_config().unwrap();
        c.rebalance_delay_us.max = 1_000;
        let err = c.sanity_check().unwrap_err();
        assert!(err.to_string().contains("rebalance_delay_us.max"), "{err}");
    }

    #[test]
    fn volume_perturb_max_too_large_rejected() {
        let mut c = load_config().unwrap();
        c.volume_perturb_bps.max = 10_000;
        let err = c.sanity_check().unwrap_err();
        assert!(err.to_string().contains("volume_perturb_bps.max"), "{err}");
    }

    #[test]
    fn price_perturb_max_too_large_rejected() {
        let mut c = load_config().unwrap();
        c.price_perturb_bps.max = 10_000;
        let err = c.sanity_check().unwrap_err();
        assert!(err.to_string().contains("price_perturb_bps.max"), "{err}");
    }
}
