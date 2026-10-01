# Vendored crates

- `h264-reader` 0.8.0 (MIT or Apache-2.0), which retina parses every SPS with, and fails the stream
  when it rejects one. A NOS unit (screen mirror from GStreamer) sends an SPS whose VUI has
  `max_num_reorder_frames` larger than `max_dec_frame_buffering`, which the spec forbids, so no frame
  came through. Changed here: `src/nal/sps.rs` no longer checks `max_num_reorder_frames` and
  `max_dec_frame_buffering` against each other or against `max_num_ref_frames` (hints for a decoder;
  browsers take them as they come). The tests, examples and benches are left out. Drop this copy
  once h264-reader accepts such an SPS (https://github.com/dholroyd/h264-reader/pull/164).
