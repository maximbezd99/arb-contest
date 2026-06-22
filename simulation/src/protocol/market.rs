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

/// Format:
///
/// fee:<fee>;tokens:<token>[,<token>]*;pairs:<pair>[,<pair>]*;
///
/// Fee (fixed 16 bytes):
///   static_atomic_usd (u64 LE) | variable_bps (u64 LE)
///
/// Token (fixed 16 bytes):
///   id (u64 LE) | decimals (u64 LE)
///
/// Pair (fixed 40 bytes):
///   id (u64 LE) | base (u64 LE) | quote (u64 LE) | price (u64 LE) | volume (u64 LE)
///
/// All integers are little-endian. The ASCII `,` and `;` bytes are framing only.
pub fn serialize_market(market: &Market) -> Vec<u8> {
    let mut out = Vec::new();

    out.extend_from_slice(b"fee:");
    serialize_fee(&market.fee, &mut out);
    out.push(b';');

    out.extend_from_slice(b"tokens:");
    for (i, t) in market.tokens.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        serialize_token(t, &mut out);
    }
    out.push(b';');

    out.extend_from_slice(b"pairs:");
    for (i, p) in market.pairs.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        serialize_pair(p, &mut out);
    }
    out.push(b';');

    out
}

fn serialize_fee(f: &Fee, out: &mut Vec<u8>) {
    out.extend_from_slice(&f.static_atomic_usd.to_le_bytes());
    out.extend_from_slice(&f.variable_bps.to_le_bytes());
}

fn serialize_token(t: &Token, out: &mut Vec<u8>) {
    out.extend_from_slice(&t.id.to_le_bytes());
    out.extend_from_slice(&t.decimals.to_le_bytes());
}

fn serialize_pair(p: &TokenPair, out: &mut Vec<u8>) {
    out.extend_from_slice(&p.id.to_le_bytes());
    out.extend_from_slice(&p.base.to_le_bytes());
    out.extend_from_slice(&p.quote.to_le_bytes());
    out.extend_from_slice(&p.price.to_le_bytes());
    out.extend_from_slice(&p.volume.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fee() -> Fee {
        Fee {
            static_atomic_usd: 10_000,
            variable_bps: 5,
        }
    }

    #[test]
    fn empty_market() {
        let m = Market {
            fee: fee(),
            tokens: vec![],
            pairs: vec![],
        };
        let bytes = serialize_market(&m);
        let mut expected = Vec::new();
        expected.extend_from_slice(b"fee:");
        expected.extend_from_slice(&10_000u64.to_le_bytes());
        expected.extend_from_slice(&5u64.to_le_bytes());
        expected.push(b';');
        expected.extend_from_slice(b"tokens:;pairs:;");
        assert_eq!(bytes, expected);
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

        let mut expected = Vec::new();
        expected.extend_from_slice(b"fee:");
        expected.extend_from_slice(&10_000u64.to_le_bytes());
        expected.extend_from_slice(&5u64.to_le_bytes());
        expected.push(b';');
        expected.extend_from_slice(b"tokens:");
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(&6u64.to_le_bytes());
        expected.push(b';');
        expected.extend_from_slice(b"pairs:");
        expected.extend_from_slice(&2u64.to_le_bytes());
        expected.extend_from_slice(&3u64.to_le_bytes());
        expected.extend_from_slice(&1u64.to_le_bytes());
        expected.extend_from_slice(&4u64.to_le_bytes());
        expected.extend_from_slice(&5u64.to_le_bytes());
        expected.push(b';');

        assert_eq!(serialize_market(&m), expected);
    }

    #[test]
    fn multiple_entries_comma_separated() {
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

        let bytes = serialize_market(&m);
        // "fee:" (4) + 16 fee bytes + ";" (1) = 21
        // "tokens:" (7) + 2*16 tokens + 1 comma + ";" (1) = 41
        // "pairs:"  (6) + 2*40 pairs  + 1 comma + ";" (1) = 88
        assert_eq!(bytes.len(), 21 + 41 + 88);
        assert!(bytes.starts_with(b"fee:"));
        assert!(bytes.ends_with(b";"));
        assert!(bytes.windows(8).any(|w| w == b";tokens:"));
        assert!(bytes.windows(7).any(|w| w == b";pairs:"));
    }
}
