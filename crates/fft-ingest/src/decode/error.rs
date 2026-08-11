//! Ingest failure types. Every variant carries enough raw content to reproduce the record.

use std::fmt;

/// Ingest failure. Every variant carries enough raw content to reproduce the record.
#[derive(Debug)]
pub enum IngestError {
    Dbn(dbn::Error),
    /// DBN action with no canonical carrier (`'N'` None, or an unknown byte).
    UnmappableAction {
        action: u8,
        record: String,
    },
    /// DBN side byte outside `'B'`/`'A'`/`'N'`.
    UnmappableSide {
        side: u8,
        record: String,
    },
    /// A record in the stream that is not an `MboMsg`.
    UnexpectedRecordType {
        record: String,
    },
    /// `UNDEF_PRICE`/`UNDEF_ORDER_SIZE` on a record kind where the field is meaningful.
    UndefinedField {
        field: &'static str,
        record: String,
    },
    /// Instrument tick metadata is not on disk — see [`super::instrument_meta`].
    MissingDefinition {
        dataset: String,
        schema: String,
    },
    /// fftlog v2 write/read failure.
    Log(fft_log::LogError),
    /// CLI / write-config misuse (missing required flags, bad values).
    Cli(String),
    /// Instrument id has no symbology mapping and `--symbol` was not supplied.
    UnknownInstrument {
        instrument_id: u32,
        hint: String,
    },
    /// Filter matched zero events — refuse to write an empty log.
    NoEventsWritten {
        instrument_id: u32,
        trade_date: String,
    },
}

impl fmt::Display for IngestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IngestError::Dbn(err) => write!(f, "DBN decode error: {err}"),
            IngestError::UnmappableAction { action, record } => write!(
                f,
                "unmappable DBN action {:?} (0x{action:02x}) — no canonical event kind; \
                 raw record: {record}",
                char::from(*action),
            ),
            IngestError::UnmappableSide { side, record } => write!(
                f,
                "unmappable DBN side {:?} (0x{side:02x}); raw record: {record}",
                char::from(*side),
            ),
            IngestError::UnexpectedRecordType { record } => write!(
                f,
                "unexpected non-MBO record in mbo stream; raw record: {record}"
            ),
            IngestError::UndefinedField { field, record } => write!(
                f,
                "undefined {field} on a record where it is meaningful; raw record: {record}"
            ),
            IngestError::MissingDefinition { dataset, schema } => write!(
                f,
                "cannot build InstrumentMeta: dataset {dataset} on disk carries schema \
                 `{schema}` only; `min_price_increment` and `unit_of_measure_qty` come from \
                 the Databento `definition` schema (InstrumentDefMsg), which batch job \
                 GLBX-20260803-4WJS899FNL did not include. Acquire the definition schema \
                 for the week before writing fftlog headers — do not fabricate tick metadata. \
                 For `fft-ingest write`, pass --tick/--uom-qty/--display-factor explicitly."
            ),
            IngestError::Log(err) => write!(f, "fftlog error: {err}"),
            IngestError::Cli(msg) => write!(f, "{msg}"),
            IngestError::UnknownInstrument {
                instrument_id,
                hint,
            } => write!(f, "unknown instrument_id {instrument_id}: {hint}"),
            IngestError::NoEventsWritten {
                instrument_id,
                trade_date,
            } => write!(
                f,
                "no events for instrument_id {instrument_id} on trade date {trade_date}; \
                 refusing to write an empty fftlog"
            ),
        }
    }
}

impl std::error::Error for IngestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IngestError::Dbn(err) => Some(err),
            IngestError::Log(err) => Some(err),
            _ => None,
        }
    }
}

impl From<dbn::Error> for IngestError {
    fn from(err: dbn::Error) -> Self {
        IngestError::Dbn(err)
    }
}

impl From<fft_log::LogError> for IngestError {
    fn from(err: fft_log::LogError) -> Self {
        IngestError::Log(err)
    }
}
