use std::fmt;

#[derive(Debug, Clone)]
pub struct RouteSubmission {
    pub legs: Vec<RouteSubmissionLeg>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RouteSubmissionLeg {
    pub pair_id: u64,
    pub direction: Direction,
    pub price: u64,
    pub volume: u64,
}

#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Buy = 0,
    Sell = 1,
}

#[derive(Debug, Clone, Copy)]
pub enum DeserializeError {
    /// Buffer length isn't a multiple of [`RouteSubmissionLeg::SIZE`].
    BadLength { got: usize, leg_size: usize },
}

impl fmt::Display for DeserializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLength { got, leg_size } => write!(
                f,
                "submission buffer length {got} not a multiple of leg size {leg_size}"
            ),
        }
    }
}

impl std::error::Error for DeserializeError {}

impl RouteSubmissionLeg {
    pub const SIZE: usize = std::mem::size_of::<Self>();
}

impl RouteSubmission {
    pub fn deserialize(buf: &[u8]) -> Result<Self, DeserializeError> {
        let leg_size = RouteSubmissionLeg::SIZE;
        if !buf.len().is_multiple_of(leg_size) {
            return Err(DeserializeError::BadLength {
                got: buf.len(),
                leg_size,
            });
        }
        let n = buf.len() / leg_size;
        let mut legs: Vec<RouteSubmissionLeg> = Vec::with_capacity(n);
        unsafe {
            std::ptr::copy_nonoverlapping(buf.as_ptr(), legs.as_mut_ptr() as *mut u8, buf.len());
            legs.set_len(n);
        }
        Ok(Self { legs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg_bytes(
        pair_id: u64,
        dir: u64,
        price: u64,
        volume: u64,
    ) -> [u8; RouteSubmissionLeg::SIZE] {
        let mut b = [0u8; RouteSubmissionLeg::SIZE];
        b[0..8].copy_from_slice(&pair_id.to_le_bytes());
        b[8..16].copy_from_slice(&dir.to_le_bytes());
        b[16..24].copy_from_slice(&price.to_le_bytes());
        b[24..32].copy_from_slice(&volume.to_le_bytes());
        b
    }

    #[test]
    fn leg_size_is_32() {
        assert_eq!(RouteSubmissionLeg::SIZE, 32);
    }

    #[test]
    fn deserialize_empty_buffer_yields_empty_legs() {
        let sub = RouteSubmission::deserialize(&[]).unwrap();
        assert!(sub.legs.is_empty());
    }

    #[test]
    fn deserialize_single_leg() {
        let bytes = leg_bytes(42, 1, 1_000_000, 5);
        let sub = RouteSubmission::deserialize(&bytes).unwrap();
        assert_eq!(sub.legs.len(), 1);
        let leg = &sub.legs[0];
        assert_eq!(leg.pair_id, 42);
        assert_eq!(leg.direction, Direction::Sell);
        assert_eq!(leg.price, 1_000_000);
        assert_eq!(leg.volume, 5);
    }

    #[test]
    fn deserialize_multiple_legs() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&leg_bytes(1, 0, 100, 1));
        buf.extend_from_slice(&leg_bytes(2, 1, 200, 2));
        buf.extend_from_slice(&leg_bytes(3, 0, 300, 3));
        let sub = RouteSubmission::deserialize(&buf).unwrap();
        assert_eq!(sub.legs.len(), 3);
        assert_eq!(sub.legs[0].pair_id, 1);
        assert_eq!(sub.legs[1].direction, Direction::Sell);
        assert_eq!(sub.legs[2].volume, 3);
    }

    #[test]
    fn bad_length_rejected() {
        let err = RouteSubmission::deserialize(&[0u8; 33]).unwrap_err();
        assert!(matches!(err, DeserializeError::BadLength { got: 33, .. }));
    }
}
