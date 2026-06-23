pub fn pow10(n: u64) -> u64 {
    10u64.pow(n as u32)
}

/// Price of a cross pair `(base, quote)`, derived from each side's USD price.
///
/// `price = pu_base × 10^quote_decimals / pu_quote`. The `10^usd_decimals`
/// factor inside both `pu` values cancels.
pub fn cross_pair_price(pu_base: u64, pu_quote: u64, quote_decimals: u64) -> u64 {
    pu_base * pow10(quote_decimals) / pu_quote
}

/// Convert a USD-denominated pool size into atomic units of the pair's base.
///
/// `volume in atomic-base = vol_usd_atomic × 10^base_decimals / pu_atomic`,
/// where `pu_atomic` is the base token's USD price in atomic-USD per whole
/// base.
pub fn volume_atomic(vol_usd_atomic: u64, base_decimals: u64, pu_atomic: u64) -> u64 {
    vol_usd_atomic * pow10(base_decimals) / pu_atomic
}
