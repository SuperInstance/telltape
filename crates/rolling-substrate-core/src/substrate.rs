//! SubstrateCore — the ring buffer (Pillar 1).
//!
//! ## Email claims kept
//! * Lock-free, multi-producer ring buffer.
//! * Slots aligned to 64 bytes (`#[repr(align(64))]`) so distinct slots
//!   do not share a cache line — the email's alignment rationale is
//!   correct.
//! * Power-of-two capacity so indexing is a mask, not a division.
//! * A lineage header lives in each slot (but fixed — see [`crate::lineage`]).
//!
//! ## Email claims fixed / not claimed
//! * The email code's sequence handling was muddled: its
//!   `lease_slot_for_execution` spun on `sequence_id == current_cursor`
//!   while `finalize_write` stored `write_cursor + capacity` read from
//!   the *global* cursor, not the lease's own claim, so concurrent
//!   producers could publish the wrong sequence. Here the Disruptor
//!   protocol is stated and followed exactly (below).
//! * No claim of being "pinned in unified LPDDR5X" or DMA-accessible:
//!   this is ordinary heap memory. On Thor, unified memory makes CPU
//!   and GPU share physical RAM, but turning a heap allocation into
//!   something a CUDA kernel may dereference needs CUDA allocation
//!   APIs that are not in std and not in this crate.
//!
//! ## Protocol (Disruptor-style)
//! * `capacity` is a power of two; `mask = capacity - 1`.
//! * Slot `i` starts with `sequence = i`.
//! * A producer claims the next global sequence `c` with one
//!   `fetch_add`. It owns slot `c & mask` once that slot's
//!   `sequence == c` (spin-wait; the previous lap must have finished).
//! * While holding the [`Lease`], the producer has exclusive access to
//!   the slot's data — exclusivity is a protocol invariant, which is
//!   why data access goes through `&mut Lease`.
//! * [`Lease::finalize`] publishes by storing `sequence = c + capacity`
//!   with `Release` ordering; a consumer can test publication of claim
//!   `c` with [`FluidSubstrateBuffer::is_published`].
//!
//! Multi-producer safety rests on: (1) each claim `c` is handed out
//! exactly once (atomic `fetch_add`), (2) a slot is only leased for
//! claim `c` after its previous lap (`c - capacity`) was finalized.

use crate::lineage::LineageHeader;
use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, Ordering};

/// One ring slot. 64-byte aligned: the header fields sit at the start
/// of a cache line and the slot never shares its first line with a
/// neighbour (slots are stored in a `Vec<Slot>`, and `align(64)` pads
/// the struct size up to a multiple of 64 — asserted in tests).
#[repr(align(64))]
pub struct Slot {
    sequence: AtomicU64,
    /// Lineage for whatever currently occupies this slot.
    pub lineage: LineageHeader,
    data: UnsafeCell<Box<[f32]>>,
}

// SAFETY: `data` is only accessed through a `Lease`, and the sequence
// protocol guarantees at most one live lease per slot at a time:
// claim values are unique (atomic fetch_add) and a lease for lap k+1
// cannot be granted until lap k's finalize stored the release
// sequence this lease spins on (Acquire). All other fields are atomic.
unsafe impl Sync for Slot {}

impl Slot {
    fn new(index: usize, dimension: usize) -> Self {
        Self {
            sequence: AtomicU64::new(index as u64),
            lineage: LineageHeader::new(),
            data: UnsafeCell::new(vec![0.0f32; dimension].into_boxed_slice()),
        }
    }
}

/// The ring buffer. `Send + Sync`; share it in an `Arc` between
/// producer threads.
pub struct FluidSubstrateBuffer {
    ring: Vec<Slot>,
    capacity: usize,
    mask: usize,
    next_claim: AtomicU64,
    dimension: usize,
}

impl FluidSubstrateBuffer {
    /// `capacity` must be a power of two. `dimension` is the number of
    /// `f32` elements per slot.
    ///
    /// # Panics
    /// Panics if `capacity` is not a power of two or `dimension` is
    /// zero. These are construction-time contract violations (the
    /// caller picks both, typically as constants), not data-dependent
    /// failures: the mask arithmetic below is only sound for a
    /// power-of-two capacity.
    pub fn new(capacity: usize, dimension: usize) -> Self {
        assert!(
            capacity.is_power_of_two(),
            "capacity must be a power of two"
        );
        assert!(dimension > 0, "dimension must be non-zero");
        Self {
            ring: (0..capacity).map(|i| Slot::new(i, dimension)).collect(),
            capacity,
            mask: capacity - 1,
            next_claim: AtomicU64::new(0),
            dimension,
        }
    }

