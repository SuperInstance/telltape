//! Randomised invariant (property) tests for [`SpscRing`], the
//! DESIGN §3.2 ring — plus the head-to-head bench against the
//! shipped Disruptor ring, kept as an `#[ignore]`d test so it lives
//! with the properties it prices (run:
//! `cargo test --release --test spsc_properties -- --ignored --nocapture`).
//!
//! Std-only, hand-rolled, same discipline as `tests/properties.rs`:
//! the workspace LCG (Knuth's 1664525/1013904223 constants), seeded
//! per test so any failure reproduces from the printed seed.
//!
//! Properties pinned:
//! * INV-R1/R3 (Block): in-order, exactly-once, no-loss delivery
//!   across many wraparounds, under a randomised single-thread
//!   schedule and in a real two-thread run (100k messages).
//! * Full-ring behaviour per §3.4 for all three policies: Block
//!   holds a full `capacity` of messages (the §3.2 arithmetic
//!   sacrifices no slot); DropNewest hands the payload back, counts
//!   the drop, and the consumer skips exactly the dropped
//!   generations; DropOldest delivers exactly the newest `capacity`
//!   generations after a lap, with the overwritten ones accounted.
//! * INV-R4: `produced == consumed + dropped + in_flight` at drain,
//!   for every policy, including under random schedules checked
//!   against a naive queue model.

use rolling_substrate_core::spsc::{Policy, SpscRing};
use rolling_substrate_core::substrate::FluidSubstrateBuffer;
use std::collections::VecDeque;
use std::time::Instant;

/// The workspace LCG (same constants as `tests/properties.rs` and
/// `cuda_thor::test_input`), seedable so failures reproduce.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        Rng(seed | 1)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }
    fn coin(&mut self) -> bool {
        self.next_u32() & 1 == 0
    }
}

#[test]
fn block_fifo_no_loss_random_schedule_many_wraps() {
    let seed = 0x5350_5C01u32;
    let mut rng = Rng::new(seed);
    let cap = 8usize;
    let total = (cap * 5 + 7) as u64; // > 5 laps: > 3 wraparounds
    let ring = SpscRing::new(cap, Policy::Block);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    let mut sent = 0u64;
    let mut received: Vec<u64> = Vec::new();
    while (received.len() as u64) < total {
        let can_send = sent < total;
        if can_send && (received.len() as u64 == sent || rng.coin()) {
            tx.send(sent)
                .unwrap_or_else(|_| panic!("Block send dropped (seed {seed:#x})"));
            sent += 1;
        } else if let Some(v) = rx.recv() {
            received.push(v);
        } else {
            // Empty but not done: the schedule must make progress
            // by sending (can_send is true whenever recv is dry and
            // received < sent is false... guard against a stall).
            tx.send(sent)
                .unwrap_or_else(|_| panic!("Block send dropped (seed {seed:#x})"));
            sent += 1;
        }
    }
    assert_eq!(
        received,
        (0..total).collect::<Vec<_>>(),
        "FIFO/no-loss violated (seed {seed:#x})"
    );
    let s = ring.stats();
    assert_eq!(s.produced, total);
    assert_eq!(s.consumed, total);
    assert_eq!(s.dropped_newest + s.dropped_oldest, 0);
    assert_eq!(s.in_flight(), 0, "INV-R4 at drain (seed {seed:#x})");
    assert!(s.conservation_holds());
}

#[test]
fn block_wraps_tiny_ring_many_laps() {
    let ring = SpscRing::new(2, Policy::Block);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    for i in 0..50u64 {
        tx.send(i).unwrap();
        assert_eq!(rx.recv(), Some(i));
    }
    assert_eq!(ring.stats().in_flight(), 0);
}

#[test]
fn block_full_ring_holds_entire_capacity() {
    // §3.2's arithmetic sacrifices no slot: a capacity-8 ring holds
    // 8 unread messages before the producer would have to wait.
    let ring = SpscRing::new(8, Policy::Block);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    for i in 0..8u64 {
        tx.send(i).unwrap();
    }
    assert_eq!(ring.stats().in_flight(), 8);
    for i in 0..8u64 {
        assert_eq!(rx.recv(), Some(i));
    }
    assert_eq!(rx.recv(), None);
    assert_eq!(ring.stats().in_flight(), 0);
}

