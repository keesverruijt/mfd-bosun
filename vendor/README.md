# Vendored crates

- `dimpl` 0.7.4 (MIT or Apache-2.0), the DTLS stack of str0m. Its `rcgen` feature, which str0m's
  `rust-crypto` backend needs for its DTLS certificate, also turns on aws-lc: C that needs a C library
  for the target, so a static Linux binary would no longer cross-compile with clang alone. Changed
  here: `rcgen` no longer turns on `aws-lc-rs` and uses rcgen's `ring` backend (in `Cargo.toml`), and
  `src/certificate.rs` takes its SHA-256 from sha2. The tests are left out. Drop this copy once dimpl
  lets `rcgen` go without aws-lc.
- `h264-reader` 0.8.0 (MIT or Apache-2.0), which retina parses every SPS with, and fails the stream
  when it rejects one. A NOS unit (screen mirror from GStreamer) sends an SPS whose VUI has
  `max_num_reorder_frames` larger than `max_dec_frame_buffering`, which the spec forbids, so no frame
  came through. Changed here: `src/nal/sps.rs` no longer checks `max_num_reorder_frames` and
  `max_dec_frame_buffering` against each other or against `max_num_ref_frames` (hints for a decoder;
  browsers take them as they come). The tests, examples and benches are left out. Drop this copy
  once h264-reader accepts such an SPS.
