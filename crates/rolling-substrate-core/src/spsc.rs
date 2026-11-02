//! `SpscRing<T>` — the ring DESIGN.md §3.2 actually specifies.
//!
//! The shipped ring in [`crate::substrate`] is a multi-producer
//! Disruptor claim/finalize ring. DESIGN.md §3.2 specifies something
//! different — a per-lane **single-producer / single-consumer** ring
//! with a per-slot sequence protocol and consumer acquire/release —
//! and §0.2 records that SPSC was *chosen over* the Disruptor/Vyukov
//! family for v0.1. This module implements §3.2 as written, so the
//! fork can be settled by measurement (`docs/RING-BENCH.md`) instead
//! of by which code happened to land first.
//!
//! ## Protocol (DESIGN §3.2, followed literally)
//! * `capacity` is a power of two, asserted at construction (F9);
//!   slot `i` starts with `sequence = i` ("free for generation `i`").
//! * Producer, for message `n`: slot `i = n & mask`; it may write iff
//!   `sequence[i] == n`, subject to the backpressure policy (§3.4).
//!   Payload writes are plain (non-atomic) — SPSC plus the
//!   acquire/release pairing below gives the producer exclusive
//!   access to the slot for that generation.
//! * Publish: `sequence[i].store(n + 1, Release)` — "generation `n`
//!   is ready to read". Note this is **not** the shipped ring's
//!   `claim + capacity`: ready and free are distinct states here.
//! * Consumer, expecting message `m`: polls slot `j = m & mask` until
//!   `sequence[j].load(Acquire) == m + 1`, reads the payload, then
//!   releases with `sequence[j].store(m + capacity, Release)` —
//!   "free for generation `m + capacity`", exactly the value that
//!   generation's producer will claim against.
//! * Only the `sequence` word synchronises. Counters are `Relaxed`
//!   and never load-bearing, as §3.2 permits for metrics.
//!
//! ## Policies (DESIGN §3.4, chosen at construction)
//! * [`Policy::Block`] — producer spins with backoff until the slot
//!   is free; time spent waiting accumulates into `blocked_ns`.
//! * [`Policy::DropNewest`] — producer abandons the message (handing
//!   it back in [`SendError`]), advances its counter past it, and
//!   counts `dropped_newest`. The consumer detects the gap and skips.
//! * [`Policy::DropOldest`] — producer reclaims a slot whose previous
//!   generation is still unread, destroying that payload; the
//!   consumer detects the overwrite from the slot sequence, skips,
//!   and counts `dropped_oldest`.
//!
//! Across all three, loss is exactly accounted (INV-R4):
//! `produced == consumed + dropped_newest + dropped_oldest +
//! in_flight`, with `in_flight` observable via [`RingStats`].
//!
//! ## Where §3.2/§3.4 under-specify (readings taken — see RING-BENCH.md)
//! * **A1, DropNewest gap mechanics.** §3.4 says the consumer "sees a
//!   sequence gap of known size", but §3.2 gives the consumer no
//!   signal for a drop: a dropped generation is never published, and
//!   it is never *released* either, so without repair the slot's
//!   free-value arithmetic wedges permanently. Readings implemented:
//!   (a) the producer publishes its next counter in a ring-level
//!   `producer_head` atomic (stored after the publish/drop it
//!   describes, `Release`; read `Acquire`) used **only** for gap
//!   detection, never for payload synchronisation; (b) a consumer
//!   skipping a dropped generation performs that generation's
//!   release store (`m + capacity`) — there is nothing to read, and
//!   freeing the slot is the only act that keeps the §3.2 arithmetic
//!   coherent for generation `m + capacity`.
//! * **A2, DropOldest vs the claim precondition.** §3.2 allows a
//!   write iff `sequence[i] == n`; DropOldest by definition writes
//!   over an unread slot. Reading: the policy is a §3.4 override of
//!   the precondition — the producer reclaims when
//!   `sequence[i] == n - capacity + 1` (previous generation published
//!   and unread), destroying the old payload. §3.4 is silent on a
//!   consumer that is *mid-read* during a reclaim; the consumer here
//!   revalidates the slot sequence after copying the payload out and
//!   discards a torn copy (counted as dropped). A formal treatment
//!   would need a consumer-progress signal §3.2 does not provide.
//! * **A3, Block "park".** §3.4 says "spin-with-backoff, then park"
//!   but names no parking primitive, and a `Mutex`/`Condvar` would
//!   leave the stated protocol. Implemented: spin (`spin_loop`)
//!   escalating to `yield_now`. No park.
//! * **A5, payload shape.** §3.1/§3.2 are written around `f32`
//!   section payloads with stamp/lineage headers. This ring is
//!   generic over the payload (`T: Send`); stamps and lineage are
//!   payload-side concerns (§1 assigns the ring no knowledge of
//!   payload meaning) and ride inside `T` if the caller wants them.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Backpressure policy, chosen at ring construction (DESIGN §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Producer waits (spin with backoff) until the target slot is
    /// free. Nothing is ever lost.
    Block,
    /// Producer abandons a message whose target slot is not free and
    /// advances past it; the drop is counted and the consumer skips
    /// the gap.
    DropNewest,
    /// Producer overwrites the oldest unread generation when the
    /// ring is full; the consumer skips overwritten generations and
    /// counts them.
    DropOldest,
}

