## Remote latency measurement: 250 ms round trip, 2026-10-17

Captured with `scripts/measure-remote-latency.ts` on Linux x64: a debug
`bibcode serve` with the packaged-UI provider fixtures, the web build served
locally, headless Chromium, and only server traffic through
`scripts/throttle-proxy.ts` with a 125 ms one-way delay. Typing is one key every
100 ms. "Before" is v0.9.4 (`62c18c63`); "after" adds stream chunk coalescing and
plain `/ws` compression. Raw data:
[remote-latency-250ms-20261017.json](remote-latency-250ms-20261017.json).

"Sequential" is the longest chain of requests each sent right after a reply to
the previous one; it is inferred from timing and is noise for the typing rows,
where echo lag is the measure.

### 250 ms round trip

| Action                                   | RPCs | Sequential | Received before → after | Visible before → after | Echo lag p50 / p95 before → after |
| ---------------------------------------- | ---: | ---------: | ----------------------: | ---------------------: | --------------------------------: |
| Composer: type 40 characters             |    0 |          0 |          20.6 → 19.7 KB |         4121 → 4106 ms |                                   |
| Send message: user message visible       |    1 |          0 |            6.4 → 7.8 KB |             27 → 25 ms |                                   |
| Send message: response visible           |    1 |          1 |           9.7 → 12.6 KB |           394 → 389 ms |                                   |
| Switch to a thread with history          |    2 |          0 |            1.7 → 3.2 KB |             32 → 42 ms |                                   |
| Source Control: open panel               |    0 |          0 |            3.3 → 4.8 KB |           356 → 345 ms |                                   |
| Files: open panel                        |    2 |          0 |            7.4 → 5.9 KB |           361 → 355 ms |                                   |
| Terminal: open                           |    5 |          1 |            7.8 → 6.3 KB |           325 → 327 ms |                                   |
| Terminal: type 40 characters             |   41 |          — |          65.3 → 30.8 KB |        10544 → 4340 ms |        3458 / 6376 → 362 / 477 ms |
| Open thread after reload, no local cache |   24 |          3 |          52.4 → 37.6 KB |         2016 → 1956 ms |                                   |
| Reconnect until the thread resynced      |   25 |          2 |          52.4 → 35.2 KB |         2166 → 2213 ms |                                   |

With 20 ms of jitter each way, typing 40 characters echoed in 11263 ms before and
4500 ms after (echo lag 3737 / 7076 ms before, 411 / 551 ms after).

At no delay every row is unchanged within noise (typing echo lag 45 ms).

### Large thread (100 exchanges), 250 ms

| Action                                   | Compression off | Compression on |
| ---------------------------------------- | --------------: | -------------: |
| Open thread after reload, no local cache |        176.3 KB |        49.5 KB |
| Reconnect until the thread resynced      |        176.4 KB |        49.6 KB |

The proxy did not limit bandwidth, so these rows show bytes, not time; on a
link of 1–2 MB/s the saving is about 0.1 s per 176 KB.
