pub struct RouteSubmission {
    pub legs: Vec<RouteSubmissionLeg>,
}

pub struct RouteSubmissionLeg {
    pub pair_id: u64,
    pub direction: Direction,
    pub price: u64,
    pub volume: u64,
}

#[repr(u64)]
pub enum Direction {
    Buy = 0,
    Sell = 1,
}
