.PHONY: build test check fmt clippy verify run shell release package install-check linux-packages clean

build:
	docker compose run --rm -T mellow-dev cargo build

test:
	docker compose run --rm -T mellow-dev cargo test --all-targets

check:
	docker compose run --rm -T mellow-dev cargo check --all-targets

fmt:
	docker compose run --rm -T mellow-dev cargo fmt --all -- --check

clippy:
	docker compose run --rm -T mellow-dev cargo clippy --all-targets --all-features -- -D warnings

verify:
	./scripts/verify.sh

release:
	docker compose run --rm -T mellow-dev cargo build --locked --release

package:
	docker compose run --rm -T mellow-dev cargo package --locked --allow-dirty

install-check:
	docker compose run --rm -T mellow-dev bash -c 'rm -rf /tmp/mellow-install && cargo install --locked --path . --root /tmp/mellow-install && /tmp/mellow-install/bin/mellow --version'

linux-packages:
	./scripts/package-linux.sh

run:
	docker compose run --rm mellow-dev cargo run -- $(FILE)

shell:
	docker compose run --rm mellow-dev bash

clean:
	docker compose down -v --remove-orphans
