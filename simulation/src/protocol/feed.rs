#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PriceUpdate {
    pub token_pair_id: u64,
    pub price: u64,
    pub volume: u64,
}

impl PriceUpdate {
    pub fn as_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                self as *const Self as *const u8,
                std::mem::size_of::<Self>(),
            )
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FeedTick {
    pub delay_us: u64,
    pub update: PriceUpdate,
}
