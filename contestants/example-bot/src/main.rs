use std::env;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::ExitCode;

mod market;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[example-bot] fatal: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let addr = env::var("SIM_HTTP_ADDR").map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "SIM_HTTP_ADDR env var required (e.g. simulation:9003)",
        )
    })?;

    eprintln!("[example-bot] GET http://{addr}/market");
    let body = http_get(&addr, "/market")?;
    eprintln!("[example-bot] received {} bytes", body.len());

    let market = market::parse_market(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("parse: {e}")))?;

    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "fee: static={} atomic-USD, variable={} bps",
        market.fee.static_atomic_usd, market.fee.variable_bps
    )?;
    writeln!(out, "tokens: {} (first 3:)", market.tokens.len())?;
    for t in market.tokens.iter().take(3) {
        writeln!(out, "  id={} decimals={}", t.id, t.decimals)?;
    }
    writeln!(out, "pairs: {} (first 3:)", market.pairs.len())?;
    for p in market.pairs.iter().take(3) {
        writeln!(
            out,
            "  id={} base={} quote={} price={} volume={}",
            p.id, p.base, p.quote, p.price, p.volume
        )?;
    }
    Ok(())
}

fn http_get(addr: &str, path: &str) -> io::Result<Vec<u8>> {
    let sock_addr = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("resolve {addr}")))?;
    let mut stream = TcpStream::connect(sock_addr)?;

    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes())?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;

    let header_end = find(&raw, b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no \\r\\n\\r\\n in response"))?;
    let status_end = find(&raw, b"\r\n").unwrap_or(0);
    let status = std::str::from_utf8(&raw[..status_end]).unwrap_or("(non-utf8 status)");
    if !status.contains(" 200 ") {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("bad HTTP status: {status}"),
        ));
    }
    Ok(raw[header_end + 4..].to_vec())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
