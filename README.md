# `flip-lld`

> Flips the memory layout of a program to add zero cost stack overflow
> protection

For more details see [this blog
post](https://blog.japaric.io/stack-overflow-protection/). 

This is more generic re-implementation of the discontinued [`cortex-m-rt-ld`]
linker wrapper. It uses the LLD binary shipped with the Rust toolchain instead
of the system linker. The goal of this new implementation is to support all kind
of runtime crates, not just the ARM Cortex-M one (`cortex-m-rt`). 

[`cortex-m-rt-ld`]: https://github.com/japaric/cortex-m-rt-ld

# License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)

- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
