# FFT — Product Requirements Document

**Working title:** FFT  
**Category:** Native futures market-analysis and order-flow workstation  
**Platform:** Cross-Platform (Linux / macOS / Windows)  
**Stack:** Rust (Ed. 2024) · GPUI · Databento Binary Encoding (DBN) · Tokio  
**Doc version:** 2.1  
**Author / primary user:** René Limberger  
**Status:** Production Blueprint  

---

## Clarification: Rust Edition 2024

Rust releases a new major **Edition** once every 3 years (2015, 2018, 2021, 2024).

**Rust 2024 is the latest stable edition** (stabilized in compiler version 1.85). It represents the modern standard for the Rust language, introducing essential capabilities used heavily in this project:
* Native `async fn` in traits (without needing the `async-trait` macro overhead).
* Native `gen` blocks and async generators (ideal for high-frequency data streaming).
* `let-chains` for cleaner, allocation-free pattern matching in the MBO parser.

---

## 1. Executive Summary & Thesis

Market Profile tools and DOM tools are two halves of the same idea, historically separated across distinct windows. 

Market Profile identifies *where* value sits and *where* auctions remain unfinished. The DOM reveals *what is happening at the touch right now*. **The price axis is the join key.** A DOM ladder and a Market Profile are functions over the same discrete tick ladder. By sharing one unified price axis, one GPU renderer, and one nanosecond-accurate event log, structural context is directly visible on the row an operator is about to click.

FFT couples **CME Market-by-Order (MBO/L3)** data from Databento with **GPUI**—a retained-mode 2D GPU rendering engine built for locked 120 FPS performance. Inference (pulling/stacking heuristics) is replaced with direct observation: exact queue position, real cancellation attribution, and deterministic iceberg refresh detection.

> **One sentence:** A single GPU-rendered price ladder where the depth of market, reconstructed tape, day’s profile, and composite structure are four viewports onto one nanosecond-accurate event stream.

---

## 2. Core UX Principles

* **Platform Neutrality:** Zero platform-specific code paths or conditional compilations (`cfg(target_os)`). Builds, runs, and renders identically on Linux, macOS, and Windows.
* **The Row is the Unit of Meaning:** A single horizontal price level displays historical value, current resting liquidity, and order-flow dynamics concurrently.
* **Density with Hierarchy:** High-density financial data prioritized visually so the inside market (the touch) remains instantaneous to locate.
* **Pure Reactive Snapshots:** The UI thread never mutates market state. The rendering pipeline reads an immutable `RenderSnapshot` from a lock-free atomic buffer at every display vsync.
* **Scrubbable Time:** Time is a viewport parameter, not an application state. Pinned to the head of the log, the UI is live; dragged backward, it rewinds every component (DOM, Tape, Profile) in lockstep.

---

## 3. System Architecture & Thread Topology