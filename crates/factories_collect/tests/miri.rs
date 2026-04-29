use factories_collect::{
    global_collection, register_global_collection_entry, unsafe_run_on_binary_load,
    GlobalCollection, GlobalCollectionEntry, GlobalCollectionIter,
};
use std::sync::Barrier;

fn collect_all<T: Copy>(collection: &GlobalCollection<T>) -> Vec<T> {
    GlobalCollectionIter::new(collection).copied().collect()
}

const MACRO_COLLECTION: &'static GlobalCollection<u32> = global_collection!(u32);

static MACRO_E1: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&111);
static MACRO_E2: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&222);
static MACRO_E3: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&333);

register_global_collection_entry!(MACRO_COLLECTION, MACRO_E1);
register_global_collection_entry!(MACRO_COLLECTION, MACRO_E2);
register_global_collection_entry!(MACRO_COLLECTION, MACRO_E3);

#[test]
fn macro_entries_registered_before_main() {
    let mut values = collect_all(MACRO_COLLECTION);
    values.sort();
    assert_eq!(values, vec![111, 222, 333]);
}

static mut RAN_ON_LOAD: bool = false;

unsafe_run_on_binary_load!(|| {
    // SAFETY: This runs single-threaded before main
    unsafe { RAN_ON_LOAD = true };
});

#[test]
fn unsafe_run_on_binary_load_executes() {
    // SAFETY: Only read after init is complete, no concurrent writes
    assert!(unsafe { RAN_ON_LOAD });
}

#[test]
fn empty_collection() {
    let collection = GlobalCollection::<u32>::new();
    assert!(collect_all(&collection).is_empty());
}

#[test]
fn single_entry() {
    static ENTRY: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&42);
    let collection = GlobalCollection::new();
    collection.register(&ENTRY);

    let values = collect_all(&collection);
    assert_eq!(values, vec![42]);
}

#[test]
fn multiple_entries() {
    static E1: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&1);
    static E2: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&2);
    static E3: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&3);
    let collection = GlobalCollection::new();

    collection.register(&E1);
    collection.register(&E2);
    collection.register(&E3);

    let mut values = collect_all(&collection);
    values.sort();
    assert_eq!(values, vec![1, 2, 3]);
}

#[test]
fn double_register_is_idempotent() {
    static ENTRY: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&99);
    let collection = GlobalCollection::new();

    collection.register(&ENTRY);
    collection.register(&ENTRY);

    let values = collect_all(&collection);
    assert_eq!(values, vec![99]);
}

#[test]
fn concurrent_registration() {
    static E1: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&10);
    static E2: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&20);
    static E3: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&30);
    static E4: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&40);

    static COLLECTION: GlobalCollection<u32> = GlobalCollection::new();

    let barrier = Barrier::new(4);
    let barrier = &barrier;

    std::thread::scope(|s| {
        for entry in [&E1, &E2, &E3, &E4] {
            s.spawn(move || {
                barrier.wait();
                COLLECTION.register(entry);
            });
        }
    });

    let mut values = collect_all(&COLLECTION);
    values.sort();
    assert_eq!(values, vec![10, 20, 30, 40]);
}

#[test]
fn iterate_while_registering() {
    static E1: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&1);
    static E2: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&2);

    static COLLECTION: GlobalCollection<u32> = GlobalCollection::new();

    COLLECTION.register(&E1);

    // Start iterating - sees E1
    let mut iter = GlobalCollectionIter::new(&COLLECTION);
    assert_eq!(iter.next(), Some(&1));

    // Register E2 concurrently - iterator already captured the snapshot via head
    COLLECTION.register(&E2);

    // Iterator should terminate cleanly (E1.next was None at snapshot time)
    assert_eq!(iter.next(), None);

    // A fresh iterator sees both
    let mut values = collect_all(&COLLECTION);
    values.sort();
    assert_eq!(values, vec![1, 2]);
}

#[test]
fn concurrent_register_and_iterate() {
    static E1: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&100);
    static E2: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&200);
    static E3: GlobalCollectionEntry<u32> = GlobalCollectionEntry::new(&300);

    static COLLECTION: GlobalCollection<u32> = GlobalCollection::new();

    // Pre-register one so iterators always find something
    COLLECTION.register(&E1);

    let barrier = Barrier::new(3);
    let barrier = &barrier;

    std::thread::scope(|s| {
        // Two threads registering
        s.spawn(move || {
            barrier.wait();
            COLLECTION.register(&E2);
        });
        s.spawn(move || {
            barrier.wait();
            COLLECTION.register(&E3);
        });
        // One thread iterating repeatedly
        s.spawn(move || {
            barrier.wait();
            for _ in 0..10 {
                let values: Vec<_> = GlobalCollectionIter::new(&COLLECTION).copied().collect();
                // Must always contain E1, and all values must be from our set
                assert!(values.contains(&100));
                for v in &values {
                    assert!([100, 200, 300].contains(v));
                }
            }
        });
    });
}
