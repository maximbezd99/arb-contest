use std::fmt;

#[derive(Debug, Clone)]
pub struct RouteSubmission {
    pub sub_id: u64,
    pub legs: Vec<RouteSubmissionLeg>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RouteSubmissionLeg {
    /// Pair to trade on.
    pub pair_id: u64,
    /// Trade direction.
    /// `Buy`  = pay quote, receive base.
    /// `Sell` = pay base,  receive quote.
    pub direction: Direction,
    /// Price the contestant claims is current, in **atomic-quote per 1 whole base**.
    /// Must equal `pair.price` at evaluation time or the submission is rejected.
    pub price: u64,
    /// Amount to fill on this leg, in **atomic units of base**.
    /// For a `Buy` this is the base amount received.
    /// For a `Sell` the base amount spent.
    pub volume: u64,
}

#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Buy = 0,
    Sell = 1,
}

/// `[sub_id u64 LE][num_legs u8]`. Leg payload follows.
pub const SUBMISSION_HEADER_SIZE: usize = 9;

/// Hard cap on legs per submission. Anything above is treated as a framing
/// error and the stream is dropped with `DeserializeError::TooManyLegs`.
pub const MAX_LEGS: u8 = 32;

/// Maximum on-the-wire bytes for one valid submission.
pub const MAX_SUBMISSION_SIZE: usize = SUBMISSION_HEADER_SIZE + (MAX_LEGS as usize) * std::mem::size_of::<RouteSubmissionLeg>();

#[derive(Debug, Clone, Copy)]
pub enum DeserializeError {
    /// Not enough bytes for a full submission yet. Caller should buffer more.
    Incomplete,
    /// `num_legs == 0` — empty submissions aren't valid.
    EmptySubmission,
    /// `num_legs > MAX_LEGS` — almost certainly a desync.
    TooManyLegs { got: u8, max: u8 },
    /// `direction` was neither `Buy (0)` nor `Sell (1)` — almost certainly a desync.
    BadDirection { value: u64 },
}

impl fmt::Display for DeserializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Incomplete => write!(f, "incomplete submission"),
            Self::EmptySubmission => write!(f, "submission has zero legs"),
            Self::TooManyLegs { got, max } => {
                write!(f, "submission has {got} legs (max {max})")
            }
            Self::BadDirection { value } => {
                write!(f, "leg direction {value} is neither 0 (buy) nor 1 (sell)")
            }
        }
    }
}

impl std::error::Error for DeserializeError {}

impl RouteSubmissionLeg {
    pub const SIZE: usize = std::mem::size_of::<Self>();
}

impl RouteSubmission {
    /// Try to parse one submission off the front of `buf`.
    /// Returns the parsed submission and the number of bytes consumed.
    pub fn deserialize(buf: &[u8]) -> Result<(Self, usize), DeserializeError> {
        if buf.len() < SUBMISSION_HEADER_SIZE {
            return Err(DeserializeError::Incomplete);
        }

        let sub_id = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let num_legs_byte = buf[8];
        if num_legs_byte == 0 {
            return Err(DeserializeError::EmptySubmission);
        }

        if num_legs_byte > MAX_LEGS {
            return Err(DeserializeError::TooManyLegs {
                got: num_legs_byte,
                max: MAX_LEGS,
            });
        }

        let num_legs = num_legs_byte as usize;
        let legs_bytes = num_legs * RouteSubmissionLeg::SIZE;
        let total = SUBMISSION_HEADER_SIZE + legs_bytes;
        if buf.len() < total {
            return Err(DeserializeError::Incomplete);
        }

        let mut legs: Vec<RouteSubmissionLeg> = Vec::with_capacity(num_legs);
        for i in 0..num_legs {
            let off = SUBMISSION_HEADER_SIZE + i * RouteSubmissionLeg::SIZE;
            let pair_id = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
            let dir_val = u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap());
            let direction = match dir_val {
                0 => Direction::Buy,
                1 => Direction::Sell,
                _ => return Err(DeserializeError::BadDirection { value: dir_val }),
            };
            let price = u64::from_le_bytes(buf[off + 16..off + 24].try_into().unwrap());
            let volume = u64::from_le_bytes(buf[off + 24..off + 32].try_into().unwrap());
            legs.push(RouteSubmissionLeg {
                pair_id,
                direction,
                price,
                volume,
            });
        }

        Ok((Self { sub_id, legs }, total))
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SubmissionResponse {
    pub sub_id: u64,
    pub ok: u64,
    /// Contestant's balance after this submission, in atomic-USD. **Signed**.
    pub balance: i64,
}

impl SubmissionResponse {
    pub fn as_bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, std::mem::size_of::<Self>()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg_bytes(pair_id: u64, dir: u64, price: u64, volume: u64) -> [u8; RouteSubmissionLeg::SIZE] {
        let mut b = [0u8; RouteSubmissionLeg::SIZE];
        b[0..8].copy_from_slice(&pair_id.to_le_bytes());
        b[8..16].copy_from_slice(&dir.to_le_bytes());
        b[16..24].copy_from_slice(&price.to_le_bytes());
        b[24..32].copy_from_slice(&volume.to_le_bytes());
        b
    }