/// A message the producer abandoned under [`Policy::DropNewest`].
/// The payload is handed back so the caller keeps ownership.
pub struct SendError<T>(pub T);

impl<T> std::fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SendError(..)")
    }
}

impl<T> std::fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("message dropped: ring full under DropNewest")
    }
}

/// Point-in-time snapshot of the ring's accounting counters
/// (DESIGN §3.4 / INV-R4). Counters are `Relaxed` atomics, so a
/// snapshot taken mid-flight is a set of individually-current values,
/// not a transactional read; at quiescence (both ends idle) the
/// conservation identity holds exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingStats {
    /// Messages the producer attempted (published + dropped-newest).
    pub produced: u64,
    /// Messages the consumer actually delivered.
    pub consumed: u64,
    /// Messages abandoned by the producer under DropNewest.
    pub dropped_newest: u64,
    /// Generations the consumer skipped as overwritten under
    /// DropOldest.
    pub dropped_oldest: u64,
    /// Total nanoseconds producers spent waiting for a free slot
    /// under Block.
    pub blocked_ns: u64,
}

impl RingStats {
    /// `produced - consumed - dropped` — messages buffered in the
    /// ring plus any whose accounting is still in transit. Under
    /// DropOldest a just-overwritten generation counts as in-flight
    /// until the consumer's skip accounts it; at drain this is 0.
    pub fn in_flight(&self) -> u64 {
        self.produced
            .saturating_sub(self.consumed + self.dropped_newest + self.dropped_oldest)
    }

    /// The INV-R4 identity, exact at quiescence:
    /// `produced == consumed + dropped_newest + dropped_oldest + in_flight`.
    pub fn conservation_holds(&self) -> bool {
        self.produced
            == self.consumed + self.dropped_newest + self.dropped_oldest + self.in_flight()
    }
}

/// One ring slot. 64-byte aligned, mirroring §3.1's rationale: the
/// protocol word and the neighbouring slot's header do not share a
/// cache line on 64-byte-line machines.
#[repr(align(64))]
struct Slot<T> {
    sequence: AtomicU64,
    /// True while the slot holds a live payload (written, not yet
    /// read out or reclaimed). Touched only by whichever party the
    /// protocol currently grants the slot, exactly like `data`.
    occupied: UnsafeCell<bool>,
    data: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: `data`/`occupied` are only accessed under the §3.2
// protocol: the producer writes a slot only when its sequence names
// the producer's generation (or, under DropOldest, the recorded
// reclaim state), and the consumer reads only after an Acquire load
// observes the publish value, before its Release store frees the
// slot. The sequence word's acquire/release pairing is what makes
// the plain payload accesses race-free in the Block/DropNewest
// policies; DropOldest's reclaim race is fenced by the consumer's
// post-copy revalidation (see module docs, A2).
unsafe impl<T: Send> Sync for Slot<T> {}

struct Inner<T> {
    slots: Box<[Slot<T>]>,
    capacity: u64,
    mask: u64,
    policy: Policy,
    /// DropNewest gap signal only (module docs, A1). Stored by the
    /// producer after each publish/drop; never synchronises payload.
    producer_head: AtomicU64,
    produced: AtomicU64,
    consumed: AtomicU64,
    dropped_newest: AtomicU64,
    dropped_oldest: AtomicU64,
    blocked_ns: AtomicU64,
    producer_taken: AtomicBool,
    consumer_taken: AtomicBool,
}

// SAFETY: all shared mutable state is either atomic or lives behind
// the slot protocol described on `Slot`'s Sync impl.
unsafe impl<T: Send> Send for Inner<T> {}
// SAFETY: as above.
unsafe impl<T: Send> Sync for Inner<T> {}

impl<T> Inner<T> {
    fn stats(&self) -> RingStats {
        RingStats {
            produced: self.produced.load(Ordering::Relaxed),
            consumed: self.consumed.load(Ordering::Relaxed),
            dropped_newest: self.dropped_newest.load(Ordering::Relaxed),
            dropped_oldest: self.dropped_oldest.load(Ordering::Relaxed),
            blocked_ns: self.blocked_ns.load(Ordering::Relaxed),
        }
    }
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        // Both ends are gone (they hold the `Arc`s), so no protocol
        // peer can still touch a slot: destroy any payload that was
        // published but never consumed.
        for slot in self.slots.iter() {
            // SAFETY: exclusive access via `&mut self` on the last
            // `Arc`; `occupied` is the protocol's own liveness flag.
            unsafe {
                if *slot.occupied.get() {
                    (*slot.data.get()).assume_init_drop();
                }
            }
        }
    }
}

