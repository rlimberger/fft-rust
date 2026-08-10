//! DBN → fftlog v2 ingest: canonical decode of Databento MBO data with sequence-gap
//! synthesis, America/Chicago trade-date bucketing, and fftlog v2 write via
//! [`write::write_fftlog`].
//!
//! Instrument tick fields (`min_price_increment`, `unit_of_measure_qty`, `display_factor`)
//! are **not** on the mbo-only batch. [`decode::instrument_meta`] fails loudly;
//! `fft-ingest write` requires explicit `--tick` / `--uom-qty` / `--display-factor`
//! (option A — ES reference values appear in help only, never assumed).

#![forbid(unsafe_code)]

pub mod decode;
pub mod session;
pub mod write;