    fn frame(sub_id: u64, legs: &[[u8; RouteSubmissionLeg::SIZE]]) -> Vec<u8> {
        let mut out = Vec::with_capacity(SUBMISSION_HEADER_SIZE + legs.len() * RouteSubmissionLeg::SIZE);
        out.extend_from_slice(&sub_id.to_le_bytes());
        out.push(legs.len() as u8);
        for l in legs {
            out.extend_from_slice(l);
        }
        out
    }

    #[test]
    fn leg_size_is_32() {
        assert_eq!(RouteSubmissionLeg::SIZE, 32);
    }

    #[test]
    fn deserialize_single_leg() {
        let buf = frame(7, &[leg_bytes(42, 1, 1_000_000, 5)]);
        let (sub, n) = RouteSubmission::deserialize(&buf).unwrap();
        assert_eq!(n, buf.len());
        assert_eq!(sub.sub_id, 7);
        assert_eq!(sub.legs.len(), 1);
        let leg = &sub.legs[0];
        assert_eq!(leg.pair_id, 42);
        assert_eq!(leg.direction, Direction::Sell);
        assert_eq!(leg.price, 1_000_000);
        assert_eq!(leg.volume, 5);
    }

    #[test]
    fn deserialize_multiple_legs() {
        let buf = frame(99, &[leg_bytes(1, 0, 100, 1), leg_bytes(2, 1, 200, 2), leg_bytes(3, 0, 300, 3)]);
        let (sub, n) = RouteSubmission::deserialize(&buf).unwrap();
        assert_eq!(n, buf.len());
        assert_eq!(sub.sub_id, 99);
        assert_eq!(sub.legs.len(), 3);
        assert_eq!(sub.legs[0].pair_id, 1);
        assert_eq!(sub.legs[1].direction, Direction::Sell);
        assert_eq!(sub.legs[2].volume, 3);
    }

    #[test]
    fn coalesced_two_submissions() {
        let mut buf = frame(1, &[leg_bytes(10, 0, 10, 10)]);
        buf.extend_from_slice(&frame(2, &[leg_bytes(20, 1, 20, 20), leg_bytes(21, 0, 21, 21)]));
        let total = buf.len();

        let (a, na) = RouteSubmission::deserialize(&buf).unwrap();
        assert_eq!(a.sub_id, 1);
        assert_eq!(a.legs.len(), 1);

        let (b, nb) = RouteSubmission::deserialize(&buf[na..]).unwrap();
        assert_eq!(b.sub_id, 2);
        assert_eq!(b.legs.len(), 2);
        assert_eq!(na + nb, total);
    }

    #[test]
    fn incomplete_header() {
        let err = RouteSubmission::deserialize(&[0u8; 4]).unwrap_err();
        assert!(matches!(err, DeserializeError::Incomplete));
    }

    #[test]
    fn incomplete_legs() {
        let mut buf = frame(1, &[leg_bytes(0, 0, 0, 0), leg_bytes(0, 0, 0, 0)]);
        buf.truncate(buf.len() - 1);
        let err = RouteSubmission::deserialize(&buf).unwrap_err();
        assert!(matches!(err, DeserializeError::Incomplete));
    }

    #[test]
    fn zero_legs_rejected() {
        let buf = frame(1, &[]);
        let err = RouteSubmission::deserialize(&buf).unwrap_err();
        assert!(matches!(err, DeserializeError::EmptySubmission));
    }

    #[test]
    fn too_many_legs_rejected_before_waiting_for_bytes() {
        // Header claims MAX_LEGS + 1 but no leg bytes follow — must fail
        // immediately (not block waiting for more bytes).
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u64.to_le_bytes());
        buf.push(MAX_LEGS + 1);
        let err = RouteSubmission::deserialize(&buf).unwrap_err();
        assert!(matches!(
            err,
            DeserializeError::TooManyLegs { got, max } if got == MAX_LEGS + 1 && max == MAX_LEGS
        ));
    }

    #[test]
    fn bad_direction_rejected() {
        let buf = frame(1, &[leg_bytes(0, 42, 0, 0)]);
        let err = RouteSubmission::deserialize(&buf).unwrap_err();
        assert!(matches!(err, DeserializeError::BadDirection { value: 42 }));
    }

    #[test]
    fn response_as_bytes_roundtrip() {
        let r = SubmissionResponse {
            sub_id: 0xDEAD_BEEF,
            ok: 1,
            balance: -1_234_567,
        };
        let bytes = r.as_bytes();
        assert_eq!(bytes.len(), std::mem::size_of::<SubmissionResponse>());
        assert_eq!(u64::from_le_bytes(bytes[0..8].try_into().unwrap()), r.sub_id);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), r.ok);
        assert_eq!(i64::from_le_bytes(bytes[16..24].try_into().unwrap()), r.balance);
    }
}