/// The §3.2 SPSC ring. Create one, mint exactly one [`Producer`]
/// and one [`Consumer`] from it, and move the ends to their threads.
pub struct SpscRing<T> {
    inner: Arc<Inner<T>>,
}

impl<T> SpscRing<T> {
    /// `capacity` must be a power of two (DESIGN F9: the mask
    /// arithmetic is only sound under this precondition, so it is
    /// checked where the mask is born). No allocation happens after
    /// this constructor.
    ///
    /// # Panics
    /// Panics if `capacity` is not a power of two.
    pub fn new(capacity: usize, policy: Policy) -> Self {
        assert!(
            capacity.is_power_of_two(),
            "capacity must be a power of two"
        );
        let slots: Box<[Slot<T>]> = (0..capacity)
            .map(|i| Slot {
                sequence: AtomicU64::new(i as u64),
                occupied: UnsafeCell::new(false),
                data: UnsafeCell::new(MaybeUninit::uninit()),
            })
            .collect();
        Self {
            inner: Arc::new(Inner {
                slots,
                capacity: capacity as u64,
                mask: (capacity - 1) as u64,
                policy,
                producer_head: AtomicU64::new(0),
                produced: AtomicU64::new(0),
                consumed: AtomicU64::new(0),
                dropped_newest: AtomicU64::new(0),
                dropped_oldest: AtomicU64::new(0),
                blocked_ns: AtomicU64::new(0),
                producer_taken: AtomicBool::new(false),
                consumer_taken: AtomicBool::new(false),
            }),
        }
    }

    /// Number of slots (a power of two). Per the §3.2 arithmetic the
    /// full capacity is usable: generation `n` is claimable exactly
    /// when its slot's sequence equals `n`, and the consumer's
    /// release manufactures that value, so no slot is sacrificed to
    /// distinguish full from empty.
    pub fn capacity(&self) -> usize {
        self.inner.capacity as usize
    }

    /// The backpressure policy chosen at construction.
    pub fn policy(&self) -> Policy {
        self.inner.policy
    }

    /// The ring's accounting counters (INV-R4).
    pub fn stats(&self) -> RingStats {
        self.inner.stats()
    }

    /// Mint the (single) producer end.
    ///
    /// # Panics
    /// Panics if a producer was already minted — SPSC is the
    /// contract, and a second producer would void the protocol.
    pub fn producer(&self) -> Producer<T> {
        assert!(
            !self.inner.producer_taken.swap(true, Ordering::AcqRel),
            "SpscRing is single-producer: producer already minted"
        );
        Producer {
            inner: Arc::clone(&self.inner),
            next: 0,
        }
    }

    /// Mint the (single) consumer end.
    ///
    /// # Panics
    /// Panics if a consumer was already minted.
    pub fn consumer(&self) -> Consumer<T> {
        assert!(
            !self.inner.consumer_taken.swap(true, Ordering::AcqRel),
            "SpscRing is single-consumer: consumer already minted"
        );
        Consumer {
            inner: Arc::clone(&self.inner),
            next: 0,
        }
    }
}

/// The producing end. `next` is a plain field: exactly one producer
/// exists, so the per-lane counter needs no atomic (contrast the
/// shipped ring's `fetch_add` claim, which exists to arbitrate
/// between producers this design does not have).
pub struct Producer<T> {
    inner: Arc<Inner<T>>,
    next: u64,
}