#[test]
fn block_two_threads_100k_in_order() {
    let total = 100_000u64;
    let ring = SpscRing::new(256, Policy::Block);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    let producer = std::thread::spawn(move || {
        for i in 0..total {
            tx.send(i).unwrap();
        }
        tx.stats()
    });
    let mut expected = 0u64;
    while expected < total {
        match rx.recv() {
            Some(v) => {
                assert_eq!(v, expected, "order violated at message {expected}");
                expected += 1;
            }
            None => std::hint::spin_loop(),
        }
    }
    let ps = producer.join().unwrap();
    assert_eq!(ps.produced, total);
    let s = rx.stats();
    assert_eq!(s.consumed, total);
    assert_eq!(s.in_flight(), 0, "INV-R4 after two-thread drain");
    assert!(s.conservation_holds());
}

#[test]
fn drop_newest_full_ring_gap_is_exact_and_accounted() {
    let ring = SpscRing::new(4, Policy::DropNewest);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    for i in 0..4u64 {
        tx.send(i).unwrap();
    }
    // Ring full: both further sends are abandoned, payload returned.
    assert_eq!(tx.send(4).unwrap_err().0, 4);
    assert_eq!(tx.send(5).unwrap_err().0, 5);
    // Consumer receives the four real messages, then nothing: the
    // gap (4, 5) is skipped, not waited on and not fabricated.
    for i in 0..4u64 {
        assert_eq!(rx.recv(), Some(i));
    }
    assert_eq!(rx.recv(), None);
    let s = ring.stats();
    assert_eq!(s.produced, 6);
    assert_eq!(s.consumed, 4);
    assert_eq!(s.dropped_newest, 2);
    assert_eq!(s.in_flight(), 0, "INV-R4 at drain");
    // The A1b repair: after the gap, the ring still works — slot
    // free-values stayed coherent across the skipped generations.
    tx.send(6).unwrap();
    assert_eq!(rx.recv(), Some(6));
    assert_eq!(ring.stats().in_flight(), 0);
}

#[test]
fn drop_newest_random_schedule_matches_naive_model() {
    let seed = 0x5350_5C02u32;
    let mut rng = Rng::new(seed);
    let cap = 8usize;
    let total = 400u64;
    let ring = SpscRing::new(cap, Policy::DropNewest);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    let mut model: VecDeque<u64> = VecDeque::new();
    let mut model_dropped = 0u64;
    let mut sent = 0u64;
    while sent < total {
        if rng.coin() {
            if model.len() == cap {
                model_dropped += 1;
                assert!(tx.send(sent).is_err(), "model says full (seed {seed:#x})");
            } else {
                tx.send(sent)
                    .unwrap_or_else(|_| panic!("model says room (seed {seed:#x})"));
                model.push_back(sent);
            }
            sent += 1;
        } else {
            assert_eq!(
                rx.recv(),
                model.pop_front(),
                "order vs model (seed {seed:#x})"
            );
        }
    }
    while let Some(v) = rx.recv() {
        assert_eq!(Some(v), model.pop_front());
    }
    assert!(model.is_empty());
    let s = ring.stats();
    assert_eq!(s.produced, total);
    assert_eq!(s.dropped_newest, model_dropped, "seed {seed:#x}");
    assert_eq!(s.consumed + s.dropped_newest, total);
    assert_eq!(s.in_flight(), 0, "INV-R4 at drain (seed {seed:#x})");
}

#[test]
fn drop_oldest_lap_delivers_newest_capacity_and_accounts() {
    let ring = SpscRing::new(4, Policy::DropOldest);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    // Six sends, no receives: generations 0 and 1 are overwritten.
    for i in 0..6u64 {
        tx.send(i).unwrap();
    }
    for i in 2..6u64 {
        assert_eq!(rx.recv(), Some(i));
    }
    assert_eq!(rx.recv(), None);
    let s = ring.stats();
    assert_eq!(s.produced, 6);
    assert_eq!(s.consumed, 4);
    assert_eq!(s.dropped_oldest, 2);
    assert_eq!(s.in_flight(), 0, "INV-R4 at drain");
}

