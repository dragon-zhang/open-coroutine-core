use crate::coroutine::suspend::Suspender;
use crate::scheduler::Scheduler;
use genawaiter::stack::{Gen, Shelf};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};
use std::future::Future;
use std::ops::DerefMut;
use std::pin::Pin;

pub use genawaiter::GeneratorState;

pub mod suspend;

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum State {
    ///协程被创建
    Created,
    ///等待运行
    Ready,
    ///运行中
    Running,
    ///被挂起
    Suspend(u64),
    ///执行系统调用
    SystemCall,
    ///执行用户函数完成
    Finished,
}

#[macro_export]
macro_rules! co {
    ($f:expr $(,)?) => {
        $crate::coroutine::ScopedCoroutine::new(Box::from(uuid::Uuid::new_v4().to_string()), $f)
    };
    ($name:literal, $f:expr $(,)?) => {
        $crate::coroutine::ScopedCoroutine::new(Box::from($name), $f)
    };
}

thread_local! {
    static COROUTINE: Box<RefCell<*const c_void>> = Box::new(RefCell::new(std::ptr::null()));
}

#[repr(C)]
pub struct ScopedCoroutine<'c, 's, P, Y, R> {
    name: &'c str,
    sp: RefCell<Gen<'c, Y, P, Box<dyn Future<Output = R> + Unpin>>>,
    state: Cell<State>,
    scheduler: RefCell<Option<&'c Scheduler<'s>>>,
}

unsafe impl<P, Y, R> Send for ScopedCoroutine<'_, '_, P, Y, R> {}

impl<'c, P: 'static, Y: 'static, R: 'static> ScopedCoroutine<'c, '_, P, Y, R> {
    pub fn new<F>(name: Box<str>, f: impl FnOnce(Suspender<'c, Y, P>) -> F + 'static) -> Self
    where
        F: Future<Output = R>,
    {
        let shelf = Box::leak(Box::new(Shelf::new()));
        ScopedCoroutine {
            name: Box::leak(name),
            sp: RefCell::new(unsafe {
                genawaiter::stack::Gen::new(shelf, |co| {
                    Box::new(Box::pin(async move {
                        let mut suspender = Suspender::new(co);
                        Suspender::init_current(&mut suspender);
                        (f)(suspender).await
                    }))
                })
            }),
            state: Cell::new(State::Created),
            scheduler: RefCell::new(None),
        }
    }
}

impl<'c, 's, P, Y, R> ScopedCoroutine<'c, 's, P, Y, R> {
    fn init_current(coroutine: &ScopedCoroutine<'c, 's, P, Y, R>) {
        COROUTINE.with(|boxed| {
            *boxed.borrow_mut() = coroutine as *const _ as *const c_void;
        })
    }

    pub fn current() -> Option<&'c ScopedCoroutine<'c, 's, P, Y, R>> {
        COROUTINE.with(|boxed| {
            let ptr = *boxed.borrow_mut();
            if ptr.is_null() {
                None
            } else {
                Some(unsafe { &*(ptr as *const ScopedCoroutine<'c, 's, P, Y, R>) })
            }
        })
    }

    fn clean_current() {
        COROUTINE.with(|boxed| *boxed.borrow_mut() = std::ptr::null())
    }

    pub fn resume_with(&self, arg: P) -> GeneratorState<Y, R> {
        self.set_state(State::Running);
        ScopedCoroutine::init_current(self);
        let mut binding = self.sp.borrow_mut();
        let mut sp = Pin::new(binding.deref_mut());
        let state = match sp.resume_with(arg) {
            GeneratorState::Complete(r) => {
                self.set_state(State::Finished);
                GeneratorState::Complete(r)
            }
            GeneratorState::Yielded(y) => {
                self.set_state(State::Suspend(Suspender::<Y, P>::delay_time()));
                Suspender::<Y, P>::clean_delay();
                GeneratorState::Yielded(y)
            }
        };
        ScopedCoroutine::<P, Y, R>::clean_current();
        state
    }

    pub fn yields(&self) {
        if let Some(scheduler) = self.get_scheduler() {
            self.set_state(State::Suspend(0));
            scheduler.try_schedule();
            self.set_state(State::Running);
        }
    }

    pub fn get_name(&self) -> &str {
        self.name
    }

    pub fn get_state(&self) -> State {
        self.state.get()
    }

    pub(crate) fn set_state(&self, state: State) {
        self.state.set(state);
    }

    pub fn is_finished(&self) -> bool {
        self.get_state() == State::Finished
    }

    pub fn get_scheduler(&self) -> Option<&'c Scheduler<'s>> {
        *self.scheduler.borrow()
    }

    pub(crate) fn set_scheduler(&self, scheduler: &'s Scheduler<'s>) -> Option<&'c Scheduler<'s>> {
        self.scheduler.replace(Some(scheduler))
    }
}

impl<Y, R> ScopedCoroutine<'_, '_, (), Y, R> {
    pub fn resume(&self) -> GeneratorState<Y, R> {
        self.resume_with(())
    }
}

impl<P, Y, R> Debug for ScopedCoroutine<'_, '_, P, Y, R> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coroutine")
            .field("name", &self.name)
            .field("state", &self.state)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base() {
        let s = Box::new(1);
        let co = ScopedCoroutine::new(
            Box::from(uuid::Uuid::new_v4().to_string()),
            |co: Suspender<'static, _, _>| async move {
                co.suspend(10).await;
                print!("{} ", s);
                co.suspend(20).await;
                "2"
            },
        );
        assert_eq!(co.resume(), GeneratorState::Yielded(10));
        assert_eq!(co.resume(), GeneratorState::Yielded(20));
        match co.resume() {
            GeneratorState::Yielded(_) => panic!(),
            GeneratorState::Complete(r) => println!("{}", r),
        };
    }

    #[test]
    fn test_return() {
        let co: ScopedCoroutine<_, (), _> = co!(|_| async move {});
        assert_eq!(GeneratorState::Complete(()), co.resume());
    }

    #[test]
    fn test_yield_once() {
        let co: ScopedCoroutine<_, (), _> = co!(|co: Suspender<'static, _, _>| async move {
            co.suspend(()).await;
        });
        assert_eq!(GeneratorState::Yielded(()), co.resume());
    }

    #[test]
    fn test_yield() {
        let s = "hello";
        let co = co!("test", move |co: Suspender<'static, _, _>| async move {
            co.suspend(10).await;
            println!("{}", s);
            co.suspend(20).await;
            "world"
        });
        assert_eq!(co.resume(), GeneratorState::Yielded(10));
        assert_eq!(co.resume(), GeneratorState::Yielded(20));
        assert_eq!(co.resume(), GeneratorState::Complete("world"));
        assert_eq!(co.get_name(), "test");
    }

    #[test]
    fn test_current() {
        assert!(ScopedCoroutine::<(), (), ()>::current().is_none());
        let co: ScopedCoroutine<_, (), _> = co!(|_| async move {
            assert!(ScopedCoroutine::<(), (), ()>::current().is_some());
        });
        assert_eq!(GeneratorState::Complete(()), co.resume());
    }
}
