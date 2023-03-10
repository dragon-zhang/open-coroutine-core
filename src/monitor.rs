use once_cell::sync::OnceCell;
use once_cell::unsync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

static mut GLOBAL: Lazy<Monitor> = Lazy::new(Monitor::new);

static MONITOR: OnceCell<JoinHandle<()>> = OnceCell::new();

pub(crate) struct Monitor {
    #[cfg(all(unix, feature = "preemptive-schedule"))]
    task: timer_utils::TimerList<libc::pthread_t>,
    flag: AtomicBool,
}

impl Monitor {
    #[allow(dead_code)]
    #[cfg(unix)]
    fn register_handler(sigurg_handler: libc::sighandler_t) {
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = sigurg_handler;
            assert_eq!(libc::sigaddset(&mut act.sa_mask, libc::SIGURG), 0);
            act.sa_flags = libc::SA_RESTART;
            assert_eq!(libc::sigaction(libc::SIGURG, &act, std::ptr::null_mut()), 0);
        }
    }

    fn new() -> Self {
        #[cfg(all(unix, feature = "preemptive-schedule"))]
        {
            extern "C" fn sigurg_handler(_signal: libc::c_int) {
                // invoke by Monitor::signal()
                if let Some(co) = crate::scheduler::SchedulableCoroutine::current() {
                    co.yields();
                }
            }
            Monitor::register_handler(sigurg_handler as libc::sighandler_t);
        }
        //通过这种方式来初始化monitor线程
        let _ = MONITOR.get_or_init(|| {
            std::thread::spawn(|| {
                let monitor = Monitor::global();
                while monitor.flag.load(Ordering::Acquire) {
                    #[cfg(all(unix, feature = "preemptive-schedule"))]
                    monitor.signal();
                    std::thread::sleep(Duration::from_millis(1));
                    //尽量至少wait 1ms
                    // let timeout_time = timer_utils::add_timeout_time(1_999_999);
                    // let _ = EventLoop::round_robin_timeout_schedule(timeout_time);
                }
            })
        });
        Monitor {
            #[cfg(all(unix, feature = "preemptive-schedule"))]
            task: timer_utils::TimerList::new(),
            flag: AtomicBool::new(true),
        }
    }

    fn global() -> &'static mut Monitor {
        unsafe { &mut GLOBAL }
    }

    /// 只在测试时使用
    #[allow(dead_code)]
    pub(crate) fn stop() {
        Monitor::global().flag.store(false, Ordering::Release);
    }

    #[cfg(all(unix, feature = "preemptive-schedule"))]
    fn signal(&mut self) {
        //只遍历，不删除，如果抢占调度失败，会在1ms后不断重试，相当于主动检测
        for entry in self.task.iter() {
            let exec_time = entry.get_time();
            if timer_utils::now() < exec_time {
                break;
            }
            for p in entry.iter() {
                unsafe {
                    let pthread = std::ptr::read_unaligned(p);
                    assert_eq!(libc::pthread_kill(pthread, libc::SIGURG), 0);
                }
            }
        }
    }

    #[allow(unused_variables)]
    pub(crate) fn add_task(time: u64) {
        #[cfg(all(unix, feature = "preemptive-schedule"))]
        unsafe {
            let pthread = libc::pthread_self();
            Monitor::global().task.insert(time, pthread);
        }
    }

    pub(crate) fn clean_task(_time: u64) {
        #[cfg(all(unix, feature = "preemptive-schedule"))]
        if let Some(entry) = Monitor::global().task.get_entry(_time) {
            unsafe {
                let pthread = libc::pthread_self();
                let _ = entry.remove(pthread);
            }
        }
    }
}

#[cfg(all(test, unix, feature = "preemptive-schedule"))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test() {
        extern "C" fn sigurg_handler(_signal: libc::c_int) {
            println!("sigurg handled");
        }
        Monitor::register_handler(sigurg_handler as libc::sighandler_t);
        let time = timer_utils::get_timeout_time(Duration::from_millis(10));
        Monitor::add_task(time);
        std::thread::sleep(Duration::from_millis(20));
        Monitor::clean_task(time);
    }

    #[test]
    fn test_clean() {
        extern "C" fn sigurg_handler(_signal: libc::c_int) {
            println!("sigurg should not handle");
        }
        Monitor::register_handler(sigurg_handler as libc::sighandler_t);
        let time = timer_utils::get_timeout_time(Duration::from_millis(500));
        Monitor::add_task(time);
        Monitor::clean_task(time);
        std::thread::sleep(Duration::from_millis(600));
    }

    #[test]
    fn test_sigmask() {
        extern "C" fn sigurg_handler(_signal: libc::c_int) {
            println!("sigurg should not handle");
        }
        Monitor::register_handler(sigurg_handler as libc::sighandler_t);
        let _ = shield!();
        let time = timer_utils::get_timeout_time(Duration::from_millis(1000));
        Monitor::add_task(time);
        std::thread::sleep(Duration::from_millis(1100));
        Monitor::clean_task(time);
    }
}
