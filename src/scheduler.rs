use crate::co;
use crate::coroutine::{ScopedCoroutine, State};
use std::ffi::c_void;
use std::sync::atomic::AtomicBool;
use work_steal_queue::WorkStealQueue;

/// 根协程
// type RootCoroutine<'c, 's> = ScopedCoroutine<'c, 's, (), (), ()>;

/// 用户协程
pub type SchedulableCoroutine = ScopedCoroutine<'static, 'static, (), (), &'static mut c_void>;

#[repr(C)]
#[derive(Debug)]
pub struct Scheduler<'s> {
    name: &'s str,
    scheduling: AtomicBool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use genawaiter::GeneratorState;

    fn result(result: usize) -> &'static mut c_void {
        unsafe { std::mem::transmute(result) }
    }

    #[test]
    fn base() {
        let queue = WorkStealQueue::default();
        let ready = queue.local_queue();

        let co1: SchedulableCoroutine = co!(|_| async move {
            println!("1");
            result(1)
        });
        co1.set_state(State::Ready);

        let co2: SchedulableCoroutine = co!(|_| async move {
            println!("2");
            result(2)
        });
        co2.set_state(State::Ready);

        ready.push_back(co1);
        ready.push_back(co2);
        for i in 1..=2 {
            let co = ready.pop_front().unwrap();
            match co.resume() {
                GeneratorState::Yielded(_) => panic!(),
                GeneratorState::Complete(v) => {
                    assert_eq!(v as *mut c_void as usize, i);
                }
            }
        }
    }
}
