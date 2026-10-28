//! Dispatch groups: a count of work not finished yet, waits for it to reach
//! zero, and blocks sent to their queues when it does (dispatch_group_notify).
//! dispatch_group_async counts its block in and out around running it on
//! the queue. Their copies of the game's blocks are released once run.

use crate::libsystem::mach::{self, BlockHeader};
use crate::symbols::{leak, Registry};
use std::sync::{Condvar, Mutex};

const BLOCK_NEEDS_FREE: i32 = 1 << 24;
const BLOCK_HAS_COPY_DISPOSE: i32 = 1 << 25;

struct Group {
    state: Mutex<GroupState>,
    empty: Condvar,
}

#[derive(Default)]
struct GroupState {
    count: i64,
    /// How many times the count reached zero: a wait ends once this moves,
    /// even if new work entered before the waiter woke (as libdispatch's).
    emptied: u64,
    /// (queue, copied block) to send once the count is zero.
    notify: Vec<(usize, usize)>,
}

fn group(group: usize) -> &'static Group {
    unsafe { &*(group as *const Group) }
}

/// Groups are never freed (dispatch_release does nothing here).
pub(crate) extern "C" fn dispatch_group_create() -> usize {
    Box::leak(Box::new(Group { state: Mutex::new(GroupState::default()), empty: Condvar::new() })) as *const Group as usize
}

extern "C" fn dispatch_group_enter(handle: usize) {
    group(handle).state.lock().unwrap().count += 1;
}

extern "C" fn dispatch_group_leave(handle: usize) {
    let group = group(handle);
    let ready = {
        let mut state = group.state.lock().unwrap();
        state.count -= 1;
        if state.count < 0 {
            crate::log!("dispatch_group_leave without a matching enter (Apple stops the program here); count kept at 0");
            state.count = 0;
        }
        if state.count > 0 {
            return;
        }
        state.emptied += 1;
        std::mem::take(&mut state.notify)
    };
    group.empty.notify_all();
    for (queue, block) in ready {
        mach::send_copied(queue, block);
    }
}

/// 0 once the group is (or has been, since the call) empty, KERN_OPERATION_TIMED_OUT if `timeout` passed first.
pub(crate) extern "C" fn dispatch_group_wait(handle: usize, timeout: u64) -> isize {
    let group = group(handle);
    let deadline = mach::deadline_nanos(timeout);
    let mut state = group.state.lock().unwrap();
    if state.count == 0 {
        return 0;
    }
    let started = state.emptied;
    while state.emptied == started {
        match deadline {
            None => state = group.empty.wait(state).unwrap(),
            Some(at) => {
                let now = mach::now_nanos();
                if now >= at {
                    return mach::TIMED_OUT as isize;
                }
                state = group.empty.wait_timeout(state, std::time::Duration::from_nanos(at - now)).unwrap().0;
            }
        }
    }
    0
}

pub(crate) extern "C" fn dispatch_group_notify(handle: usize, queue: usize, block: usize) {
    let copy = mach::block_copy(block as *mut BlockHeader) as usize;
    let mut state = group(handle).state.lock().unwrap();
    if state.count == 0 {
        drop(state);
        mach::send_copied(queue, copy);
    } else {
        state.notify.push((queue, copy));
    }
}

/// The block dispatch_group_async sends (a heap block the queue releases
/// once run): runs the work, releases it, then leaves the group, in
/// libdispatch's order.
#[repr(C)]
struct GroupBlock {
    header: BlockHeader,
    group: usize,
    /// The copied game block, 0 once it ran.
    work: usize,
}

extern "C" fn run_group_block(block: usize) {
    let block = block as *mut GroupBlock;
    let (work, group) = unsafe { (std::mem::replace(&mut (*block).work, 0), (*block).group) };
    unsafe { mach::call_block(work) };
    mach::block_release(work as *mut BlockHeader);
    dispatch_group_leave(group);
}

extern "C" fn copy_group_block(_destination: usize, _source: usize) {}

/// Its last release: the work, if it never ran, goes too.
extern "C" fn dispose_group_block(block: usize) {
    mach::block_release(unsafe { (*(block as *const GroupBlock)).work } as *mut BlockHeader);
}

pub(crate) extern "C" fn dispatch_group_async(handle: usize, queue: usize, block: usize) {
    static DESCRIPTOR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let descriptor = *DESCRIPTOR.get_or_init(|| {
        leak([0usize, std::mem::size_of::<GroupBlock>(), copy_group_block as usize, dispose_group_block as usize])
    }) as *const [usize; 4];
    let isa = mach::MALLOC_BLOCK_CLASS.load(std::sync::atomic::Ordering::Relaxed);
    dispatch_group_enter(handle);
    let runner = GroupBlock {
        header: BlockHeader { isa, flags: BLOCK_NEEDS_FREE | BLOCK_HAS_COPY_DISPOSE | 2, reserved: 0, invoke: run_group_block as usize, descriptor },
        group: handle,
        work: mach::block_copy(block as *mut BlockHeader) as usize,
    };
    let copy = unsafe { libc::malloc(std::mem::size_of::<GroupBlock>()) as *mut GroupBlock };
    unsafe { copy.write(runner) };
    mach::send_copied(queue, copy as usize);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_wait_ends_when_its_work_finished_even_if_new_work_entered_since() {
        let group = dispatch_group_create();
        dispatch_group_enter(group);
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(dispatch_group_wait(group, !0)).unwrap());
        std::thread::sleep(Duration::from_millis(50)); // the waiter sleeps at count 1
        // The group empties, then new work enters before the waiter wakes up.
        dispatch_group_leave(group);
        dispatch_group_enter(group);
        let answer = finished.recv_timeout(Duration::from_millis(500));
        dispatch_group_leave(group);
        assert_eq!(answer, Ok(0), "the wait ends: the group was empty after it began");
    }

    /// A wait that times out answers KERN_OPERATION_TIMED_OUT (49), as Apple's.
    #[test]
    fn a_timed_out_wait_answers_49() {
        let group = dispatch_group_create();
        dispatch_group_enter(group);
        assert_eq!(dispatch_group_wait(group, mach::now_nanos() + 1_000_000), 49);
        dispatch_group_leave(group);
    }
}

pub fn register(r: &mut Registry) {
    r.function("dispatch_group_create", dispatch_group_create as usize);
    r.function("dispatch_group_enter", dispatch_group_enter as usize);
    r.function("dispatch_group_leave", dispatch_group_leave as usize);
    r.function("dispatch_group_wait", dispatch_group_wait as usize);
    r.function("dispatch_group_notify", dispatch_group_notify as usize);
    r.function("dispatch_group_async", dispatch_group_async as usize);
}
