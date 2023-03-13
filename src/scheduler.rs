use crate::co;
use crate::coroutine::suspend::Suspender;
use crate::coroutine::{GeneratorState, ScopedCoroutine, State};
use once_cell::sync::Lazy;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::future::Future;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};
use timer_utils::TimerList;
use uuid::Uuid;
use work_steal_queue::{LocalQueue, WorkStealQueue};

/// 根协程
type RootCoroutine<'c> = ScopedCoroutine<'c, 'static, *const Scheduler<'static>, (), ()>;

/// 用户协程
pub type SchedulableCoroutine = ScopedCoroutine<'static, 'static, (), (), &'static mut c_void>;

static QUEUE: Lazy<WorkStealQueue<SchedulableCoroutine>> = Lazy::new(WorkStealQueue::default);

static mut SUSPEND_TABLE: Lazy<TimerList<SchedulableCoroutine>> = Lazy::new(TimerList::new);

static mut SYSTEM_CALL_TABLE: Lazy<HashMap<&str, SchedulableCoroutine>> = Lazy::new(HashMap::new);

#[repr(C)]
#[derive(Debug)]
pub struct Scheduler<'s> {
    name: &'s str,
    ready: LocalQueue<'static, SchedulableCoroutine>,
    scheduling: AtomicBool,
}

impl Drop for Scheduler<'_> {
    fn drop(&mut self) {
        assert!(
            self.ready.is_empty(),
            "there are still tasks to be carried out !"
        );
    }
}

thread_local! {
    static YIELDER: Box<RefCell<MaybeUninit<Suspender<'static,(), *const Scheduler<'static>>>>> = Box::new(RefCell::new(MaybeUninit::uninit()));
    static TIMEOUT_TIME: Box<RefCell<u64>> = Box::new(RefCell::new(0));
}

impl<'s> Scheduler<'s> {
    pub fn new() -> Self {
        Scheduler {
            name: Box::leak(Box::from(Uuid::new_v4().to_string())),
            ready: QUEUE.local_queue(),
            scheduling: AtomicBool::new(false),
        }
    }

    pub fn with_name(name: &'s str) -> Self {
        Scheduler {
            name: Box::leak(Box::from(name)),
            ready: QUEUE.local_queue(),
            scheduling: AtomicBool::new(false),
        }
    }

    fn init_yielder(yielder: Suspender<'static, (), *const Scheduler>) {
        YIELDER.with(|boxed| {
            let _ = boxed.replace(MaybeUninit::new(yielder));
        });
    }

    fn yielder() -> Suspender<'static, (), *const Scheduler<'static>> {
        YIELDER.with(|boxed| unsafe { (**boxed).replace(MaybeUninit::uninit()).assume_init() })
    }

    fn init_timeout_time(timeout_time: u64) {
        TIMEOUT_TIME.with(|boxed| {
            *boxed.borrow_mut() = timeout_time;
        });
    }

    fn timeout_time() -> u64 {
        TIMEOUT_TIME.with(|boxed| *boxed.borrow_mut())
    }

    fn clean_time() {
        TIMEOUT_TIME.with(|boxed| *boxed.borrow_mut() = 0)
    }

    fn back_to_main() {
        //跳回主线程
        //fixme
        let _ = Scheduler::yielder().suspend(());
    }

    pub(crate) fn do_schedule(&self) {
        if Scheduler::timeout_time() <= timer_utils::now() {
            Scheduler::back_to_main()
        }
        //self.check_ready().unwrap();
        match self.ready.pop_front() {
            Some(coroutine) => {
                // let start = timer_utils::get_timeout_time(Duration::from_millis(10));
                // Monitor::add_task(start);
                //see OpenCoroutine::child_context_func
                match coroutine.resume() {
                    GeneratorState::Yielded(()) => {
                        match coroutine.get_state() {
                            State::SystemCall => unsafe {
                                SYSTEM_CALL_TABLE
                                    .insert(Box::leak(Box::from(coroutine.get_name())), coroutine);
                            },
                            State::Suspend(delay_time) => {
                                if delay_time > 0 {
                                    //挂起协程到时间轮
                                    unsafe {
                                        SUSPEND_TABLE.insert(
                                            timer_utils::add_timeout_time(delay_time),
                                            coroutine,
                                        );
                                    }
                                } else {
                                    //放入就绪队列尾部
                                    self.ready.push_back(coroutine);
                                }
                            }
                            _ => unreachable!(),
                        };
                    }
                    GeneratorState::Complete(_) => unreachable!("never have a result"),
                };
                self.do_schedule();
            }
            None => Scheduler::back_to_main(),
        }
    }
}

impl Scheduler<'static> {
    pub fn submit<F>(
        &'static self,
        f: impl FnOnce(Suspender<'static, (), ()>) -> F + 'static,
    ) -> &str
    where
        F: Future<Output = &'static mut c_void>,
    {
        let coroutine = SchedulableCoroutine::new(
            Box::from(self.name.to_owned() + &Uuid::new_v4().to_string()),
            f,
        );
        coroutine.set_state(State::Ready);
        let _ = coroutine.set_scheduler(self);
        let co_name = Box::leak(Box::from(coroutine.get_name()));
        self.ready.push_back(coroutine);
        co_name
    }

    pub fn try_timeout_schedule(&self, timeout_time: u64) {
        if self
            .scheduling
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        Scheduler::init_timeout_time(timeout_time);
        let main: RootCoroutine = co!(
            "root-coroutine",
            |yielder: Suspender<'static, _, _>| async move {
                let scheduler = yielder.suspend(()).await;
                Scheduler::init_yielder(yielder);
                unsafe { (*scheduler).do_schedule() };
                unreachable!("should not execute to here !")
            },
        );
        assert_eq!(main.resume_with(self), GeneratorState::Yielded(()));
        assert_eq!(main.resume_with(self), GeneratorState::Yielded(()));
        Scheduler::clean_time();
        self.scheduling.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_simple() {
        let scheduler = Box::leak(Box::new(Scheduler::new()));
        let _ = scheduler.submit(|_| async move {
            println!("1");
            result(1)
        });
        let _ = scheduler.submit(|_| async move {
            println!("2");
            result(2)
        });
        scheduler.try_timeout_schedule(u64::MAX);
    }
}