#[test]
fn drop_oldest_random_schedule_matches_naive_model() {
    let seed = 0x5350_5C03u32;
    let mut rng = Rng::new(seed);
    let cap = 8usize;
    let total = 400u64;
    let ring = SpscRing::new(cap, Policy::DropOldest);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    let mut model: VecDeque<u64> = VecDeque::new();
    let mut model_overwritten = 0u64;
    let mut sent = 0u64;
    while sent < total {
        if rng.coin() {
            if model.len() == cap {
                model.pop_front();
                model_overwritten += 1;
            }
            model.push_back(sent);
            tx.send(sent).unwrap();
            sent += 1;
        } else {
            assert_eq!(
                rx.recv(),
                model.pop_front(),
                "order vs model (seed {seed:#x})"
            );
        }
    }
    while let Some(v) = rx.recv() {
        assert_eq!(Some(v), model.pop_front());
    }
    assert!(model.is_empty());
    let s = ring.stats();
    assert_eq!(s.produced, total);
    // Our consumer counts skips lazily; the final drain above forces
    // every skip to be observed, so the totals must meet exactly.
    assert_eq!(s.dropped_oldest, model_overwritten, "seed {seed:#x}");
    assert_eq!(s.consumed + s.dropped_oldest, total);
    assert_eq!(s.in_flight(), 0, "INV-R4 at drain (seed {seed:#x})");
}

// ------------------------------------------------------------------
// Head-to-head bench (ignored by default): SpscRing (§3.2) vs the
// shipped Disruptor ring, 1 producer -> 1 consumer, same payload.
// Methodology is written up in docs/RING-BENCH.md; the short version:
// payload [f32; 8] (the substrate slot shape at dimension 8),
// two threads for throughput (1M messages), a pair of rings for
// ping-pong latency (20k round trips x 5 runs, median reported).
// ------------------------------------------------------------------

const BENCH_PAYLOAD: usize = 8;

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
        })
        .unwrap_or_else(|| "unknown cpu".to_string())
}

fn bench_spsc_throughput(n: u64, cap: usize) -> f64 {
    let ring = SpscRing::new(cap, Policy::Block);
    let mut tx = ring.producer();
    let mut rx = ring.consumer();
    let t0 = Instant::now();
    let prod = std::thread::spawn(move || {
        for i in 0..n {
            tx.send([i as f32; BENCH_PAYLOAD]).unwrap();
        }
    });
    let mut sum = 0f64;
    let mut got = 0u64;
    while got < n {
        match rx.recv() {
            Some(v) => {
                sum += v[0] as f64;
                got += 1;
            }
            None => std::hint::spin_loop(),
        }
    }
    prod.join().unwrap();
    let secs = t0.elapsed().as_secs_f64();
    let expect = (n - 1) as f64 * n as f64 / 2.0;
    assert_eq!(sum, expect, "SPSC bench lost or reordered messages");
    n as f64 / secs / 1e6
}

fn bench_disruptor_throughput(n: u64, cap: usize) -> (f64, u64) {
    let buf = std::sync::Arc::new(FluidSubstrateBuffer::new(cap, BENCH_PAYLOAD));
    let b2 = std::sync::Arc::clone(&buf);
    let t0 = Instant::now();
    let prod = std::thread::spawn(move || {
        for i in 0..n {
            let mut lease = b2.lease();
            lease.data_mut().fill(i as f32);
            lease.finalize();
        }
    });
    // The shipped ring's consumer contract: poll `is_published`,
    // then read. There is no consumer release — the producer does
    // not wait for this reader — so a torn read is possible in
    // principle; count slots whose contents are not uniform.
    let mut torn = 0u64;
    for c in 0..n {
        while !buf.is_published(c) {
            std::hint::spin_loop();
        }
        // SAFETY: claim c is published. The next-lap lease may
        // already be writing this slot (the shipped protocol has no
        // consumer release) — that hazard is precisely what the
        // torn counter below measures; the bench accepts it because
        // there is no safer consumer the shipped API offers.
        let data = unsafe { buf.read_published(c) };
        if data.iter().any(|&v| v != c as f32) {
            torn += 1;
        }
    }
    prod.join().unwrap();
    let secs = t0.elapsed().as_secs_f64();
    (n as f64 / secs / 1e6, torn)
}