    /// Number of slots in the ring (a power of two, fixed at
    /// construction).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of `f32` elements in each slot's data.
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Claim the next slot, spinning until its previous lap finishes.
    ///
    /// This spin is the one deliberate busy-wait in the crate: it is
    /// the Disruptor claim, expected to be brief because producers
    /// finalize promptly. It is *not* simulated compute.
    pub fn lease(&self) -> Lease<'_> {
        let claim = self.next_claim.fetch_add(1, Ordering::AcqRel);
        let index = (claim as usize) & self.mask;
        let slot = &self.ring[index];
        while slot.sequence.load(Ordering::Acquire) != claim {
            std::hint::spin_loop();
        }
        // New lap, new lineage.
        slot.lineage.clear();
        Lease {
            buffer: self,
            claim,
            index,
        }
    }

    /// True if claim `claim` has been finalized (published) at some
    /// point. Slot sequences only move forward, so this is a
    /// monotonic fact: sequence `== claim` means leased-but-not-yet
    /// (or ready-to-lease), anything greater means the finalize for
    /// `claim` has happened (a later lap may since have reused the
    /// slot — publication is a fact about the claim, not a guarantee
    /// that its data is still in the slot; see `read_published`).
    ///
    /// Era caveat: claim and sequence arithmetic is modulo 2^64 (see
    /// [`Lease::finalize`]), so the `>` comparison is only meaningful
    /// for claims of the *current* era. After the claim counter wraps
    /// past 2^64 total leases, a pre-wrap claim reads as unpublished
    /// even though it was published. At a sustained (and unrealistic)
    /// 10^9 leases/second that boundary is ~585 years out; the wrap
    /// behaviour itself is pinned by
    /// `tests::claim_counter_wrap_is_modulo_2_to_the_64`.
    pub fn is_published(&self, claim: u64) -> bool {
        let index = (claim as usize) & self.mask;
        self.ring[index].sequence.load(Ordering::Acquire) > claim
    }

    /// Read a published slot's data by claim.
    ///
    /// # Safety
    /// Caller must ensure the claim is published
    /// ([`Self::is_published`]) and that no lease for the *next* lap of
    /// the same slot is concurrently writing. The returned slice
    /// borrows the buffer; the protocol, not the type system, is what
    /// keeps that sound while other laps proceed on other slots — the
    /// same honesty standard the rest of this crate applies to unsafe.
    pub unsafe fn read_published(&self, claim: u64) -> &[f32] {
        let index = (claim as usize) & self.mask;
        // SAFETY: the caller contract above — the claim is published
        // and no next-lap lease is writing this slot — is exactly the
        // condition under which forming a shared reference to the
        // slot's `UnsafeCell` contents is sound.
        &*self.ring[index].data.get()
    }
}

/// Exclusive ownership of one slot for one lap. Obtained from
/// [`FluidSubstrateBuffer::lease`]; publish with [`Lease::finalize`].
pub struct Lease<'a> {
    buffer: &'a FluidSubstrateBuffer,
    claim: u64,
    index: usize,
}

impl<'a> Lease<'a> {
    /// The global sequence this lease claimed.
    pub fn claim(&self) -> u64 {
        self.claim
    }

    /// The ring slot this lease owns (`claim & (capacity - 1)`).
    pub fn index(&self) -> usize {
        self.index
    }

    /// The slot's data. `&mut self` is what makes this safe: a lease
    /// is a unique token, so no other `&mut [f32]` to this slot can
    /// be minted while this one lives.
    pub fn data_mut(&mut self) -> &mut [f32] {
        // SAFETY: lease exclusivity per the protocol in the module
        // docs — this slot's sequence equalled our claim when leased,
        // and only `finalize` (which consumes the lease) advances it.
        unsafe { &mut *self.buffer.ring[self.index].data.get() }
    }

    /// Record a lineage visit in this slot's header.
    pub fn record_visit(&self, node: usize) {
        self.buffer.ring[self.index].lineage.record(node);
    }

    /// The slot's lineage header (cleared when the lease was taken).
    pub fn lineage(&self) -> &LineageHeader {
        &self.buffer.ring[self.index].lineage
    }

