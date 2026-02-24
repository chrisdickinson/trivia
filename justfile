set positional-arguments

export CLAUDE_PLUGIN_ROOT := justfile_directory()

_help:
	just -l

build:
	cargo build --release

run:
	cargo run -p trivia-mcp

test:
	cargo test --workspace

www:
	cargo run --bin trivia -- www

vite:
	npm run dev --prefix apps/cli/www

#[parallel]
dev: \
		www \
		vite

[no-cd]
claude *args:
	claude --plugin-dir "{{ justfile_directory() }}" "$@"
