//! Dispatch groups: a count of work not finished yet, waits for it to reach
//! zero, and blocks sent to their queues when it does (dispatch_group_notify).
//! dispatch_group_async counts its block in and out around running it on
//! the queue.

use crate::libsystem::mach::{self, BlockHeader};
use crate::symbols::{leak, Registry};
use std::sync::{Condvar, Mutex};

const BLOCK_IS_GLOBAL: i32 = 1 << 28;

struct Group {
    state: Mutex<GroupState>,
    empty: Condvar,
}

#[derive(Default)]
struct GroupState {
    count: i64,
    /// (queue, copied block) to send once the count is zero.
    notify: Vec<(usize, usize)>,
}

fn group(group: usize) -> &'static Group {
    unsafe { &*(group as *const Group) }
}

/// Groups are never freed (dispatch_release does nothing here).
extern "C" fn dispatch_group_create() -> usize {
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
        std::mem::take(&mut state.notify)
    };
    group.empty.notify_all();
    for (queue, block) in ready {
        mach::dispatch_async(queue, block);
    }
}

/// 0 once the group is empty, non-zero if `timeout` passed first.
extern "C" fn dispatch_group_wait(handle: usize, timeout: u64) -> isize {
    let group = group(handle);
    let deadline = mach::deadline_nanos(timeout);
    let mut state = group.state.lock().unwrap();
    while state.count > 0 {
        match deadline {
            None => state = group.empty.wait(state).unwrap(),
            Some(at) => {
                let now = mach::now_nanos();
                if now >= at {
                    return 1;
                }
                state = group.empty.wait_timeout(state, std::time::Duration::from_nanos(at - now)).unwrap().0;
            }
        }
    }
    0
}

extern "C" fn dispatch_group_notify(handle: usize, queue: usize, block: usize) {
    let copy = mach::block_copy(block as *mut BlockHeader) as usize;
    let mut state = group(handle).state.lock().unwrap();
    if state.count == 0 {
        drop(state);
        mach::dispatch_async(queue, copy);
    } else {
        state.notify.push((queue, copy));
    }
}

/// The block dispatch_group_async sends: runs the work, then leaves the group.
#[repr(C)]
struct GroupBlock {
    header: BlockHeader,
    group: usize,
    work: usize,
}

extern "C" fn run_group_block(block: usize) {
    let block = unsafe { Box::from_raw(block as *mut GroupBlock) };
    unsafe { mach::call_block(block.work) };
    dispatch_group_leave(block.group);
}

extern "C" fn dispatch_group_async(handle: usize, queue: usize, block: usize) {
    static DESCRIPTOR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let descriptor = *DESCRIPTOR.get_or_init(|| leak([0usize, std::mem::size_of::<GroupBlock>(), 0, 0])) as *const [usize; 4];
    let isa = crate::symbols::lookup("__NSConcreteGlobalBlock").map_or(0, |(address, _)| address);
    dispatch_group_enter(handle);
    // Global, so the queue doesn't copy it; it frees itself once it has run.
    let runner = Box::new(GroupBlock {
        header: BlockHeader { isa, flags: BLOCK_IS_GLOBAL, reserved: 0, invoke: run_group_block as usize, descriptor },
        group: handle,
        work: mach::block_copy(block as *mut BlockHeader) as usize,
    });
    mach::dispatch_async(queue, Box::into_raw(runner) as usize);
}

pub fn register(r: &mut Registry) {
    r.function("dispatch_group_create", dispatch_group_create as usize);
    r.function("dispatch_group_enter", dispatch_group_enter as usize);
    r.function("dispatch_group_leave", dispatch_group_leave as usize);
    r.function("dispatch_group_wait", dispatch_group_wait as usize);
    r.function("dispatch_group_notify", dispatch_group_notify as usize);
    r.function("dispatch_group_async", dispatch_group_async as usize);
}
