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

# Build the image from dist/<arch>/trivia and smoke-test it
docker-test arch=`docker version -f '{{.Server.Arch}}'`:
	#!/usr/bin/env bash
	set -euo pipefail
	arch="$1"
	bin="dist/$arch/trivia"
	[[ -x "$bin" ]] || { echo "missing $bin (a linux/$arch release build)" >&2; exit 1; }

	tag="trivia:test-$arch"
	name="trivia-test-$arch-$$"
	trap 'docker rm -f "$name" >/dev/null 2>&1 || true; docker volume rm "$name" >/dev/null 2>&1 || true' EXIT
	fail() { echo "FAIL: $*" >&2; exit 1; }
	run() { docker run --rm --platform "linux/$arch" -v "$name:/data" "$tag" "$@"; }

	docker build --platform "linux/$arch" -t "$tag" .

	user=$(docker image inspect -f '{{{{.Config.User}}' "$tag")
	[[ -n "$user" && "$user" != root && "$user" != 0 ]] || fail "image runs as root"

	run --help >/dev/null || fail "binary does not start (missing libs or glibc too old?)"

	run memorize docker-smoke-test "the image can embed and write" -t smoke >/dev/null \
		|| fail "memorize failed (volume not writable by $user?)"
	run recall "can the image embed" --json | grep -q docker-smoke-test || fail "recall missed memory"

	docker run -d --name "$name" --platform "linux/$arch" -v "$name:/data" -p 127.0.0.1::3000 "$tag" >/dev/null
	url="http://$(docker port "$name" 3000/tcp | head -1)"
	curl -fsS --retry 20 --retry-delay 1 --retry-all-errors "$url/api/memories" | grep -q docker-smoke-test \
		|| { docker logs "$name" >&2; fail "www did not serve memories"; }
	curl -fsS "$url/" | grep -qi '<html' || fail "www did not serve the web UI"

	echo "ok: $tag"

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
