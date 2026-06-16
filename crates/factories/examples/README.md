# factories examples — a guided tour

Each example is a small, self-contained program that introduces one concept
and builds on the ones before it. Run any of them with:

```sh
cargo run -p factories --example <name>
```

Read them in order — the book follows the same arc.

| # | Example                           | Concept                                                                           |
|---|-----------------------------------|-----------------------------------------------------------------------------------|
| 1 | [`counter`](counter.rs)           | Define an actor, spawn it, and the **tell vs. ask** distinction.                  |
| 2 | [`bank_account`](bank_account.rs) | Request/response with a **`Result` answer** — failing without dying.              |
| 3 | [`concurrency`](concurrency.rs)   | **Opting into concurrency**: overlapping `&self` reads under a `TokioRwLock`.     |
| 4 | [`supervision`](supervision.rs)   | **Watching** another actor and reacting to its `Terminated` signal.               |
| 5 | [`chat_room`](chat_room.rs)       | **Protocols**: one erased handle over many different actor types; broadcast.      |
| 6 | [`heartbeat`](heartbeat.rs)       | A self-driving **event source** plus `on_start` / `on_stop` **lifecycle hooks**.  |

The first two examples lean entirely on `factories::prelude::*`. From
`concurrency` on, you'll see explicit `use factories::runtime::…` imports for the
run loops and locks — those live outside the prelude on purpose: reaching for a
non-default loop or lock is a deliberate choice, and the import says so.
