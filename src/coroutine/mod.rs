use crate::coroutine::suspend::Suspender;
use crate::scheduler::Scheduler;
use genawaiter::stack::{Co, Gen, Shelf};
use std::cell::{Cell, RefCell};
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
    ($f:expr $(,)?) => {{
        let shelf = Box::leak(Box::new(Shelf::new()));
        ScopedCoroutine::new(Box::from(uuid::Uuid::new_v4().to_string()), unsafe {
            Gen::new(shelf, |co| {
                Box::new(Box::pin(async move { ($f)(Suspender::new(co)).await }))
            })
        })
    }};
    ($name:literal, $f:expr $(,)?) => {{
        let shelf = Box::leak(Box::new(Shelf::new()));
        ScopedCoroutine::new(Box::from($name), unsafe {
            Gen::new(shelf, |co| {
                Box::new(Box::pin(async move { ($f)(Suspender::new(co)).await }))
            })
        })
    }};
}

#[repr(C)]
pub struct ScopedCoroutine<'c, 's, P, Y, R> {
    name: &'c str,
    sp: RefCell<Gen<'c, Y, P, Box<dyn Future<Output = R> + Unpin>>>,
    state: Cell<State>,
    scheduler: RefCell<Option<&'c Scheduler<'s>>>,
}

impl<'c, 's, P, Y, R> ScopedCoroutine<'c, 's, P, Y, R> {
    fn new(name: Box<str>, generator: Gen<'c, Y, P, Box<dyn Future<Output = R> + Unpin>>) -> Self {
        ScopedCoroutine {
            name: Box::leak(name),
            sp: RefCell::new(generator),
            state: Cell::new(State::Created),
            scheduler: RefCell::new(None),
        }
    }

    pub fn resume_with(&self, arg: P) -> GeneratorState<Y, R> {
        self.set_state(State::Running);
        let mut binding = self.sp.borrow_mut();
        let mut sp = Pin::new(binding.deref_mut());
        match sp.resume_with(arg) {
            GeneratorState::Yielded(y) => {
                if Suspender::<Y, P>::syscall_flag() {
                    self.set_state(State::SystemCall);
                    Suspender::<Y, P>::clean_syscall_flag();
                } else {
                    self.set_state(State::Suspend(Suspender::<Y, P>::delay_time()));
                    Suspender::<Y, P>::clean_delay();
                }
                GeneratorState::Yielded(y)
            }
            GeneratorState::Complete(r) => {
                self.set_state(State::Finished);
                GeneratorState::Complete(r)
            }
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

    pub fn get_scheduler(&self) -> Option<&'c Scheduler<'s>> {
        *self.scheduler.borrow()
    }

    pub(crate) fn set_scheduler(&self, scheduler: &'s Scheduler<'s>) -> Option<&'c Scheduler<'s>> {
        self.scheduler.replace(Some(scheduler))
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
        let f = |co: Co<'static, _, _>| async move {
            co.yield_(10).await;
            print!("{} ", s);
            co.yield_(20).await;
            "2"
        };
        let co = {
            let shelf = Box::leak(Box::new(Shelf::new()));
            ScopedCoroutine::new(Box::from(uuid::Uuid::new_v4().to_string()), unsafe {
                Gen::new(shelf, |co| Box::new(Box::pin(async move { f(co).await })))
            })
        };
        assert_eq!(co.resume_with(()), GeneratorState::Yielded(10));
        assert_eq!(co.resume_with(()), GeneratorState::Yielded(20));
        match co.resume_with(()) {
            GeneratorState::Yielded(_) => panic!(),
            GeneratorState::Complete(r) => println!("{}", r),
        };
    }

    #[test]
    fn test_return() {
        let co: ScopedCoroutine<_, (), _> = co!(|_| async move {});
        assert_eq!(GeneratorState::Complete(()), co.resume_with(()));
    }

    #[test]
    fn test_yield() {
        let s = "hello";
        let co = co!("test", |co: Suspender<'static, _, _>| async move {
            co.suspend(10).await;
            println!("{}", s);
            co.suspend(20).await;
            "world"
        });
        assert_eq!(co.resume_with(()), GeneratorState::Yielded(10));
        assert_eq!(co.resume_with(()), GeneratorState::Yielded(20));
        assert_eq!(co.resume_with(()), GeneratorState::Complete("world"));
        assert_eq!(co.get_name(), "test");
    }
}
