//! What an engine keeps after a burst (ledger 140: fifty engines in one
//! process grew by 80–120 MiB each with a burst and stayed there). A
//! counting allocator tells the heap the engine still holds (live bytes)
//! from what the process keeps (its resident size): after the burst has
//! been applied and the engine has settled, the live bytes must be back
//! near what they were before, whatever the resident size does. Its own
//! test binary, because the allocator counts everything in the process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers fail the test loudly"
)]

#[path = "support/mod.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use ic_mock::{MockConfig, scenarios};
use ic_model::ObjectKey;

use support::{Launch, mock};

/// Counts the bytes allocated and not yet freed.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to the system allocator unchanged and only
// counts sizes.
#[expect(unsafe_code, reason = "a counting global allocator for the test")]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract is the system allocator's.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: as above.
        let new = unsafe { System.realloc(pointer, layout, size) };
        if !new.is_null() {
            LIVE.fetch_add(size, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        new
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Reads the engine's events for `duration`, holding none but the latest
/// snapshot (as the app's window does).
async fn drain_for(engine: &mut support::Engine, duration: Duration) {
    engine.seen.clear();
    let until = tokio::time::Instant::now() + duration;
    let mut held = None;
    while let Ok(Some(event)) = tokio::time::timeout_at(until, engine.events.next()).await {
        if let ic_core::CoreEvent::Snapshot(snapshot) = event {
            held = Some(snapshot);
        }
    }
    drop(held);
}

fn mib(bytes: usize) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "a report")]
    let mib = bytes as f64 / (1024.0 * 1024.0);
    mib
}

/// The process's resident size (Linux), in bytes.
fn resident() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: usize = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

/// Every service flipped critical and back, twice: 12 000 state changes
/// with their check results on 3 000 services.
fn burst(control: &ic_mock::MockControl) -> (ObjectKey, u8) {
    let keys: Vec<(ObjectKey, u8)> = control
        .services()
        .iter()
        .map(|service| {
            let code = match service.state {
                ic_model::ServiceState::Ok | ic_model::ServiceState::Pending => 0,
                ic_model::ServiceState::Warning => 1,
                ic_model::ServiceState::Critical => 2,
                ic_model::ServiceState::Unknown => 3,
            };
            (ObjectKey::from(service.key.clone()), code)
        })
        .collect();
    for round in 0..4 {
        for (key, code) in &keys {
            let flipped = if round % 2 == 0 {
                2 - code.min(&2)
            } else {
                *code
            };
            control
                .process_check_result(key, flipped, "burst", &[])
                .unwrap();
        }
    }
    keys.last().unwrap().clone()
}

/// A live engine on 3 000 services takes a burst of 12 000 state changes
/// with their check results; once it has applied them and settled, the
/// heap it holds is back within a few MiB of before. The mock runs in the
/// same process, so what the same burst makes a mock with no client keep
/// is measured first and taken off. The resident size is printed: what
/// the process keeps beyond the live bytes is the allocator's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_engine_gives_back_what_a_burst_made_it_allocate() {
    // The mock alone.
    let alone = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        200, 3,
    )))
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mock_before = LIVE.load(Ordering::Relaxed);
    burst(&alone.control());
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mock_kept = LIVE.load(Ordering::Relaxed).saturating_sub(mock_before);
    alone.shutdown().await;

    let server = mock(MockConfig::with_scenario(scenarios::large_with_hosts(
        200, 3,
    )))
    .await;
    let control = server.control();
    let mut engine = Launch::new(&server).start();
    engine.connected().await;
    let services = control.services().len();
    engine
        .snapshot(|snapshot| snapshot.services.len() == services)
        .await;
    drain_for(&mut engine, Duration::from_secs(2)).await;
    let before = LIVE.load(Ordering::Relaxed);
    let resident_before = resident();
    let peak = std::sync::Arc::new(AtomicUsize::new(before));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let (peak, stop) = (std::sync::Arc::clone(&peak), std::sync::Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let (last, code) = burst(&control);
    let ObjectKey::Service { key: last } = last else {
        unreachable!()
    };
    // Applied: the store shows the last round.
    engine
        .snapshot(|snapshot| {
            snapshot.services.get(&last).is_some_and(|service| {
                service
                    .check
                    .result
                    .as_ref()
                    .is_some_and(|result| result.exit_status == i32::from(code))
            })
        })
        .await;
    // Settled: no events for a while.
    drain_for(&mut engine, Duration::from_secs(5)).await;
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();
    let after = LIVE.load(Ordering::Relaxed);
    let kept = after.saturating_sub(before).saturating_sub(mock_kept);
    eprintln!(
        "live heap: before {:.1} MiB, peak {:.1} MiB, after {:.1} MiB, of it the mock's {:.1} MiB: the engine kept {:.1} MiB; resident: before {:.1} MiB, after {:.1} MiB",
        mib(before),
        mib(peak.load(Ordering::Relaxed)),
        mib(after),
        mib(mock_kept),
        mib(kept),
        mib(resident_before.unwrap_or(0)),
        mib(resident().unwrap_or(0)),
    );
    // A check result per service in full now (a lean load had none), the
    // event log's latest entries: a few MiB, not tens.
    assert!(
        kept <= 16 * 1024 * 1024,
        "the engine still holds {:.1} MiB more than before the burst",
        mib(kept)
    );
    engine.shutdown();
}
