#![cfg(feature = "capture")]

//! End-to-end: a real mesh captured through [`BufferedCaptureSink`], whose bytes
//! decode back into the spawn / message / death events that occurred.

use std::io::Write;
use std::sync::{Arc, Mutex};

use factories::actor::Actor;
use factories::capture::{CAPTURE_SINK, CaptureSink};
use factories::capture_codec::segment::Record;
use factories::capture_codec::stream::{read_stream_header, segments};
use factories::capture_sink::BufferedCaptureSink;
use factories::runtime::lock::UnguardedLock;
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::TokioTaskSpawner;
use factories::spawn::ActorLauncher;

/// A `Write` whose bytes we can read back once the capture is flushed.
#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Node;

#[factories::messages]
impl Node {
    #[handler]
    async fn ping(&self) {}
}

#[tokio::test]
async fn buffered_sink_writes_a_decodable_capture() {
    // `#[tokio::test]` is single-threaded, so the actor loop runs on this thread
    // and records into this thread's buffer - which `flush()` then drains.
    let raw = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(BufferedCaptureSink::new(SharedWriter(raw.clone())));
    let sink_dyn: Arc<dyn CaptureSink> = sink.clone();

    let spawner = TokioTaskSpawner::current();
    let handle = ActorLauncher::default()
        .with_extension(CAPTURE_SINK, sink_dyn)
        .spawn_ready(&spawner, Node)
        .await
        .expect("init");

    handle.ping().await.expect("ask"); // external -> Node, an Ask message
    let state = handle.state().clone();
    drop(handle); // close the mailbox so the actor drains and stops
    state.wait_for_terminal().await;

    sink.flush();

    let bytes = raw.lock().expect("writer mutex").clone();
    let body = read_stream_header(&bytes).expect("valid stream header");
    let decoded: Vec<_> = segments(body)
        .collect::<Result<_, _>>()
        .expect("all segments decode");

    let (mut saw_spawn, mut saw_ping, mut saw_death) = (false, false, false);
    for segment in &decoded {
        let name = |idx: u32| segment.strings[idx as usize].as_str();
        for record in &segment.records {
            match record {
                Record::Spawned { actor_type, .. } if name(*actor_type) == "Node" => {
                    saw_spawn = true;
                }
                Record::Message { message_type, .. } if name(*message_type) == "Ping" => {
                    saw_ping = true;
                }
                Record::Died { actor_type, .. } if name(*actor_type) == "Node" => {
                    saw_death = true;
                }
                _ => {}
            }
        }
    }

    assert!(saw_spawn, "Node's spawn should be captured: {decoded:?}");
    assert!(saw_ping, "the ping message edge should be captured: {decoded:?}");
    assert!(saw_death, "Node's death should be captured: {decoded:?}");
}
