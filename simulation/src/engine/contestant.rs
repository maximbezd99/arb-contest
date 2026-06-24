#[derive(Debug, Clone)]
pub struct Contestant {
    /// ID handed out by `/register`.
    pub id: u64,
    /// Available balance, in atomic-USD.
    pub balance: u64,
}

impl Contestant {
    pub fn new(id: u64, initial_balance_usd: u64, usd_decimals: u64) -> Self {
        Self {
            id,
            balance: initial_balance_usd * 10u64.pow(usd_decimals as u32),
        }
    }
}