fn bench_spsc_pingpong(rounds: u64) -> f64 {
    let ab = SpscRing::new(64, Policy::Block);
    let ba = SpscRing::new(64, Policy::Block);
    let mut tx_ab = ab.producer();
    let mut rx_ab = ab.consumer();
    let mut tx_ba = ba.producer();
    let mut rx_ba = ba.consumer();
    let echo = std::thread::spawn(move || {
        for _ in 0..rounds {
            loop {
                match rx_ab.recv() {
                    Some(v) => {
                        tx_ba.send(v).unwrap();
                        break;
                    }
                    None => std::hint::spin_loop(),
                }
            }
        }
    });
    let t0 = Instant::now();
    for i in 0..rounds {
        tx_ab.send([i as f32; BENCH_PAYLOAD]).unwrap();
        loop {
            match rx_ba.recv() {
                Some(v) => {
                    assert_eq!(v[0], i as f32);
                    break;
                }
                None => std::hint::spin_loop(),
            }
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    echo.join().unwrap();
    secs / rounds as f64 * 1e6
}

fn bench_disruptor_pingpong(rounds: u64) -> f64 {
    // Two buffers, one message in flight per buffer, so the shipped
    // ring's no-release consumer contract is safe here by pacing.
    let ab = std::sync::Arc::new(FluidSubstrateBuffer::new(64, BENCH_PAYLOAD));
    let ba = std::sync::Arc::new(FluidSubstrateBuffer::new(64, BENCH_PAYLOAD));
    let ab2 = std::sync::Arc::clone(&ab);
    let ba2 = std::sync::Arc::clone(&ba);
    let echo = std::thread::spawn(move || {
        for c in 0..rounds {
            while !ab2.is_published(c) {
                std::hint::spin_loop();
            }
            // SAFETY: claim c published on ab; at most one message
            // is in flight per buffer in this bench, so the slot
            // cannot be re-leased until we publish the echo.
            let v = unsafe { ab2.read_published(c) }[0];
            let mut lease = ba2.lease();
            lease.data_mut().fill(v);
            lease.finalize();
        }
    });
    let t0 = Instant::now();
    for i in 0..rounds {
        let mut lease = ab.lease();
        lease.data_mut().fill(i as f32);
        lease.finalize();
        while !ba.is_published(i) {
            std::hint::spin_loop();
        }
        // SAFETY: as on the echo side — one in flight per buffer.
        let v = unsafe { ba.read_published(i) }[0];
        assert_eq!(v, i as f32);
    }
    let secs = t0.elapsed().as_secs_f64();
    echo.join().unwrap();
    secs / rounds as f64 * 1e6
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[xs.len() / 2]
}

#[test]
#[ignore = "bench: run with --release --ignored --nocapture"]
fn bench_rings_head_to_head() {
    println!("cpu: {}", cpu_model());
    println!(
        "threads available: {}",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );
    let n = 1_000_000u64;
    for cap in [256usize, 4096] {
        let spsc = bench_spsc_throughput(n, cap);
        let (dis, torn) = bench_disruptor_throughput(n, cap);
        println!(
            "throughput cap={cap}: SPSC {spsc:.2} M msgs/s | Disruptor {dis:.2} M msgs/s | torn reads: {torn}"
        );
    }
    let rounds = 20_000u64;
    let spsc_pp: Vec<f64> = (0..5).map(|_| bench_spsc_pingpong(rounds)).collect();
    let dis_pp: Vec<f64> = (0..5).map(|_| bench_disruptor_pingpong(rounds)).collect();
    println!(
        "ping-pong round-trip: SPSC median {:.2} us (runs {:?}) | Disruptor median {:.2} us (runs {:?})",
        median(spsc_pp.clone()),
        spsc_pp,
        median(dis_pp.clone()),
        dis_pp
    );
}
