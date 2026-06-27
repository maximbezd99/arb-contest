# How to start

## How to add a contestant bot
0. Use example-bot as an example.
1. Add dir with your bot in contestants/. It should have a `Dockerfile`.
2. Add your bot as a new service to `docker-compose.yml`. It should be identical to example-bot. **cpuset must be** = uppercase + `-`→`_`, suffix `_CPUSET` (if your service name is `java-bot` -> cpuset = `JAVA_BOT_CPUSET`).
3. You can run the simulation with 1 or 2 active bots: `python scripts/run.py example-bot your-bot`. You can provide `--seed` for reproducible generations.

## Provided env vars
`SIM_HTTP_ADDR` - ip:port for simulation http server.

`SIM_UDP_GROUP` - ip:port for UDP **multicast** your bot should listen for market updates.

`SIM_SUBMISSION_ADDR` - ip:port for TCP address you should connect to for sumbitting routes.

`SIM_INITIAL_BALANCE_USD` - initial balance for a contestant.

## Endpoints
Those endpoints are served by HTTP server running in simulation. They are callable by your bot.

`GET /health` - not useful for bots.

`GET /market` - get all tokens + pairs + other data about the simulation.

`GET /market/json` - same as `/market` but in json format.

`POST /register` - get a contestant-id for your bot. It's required to call it.

`POST {contestant-id}/ready` - you must call this once your bot is fully ready to listen/submit. Simulation won't run until all bots called it.

## Protocol

**Startup sequence (per bot)**

1. `GET /market` - parse the binary snapshot. `GET /market/json` if you want json instead.
2. `POST /register` - read 8 bytes; that's your `contestant_id` (u64 LE).
3. TCP connect `SIM_SUBMISSION_ADDR`. **Write 8 bytes** (your `contestant_id` as u64 LE) as first bytes. Only one tcp connection could be opened. If you open more then one - previous active is dropped.
4.  Bind a UDP socket on `SIM_UDP_GROUP`'s port and join multicast group. You **must** set `SO_REUSEADDR` and `SO_REUSEPORT` on the socket **before** binding — every contestant container runs in the host network namespace, so without these the second bot's bind would fail. See `contestants/example-bot/src/main.rs` (`bind_multicast_socket`) for the pattern.
5. `POST /{contestant_id}/ready` → simulation counts you toward `expected_contestants` and starts the runloop once all are ready.

**UDP tick (32 bytes, LE):**

```
seq     : u64   # monotonic per-run counter; use to detect drops/reorders
pair_id : u64   
price   : u64   # in atomic-quote per 1 whole base
volume  : u64   # in atomic-base
```

**TCP handshake.** Immediately after `connect`, write your `contestant_id` as 8 bytes (u64 LE). No reply — submissions follow on the same stream.

**TCP submission frame.** Each submission is a 9-byte header followed by `num_legs` × 25-byte legs (see `simulation/src/protocol/submission.rs`):

```
header (9 bytes, LE):
  sub_id   : u64    # contestant-chosen id, echoed back in the response
  num_legs : u8     # 1..=32

leg (25 bytes, LE, repeated num_legs times):
  pair_id   : u64
  direction : u8    # 0 = Buy (pay quote, receive base), 1 = Sell (pay base, receive quote)
  price     : u64   # in atomic-quote per 1 whole base — must equal pair.price at evaluation time
  volume    : u64   # in atomic-base
```

Multiple submissions may be sent back-to-back on the same stream. Any framing error (`num_legs == 0`, `num_legs > 32`, `direction` not in {0, 1}) drops the stream.

**TCP submission response.** Per submission the simulation writes back 17 bytes:

```
sub_id  : u64     # echoes the submission's sub_id
ok      : u8      # 1 = accepted, 0 = rejected
balance : i64     # contestant balance after this submission, atomic-USD (signed)
```

## Other
Some of stuff you can check out in `contestants/example-bot/src/main.rs`.

By default simulation config is set with very modest values so that `example-bot` which logs every update behaves good. You can adjust `simulation/config.json` (you can see doc for it in `simulation/stc/generation/config.rs`). For volume you should tweak `updates_per_sec`, `rebalance_delay_us`. This value controls the amount of "original" mispricings which happen every second, number of total updates per second will be a lot higher (depending on number of tokens and pairs).

Files with structs related to simultion <-> bot communication you can find in `simulation/src/protocol`. Reference `contestants/example-bot` to see how binary `/market` data can be parsed. This endpoint exists (in addition to `/market/json`) because not all languages have built-in json support.

After simulation is finished check out stats printed. `final_overshoot_ns` must be low (for me it's usually <100), if it's high every run it means the machine is not keeping up with the volume of updates on simulation side. But it's more realistic that the UDP sender would be lagging behind instead. To see if UDP sender is lagging you can check `udp_queue_depth` - it shouldn't have too big numbers, otherwise it means that UDP sender is lagging behind a simulation.