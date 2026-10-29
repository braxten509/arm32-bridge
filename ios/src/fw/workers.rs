//! A few drawing threads kept for the whole run: big pictures are drawn (and
//! textures unpacked) a band of rows per core without starting new threads
//! each time, which cost more than the drawing in busy screens (a fight
//! redraws grouped pictures every frame).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Pool {
    jobs: Mutex<mpsc::Sender<Job>>,
    threads: usize,
}

thread_local! {
    /// This thread is one of the pool's (its own splits run in place).
    static IN_POOL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8).saturating_sub(1);
        let (send, receive) = mpsc::channel::<Job>();
        let receive = Arc::new(Mutex::new(receive));
        for i in 0..threads {
            let receive = receive.clone();
            let _ = std::thread::Builder::new().name(format!("ios-draw-{i}")).spawn(move || {
                IN_POOL.with(|p| p.set(true));
                loop {
                    let job = receive.lock().unwrap_or_else(|e| e.into_inner()).recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => return,
                    }
                }
            });
        }
        Pool { jobs: Mutex::new(send), threads }
    })
}

thread_local! {
    /// How long this thread has waited for the pool's parts after doing its own (IOS_DRAW_LOG).
    pub static WAITED: std::cell::Cell<std::time::Duration> = const { std::cell::Cell::new(std::time::Duration::ZERO) };
}

/// Cores `split` can use.
pub fn cores() -> usize {
    pool().threads + 1
}

/// One `split` call's pieces: taken in turn by whichever thread is free.
struct Pieces {
    /// Each piece's first item (an address) and length.
    list: Vec<(usize, usize)>,
    /// The next piece to take.
    next: AtomicUsize,
    /// Pieces finished (and whether one panicked).
    done: Mutex<(usize, bool)>,
    all_done: Condvar,
}

impl Pieces {
    /// Takes and works pieces until none are left.
    ///
    /// Safety: `work` must point to the call's work for items of type T. It
    /// is only used once a piece is taken: the call waits until every taken
    /// piece is done, but a job that starts after the call has returned
    /// finds none left and must not touch `work`.
    unsafe fn work_on<'w, T>(&self, work: *const (dyn Fn(&mut [T]) + Sync + 'w)) {
        loop {
            let at = self.next.fetch_add(1, Ordering::Relaxed);
            let Some(&(start, len)) = self.list.get(at) else { return };
            let work = unsafe { &*work };
            let piece = unsafe { std::slice::from_raw_parts_mut(start as *mut T, len) };
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(piece))).is_err();
            let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
            done.0 += 1;
            done.1 |= panicked;
            if done.0 == self.list.len() {
                self.all_done.notify_all();
            }
        }
    }
}

/// `work` on runs of `items`, on up to `parts` cores at once (the pool's
/// threads and this one); returns when all are done. The runs are small
/// and taken in turn, so a slower core (the phone's little ones) or one
/// that wakes late takes fewer, and this thread waits only for runs
/// already started, never for a core that has not begun.
pub fn split<T: Send>(items: &mut [T], parts: usize, work: &(dyn Fn(&mut [T]) + Sync)) {
    let parts = parts.min(cores()).min(items.len()).max(1);
    if parts == 1 || IN_POOL.with(|p| p.get()) {
        work(items);
        return;
    }
    let per_piece = items.len().div_ceil(parts * PIECES_PER_PART);
    let list = items.chunks_mut(per_piece).map(|c| (c.as_mut_ptr() as usize, c.len())).collect();
    let pieces = Arc::new(Pieces { list, next: AtomicUsize::new(0), done: Mutex::new((0, false)), all_done: Condvar::new() });
    {
        let jobs = pool().jobs.lock().unwrap_or_else(|e| e.into_inner());
        let work_at = work as *const (dyn Fn(&mut [T]) + Sync);
        // Safety: a job only turns `work` back into a reference once it has
        // taken a piece, and this call waits (below) until every taken piece
        // is done.
        let [data, vtable]: [usize; 2] = unsafe { std::mem::transmute::<*const (dyn Fn(&mut [T]) + Sync), [usize; 2]>(work_at) };
        for _ in 1..parts {
            let pieces = pieces.clone();
            let job: Box<dyn FnOnce() + Send> = Box::new(move || {
                let work = unsafe { std::mem::transmute::<[usize; 2], *const (dyn Fn(&mut [T]) + Sync)>([data, vtable]) };
                unsafe { pieces.work_on(work) };
            });
            let job: Job = unsafe { std::mem::transmute::<Box<dyn FnOnce() + Send + '_>, Job>(job) };
            if let Err(mpsc::SendError(job)) = jobs.send(job) {
                job();
            }
        }
    }
    unsafe { pieces.work_on(work as *const (dyn Fn(&mut [T]) + Sync + '_)) };
    let began = std::time::Instant::now();
    let mut done = pieces.done.lock().unwrap_or_else(|e| e.into_inner());
    while done.0 < pieces.list.len() {
        done = pieces.all_done.wait(done).unwrap_or_else(|e| e.into_inner());
    }
    WAITED.with(|w| w.set(w.get() + began.elapsed()));
    if done.1 {
        panic!("a drawing part failed");
    }
}

/// Runs per core a `split` is cut into.
const PIECES_PER_PART: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    /// When the pool's cores are slower (the phone's little ones) or wake
    /// late, the calling thread takes on more than an equal share instead
    /// of waiting for theirs.
    #[test]
    fn the_caller_takes_up_the_work_of_slow_cores() {
        let mut items: Vec<u32> = (0..64).collect();
        let mine = std::sync::atomic::AtomicUsize::new(0);
        split(&mut items, 8, &|part| {
            if IN_POOL.with(|p| p.get()) {
                std::thread::sleep(std::time::Duration::from_millis(5 * part.len() as u64));
            } else {
                mine.fetch_add(part.len(), std::sync::atomic::Ordering::Relaxed);
            }
            part.iter_mut().for_each(|x| *x += 1);
        });
        assert!(items.iter().enumerate().all(|(i, &x)| x == i as u32 + 1));
        let mine = mine.into_inner();
        assert!(mine > 32, "the caller did {mine} of 64");
    }

    #[test]
    fn every_item_is_worked_once_and_no_thread_is_started_per_call() {
        let mut items: Vec<u32> = (0..1000).collect();
        split(&mut items, 8, &|part| part.iter_mut().for_each(|x| *x += 1));
        assert!(items.iter().enumerate().all(|(i, &x)| x == i as u32 + 1));
        // The pool's threads stay the same from call to call.
        let names = Mutex::new(std::collections::HashSet::new());
        for _ in 0..50 {
            split(&mut items, 8, &|part| {
                names.lock().unwrap().insert(std::thread::current().id());
                part.iter_mut().for_each(|x| *x += 1);
            });
        }
        assert!(names.into_inner().unwrap().len() <= cores());
        assert!(items.iter().enumerate().all(|(i, &x)| x == i as u32 + 51));
    }
}
