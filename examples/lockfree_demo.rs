/// Phase 1 lock-free optimization simple demonstration
///
/// Demonstrates basic functionality and performance of lock-free HashMap, queue, and counter
use msgtrans::transport::lockfree::{LockFreeCounter, LockFreeHashMap, LockFreeQueue};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

fn main() {
    println!("[START] Phase 1 lock-free optimization demonstration");
    println!("=======================");

    // 1. Lock-free HashMap demonstration
    demo_lockfree_hashmap();

    // 2. Lock-free queue demonstration
    demo_lockfree_queue();

    // 3. Lock-free counter demonstration
    demo_lockfree_counter();

    // 4. Concurrent performance test
    demo_concurrent_performance();

    println!("\n[SUCCESS] All demonstrations completed!");
}

/// Demonstrate lock-free HashMap
fn demo_lockfree_hashmap() {
    println!("\n[INFO] 1. Lock-free HashMap demonstration");

    let map = Arc::new(LockFreeHashMap::new());

    // Basic operations demonstration
    println!("Basic operations:");
    map.insert("key1".to_string(), "value1".to_string())
        .unwrap();
    map.insert("key2".to_string(), "value2".to_string())
        .unwrap();

    println!("  - Insert key1 -> value1");
    println!("  - Insert key2 -> value2");

    if let Some(value) = map.get(&"key1".to_string()) {
        println!("  - Read key1: {}", value);
    }

    println!("  - Current size: {}", map.len());

    // Concurrent operations demonstration
    println!("Concurrent operations test:");
    let map_clone = Arc::clone(&map);
    let mut handles = vec![];

    let start = Instant::now();

    // Start 10 threads for concurrent writes
    for i in 0..10 {
        let map = Arc::clone(&map_clone);
        let handle = thread::spawn(move || {
            for j in 0..100 {
                let key = format!("thread_{}_key_{}", i, j);
                let value = format!("thread_{}_value_{}", i, j);
                let _ = map.insert(key, value);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let duration = start.elapsed();
    let stats = map.stats();

    println!("  - 10 threads, each writing 100 times");
    println!("  - Total time: {:?}", duration);
    println!("  - Final size: {}", map.len());
    println!(
        "  - Read count: {}",
        stats.reads.load(std::sync::atomic::Ordering::Relaxed)
    );
    println!(
        "  - Write count: {}",
        stats.writes.load(std::sync::atomic::Ordering::Relaxed)
    );
    println!(
        "  - CAS success rate: {:.2}%",
        stats.cas_success_rate() * 100.0
    );
}

/// Demonstrate lock-free queue
fn demo_lockfree_queue() {
    println!("\n[INFO] 2. Lock-free queue demonstration");

    let queue = Arc::new(LockFreeQueue::new());

    // Basic operations demonstration
    println!("Basic operations:");
    queue.push(1).unwrap();
    queue.push(2).unwrap();
    queue.push(3).unwrap();

    println!("  - Push: 1, 2, 3");
    println!("  - Queue length: {}", queue.len());

    while let Some(value) = queue.pop() {
        println!("  - Pop: {}", value);
    }

    println!("  - Queue length: {}", queue.len());

    // Concurrent operations demonstration
    println!("Concurrent operations test:");
    let queue_clone = Arc::clone(&queue);
    let mut handles = vec![];

    let start = Instant::now();

    // 5 producer threads
    for i in 0..5 {
        let queue = Arc::clone(&queue_clone);
        let handle = thread::spawn(move || {
            for j in 0..200 {
                let _ = queue.push(i * 200 + j);
            }
        });
        handles.push(handle);
    }

    // 3 consumer threads
    for _ in 0..3 {
        let queue = Arc::clone(&queue_clone);
        let handle = thread::spawn(move || {
            for _ in 0..300 {
                while queue.pop().is_none() {
                    std::thread::yield_now();
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let duration = start.elapsed();
    let stats = queue.stats();

    println!("  - 5 producers, 3 consumers");
    println!("  - Total time: {:?}", duration);
    println!("  - Remaining queue length: {}", queue.len());
    println!(
        "  - Total enqueued: {}",
        stats.enqueued.load(std::sync::atomic::Ordering::Relaxed)
    );
    println!(
        "  - Total dequeued: {}",
        stats.dequeued.load(std::sync::atomic::Ordering::Relaxed)
    );
}

/// Demonstrate lock-free counter
fn demo_lockfree_counter() {
    println!("\n[INFO] 3. Lock-free counter demonstration");

    let counter = Arc::new(LockFreeCounter::new(0));

    // Basic operations demonstration
    println!("Basic operations:");
    counter.increment();
    counter.increment();
    println!("  - Increment 2 times, current value: {}", counter.get());

    counter.decrement();
    println!("  - Decrement 1 time, current value: {}", counter.get());

    // Concurrent operations demonstration
    println!("Concurrent operations test:");
    let counter_clone = Arc::clone(&counter);
    let mut handles = vec![];

    let start = Instant::now();

    // 10 threads concurrent operations
    for i in 0..10 {
        let counter = Arc::clone(&counter_clone);
        let handle = thread::spawn(move || {
            for _ in 0..1000 {
                if i % 2 == 0 {
                    counter.increment();
                } else {
                    counter.decrement();
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let duration = start.elapsed();
    let stats = counter.stats();

    println!("  - 10 threads, each operating 1000 times");
    println!("  - Total time: {:?}", duration);
    println!("  - Final value: {}", counter.get());
    println!(
        "  - Increment count: {}",
        stats.increments.load(std::sync::atomic::Ordering::Relaxed)
    );
    println!(
        "  - Decrement count: {}",
        stats.decrements.load(std::sync::atomic::Ordering::Relaxed)
    );
    println!(
        "  - Read count: {}",
        stats.reads.load(std::sync::atomic::Ordering::Relaxed)
    );
}

/// Concurrent performance test
fn demo_concurrent_performance() {
    println!("\n[PERF] 4. Concurrent performance test");

    let map = Arc::new(LockFreeHashMap::new());
    let queue = Arc::new(LockFreeQueue::new());
    let counter = Arc::new(LockFreeCounter::new(0));

    let start = Instant::now();
    let mut handles = vec![];

    // Start 20 threads for mixed operations
    for i in 0..20 {
        let map = Arc::clone(&map);
        let queue = Arc::clone(&queue);
        let counter = Arc::clone(&counter);

        let handle = thread::spawn(move || {
            for j in 0..1000 {
                // HashMap operations
                let key = format!("perf_{}_{}", i, j);
                let value = format!("data_{}_{}", i, j);
                let _ = map.insert(key.clone(), value);
                let _ = map.get(&key);

                // Queue operations
                let _ = queue.push(i * 1000 + j);
                let _ = queue.pop();

                // Counter operations
                counter.increment();
                if j % 2 == 0 {
                    counter.decrement();
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let duration = start.elapsed();

    println!("Mixed concurrent test results:");
    println!("  - Thread count: 20");
    println!("  - Operations per thread: 1000 x 6 = 6000");
    println!("  - Total operations: {}", 20 * 6000);
    println!("  - Total time: {:?}", duration);
    println!(
        "  - QPS: {:.0} ops/sec",
        (20 * 6000) as f64 / duration.as_secs_f64()
    );

    // Final state
    println!("Final state:");
    println!("  - HashMap size: {}", map.len());
    println!("  - Queue length: {}", queue.len());
    println!("  - Counter value: {}", counter.get());

    // Performance statistics
    let map_stats = map.stats();
    let queue_stats = queue.stats();
    let counter_stats = counter.stats();

    println!("Performance statistics:");
    println!(
        "  - HashMap CAS success rate: {:.2}%",
        map_stats.cas_success_rate() * 100.0
    );
    println!(
        "  - Average read latency: {}ns",
        map_stats
            .avg_read_latency_ns
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    println!(
        "  - Queue throughput: {:.0} ops/sec",
        (queue_stats
            .enqueued
            .load(std::sync::atomic::Ordering::Relaxed)
            + queue_stats
                .dequeued
                .load(std::sync::atomic::Ordering::Relaxed)) as f64
            / duration.as_secs_f64()
    );
    println!(
        "  - Counter operations: {}",
        counter_stats
            .increments
            .load(std::sync::atomic::Ordering::Relaxed)
            + counter_stats
                .decrements
                .load(std::sync::atomic::Ordering::Relaxed)
    );
}
