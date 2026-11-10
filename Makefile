.PHONY: check

# Single check command: format, types, tests and forbidden dependencies.
# Each step runs only once the thing it checks exists.
check:
	@if [ -f Cargo.toml ]; then cargo fmt --all --check && cargo test --workspace --locked; else echo "check: no Cargo workspace yet"; fi
	@if [ -d bench ]; then python3 -m unittest discover -s bench -p 'test_*.py'; else echo "check: no bench yet"; fi
	@if [ -f bench/deps.py ]; then python3 bench/deps.py forbidden; fi
