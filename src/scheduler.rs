use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use work_steal_queue::LocalQueue;

#[repr(C)]
#[derive(Debug)]
pub struct Scheduler<'s> {
    id: usize,
    ready: LocalQueue<'s, &'static mut c_void>,
    scheduling: AtomicBool,
}
