# marigold-pool

A mining pool for [Marigold](https://github.com/marigoldcash/marigold), and for Kaspa from the same code. ASICs and CPU wallets connect over stratum, every accepted share is counted, and when the pool's address receives a block reward it is split across the miners whose shares fall in the recent window, then paid out in batches. One program, one database file, one process beside the node.

It is being built step by step and is not runnable yet: today the binary only prints its version. To build and test it, use `scripts/remote-build.sh`, which runs cargo on the build host and never on your own machine (`TEST=1`, `CHECK=1` and `LINT=1` select tests, a type check, and formatting plus clippy). The pool is built against one pinned commit of the node, set in `Cargo.toml`, and one binary is built per chain: `chain-marigold` is the default, `chain-kaspa` is added in the last step.

The plan is [PLAN.md](PLAN.md): twelve steps, M0 to M11, each with the tests that prove it and the command that verifies it, ticked off as they pass. [CLAUDE.md](CLAUDE.md) tells a coding session how to work through it. Licensed under either of MIT or Apache-2.0, at your option.
