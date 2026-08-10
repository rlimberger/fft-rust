# fftlog v2 — Wire Format (M0 freeze 1)

**Status: FROZEN.** Changes require an orchestrator-approved spec edit and a version bump,
in the same commit as any implementation change. `fft-log` owns framing and primitive
codecs only; book/profile/engine crates encode their checkpoint sections *through* the
primitive writers defined here — `fft-log` never depends on their internals.

## 1. Conventions

- All integers **little-endian**, fixed width. No varints.
- Timestamps: `u64` nanoseconds since Unix epoch, UTC (Databento `ts_event` basis).
  Trade-date semantics stay in `America/Chicago`; the wire carries only UTC ns.
- Prices: `i64` fixed-point, 1e-9 units (Databento native scale).
- Checksums: **xxh3-64** (integrity, not authentication) — fast enough to checksum
  everything, which we do.
- File layout: `Header · Frame* · Footer?` (footer present only after clean close).

## 2. Header

| Field | Type | Value |
|---|---|---|
| magic | `[u8; 8]` | `"FFTLOG2\0"` |
| version_major | `u16` | 2 |
| version_minor | `u16` | 0 |
| flags | `u32` | bit 0 = `LIVE` (set while a writer is appending; cleared on clean close) |
| meta_len | `u32` | length of metadata block |
| metadata | bytes | see below |
| header_xxh3 | `u64` | over all preceding header bytes |

Metadata block (fixed order, no TLV): raw contract symbol (u16-LE-len-prefixed UTF-8,
e.g. `ESU6`), dataset id (same prefix), `instrument_id: u32`, `min_price_increment: i64`
(1e-9), `unit_of_measure_qty: i64` (1e-9), display factor, CT trade date (`u32` days since
epoch), Globex session-open ts, source schema tag (the constant `mbo` in v2.0).

Clean close clears `LIVE` by rewriting the header with a recomputed checksum — the
format's **only** in-place write; frame bytes are never overwritten.

**Compatibility policy:** a reader rejects any file whose `version_major` ≠ 2, loudly.
Minor bumps are additive only: they may append new metadata fields (readers stop at
`meta_len`) and new OPTIONAL checkpoint sections (§5). Unknown REQUIRED content is a loud
error, never skipped.

## 3. Frames

Every frame starts with a fixed 64-byte header, **uncompressed**, so a reader can walk the
frame chain without decompressing anything:

| Field | Type | Notes |
|---|---|---|
| kind | `u8` | 1 = EVENTS, 2 = CHECKPOINT |
| reserved | `[u8; 3]` | zero |
| count | `u32` | events in frame (EVENTS) or sections (CHECKPOINT) |
| compressed_len | `u32` | ≤ **16 MiB** |
| uncompressed_len | `u32` | ≤ **64 MiB** |
| first_ts / last_ts | `u64` × 2 | event-time bounds |
| first_seq / last_seq | `u64` × 2 | source-sequence bounds |
| payload_xxh3 | `u64` | over the compressed payload |
| header_xxh3 | `u64` | over the preceding 56 bytes |

Payloads are zstd-compressed. Both length limits are validated **before** any allocation;
a decoder never allocates more than the declared `uncompressed_len`, and never more than
the ceiling, regardless of input (fuzz target in M7).

## 4. Canonical event schema (EVENTS payload)

Fixed 32-byte records after decompression:

| Field | Type | Notes |
|---|---|---|
| kind | `u8` | 1 Add · 2 Cancel · 3 Modify · 4 Trade · 5 Fill · 6 Clear · 7 Status · 8 **Gap** · 9 TsReset |
| side | `u8` | 0 none · 1 bid · 2 ask |
| flags | `u16` | source flags (e.g. end-of-event grouping) |
| size | `u32` | contracts |
| ts_delta | `u32` | ns since frame `first_ts`; on overflow a TsReset record carries a full `u64` basis in `price`+`order_id` |
| seq | `u32` | source sequence (Databento MBO channel sequence) |
| price | `i64` | 1e-9 fixed-point |
| order_id | `u64` | CME order id — the native-refresh key |

- **Delta reset rule:** deltas are relative per frame (`first_ts` is the basis); every
  frame is independently decodable given its header. A TsReset record (wire kind 9) is
  emitted whenever the delta overflows `u32` **or steps backwards**; it carries the new
  full `u64` basis duplicated in both `price` and `order_id` (decoder cross-checks the
  copies; all other fields must be zero). The frame header's `count` includes TsReset
  records, so `uncompressed_len == count × 32` is a validation invariant; `last_ts` is
  the maximum event ts in the frame.
- **Gap records** are first-class events: `price` = expected seq, `order_id` = observed
  seq. Downstream consumers (book, native-refresh classifier) must transition to their
  gap states; classification across a gap reads *unavailable*, never false (PRD §4.4).
