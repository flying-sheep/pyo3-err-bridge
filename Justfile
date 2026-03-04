build:
    RUST_BACKTRACE=1 cargo hack --feature-powerset build

check:
    RUST_BACKTRACE=1 cargo hack --feature-powerset clippy

test:
    RUST_BACKTRACE=1 cargo hack --feature-powerset test