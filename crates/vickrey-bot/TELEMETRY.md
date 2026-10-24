# Vickrey telemetry (schema v1)

`vickrey run` appends JSON Lines that the tuner reads to improve the strategy.
Field names are a contract: add fields freely, but never rename or repurpose
one without bumping `v` (`telemetry::SCHEMA_VERSION`).

| Path (live) | Path (spectate) | What |
| --- | --- | --- |
| `runs/matches/<match_id>.jsonl` | `runs/spectate/matches/<match_id>.jsonl` | Every event of one match |
| `runs/summary.jsonl` | `runs/spectate/summary.jsonl` | One line per finished match |
| `runs/heartbeat` | `runs/spectate/heartbeat` | Liveness, rewritten every 10 s |

`runs/` is set by `--runs-dir` / `VICKREY_RUNS_DIR`. Timestamps are Unix
milliseconds from the bot's clock. Every line carries `v` and `mode`
(`"live"` or `"spectate"`). In spectate mode "we/our" means the shadowed bot,
whose undisclosed starting tiles are replaced by seeded random stand-ins, and
no reducer is ever called.

## `matches/<match_id>.jsonl`

Each line has a `type`.

### `start`: once, when we begin tracking the match

| Field | Type | Meaning |
| --- | --- | --- |
| `ts_ms` | i64 | When tracking began |
| `match_id`, `my_bot_id` | u64 | |
| `auction_type` | string | `"Vickrey"` or `"FirstPrice"` |
| `n_players` | usize | Seats in the match |
| `participants` | u64[] | Bot ids in seat order |
| `starting_rack_size` | u32 | Hidden tiles dealt to each bot |
| `rack` | string | Our rack (sorted letters) after catching up |
| `resumed` | bool | True if we joined after auctions had already run (restart or reconnect) |
| `auctions_before_join` | u32 | Auctions replayed from history before we started deciding |
| `tournament_id` | u64? | Set for tournament matches |
| `duel_loaded` | bool | `config/params_duel.json` was present and valid at match start |
| `duel_active` | bool | The heads-up profile is in force (duel loaded and at most 2 players) |
| `params` | object | The full `vickrey_brain::Params` in force for this match (the duel profile when `duel_active`) |

### `auction`: one per auction we decided on, written when its result lands

| Field | Type | Meaning |
| --- | --- | --- |
| `auction_id`, `seq` | u64, u32 | Server id; 1-based index within the match |
| `letter` | string | Letter on offer |
| `bag_total` | u32 | `match_state.bag_total` when it opened (0 means the last auction) |
| `last` | bool | `bag_total == 0`: we dumped words and did not bid |
| `rack` | string | Rack we valued with (cache rack minus words in flight) |
| `balance`, `score` | i64 | Ours at decision time, counting rewards of words sent just before the bid |
| `bid` | object? | `{amount, value, lambda, p_win}` from `Brain::bid`; null on the last auction. `amount` is after clamping to balance |
| `submitted` | bool | A `submit_bid` was sent (false when amount 0, spectating, or stale) |
| `bid_error` | string? | Server rejection, e.g. `"Auction window expired"` |
| `decision_ms` | f64 | Total compute from handling the open to sending the bid |
| `plays_ms`, `bid_ms` | f64 | Split of `decision_ms` between `Brain::plays`/`final_plays` and `Brain::bid` |
| `ms_left` | i64? | `closes_at` minus our clock after deciding (includes clock skew) |
| `fast` | bool | The latency governor cut Monte Carlo samples for this decision |
| `rebid` | bool | Our rack changed while the auction was open, so we re-priced; `bid` and `rack` hold the latest values |
| `anchor` | i64? | The instant bid sent as the auction opened, before plays and valuation (live only). `bid.amount` is the full decision that replaced it; when the replacement was rejected as late, the anchor is what stood |
| `words` | string[] | Words we sent while this auction was open (`final` dump on the last one) |
| `winner` | u64? | Winning bot id; null if nobody bid |
| `we_won` | bool | |
| `paid` | i64 | Clearing price |
| `top_bid` | i64? | Highest bid, only visible to anonymous spectators (null live) |
| `balance_after` | i64 | Our balance once the result was applied |

### `word`: one per `submit_word` outcome

| Field | Type | Meaning |
| --- | --- | --- |
| `auction_id` | u64 | Auction that was open when we sent it |
| `word` | string | |
| `phase` | string | `"mid"` (from `Brain::plays`) or `"final"` (last-auction dump) |
| `expected_reward` | u32 | `vickrey_core::word_reward(word)` |
| `ok`, `error` | bool, string? | Server verdict |
| `rtt_ms` | f64 | Send to reducer result |

### `end`: once, when the match ends

`my_bot_id`, `auction_type`, `n_players`, `place` (1 = best, ties share),
`our_score`, `our_balance`, `winner_score`, `letters_won`, `spent`,
`words_played`, `auctions_seen`, `resumed`, `tournament_id`, and
`participants: [{bot_id, name, score, balance}]` for every seat.

## `summary.jsonl`

One line per finished match:
`{v, ts, mode, match_id, my_bot_id, n_players, auction_type, our_score, our_balance, place, winner_score, scores[], letters_won, spent, words_played, resumed, tournament_id}`.
`scores` is every participant's final score, highest first. `ts` is Unix ms.

## `heartbeat`

A single JSON object, replaced atomically every 10 s:
`{ts_ms, pid, mode, state, bot_id, matches_live[], matches_finished, last_event_age_ms, uptime_s, reconnects}`.
`state` is `connected`, `connecting`, `disconnected` or `stopped`. The Docker
healthcheck treats a heartbeat older than 60 s as unhealthy.
