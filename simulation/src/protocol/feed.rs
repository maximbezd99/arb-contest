#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PriceUpdate {
    /// Monotonically increasing per-run counter.
    pub seq: u64,
    /// Pair this tick refers to.
    pub token_pair_id: u64,
    /// New price for the pair, in **atomic-quote per 1 whole base**
    pub price: u64,
    /// New available pool size for the pair, in **atomic units of base**
    pub volume: u64,
}

impl PriceUpdate {
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, std::mem::size_of::<Self>()) }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FeedTick {
    pub delay_us: u64,
    pub update: PriceUpdate,
}
