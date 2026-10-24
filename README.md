# Vickey

> A sealed-bid word shark for [ScrabbleBot](https://scrabblebot.spacetimedb.com), the bot-vs-bot auction e-sport that SpacetimeDB runs at Cal Hacks 13.0. Written in Rust. It values every tile by Monte Carlo, paces its coins like an ad exchange, and tunes itself against the live league while it sleeps.

![Rust](https://img.shields.io/badge/Rust-2021-b7410e?logo=rust) ![SpacetimeDB](https://img.shields.io/badge/SpacetimeDB-2.13-1f2328) ![Cal Hacks 13.0](https://img.shields.io/badge/Cal%20Hacks-13.0-fdb515) ![Auctions](https://img.shields.io/badge/auctions-Vickrey%20%2B%20first--price-6e40c9) ![License: MIT](https://img.shields.io/badge/license-MIT-green)

**Tags:** `scrabblebot` `spacetimedb` `rust` `auction-theory` `vickrey-auction` `monte-carlo` `game-ai` `bot` `cal-hacks` `self-improving`

The name is half a pun on William Vickrey, whose second-price auction runs every round of this game, and half a promise.

---

## The game in thirty seconds

Six bots each start a match with 100 coins and five hidden tiles. The server then reveals the rest of a 98-tile bag one letter at a time, and each letter gets a **one-second sealed-bid auction**: the highest bid wins and pays the runner-up's bid. Bots spell dictionary words from their tiles whenever they like. A word pays `face value × length multiplier` (1× up to three letters, rising to 3× at seven or more), and that reward goes into both your **score** and your **coin balance**. The match ends the moment the bag runs out.

## What actually decides matches

Before writing a line of code we read the server source end to end and mined the live league's public tables: 435 matches, about 29,500 auctions, and 7,600 words. These findings drive every design choice.

| Finding | Consequence |
|---|---|
| Bidding **never lowers your score**. Score is only word rewards, and coins left at the end are worthless. | This is a budget-pacing problem, not a profit problem. Spend everything, and spend it where points per coin are highest. |
| Against real bots a letter clears at **~12 coins** but is worth ~4.4 points. | Coins are the binding constraint. A 7-letter word is also a cash machine that buys the next one. |
| Prices start **~35% hot** and fall through the mid-game. Bots leave **~31 coins** unspent on average. | Patience is cheap. Late tiles that complete a word are bargains. |
| The match ends *inside* the transaction that closes the last auction. | The last tile is worthless (bid 0), and every word must be submitted while the last auction is open. |
| Opponents' balances are **public**. | The most anyone can bid is known exactly, so the price model is truncated there. |
| When you lose a heads-up auction, the price you see is **your own bid**. | That's a lower bound on the rival's bid, not a price, so it's learned as censored data. |
| The ENABLE wordlist has no `QI` or `ZA`. Q almost always needs U. | Valuation comes from the real dictionary, never from face value. |
| Tournaments: Swiss by place, then elimination, then a **heads-up three-game final** decided by total score. Admins can switch to first-price. | Separate parameter profiles for multi-seat and heads-up tables, plus surplus-maximising bid shading for first-price. |

## How Vickey thinks

```
                public events (auctions, results, words, balances)
                                     │
                                     ▼
 ┌───────────────────────────────────────────────────────────────────────┐
 │ Tracker: reconstructs every rack it can, deduces rivals' hidden tiles │
 │          from what they play, and keeps the unseen-letter pool exact  │
 └───────────────────────────────────────────────────────────────────────┘
          │                         │                          │
          ▼                         ▼                          ▼
 ┌─────────────────┐   ┌──────────────────────────┐   ┌──────────────────┐
 │ Valuation       │   │ Market                   │   │ Play policy      │
 │ E[T(R+L+F)] −   │   │ rivals' max bid ~        │   │ cash a word when │
 │ E[T(R+F)] over  │   │ log-normal(letter,phase) │   │ reward·(1+ρλ) +  │
 │ sampled futures │   │ × heat, capped at the    │   │ E[hold rest] >   │
 │ F; T = best     │   │ richest rival's balance  │   │ E[hold all]; at  │
 │ word partition  │   │ (censored learning)      │   │ the last auction │
 └────────┬────────┘   └────────────┬─────────────┘   │ dump the exact   │
          │                         │                 │ best partition   │
          └────────────┬────────────┘                 └──────────────────┘
                       ▼
          ┌──────────────────────────────┐
          │ Pacing: solve λ (points/coin)│
          │ so expected spend over the   │
          │ usable auctions = budget,    │
          │ then bid V/λ (Vickrey) or    │
          │ argmax (V/λ−b)·P(win|b)      │
          │ (first-price)                │
          └──────────────────────────────┘
```

- **Terminal value `T(rack)`** is the best set of disjoint dictionary words a rack can play. That is exactly what the final dump scores. `vickey-core` computes it with a trie over letter multisets and an anytime branch-and-bound solver. It is exact in about 9 ms for 20 letters, and a greedy estimator handles the Monte Carlo inner loop.
- **Valuation** samples the letters we can still expect to buy (sized by budget over market price) and measures what a tile adds, using common random numbers so the difference has low variance. A shrinkage prior steadies the cheap all-letters table.
- **Pacing** is the Balseiro–Gur idea from ad auctions: one shadow price `λ` turns points into coins, chosen so that expected spend matches budget plus expected word income. Under Vickrey the truthful bid `V/λ` is optimal.
- **Denial**: at heads-up tables the letter's value to the rival is added, since only the difference counts.
- **Safety rails**: never bid on the last tile, never bid above balance, a deterministic work budget caps every decision (p99 under 260 ms on one core), the final dump has its own deadline, and the dump moves earlier when the server is slow.

## Lessons from the live league

Vickey plays as bot **Vickey** for team **Karthik**. Within its first day in the live league it played 70+ real matches, won a third of them at 6-bot tables (where 1 in 6 is break-even), and climbed into the top ten of 59 bots by rating. The live league taught us three things the simulator couldn't:

| What went wrong live | Evidence | Fix |
|---|---|---|
| **Starving with money in the rack.** Vickey went broke mid-match while holding a playable 5–6 letter word, then sat out 20–40 auctions. | m785 held JIVER through 38 dry auctions; m833 held GAUCHO; m816 held PIXELS. | Below 15 coins, with auctions still to come, cash words of 5+ letters, priced with μ (the marginal points a coin buys under the balance cap) rather than the pacing λ, which collapses exactly when you're broke. |
| **A slow shared server.** SpacetimeDB runs one database's reducers one at a time, and at hackathon peak calls took 1.8–4.3 s, so final dumps sent on the last auction landed after the match ended ("Match not running"). | Four straight last places with every final word rejected. | Track recent round trips and enter a dump zone when the time left is under `2·p90 + 0.3 s`. The bot cashes everything early, and keeps cashing whatever new letters complete. |
| **The price model was not the problem.** | 2,007 live bids: 401 predicted wins, 359 actual, calibrated in every band from 0.2 to 0.9. | We kept the model and fixed the strategy instead. |

## Self-improvement

```
 live league (public SQL) ──► vickey-arena refit ──► config/field.json
                                                         │
 runs/summary.jsonl (our matches) ◄── vickey run ◄── config/params*.json
                                                         ▲
                       vickey-arena tune (paired, 2-batch significance)
```

`vickey-arena evolve` refits the rivals' price model from the most recent all-real matches. It then hill-climbs both parameter profiles in the simulator, with the candidate and the incumbent at the same tables on the same seeds, and adopts a change only if it wins clearly on two independent batches. The live bot hot-reloads `config/` at the start of every match. The simulator can't model the strongest live bots, so the final word belongs to the league itself. **Live A/B testing** (`config/ab.json`) splits casual matches between the champion and a challenger, scores both arms on Swiss points, and promotes or retires the challenger once Welch's |t| ≥ 2 with 40+ matches per arm. Tournament matches always use the champion. `vickey ab-report` shows the scoreboard.

Evolution runs on a launchd timer locally and on a six-hourly GitHub Actions job that opens a PR with any improvement. The local timer stands down on battery and whenever a tournament is open or under way, so tuning never competes with live bids for the CPU. Heads-up tables (the three-game final) get their own profile, `config/params_duel.json`, tuned separately.

## Repo layout

| Crate | What it is |
|---|---|
| [`vickey-core`](crates/vickey-core) | Rules mirrored from the server, racks, the ENABLE dictionary indexed by multiset, and the partition solver |
| [`vickey-brain`](crates/vickey-brain) | `Tracker`, `Market`, `Brain`, `Params`: the whole strategy, with no I/O |
| [`vickey-sim`](crates/vickey-sim) | `vickey-arena`: a server-faithful match engine, opponent archetypes (exact server sims, the starter kit, calibrated field bots), tournaments, the tuner, and the live-data refit |
| [`vickey-bot`](crates/vickey-bot) | `vickey`: the SpacetimeDB client and autopilot (lobby, tournaments, telemetry, hot reload) |
| `config/` | Tuned params and the fitted field model |
| `ops/` | launchd agents, Dockerfile, binding regeneration |

## Run it

```sh
# 1. One-time: create the team and mint a bot credential.
#    Uses your `spacetime login`; no browser needed. (Ours: team Karthik, bot Vickey.)
cargo run --release -p vickey-bot -- setup --team Karthik --bot Vickey

# 2. Play forever: joins lobbies, registers for tournaments, logs everything.
cargo run --release -p vickey-bot -- run

# 3. Keep it alive across reboots and crashes (macOS), awake via caffeinate
ops/install-launchd.sh
ops/install-evolve.sh          # self-improvement every 3 h (skips on battery / during tournaments)

# What's going on?
cargo run --release -p vickey-bot -- status
tail -f runs/vickey.out.log    # every bid, play and dump, explained
cat runs/heartbeat             # liveness, current match, reconnects
```

## Simulate and tune

```sh
cargo build --release -p vickey-sim
A=./target/release/vickey-arena

$A bench --lineup vickey,shark,shark,grinder,spender,nibbler -n 200   # vs the calibrated field
$A bench --lineup vickey,shark -n 40                                   # heads-up final
$A --first-price bench --lineup vickey,shark,shark,grinder,spender,nibbler -n 100
$A trace --lineup vickey,shark --seed 3                                # every decision, explained
$A tournament --entrants vickey,shark,shark,grinder,grinder,spender,nibbler,starter,shark,grinder,spender,nibbler -n 20
$A refit                                                               # field model from live data
$A evolve                                                              # refit + tune both profiles
```

## Engineering

- `cargo fmt`, `clippy -D warnings`, and the full test suite run in CI. The suite cross-checks the solver against brute force, checks server reward arithmetic against live rows, and checks tracker deduction, market monotonicity, and the engine's rules.
- **Strategy regression gates** fail the build if Vickey's mean place slips at Vickrey or first-price tables, or against the server's own sim bots.
- Telemetry is append-only JSONL (`runs/`), documented in [`crates/vickey-bot/TELEMETRY.md`](crates/vickey-bot/TELEMETRY.md).

## License

MIT © Karthik Subramanian. The ENABLE wordlist in `data/` is public domain and is the exact list the ScrabbleBot module embeds.
