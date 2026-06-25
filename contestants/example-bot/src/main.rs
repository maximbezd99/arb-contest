use std::env;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::process::ExitCode;

mod market;

/// Print every Nth received UDP tick.
const SAMPLE_EVERY: u64 = 10000;

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
    let config = Config::parse();

    eprintln!("[example-bot] GET http://{}/market", config.sim_http_addr);
    let body = http_request(&config.sim_http_addr, "GET", "/market")?;
    eprintln!("[example-bot] received {} bytes", body.len());

    let _market = market::parse_market(&body).expect("failed to parse market");

    let id_bytes = http_request(&config.sim_http_addr, "POST", "/register")?;
    if id_bytes.len() < 8 {
        panic!("/register returned {} bytes, expected 8", id_bytes.len());
    }
    let contestant_id = u64::from_le_bytes(id_bytes[..8].try_into().unwrap());
    eprintln!("[example-bot] registered contestant_id={contestant_id}");

    let mut submission_stream = TcpStream::connect(&config.sim_submission_addr)?;
    submission_stream.write_all(&contestant_id.to_le_bytes())?;
    eprintln!(
        "[example-bot] connected submission stream to {} (local={})",
        config.sim_submission_addr,
        submission_stream.local_addr()?,
    );

    let feed_group = config.sim_udp_group;
    let udp_handle = std::thread::spawn(move || listen_feed(&feed_group));

    http_request(
        &config.sim_http_addr,
        "POST",
        &format!("/{contestant_id}/ready"),
    )?;
    eprintln!("[example-bot] /{contestant_id}/ready ok");

    let _udp_result = udp_handle.join();

    Ok(())
}

fn listen_feed(group: &str) -> io::Result<()> {
    let (ip_str, port_str) = group.rsplit_once(':').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SIM_UDP_GROUP missing port: {group}"),
        )
    })?;
    let ip: Ipv4Addr = ip_str.parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad multicast ip {ip_str}: {e}"),
        )
    })?;
    let port: u16 = port_str.parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad port {port_str}: {e}"),
        )
    })?;
    if !ip.is_multicast() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a multicast address: {ip}"),
        ));
    }

    let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let socket = UdpSocket::bind(bind_addr)?;
    socket.join_multicast_v4(&ip, &Ipv4Addr::UNSPECIFIED)?;
    eprintln!("[example-bot] joined multicast {ip}:{port}, sampling 1/{SAMPLE_EVERY}");

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut buf = [0u8; 64];
    let mut received: u64 = 0;
    loop {
        let (n, _from) = socket.recv_from(&mut buf)?;
        received += 1;
        if received % SAMPLE_EVERY != 0 {
            continue;
        }
        if n < 24 {
            writeln!(
                out,
                "[example-bot] tick #{received}: short packet ({n} bytes)"
            )?;
            continue;
        }
        // PriceUpdate layout: pair_id (u64 LE), price (u64 LE), volume (u64 LE).
        let pair_id = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let price = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let volume = u64::from_le_bytes(buf[16..24].try_into().unwrap());
        writeln!(
            out,
            "[example-bot] tick #{received}: pair_id={pair_id} price={price} volume={volume}",
        )?;
    }
}

fn http_request(addr: &str, method: &str, path: &str) -> io::Result<Vec<u8>> {
    let sock_addr = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("resolve {addr}")))?;
    let mut stream = TcpStream::connect(sock_addr)?;

    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    let req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
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

struct Config {
    sim_http_addr: String,
    sim_udp_group: String,
    sim_submission_addr: String,
}

impl Config {
    fn parse() -> Self {
        let sim_http_addr = env::var("SIM_HTTP_ADDR").expect("SIM_HTTP_ADDR must be set");
        let sim_udp_group = env::var("SIM_UDP_GROUP").expect("SIM_UDP_GROUP must be set");
        let sim_submission_addr =
            env::var("SIM_SUBMISSION_ADDR").expect("SIM_SUBMISSION_ADDR must be set");
        Self {
            sim_http_addr,
            sim_submission_addr,
            sim_udp_group,
        }
    }
}
