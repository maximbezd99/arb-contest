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
    pub decimals: Range,
    pub usd_decimals: u64,
    pub price_usd: Range,
    pub volume_usd: Range,
    pub token_count: u64,
    pub pair_count: u64,
    /// Flat fee charged per leg, in atomic-USD. Volume-independent.
    /// `10_000` with `usd_decimals = 6` means $0.01 per leg.
    pub static_fee_atomic_usd: u64,
    /// Volume-proportional fee per leg, in basis points (1 bps = 0.01%).
    /// Applied to the leg's filled quote-volume.
    pub variable_fee_bps: u64,
}

impl GenerationConfig {
    /// Verify that, given these generation bounds, the validator's
    /// `base_atomic * price` intermediate never overflows i64.
    ///
    /// Worst-case multiplication for any single leg reduces to:
    ///   volume_usd.max * 10^(base_decimals + quote_decimals) / min_quote_price_usd
    /// where:
    ///   - base_decimals, quote_decimals ≤ max(decimals.max, usd_decimals)
    ///   - min_quote_price_usd = min(price_usd.min, 1)  // USD is always $1
    pub fn check_no_overflow(&self) -> Result<()> {
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

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_config_passes() {
        load_config().unwrap().check_no_overflow().unwrap();
    }

    #[test]
    fn zero_price_min_rejected() {
        let mut c = load_config().unwrap();
        c.price_usd.min = 0;
        let err = c.check_no_overflow().unwrap_err();
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
        let err = c.check_no_overflow().unwrap_err();
        assert!(err.to_string().contains("exceeds i64::MAX"), "{err}");
    }

    #[test]
    fn too_many_decimals_overflows() {
        let mut c = load_config().unwrap();
        c.decimals.max = 12;
        c.usd_decimals = 12;
        let err = c.check_no_overflow().unwrap_err();
        assert!(err.to_string().contains("exceeds i64::MAX"), "{err}");
    }
}
