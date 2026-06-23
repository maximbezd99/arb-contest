#[derive(Debug, Clone)]
pub struct Market {
    /// Flat + variable fee schedule applied uniformly to every leg.
    pub fee: Fee,
    /// All existing tokens. Token id 0 is always USD.
    pub tokens: Vec<Token>,
    /// All tradable pairs. The first `tokens.len() - 1` entries are USD pairs, the rest are non-USD/non-USD cross pairs.
    pub pairs: Vec<TokenPair>,
}

#[derive(Debug, Clone, Copy)]
pub struct Fee {
    /// Flat per-leg fee, in **atomic-USD**. Charged once per leg regardless of fill size.
    pub static_atomic_usd: u64,
    /// Variable per-leg fee in **basis points** (1 bps = 0.01%). Applied to the leg's filled quote-volume.
    pub variable_bps: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Token {
    /// Sequential id assigned at generation time. Id 0 is USD.
    pub id: u64,
    /// Number of base-10 digits between 1 whole token and 1 atomic unit.
    /// E.g. `decimals = 6` means 1 token = 10^6 atomic units.
    pub decimals: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct TokenPair {
    /// Id assigned at generation time.
    pub id: u64,
    /// Id of the base side of this pair.
    pub base: u64,
    /// Id of the quote side of this pair.
    pub quote: u64,
    /// Price in **atomic-quote per 1 whole base**. So with `base` having
    /// `b` decimals and `quote` having `q` decimals, the human price
    /// (whole-quote per whole-base) is `price / 10^q`, and the conversion
    /// from a base amount in atomic units to a quote amount is:
    ///   `quote_atomic = base_atomic * price / 10^b`.
    pub price: u64,
    /// Available pool size on this pair, in **atomic units of base**.
    pub volume: u64,
}

/// Format (all integers little-endian, fixed order, no delimiters/markers):
///
///   static_atomic_usd (u64) | variable_bps (u64)
///   tokens_byte_len   (u64) | <tokens_byte_len bytes of tokens>
///   pairs_byte_len    (u64) | <pairs_byte_len bytes of pairs>
///
/// Each token is 16 bytes:
///   id (u64) | decimals (u64)
///
/// Each pair is 40 bytes:
///   id (u64) | base (u64) | quote (u64) | price (u64) | volume (u64)
///
/// `tokens_byte_len` is always a multiple of 16; `pairs_byte_len` of 40.
pub fn serialize_market(market: &Market) -> Vec<u8> {
    let mut out = Vec::new();

    out.extend_from_slice(&market.fee.static_atomic_usd.to_le_bytes());
    out.extend_from_slice(&market.fee.variable_bps.to_le_bytes());

    let tokens_byte_len = (market.tokens.len() as u64) * TOKEN_SIZE as u64;
    out.extend_from_slice(&tokens_byte_len.to_le_bytes());
    for t in &market.tokens {
        out.extend_from_slice(&t.id.to_le_bytes());
        out.extend_from_slice(&t.decimals.to_le_bytes());
    }

    let pairs_byte_len = (market.pairs.len() as u64) * PAIR_SIZE as u64;
    out.extend_from_slice(&pairs_byte_len.to_le_bytes());
    for p in &market.pairs {
        out.extend_from_slice(&p.id.to_le_bytes());
        out.extend_from_slice(&p.base.to_le_bytes());
        out.extend_from_slice(&p.quote.to_le_bytes());
        out.extend_from_slice(&p.price.to_le_bytes());
        out.extend_from_slice(&p.volume.to_le_bytes());
    }

    out
}

pub const TOKEN_SIZE: usize = 16;
pub const PAIR_SIZE: usize = 40;

#[cfg(test)]
mod tests {
    use super::*;

    fn fee() -> Fee {
        Fee {
            static_atomic_usd: 10_000,
            variable_bps: 5,
        }
    }

    fn serialize_fee_block() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&10_000u64.to_le_bytes());
        v.extend_from_slice(&5u64.to_le_bytes());
        v
    }

    #[test]
    fn empty_market() {
        let m = Market {
            fee: fee(),
            tokens: vec![],
            pairs: vec![],
        };
        let mut expected = serialize_fee_block();
        expected.extend_from_slice(&0u64.to_le_bytes()); // tokens_byte_len
        expected.extend_from_slice(&0u64.to_le_bytes()); // pairs_byte_len
        assert_eq!(serialize_market(&m), expected);
    }

    #[test]
    fn one_token_one_pair() {
        let m = Market {
            fee: fee(),
            tokens: vec![Token { id: 1, decimals: 6 }],
            pairs: vec![TokenPair {
                id: 2,
                base: 3,
                quote: 1,
                price: 4,
                volume: 5,
            }],
        };

        let mut expected = serialize_fee_block();
        expected.extend_from_slice(&(TOKEN_SIZE as u64).to_le_bytes());
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(&6u64.to_le_bytes());
        expected.extend_from_slice(&(PAIR_SIZE as u64).to_le_bytes());
        expected.extend_from_slice(&2u64.to_le_bytes());
        expected.extend_from_slice(&3u64.to_le_bytes());
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(&4u64.to_le_bytes());
        expected.extend_from_slice(&5u64.to_le_bytes());

        assert_eq!(serialize_market(&m), expected);
    }

    #[test]
    fn size_accounting() {
        let m = Market {
            fee: fee(),
            tokens: vec![Token { id: 0, decimals: 6 }, Token { id: 1, decimals: 6 }],
            pairs: vec![
                TokenPair {
                    id: 0,
                    base: 0,
                    quote: 1,
                    price: 100,
                    volume: 1000,
                },
                TokenPair {
                    id: 1,
                    base: 1,
                    quote: 0,
                    price: 200,
                    volume: 2000,
                },
            ],
        };

        // 16 fee + 8 tokens_len + 2*16 tokens + 8 pairs_len + 2*40 pairs
        assert_eq!(serialize_market(&m).len(), 16 + 8 + 32 + 8 + 80);
    }
}
