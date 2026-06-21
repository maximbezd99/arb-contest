# Contestant Guide

You're going to write a bot that trades on a fake crypto-style exchange.
The whole contest is one idea: **find a mispriced asset and execute a trade on it before the price is not back to normal or before someone else takes it.**

The rest of this doc explains the world, the rules, and what separates a winning bot from a losing one.

---

## ⚡ TL;DR

- There are **1,000 tokens** and **10,000 randomly generated trading pairs** between them. One token is **USD** and every token has a pair with **USD**.
- You start each round with **100 USD**. **100 rounds.** Win the most rounds, win the contest.
- The server fires publishes simulated price updates to all contestants (~10 million per round).
- Sometimes prices briefly fall out of sync, and a loop like `USD → A → B → USD` gives you back **more USD than you put in**. That's an **arbitrage**.
- You can only do one action: submit a **route** (a full loop back to USD). If it works, you keep the profit, you make a mistake in the route or it's no longer possible - you get penalized.

Two things decide everything: **how good your code is at spotting arbs (strategy)** and **how fast it reacts (performance)**.

---

## 1. 🌍 The world you're trading in

Think of a graph. Each **node** is a token. Each **edge** is a trading pair you can trade across.

```
                 ┌─────┐
        ┌────────│ USD │────────┐
        │        └─────┘        │
        │       ╱       ╲       │
     ┌─────┐ ┌─────┐  ┌─────┐ ┌─────┐
     │  A  │─│  B  │──│  C  │─│  D  │   ... 1,000 tokens total
     └─────┘ └─────┘  └─────┘ └─────┘
        ╲       │  ╲    │  ╱      ╱
         ╲      │   ╲   │ ╱      ╱
          (10,000 pairs criss-crossing them)
```

Key facts:

- **USD is always present**, and **every token has a pair with USD.** So you can always get back to USD from anywhere.
- Each pair has a **single price** and an **available volume** (how much you can trade right now).
- When you trade across a pair, you **consume some of that volume** — you do **not** move the price. The price is set by the market, not by you.
- You can only trade **up to the available volume** on a pair. Ask for more than exists and your trade can't fill (and you are penalized).

Your starting wallet each round: **100 USD.** Your goal: end the round with as much USD as possible.

---

## 2. 💡 What "arbitrage" means here

Normally the market is **consistent**: if you walk any loop of trades and come back to where you started, you end up with *exactly* what you began with. No free money.

**Example — a consistent loop (no profit):**

```
   Start: 100 USD
        │  USD → A      (you get 10 A per USD)
        ▼
     1,000 A
        │  A → B        (you get 5 B per A)
        ▼
     5,000 B
        │  B → USD      (you get 0.02 USD per B)
        ▼
      100 USD           →  exactly back to 100. No profit. 😐
```

The rates multiply to `10 × 5 × 0.02 = 1.00`. A perfectly balanced loop.

**Now — the same loop, but one leg is briefly mispriced:**

Suppose the `B → USD` pair is momentarily *too generous* (a lagging update left it at `0.021` instead of `0.02`):

```
   Start: 100 USD
        │  USD → A      (10 A per USD)
        ▼
     1,000 A
        │  A → B        (5 B per A)
        ▼
     5,000 B
        │  B → USD      (0.021 — STALE, too high!)
        ▼
      105 USD           →  +5 USD profit!  ✅ THIS is an arb.
```

Now the rates multiply to `10 × 5 × 0.021 = 1.05` → a **5% loop**. Put in 100 USD, get 105 back. Free money — *if you can grab it in time.*

> **The whole game is finding loops where the rates multiply to more than 1, and executing them before they vanish.**

---

## 3. ⏱️ Where the opportunities come from

Every token has a hidden "true value" inside a simulation engine. Once in a while, **a token's value moves.** When it does, all the pairs touching that token need to update to the new value — but they **don't all update at the same instant.** They reprice **one at a time over about 1 millisecond.**

During that ~1 ms, some pairs show the new value and some still show the old one. **That mismatch is the arb.**

```
 t = 0.0 ms   Token B's value jumps.
              Its pairs begin repricing, staggered...

  B/USD  ──reprices──►●  (now correct)
  A/B    ──────reprices──────►●  (now correct)
  C/B    ──────────reprices──────────►●  (now correct)

              |◄──────── ~1 ms ARB WINDOW ────────►|
              Loops through the not-yet-updated pairs pay.
              Once everything catches up → gone.
```

---

## 4. 🎯 Your only move: a *route*

You have exactly **one action**: submit a **route**.

A route is a **complete loop that starts and ends in USD.** You never hold anything but USD at the end — no leftover tokens. (This also means you never have to "cash out": every route returns you to USD, so at the end of the round you just have USD.)

When you submit a route, you specify:

| Field | Meaning |
|-------|---------|
| **The legs** | The ordered list of pairs to trade through, e.g. `USD → A → B → USD` |
| **The price range** | The acceptable rate you expect on each leg |
| **The volume range** | How much you want to push through |

The route is **atomic**: either **all legs execute**, or **none do.** You'll never get stuck halfway with random tokens.

---

## 5. ✅❌ Fill or fail (and the fees)

When your route reaches the server, it checks **every leg against the live market**.

**Why would a leg fail?** The arb healed before you got there, *or* another bot (or background market flow) already consumed the volume you wanted.

### The fees

Every submitted route is charged a fee, **whether it fills or fails**:

- **A flat per-route fee** (constant, paid once per submission).
- **Plus a volume-based fee**, proportional to the USD committed on your first leg.

There is no separate "success is free, failure costs you" split — the same fee structure applies either way even if your routed is invalid.

---

## 6. 🏁 Racing the others (time priority)

You're all hunting the **same** arbs. So who gets one?

**Orders are processed in the order they arrive at the server.** First correct route to land wins the arb. Everyone slower who went for the same thing gets **REJECTED → pays the penalty.**

```
   Arb appears!  💰
        │
   ┌────┴────┬─────────┬──────────┐
   ▼         ▼         ▼          ▼
 Bot A     Bot B     Bot C      Bot D
 12 µs     40 µs     55 µs      90 µs     ← reaction time
   │         │         │          │
   ✅ WINS   ❌ fail    ❌ fail     ❌ fail
   (banks    (penalty) (penalty)  (penalty)
   profit)
```

> You don't have to arbitrage the whole volume - you can only try to take a part of an opportunity. It means that a single opportunity can be used by multiple bots.

---

## 7. 📡 Networking

There are two distinct channels between your bot and the server:

- **Data feed → UDP.** The server publishes price/volume updates to all contestants over UDP via multicast.
- **Route submissions → TCP.** You submit routes back to the server over TCP. Each request is one route; the response tells you whether it filled, failed, and what fees were charged.

**The exact protocol TBD.**

---

## 8. 🏆 Scoring

- The contest is **100 rounds.**
- When a round ends, we tally everyone's USD.
- **Most USD wins that round** → 1 round-win.
- **Most round-wins across the 100 rounds wins the contest.**

---

## 9. 🛡️ The environment & what's allowed

- Your bot runs in its **own Docker container** with **pinned CPU cores and a memory limit.**
- **Allowed:** 
  - any in-game aggression.
  - your language standard lib + linux standard stuff like libc.
- **Not allowed:** 
  - anything outside the game — trying to mess with the host, the server, or another contestant's container.
  - any 3rd party dependancies.
  - **AI USAGE FOR ANYTHING RELATED TO THE CONTEST IS NOT ALLOWED. If you need info - Google it (AI inside Google also not allowed). Let's have fun without clankers.**
