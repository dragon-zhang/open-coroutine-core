use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use work_steal_queue::LocalQueue;

#[repr(C)]
#[derive(Debug)]
pub struct Scheduler {
    id: usize,
    ready: LocalQueue<'static, &'static mut c_void>,
    scheduling: AtomicBool,
}
