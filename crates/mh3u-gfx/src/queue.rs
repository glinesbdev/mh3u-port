//! The GPU queue, with its CPU-heavy part on a thread of its own.
//!
//! wgpu does most of a batch's CPU work when its encoder is finished (checking, tracking and the backend's commands) and when the queue
//! submits and presents. None of that reads guest memory, so it can run beside the game's code: this queue takes the batches (encoders
//! not yet finished), the buffer writes, the submits and the presents in the order the game's thread makes them, and a thread finishes and
//! does them in that order. Whatever the queue does not wrap (`write_texture` into a texture just made, say) reaches the real queue at
//! once through `Deref`; those writes are into resources no earlier batch uses.
//!
//! `MH3U_GFX_THREAD=off` does everything on the calling thread, as before.

use std::ops::Deref;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};

enum Op {
    Write(wgpu::Buffer, u64, Vec<u8>),
    Submit(Vec<wgpu::CommandBuffer>),
    Encode(Vec<wgpu::CommandEncoder>),
    Present(wgpu::SurfaceTexture),
    Barrier(std::sync::mpsc::Sender<()>),
}

/// Frames handed to the thread to present and not presented yet.
type Presents = Arc<(Mutex<u32>, Condvar)>;

#[derive(Clone)]
pub struct OrderedQueue {
    raw: wgpu::Queue,
    tx: Option<SyncSender<Op>>,
    presents: Presents,
}

impl Deref for OrderedQueue {
    type Target = wgpu::Queue;
    fn deref(&self) -> &wgpu::Queue {
        &self.raw
    }
}

impl OrderedQueue {
    pub fn new(raw: wgpu::Queue, threaded: bool) -> OrderedQueue {
        let presents: Presents = Arc::new((Mutex::new(0), Condvar::new()));
        let threaded = threaded && std::env::var("MH3U_GFX_THREAD").map_or(true, |v| v != "off");
        let tx = threaded.then(|| {
            // a few batches' worth: the game's thread waits here if the queue's thread is far behind
            let (tx, rx) = sync_channel(256);
            let (raw, presents) = (raw.clone(), presents.clone());
            std::thread::Builder::new()
                .name("mh3u gpu queue".into())
                .spawn(move || run(raw, rx, presents))
                .expect("the GPU queue thread");
            eprintln!("[gfx] GPU queue on its own thread (MH3U_GFX_THREAD=off to use the game's)");
            tx
        });
        OrderedQueue { raw, tx, presents }
    }

    fn send(&self, op: Op) {
        self.tx
            .as_ref()
            .expect("a threaded queue")
            .send(op)
            .expect("the GPU queue thread stopped");
    }

    pub fn write_buffer(&self, buffer: &wgpu::Buffer, offset: u64, data: &[u8]) {
        if self.tx.is_some() {
            self.send(Op::Write(buffer.clone(), offset, data.to_vec()));
        } else {
            self.raw.write_buffer(buffer, offset, data);
        }
    }

    pub fn submit<I: IntoIterator<Item = wgpu::CommandBuffer>>(&self, buffers: I) {
        if self.tx.is_some() {
            self.send(Op::Submit(buffers.into_iter().collect()));
        } else {
            self.raw.submit(buffers);
        }
    }

    /// Finishes the encoders and submits what they made: the part of a batch that costs the CPU most.
    pub fn encode_and_submit(&self, encoders: Vec<wgpu::CommandEncoder>) {
        if self.tx.is_some() {
            self.send(Op::Encode(encoders));
        } else {
            self.raw.submit(encoders.into_iter().map(|e| e.finish()));
        }
    }

    pub fn present(&self, frame: wgpu::SurfaceTexture) {
        if self.tx.is_some() {
            *self.presents.0.lock().unwrap() += 1;
            self.send(Op::Present(frame));
        } else {
            self.raw.present(frame);
        }
    }

    /// Waits until the frames handed over have been presented (the surface gives out its next texture only after that).
    pub fn wait_presented(&self) {
        if self.tx.is_some() {
            let (count, done) = &*self.presents;
            let mut n = count.lock().unwrap();
            while *n > 0 {
                n = done.wait(n).unwrap();
            }
        }
    }

    /// Waits until everything handed over so far has been done (before the game's thread reads or reconfigures what it touches).
    pub fn flush(&self) {
        if self.tx.is_some() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.send(Op::Barrier(tx));
            let _ = rx.recv();
        }
    }
}

fn run(raw: wgpu::Queue, rx: Receiver<Op>, presents: Presents) {
    while let Ok(op) = rx.recv() {
        match op {
            Op::Write(buffer, offset, data) => raw.write_buffer(&buffer, offset, &data),
            Op::Submit(buffers) => {
                raw.submit(buffers);
            }
            Op::Encode(encoders) => {
                raw.submit(encoders.into_iter().map(|e| e.finish()));
            }
            Op::Present(frame) => {
                raw.present(frame);
                let (count, done) = &*presents;
                *count.lock().unwrap() -= 1;
                done.notify_all();
            }
            Op::Barrier(reply) => {
                let _ = reply.send(());
            }
        }
    }
}
