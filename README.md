# How to start

## How to add a contestant bot
0. Use example-bot as an example.
1. Add dir with your bot in contestants/. It should have a Dockerfile.
2. Add your bot as a new service to docker-compose.yml. It should be identical to example-bot. **cpuset must be** = uppercase + `-`→`_`, suffix `_CPUSET` (if your service name is `java-bot` -> cpuset = `JAVA_BOT_CPUSET`).
3. You can run the simulation with 1 or 2 active bots: `python scripts/run.py example-bot your-bot`. You can provide `--seed` for reproducible generations.

## Provided env vars
`SIM_HTTP_ADDR` - ip:port for simulation http server.

`SIM_UDP_GROUP` - ip:port for UDP your bot should listen for market updates.

`SIM_SUBMISSION_ADDR` - ip:port for TCP address you should connect to for sumbitting routes.

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
3. `TcpStream::connect(SIM_SUBMISSION_ADDR)` → **write 8 bytes** (your `contestant_id` as u64 LE) as first bytes. Only one tcp connection could be opened. If you open more then one - previous active is dropped.
4.  Bind a UDP socket on `SIM_UDP_GROUP`'s port and `join_multicast_v4` the group.
5. `POST /{contestant_id}/ready` → simulation counts you toward `expected_contestants` and starts the runloop once all are ready.

**UDP tick (24 bytes, LE):**

```
pair_id : u64
price   : u64
volume  : u64
```

**TCP submission frame.** Write any whole number of 32-byte legs to the submission stream; the simulation parses them as `RouteSubmissionLeg`s (see `simulation/src/protocol/submission.rs`):

```
pair_id   : u64
direction : u64    # 0 = Buy, 1 = Sell
price     : u64
volume    : u64
```

Bad framing (length not a multiple of 32) drops that buffer.

See `contestants/example-bot/src/main.rs` for a working reference implementation.

> Submission works only on networking level. Logic for it is not implemented on simulation side yet.

## Code reference
You can adjust `simulation/config.json` (you can see doc for it in `simulation/stc/generation/config.rs`). For lower volume you should probably tweak `updates_per_sec`. This value controls the amount of "original" mispricings which happen every second, number of total updates per second will be a lot higher (depending on number of tokens and pairs).

Files with structs related to simultion <-> bot communication you can find in `simulation/src/protocol`. Reference `contestants/example-bot` to see how binary `/market` data can be parsed. This endpoint exists (in addition to `/market/json`) because not all languages have built-in json support.

After simulation is finished check out stats printed. `final_overshoot_ns` must be low (for me it's usually <100), if it's high every run it means the machine is not keeping up with the volume of updates on simulation side. But it's more realistic that the UDP sender would be lagging behind instead. To see if UDP sender is lagging you can check `udp_queue_depth` - it shouldn't have too big numbers, otherwise it means that UDP sender is lagging behind a simulation.