impl<T> Producer<T> {
    /// Send one message.
    ///
    /// * Under [`Policy::Block`] this waits (spin with backoff; the
    ///   wait is added to `blocked_ns`) until the target slot is
    ///   free, then always returns `Ok(())`.
    /// * Under [`Policy::DropNewest`] a non-free slot means the
    ///   message is abandoned: `Err(SendError(value))` hands the
    ///   payload back, `dropped_newest` is incremented, and the
    ///   producer counter advances past the gap.
    /// * Under [`Policy::DropOldest`] an unread previous generation
    ///   in the target slot is destroyed and overwritten; always
    ///   `Ok(())`.
    pub fn send(&mut self, value: T) -> Result<(), SendError<T>> {
        let inner = &*self.inner;
        let n = self.next;
        let slot = &inner.slots[(n & inner.mask) as usize];
        match inner.policy {
            Policy::Block => {
                if slot.sequence.load(Ordering::Acquire) != n {
                    let waited_from = Instant::now();
                    let mut spins = 0u32;
                    while slot.sequence.load(Ordering::Acquire) != n {
                        if spins < 128 {
                            std::hint::spin_loop();
                            spins += 1;
                        } else {
                            std::thread::yield_now();
                        }
                    }
                    inner
                        .blocked_ns
                        .fetch_add(waited_from.elapsed().as_nanos() as u64, Ordering::Relaxed);
                }
                // SAFETY: the slot's sequence equals `n`, so the
                // consumer has released this generation to us; the
                // Acquire load above pairs with its Release store.
                unsafe { write_slot(slot, value) };
                slot.sequence.store(n + 1, Ordering::Release);
            }
            Policy::DropNewest => {
                if slot.sequence.load(Ordering::Acquire) != n {
                    inner.dropped_newest.fetch_add(1, Ordering::Relaxed);
                    inner.produced.fetch_add(1, Ordering::Relaxed);
                    self.next = n + 1;
                    // Gap signal (A1): stored after the drop it
                    // describes, so a consumer that observes
                    // `head > m` knows message `m`'s fate is decided.
                    inner.producer_head.store(n + 1, Ordering::Release);
                    return Err(SendError(value));
                }
                // SAFETY: sequence equals `n`; exclusive per §3.2.
                unsafe { write_slot(slot, value) };
                slot.sequence.store(n + 1, Ordering::Release);
                inner.producer_head.store(n + 1, Ordering::Release);
            }
            Policy::DropOldest => {
                let seq = slot.sequence.load(Ordering::Acquire);
                if seq != n {
                    // The only other reachable state: the previous
                    // generation of this slot was published and is
                    // still unread (`n - capacity + 1`). Reclaim it
                    // (A2): destroy the stale payload in place.
                    debug_assert_eq!(
                        seq,
                        n.wrapping_sub(inner.capacity).wrapping_add(1),
                        "DropOldest: slot in an unaccounted state"
                    );
                    // SAFETY: per the protocol state just asserted,
                    // the slot holds a live payload for generation
                    // `n - capacity`, and the consumer has not
                    // released it. A consumer concurrently copying
                    // it out is the A2 race; its post-copy
                    // revalidation discards the torn copy.
                    unsafe { (*slot.data.get()).assume_init_drop() };
                }
                // SAFETY: slot is free (`seq == n`) or just
                // reclaimed by us; exclusive per §3.2/A2.
                unsafe { write_slot(slot, value) };
                slot.sequence.store(n + 1, Ordering::Release);
            }
        }
        inner.produced.fetch_add(1, Ordering::Relaxed);
        self.next = n + 1;
        Ok(())
    }

    /// The ring's accounting counters (INV-R4).
    pub fn stats(&self) -> RingStats {
        self.inner.stats()
    }
}

/// The consuming end. Holds the per-lane read counter as a plain
/// field, like the producer.
pub struct Consumer<T> {
    inner: Arc<Inner<T>>,
    next: u64,
}

