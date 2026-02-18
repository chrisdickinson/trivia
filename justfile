set positional-arguments

_help:
	just -l

build:
	cargo build --release

run:
	cargo run -p trivia-mcp

test:
	cargo test --workspace

[no-cd]
claude *args:
	claude --plugin-dir "{{ justfile_directory() }}" "$@"