    /// Publish the slot and end the lease.
    pub fn finalize(self) {
        let slot = &self.buffer.ring[self.index];
        // `wrapping_add`, deliberately: claim/sequence arithmetic is
        // modulo 2^64 by design. `capacity` is a power of two, so it
        // divides 2^64 and slot selection (`claim & mask`) and the
        // sequence equality the next lap spins on stay consistent
        // across the wrap. Plain `+` would panic on overflow in debug
        // builds at the era boundary while wrapping silently in
        // release — the protocol must not depend on the build profile.
        // Pinned by `tests::claim_counter_wrap_is_modulo_2_to_the_64`.
        slot.sequence.store(
            self.claim.wrapping_add(self.buffer.capacity as u64),
            Ordering::Release,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn slots_are_64_byte_aligned_and_padded() {
        assert_eq!(std::mem::align_of::<Slot>(), 64);
        assert_eq!(std::mem::size_of::<Slot>() % 64, 0);
    }

    #[test]
    #[should_panic]
    fn capacity_must_be_power_of_two() {
        let _ = FluidSubstrateBuffer::new(3, 8);
    }

    #[test]
    fn lease_write_finalize_read_back() {
        let buf = FluidSubstrateBuffer::new(4, 8);
        let mut lease = buf.lease();
        assert_eq!(lease.claim(), 0);
        lease.data_mut().fill(2.5);
        lease.record_visit(3);
        lease.finalize();
        assert!(buf.is_published(0));
        // SAFETY: claim 0 published, no next-lap lease exists yet.
        let data = unsafe { buf.read_published(0) };
        assert!(data.iter().all(|&v| v == 2.5));
    }

    #[test]
    fn ring_wraps_and_reuses_slots() {
        let buf = FluidSubstrateBuffer::new(4, 4);
        for lap in 0..12u64 {
            let mut lease = buf.lease();
            assert_eq!(lease.claim(), lap);
            assert_eq!(lease.index(), (lap as usize) & 3);
            lease.data_mut().fill(lap as f32);
            lease.finalize();
            assert!(buf.is_published(lap));
        }
    }

    #[test]
    fn lineage_is_cleared_on_reuse() {
        let buf = FluidSubstrateBuffer::new(2, 4);
        let l = buf.lease();
        l.record_visit(5);
        l.finalize();
        let l = buf.lease();
        l.finalize();
        // Third lease reuses slot 0; its lineage must be fresh.
        let l = buf.lease();
        assert_eq!(l.index(), 0);
        assert!(!l.lineage().visited(5));
        l.finalize();
    }

    #[test]
    fn claim_counter_wrap_is_modulo_2_to_the_64() {
        // Boundary behaviour at the claim counter's u64 wrap, seeded
        // directly (2^64 real leases are not a test plan). Protocol
        // claim, verified here: lease/finalize stay consistent across
        // the wrap because capacity (4) divides 2^64 — every slot's
        // next expected sequence is exactly the claim that leases it,
        // on both sides of the boundary. `is_published`, however, is
        // era-local (its `>` comparison cannot see across the wrap) —
        // that limitation is pinned below, not hidden.
        use std::sync::atomic::Ordering as O;
        let buf = FluidSubstrateBuffer::new(4, 2);
        // Claims to come: MAX-1 -> slot 2, MAX -> slot 3, 0 -> slot 0,
        // 1 -> slot 1. Pre-set each slot's sequence to the claim that
        // will lease it, as the previous lap's finalize would have.
        buf.ring[2].sequence.store(u64::MAX - 1, O::SeqCst);
        buf.ring[3].sequence.store(u64::MAX, O::SeqCst);
        // Slots 0 and 1 already hold sequences 0 and 1 from `new`.
        buf.next_claim.store(u64::MAX - 1, O::SeqCst);

        let expected_claims = [u64::MAX - 1, u64::MAX, 0, 1];
        for (k, &want) in expected_claims.iter().enumerate() {
            let mut lease = buf.lease();
            assert_eq!(lease.claim(), want, "claim {k} across the wrap");
            lease.data_mut().fill(want as f32);
            lease.finalize();
        }
        // Current-era claims read as published, and their data is
        // intact after the wrap.
        assert!(buf.is_published(0));
        assert!(buf.is_published(1));
        // SAFETY: claims 0 and 1 are published; their slots' next laps
        // (claims 4 and 5) have not been leased.
        assert!(unsafe { buf.read_published(0) }.iter().all(|&v| v == 0.0));
        assert!(unsafe { buf.read_published(1) }.iter().all(|&v| v == 1.0));
        // Pinned limitation: the pre-wrap claim WAS finalized, but
        // `is_published` reports false for it in the new era (slot 3's
        // sequence wrapped to 3, and 3 > u64::MAX is false). Callers
        // must not query publication across an era boundary — see the
        // `is_published` docs.
        assert!(!buf.is_published(u64::MAX));
    }

    #[test]
    fn multi_producer_claims_are_unique_and_all_publish() {
        let buf = Arc::new(FluidSubstrateBuffer::new(8, 16));
        let total = 200u64;
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let b = Arc::clone(&buf);
                std::thread::spawn(move || {
                    let mut claims = Vec::new();
                    for _ in 0..50 {
                        let mut lease = b.lease();
                        let c = lease.claim();
                        lease.data_mut().fill(c as f32);
                        lease.finalize();
                        claims.push(c);
                    }
                    claims
                })
            })
            .collect();
        let mut all: Vec<u64> = threads
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();
        all.sort_unstable();
        assert_eq!(all, (0..total).collect::<Vec<_>>());
        for c in 0..total {
            assert!(buf.is_published(c), "claim {c} not published");
        }
    }
}
