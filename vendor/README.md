# Vendored crates

- `dimpl` 0.7.4 (MIT or Apache-2.0), the DTLS stack of str0m. Its `rcgen` feature, which str0m's
  `rust-crypto` backend needs for its DTLS certificate, also turns on aws-lc: C that needs a C library
  for the target, so a static Linux binary would no longer cross-compile with clang alone. Changed
  here: `rcgen` no longer turns on `aws-lc-rs` and uses rcgen's `ring` backend (in `Cargo.toml`), and
  `src/certificate.rs` takes its SHA-256 from sha2. The tests are left out. Drop this copy once dimpl
  lets `rcgen` go without aws-lc.
