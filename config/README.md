# config/

Strategy state the live bot hot-reloads at the start of every match.

| File | Written by | Used for |
|---|---|---|
| `params.json` | `vickrey-arena tune --profile multi` / `evolve` / A/B promotion | the champion, at tables of 3+ bots (lobby, Swiss, bracket) |
| `params_duel.json` | `vickrey-arena tune --profile duel` | heads-up tables (the three-game final) |
| `field.json` | `vickrey-arena refit` / `evolve` | the price model of what rivals bid, fitted to live history |
| `ab.json` | you | optional live A/B test: `{"challenger": "params_b.json", "share": 0.5, "name": "..."}` |
| `params_b.json` | you | the challenger's params (casual matches only; tournaments always use `params.json`) |
| `params.prev.json` | the bot | the previous champion, kept when a challenger is promoted |
| `ab-queue.json` | you, then the bot | the experiment plan: `[{"name": "...", "patch": {...}}]`, popped as tests start |

Missing files fall back to the defaults compiled into `vickrey-brain`.

## Live A/B testing

With `ab.json` present, the bot hashes each casual match id to an arm (so a restart mid-match keeps the arm), plays it with the champion or the challenger, and logs the arm in telemetry. After every match it recomputes the scoreboard on Swiss points (`n − place`, what the tournament rewards). Once both arms have 40+ matches and Welch's |t| ≥ 2, it promotes the challenger to `params.json`, or retires it, and removes `ab.json`. Every decision goes to `runs/ab-history.jsonl`. Check progress with `vickrey ab-report`.

The bot rewrites `ab.json`, `params_b.json`, `params.prev.json` and `ab-queue.json` as experiments start, finish and get promoted, so they are git-ignored. Results are appended to `runs/ab-history.jsonl`. A test stops early when the evidence is clear: retired after 15 matches per arm at t ≤ −2.5, promoted after 25 at t ≥ 3.

## Running heavy jobs next to the live bot

Builds, simulations and tuning share the Mac with the bot, whose bids must land inside a one-second window. Run anything heavy through `ops/heavy.sh <command>`: it takes a lock so only one heavy job runs at a time, limits it to two threads, and gives it the lowest scheduling priority. CI and the evolve timer already follow the same limits.
