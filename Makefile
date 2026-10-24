ARENA := ./target/release/vickrey-arena
BOT   := ./target/release/vickrey

.PHONY: build test lint bench duel firstprice tournament refit evolve run status ci

build:            ## Release build of everything
	cargo build --release --workspace

test:             ## Full test suite
	cargo test --workspace --release

lint:             ## fmt + clippy, as CI runs them
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

bench: build      ## Vickrey vs the calibrated live field
	$(ARENA) bench --lineup vickrey,shark,shark,grinder,spender,nibbler -n 200

duel: build       ## Heads-up, like the tournament final
	$(ARENA) bench --lineup vickrey,shark -n 40

firstprice: build ## First-price table
	$(ARENA) --first-price bench --lineup vickrey,shark,shark,grinder,spender,nibbler -n 100

tournament: build ## Whole simulated tournaments
	$(ARENA) tournament --entrants vickrey,shark,shark,shark,grinder,grinder,spender,nibbler,starter,shark,grinder,spender -n 10

refit: build      ## Refit the rivals' price model from live public data
	$(ARENA) refit

evolve: build     ## Refit + tune both profiles (writes only improvements)
	$(ARENA) evolve

run: build        ## Play forever on the live server
	$(BOT) run

status: build     ## Bot, rating, lobby and tournament status
	$(BOT) status

ci: lint test     ## Local CI
	$(ARENA) bench --lineup vickrey,shark,shark,grinder,spender,nibbler -n 36 --seed 7 --max-place 2.0
