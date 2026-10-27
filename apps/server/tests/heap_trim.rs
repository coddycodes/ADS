//! The heap trimmer must hand freed glibc arena memory back to the OS (#79). Its own test
//! binary, so no other test's allocations move this process's resident memory.
#![cfg(all(target_os = "linux", target_env = "gnu"))]

const MIB: u64 = 1024 * 1024;

fn rss_anon_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status
        .lines()
        .find(|line| line.starts_with("RssAnon:"))
        .unwrap();
    let kib: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    kib * 1024
}

#[test]
fn trim_leaves_no_freed_arena_memory_resident() {
    // A thread of its own, so the memory comes from a secondary arena, as on a Tokio worker.
    std::thread::spawn(|| {
        let baseline = rss_anon_bytes();
        let mut chunks: Vec<Box<[u8]>> = (0..256 * 1024)
            .map(|_| vec![1_u8; 1024].into_boxed_slice())
            .collect();
        // One live chunk above the freed ones, as any surviving allocation would leave it:
        // with default settings glibc then returns nothing on free, because it trims only
        // the top of a heap. Allocator tuning that already returns it on free passes too.
        let pinned = chunks.pop();
        drop(chunks);

        coddy_studio_server::allocator::trim();

        let retained = rss_anon_bytes().saturating_sub(baseline);
        drop(pinned);
        assert!(
            retained < 64 * MIB,
            "{} MiB of the 256 MiB freed stayed resident after the trim",
            retained / MIB
        );
    })
    .join()
    .unwrap();
}
