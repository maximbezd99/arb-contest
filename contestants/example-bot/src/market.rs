#[derive(Debug)]
pub struct Market {
    pub fee: Fee,
    pub tokens: Vec<Token>,
    pub pairs: Vec<TokenPair>,
}

#[derive(Debug)]
pub struct Fee {
    pub static_atomic_usd: u64,
    pub variable_bps: u64,
}

#[derive(Debug)]
pub struct Token {
    pub id: u64,
    pub decimals: u64,
}

#[derive(Debug)]
pub struct TokenPair {
    pub id: u64,
    pub base: u64,
    pub quote: u64,
    pub price: u64,
    pub volume: u64,
}

const TOKEN_SIZE: usize = 16;
const PAIR_SIZE: usize = 40;

/// See `simulation/src/protocol/market.rs` for the format.
pub fn parse_market(bytes: &[u8]) -> Result<Market, String> {
    let mut cursor = Cursor { buf: bytes, pos: 0 };

    let fee = Fee {
        static_atomic_usd: cursor
            .read_u64()
            .expect("can't parse Fee.static_atomic_usd"),
        variable_bps: cursor.read_u64().expect("can't parse Fee.variable_bps"),
    };

    let tokens_len = cursor.read_u64().expect("can't parse tokens_len");
    let tokens = cursor
        .read(tokens_len as usize)?
        .chunks_exact(TOKEN_SIZE)
        .map(|chunk| {
            let mut cursor = Cursor::new(chunk);
            Token {
                id: cursor.read_u64().expect("can't parse Token.id"),
                decimals: cursor.read_u64().expect("can't parse Token.decimals"),
            }
        })
        .collect();

    let pairs_len = cursor.read_u64().expect("can't parse pairs_len");
    let pairs = cursor
        .read(pairs_len as usize)?
        .chunks_exact(PAIR_SIZE)
        .map(|chunk| {
            let mut cursor = Cursor::new(chunk);
            TokenPair {
                id: cursor.read_u64().expect("can't parse TokenPair.id"),
                base: cursor.read_u64().expect("can't parse TokenPair.base"),
                quote: cursor.read_u64().expect("can't parse TokenPair.quote"),
                price: cursor.read_u64().expect("can't parse TokenPair.price"),
                volume: cursor.read_u64().expect("can't parse TokenPair.volume"),
            }
        })
        .collect();

    Ok(Market { fee, tokens, pairs })
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { buf: bytes, pos: 0 }
    }

    fn read(&mut self, n: usize) -> Result<&'a [u8], String> {
        let slice = self
            .buf
            .get(self.pos..self.pos + n)
            .ok_or_else(|| format!("EOF at {} reading {n} bytes", self.pos))?;
        self.pos += n;
        Ok(slice)
    }

    fn read_u64(&mut self) -> Result<u64, String> {
        let bytes = self.read(size_of::<u64>())?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }
}