impl<T> Consumer<T> {
    /// Take the next message, in order, or `None` if the next
    /// expected generation has not been published yet.
    ///
    /// Under [`Policy::DropNewest`], generations the producer
    /// dropped are skipped transparently (their accounting already
    /// happened producer-side; the skip performs their release
    /// store — module docs A1b). Under [`Policy::DropOldest`],
    /// generations overwritten before they could be read are skipped
    /// and counted in `dropped_oldest`. Delivery of surviving
    /// messages is in-order and exactly-once.
    pub fn recv(&mut self) -> Option<T> {
        let inner = &*self.inner;
        loop {
            let m = self.next;
            let slot = &inner.slots[(m & inner.mask) as usize];
            let seq = slot.sequence.load(Ordering::Acquire);
            if seq == m + 1 {
                // SAFETY: the Acquire load observed the publish
                // value, pairing with the producer's Release store;
                // the payload for generation `m` is fully written
                // and exclusively ours until our release store.
                let value = unsafe { (*slot.data.get()).assume_init_read() };
                if inner.policy == Policy::DropOldest
                    && slot.sequence.load(Ordering::Acquire) != m + 1
                {
                    // A2 race lost: the producer reclaimed the slot
                    // while we copied. The copy's bits may mix two
                    // generations; never drop it (its provenance is
                    // unsound) and never deliver it. Leak-by-forget
                    // is bounded to this misuse case.
                    std::mem::forget(value);
                    inner.dropped_oldest.fetch_add(1, Ordering::Relaxed);
                    self.next = m + 1;
                    continue;
                }
                // SAFETY: we have read the payload out; the slot is
                // unoccupied until the next generation's write.
                unsafe { *slot.occupied.get() = false };
                slot.sequence
                    .store(m.wrapping_add(inner.capacity), Ordering::Release);
                inner.consumed.fetch_add(1, Ordering::Relaxed);
                self.next = m + 1;
                return Some(value);
            }
            match inner.policy {
                Policy::Block => return None,
                Policy::DropOldest => {
                    // Overwritten iff the slot's sequence is a whole
                    // number of laps past this generation's publish
                    // value. The `< 2^63` bound keeps a not-yet-
                    // written slot (sequence behind us, difference
                    // wrapped near 2^64) from reading as overwritten.
                    let diff = seq.wrapping_sub(m + 1);
                    if diff != 0 && diff.is_multiple_of(inner.capacity) && diff < (1u64 << 63) {
                        // No release store here: unlike DropNewest,
                        // this slot holds a *newer* live generation
                        // whose state our store would clobber.
                        inner.dropped_oldest.fetch_add(1, Ordering::Relaxed);
                        self.next = m + 1;
                        continue;
                    }
                    return None;
                }
                Policy::DropNewest => {
                    // A1: the head is stored after the publish/drop
                    // it describes, so `head > m` means generation
                    // `m`'s fate is decided. If it was published, the
                    // re-check sees it (the head store we Acquired
                    // happens-after the publish store); otherwise it
                    // was dropped and we skip it.
                    if inner.producer_head.load(Ordering::Acquire) > m {
                        if slot.sequence.load(Ordering::Acquire) == m + 1 {
                            continue;
                        }
                        // A1b: perform the dropped generation's
                        // release store, so generation
                        // `m + capacity` finds the free value §3.2
                        // promises it.
                        slot.sequence
                            .store(m.wrapping_add(inner.capacity), Ordering::Release);
                        self.next = m + 1;
                        continue;
                    }
                    return None;
                }
            }
        }
    }

    /// The ring's accounting counters (INV-R4).
    pub fn stats(&self) -> RingStats {
        self.inner.stats()
    }
}

/// Write `value` into a slot the protocol has granted us, and mark
/// it occupied.
///
/// # Safety
/// The caller must hold the slot for the current generation per the
/// §3.2 protocol (sequence check / reclaim done), and the slot must
/// not hold a live payload.
unsafe fn write_slot<T>(slot: &Slot<T>, value: T) {
    // SAFETY: guaranteed by the caller's protocol check (see the
    // type-level contract above); MaybeUninit::write does not read
    // or drop the previous (dead) contents.
    (*slot.data.get()).write(value);
    // SAFETY: same grant; `occupied` is protocol-private state.
    *slot.occupied.get() = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn capacity_must_be_power_of_two() {
        let _ = SpscRing::<u64>::new(3, Policy::Block);
    }

    #[test]
    fn slots_are_64_byte_aligned() {
        assert_eq!(std::mem::align_of::<Slot<u64>>(), 64);
    }

    #[test]
    fn send_recv_roundtrip_and_release_arithmetic() {
        let ring = SpscRing::new(4, Policy::Block);
        let mut tx = ring.producer();
        let mut rx = ring.consumer();
        assert_eq!(rx.recv(), None);
        tx.send(10).unwrap();
        tx.send(11).unwrap();
        assert_eq!(rx.recv(), Some(10));
        assert_eq!(rx.recv(), Some(11));
        assert_eq!(rx.recv(), None);
        let s = ring.stats();
        assert_eq!(s.produced, 2);
        assert_eq!(s.consumed, 2);
        assert_eq!(s.in_flight(), 0);
        assert!(s.conservation_holds());
    }
}
