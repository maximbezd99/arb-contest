#[derive(Debug, Clone)]
pub struct Contestant {
    /// ID handed out by `/register`.
    pub _id: u64,
    /// Available balance, in atomic-USD.
    pub balance: i64,
}

impl Contestant {
    pub fn new_with_decimals(id: u64, initial_balance_usd: i64, usd_decimals: i64) -> Self {
        Self {
            _id: id,
            balance: (initial_balance_usd * 10i64.pow(usd_decimals as u32)),
        }
    }

    #[allow(dead_code)]
    pub fn new(id: u64, initial_balance_usd_atomic: i64) -> Self {
        Self {
            _id: id,
            balance: initial_balance_usd_atomic,
        }
    }
}