- **Batch gap policy (frozen 2026-08-10):** Databento batch files are symbol-filtered
  (the sample job is `ES.FUT` parent), so **forward channel-seq holes are expected
  filtering artifacts, never gaps** — completeness authority for batch data is the
  job's `condition.json` (`available` = complete capture; the sample week is available
  on all five days). Batch ingest therefore synthesizes a Gap **only on a sequence
  regression** in the decoded stream (a genuine anomaly), never on a forward jump;
  ignored forward holes are counted and reported loudly in `WriteStats`
  (`seq_holes_ignored`), not silently dropped from accounting. Live ingestion (M6)
  detects real gaps at the gateway/session layer, where the full channel stream is
  visible; Gap records stay first-class in the format for that path.
- Status records carry the `status`-schema code in `size`.
- **Snapshot records** (source SNAPSHOT flag set in `flags`; frozen 2026-08-10, evidence
  in HANDOFF): Databento daily files roll at 00:00 UTC (19:00 CT) and open with a
  resting-book snapshot whose records carry **original order-entry timestamps and
  non-channel sequence numbers**. Therefore: (1) *ingest admission* — for target trade
  date D, a file's snapshot block is admitted iff the file's first non-snapshot event
  buckets to D (the stale prior-day block is dropped; for a Wed log that admits only the
  19:00 CT Tue block, 2 h after open); (2) snapshot records **bypass the gap detector**
  and carry their source seq verbatim; (3) *replay semantics* — a consumer applies a
  snapshot-flagged record as a **snapshot-load**, never a live Add: no sequence
  accounting; unknown `order_id` → insert **ahead of** every live-added order at that
  level, in block order (every snapshot order predates every observed live add by
  construction — an order entered during the observed window was already seen live and
  is a *known* id); known `order_id` → verify side/price/size, loud mismatch. Book state
  before the admitted snapshot block is partial-but-truthful: unknown-ref activity is
  counted loudly and iceberg/queue classification reads *unavailable* until observed
  (PRD §4.3–4.4).

## 5. Checkpoints

Written every **60 s wall-clock** as CHECKPOINT frames. A checkpoint restores **complete
seek-relevant state**: restore + tail-replay must be bit-identical to forward replay from
open (M2 gate). No synthetic events, ever. A checkpoint frame's ts/seq header bounds are
stamped with the last appended event's ts/seq (zero before any event) so the index can
seek checkpoints in event time.

Payload = sections in **ascending section-id order**:

| Field | Type | Notes |
|---|---|---|
| section_id | `u16` | 1 BOOK · 2 FLOW · 3 PROFILE · 4 CVD · 5 REFRESH (native-refresh state) · 6 SESSION |
| section_version | `u16` | per-section, owned by the encoding crate |
| flags | `u16` | bit 0 = OPTIONAL (reader may skip if unknown) |
| reserved | `u16` | zero |
| len | `u32` | section byte length |
| section_xxh3 | `u64` | over section bytes |

**Deterministic queue serialization (non-negotiable):** BOOK serializes each side in
price order (bids descending, asks ascending) and, within a level, orders **strictly
head-to-tail FIFO** with `order_id`, remaining size, and refresh linkage. Serialization
by hash-map iteration is forbidden. Restore comparisons are order-exact: ids, side/price,
remaining qty, FIFO traversal, contracts/orders ahead, refresh and flow-window state.

## 6. Footer and index

On clean close the writer appends: index entries `{offset: u64, kind: u8, first_ts: u64,
first_seq: u64}` for every frame, then a trailer `{index_len: u32, index_xxh3: u64,
magic: "FFTIDX2\0"}`, and clears the header `LIVE` flag. A file without a valid footer is
readable: the reader rebuilds the index by walking the uncompressed frame headers (§3).
Index corruption with an intact frame chain = rebuild **with a visible warning**; never a
silent full-file rescan on the UI thread.

## 7. Append-commit protocol and concurrent readers

- Single writer, append-only. A frame is **committed** iff its `header_xxh3` and
  `payload_xxh3` both validate. The writer writes header + payload, then flushes; it never
  overwrites committed bytes.
- Concurrent readers (live tailing, same host) walk the frame chain and trust only
  validating frames. A non-validating tail on a `LIVE` file means *not yet committed* —
  retry later. The same condition on a closed (non-`LIVE`) file is corruption: loud error.
- A checksum-**valid** frame header with illegal contents (limit breach, unknown kind,
  non-zero reserved bytes) cannot be a torn write: it is hard corruption even in a `LIVE`
  tail.
- Index rebuild (§6) is legal only when the frame chain is provably intact — a valid
  trailer delimiting the region, or no footer at all with the header walk consuming
  exactly to EOF. Anything else is a loud corruption error. On a closed file with a valid
  footer, per-frame payload validation is lazy (at frame access) — the index exists to
  avoid full-file scans at open.

## 8. Crash recovery (built in M1, not M7)

Opening a file with `LIVE` set implies an unclean shutdown: scan to the last committed
frame, logically truncate, and report — loudly — the dropped byte count and the last good
`ts`/`seq`. Recovery is deterministic and covered by torn-tail fixtures (M1 gate). Silent
truncation or silent fallback paths are forbidden.
