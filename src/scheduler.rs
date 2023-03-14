use crate::coroutine::suspend::Suspender;
use crate::coroutine::{GeneratorState, ScopedCoroutine, State};
use crate::monitor::Monitor;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::ffi::c_void;
use std::future::Future;
use std::time::Duration;
use timer_utils::TimerList;
use uuid::Uuid;
use work_steal_queue::{LocalQueue, WorkStealQueue};

/// 用户协程
pub type SchedulableCoroutine = ScopedCoroutine<'static, 'static, (), (), &'static mut c_void>;

static QUEUE: Lazy<WorkStealQueue<SchedulableCoroutine>> = Lazy::new(WorkStealQueue::default);

static mut SUSPEND_TABLE: Lazy<TimerList<SchedulableCoroutine>> = Lazy::new(TimerList::new);

#[allow(dead_code)]
static mut SYSTEM_CALL_TABLE: Lazy<HashMap<&str, SchedulableCoroutine>> = Lazy::new(HashMap::new);

static mut RESULT_TABLE: Lazy<HashMap<&str, &'static mut c_void>> = Lazy::new(HashMap::new);

#[repr(C)]
#[derive(Debug)]
pub struct Scheduler<'s> {
    name: &'s str,
    ready: LocalQueue<'static, SchedulableCoroutine>,
}

impl Drop for Scheduler<'_> {
    fn drop(&mut self) {
        assert!(
            self.ready.is_empty(),
            "there are still tasks to be carried out !"
        );
    }
}

impl<'s> Scheduler<'s> {
    pub fn new() -> Self {
        Scheduler {
            name: Box::leak(Box::from(Uuid::new_v4().to_string())),
            ready: QUEUE.local_queue(),
        }
    }

    pub fn with_name(name: &'s str) -> Self {
        Scheduler {
            name: Box::leak(Box::from(name)),
            ready: QUEUE.local_queue(),
        }
    }

    pub fn try_schedule(&self) {
        self.try_timeout_schedule(Duration::MAX.as_secs())
    }

    pub fn try_timed_schedule(&self, time: Duration) {
        self.try_timeout_schedule(timer_utils::get_timeout_time(time))
    }

    pub fn try_timeout_schedule(&self, timeout_time: u64) {
        self.check_ready();
        loop {
            if timeout_time <= timer_utils::now() {
                return;
            }
            match self.ready.pop_front() {
                Some(coroutine) => {
                    let start = timer_utils::get_timeout_time(Duration::from_millis(10));
                    Monitor::add_task(start);
                    match coroutine.resume() {
                        GeneratorState::Yielded(()) => {
                            match coroutine.get_state() {
                                State::Suspend(delay_time) => {
                                    if delay_time > 0 {
                                        //挂起协程到时间轮
                                        let time = timer_utils::add_timeout_time(delay_time);
                                        unsafe { SUSPEND_TABLE.insert(time, coroutine) };
                                    } else {
                                        //放入就绪队列尾部
                                        self.ready.push_back(coroutine);
                                    }
                                }
                                _ => unreachable!(),
                            };
                        }
                        GeneratorState::Complete(r) => {
                            let name = Box::leak(Box::from(coroutine.get_name()));
                            let _ = unsafe { RESULT_TABLE.insert(name, r) };
                        }
                    };
                    //还没执行到10ms就主动yield或者执行完毕了，此时需要清理signal
                    //否则下一个协程执行不到10ms就会被抢占调度
                    Monitor::clean_task(start);
                }
                None => return,
            }
        }
    }

    fn check_ready(&self) {
        unsafe {
            for _ in 0..SUSPEND_TABLE.len() {
                if let Some(entry) = SUSPEND_TABLE.front() {
                    let exec_time = entry.get_time();
                    if timer_utils::now() < exec_time {
                        break;
                    }
                    //移动至"就绪"队列
                    if let Some(mut entry) = SUSPEND_TABLE.pop_front() {
                        for _ in 0..entry.len() {
                            if let Some(coroutine) = entry.pop_front() {
                                coroutine.set_state(State::Ready);
                                //把到时间的协程加入就绪队列
                                self.ready.push_back(coroutine);
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn get_result(co_name: &'static str) -> Option<&'static mut c_void> {
        unsafe { RESULT_TABLE.remove(&co_name) }
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
            Box::from(format!("{}@{}", self.name, Uuid::new_v4().to_string())),
            f,
        );
        coroutine.set_state(State::Ready);
        let _ = coroutine.set_scheduler(self);
        let co_name = Box::leak(Box::from(coroutine.get_name()));
        self.ready.push_back(coroutine);
        co_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::co;

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
        scheduler.try_schedule();
    }

    #[test]
    fn with_suspend() {
        let scheduler = Box::leak(Box::new(Scheduler::new()));
        let _ = scheduler.submit(|yielder| async move {
            println!("[coroutine1] suspend");
            yielder.suspend(()).await;
            println!("[coroutine1] back");
            result(1)
        });
        let _ = scheduler.submit(|yielder| async move {
            println!("[coroutine2] suspend");
            yielder.suspend(()).await;
            println!("[coroutine2] back");
            result(2)
        });
        scheduler.try_schedule();
    }

    #[test]
    fn with_delay() {
        let scheduler = Box::leak(Box::new(Scheduler::new()));
        let _ = scheduler.submit(|yielder| async move {
            println!("[coroutine] delay");
            yielder.delay((), 100).await;
            println!("[coroutine] back");
            result(1)
        });
        scheduler.try_schedule();
        std::thread::sleep(Duration::from_millis(100));
        scheduler.try_schedule();
    }

    #[cfg(all(unix, feature = "preemptive-schedule"))]
    #[test]
    fn preemptive_schedule() -> std::io::Result<()> {
        use std::sync::{Arc, Condvar, Mutex};
        static mut TEST_FLAG: bool = true;
        let pair = Arc::new((Mutex::new(true), Condvar::new()));
        let pair2 = Arc::clone(&pair);
        let handler = std::thread::spawn(move || {
            let scheduler = Box::leak(Box::new(Scheduler::new()));
            let _ = scheduler.submit(|_| async move {
                while unsafe { TEST_FLAG } {
                    println!("loop");
                    std::thread::sleep(Duration::from_millis(10));
                }
                result(1)
            });
            let _ = scheduler.submit(|_| async move {
                unsafe { TEST_FLAG = false };
                result(2)
            });
            scheduler.try_schedule();

            let (lock, cvar) = &*pair2;
            let mut pending = lock.lock().unwrap();
            *pending = false;
            // notify the condvar that the value has changed.
            cvar.notify_one();
        });

        // wait for the thread to start up
        let (lock, cvar) = &*pair;
        let result = cvar
            .wait_timeout_while(
                lock.lock().unwrap(),
                Duration::from_millis(3000),
                |&mut pending| pending,
            )
            .unwrap();
        if result.1.timed_out() {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "preemptive schedule failed",
            ))
        } else {
            unsafe {
                handler.join().unwrap();
                assert!(!TEST_FLAG);
            }
            Ok(())
        }
    }
}
