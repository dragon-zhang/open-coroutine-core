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

#[repr(C)]
pub struct ScopedCoroutine<'c, 's, Y, R> {
    name: &'c str,
    sp: RefCell<Gen<'c, Y, R, Box<dyn Future<Output = R> + Unpin>>>,
    state: Cell<State>,
    scheduler: RefCell<Option<&'c Scheduler<'s>>>,
}

impl<'c, 's, Y, R> ScopedCoroutine<'c, 's, Y, R> {
    fn new(name: Box<str>, generator: Gen<'c, Y, R, Box<dyn Future<Output = R> + Unpin>>) -> Self {
        ScopedCoroutine {
            name: Box::leak(name),
            sp: RefCell::new(generator),
            state: Cell::new(State::Created),
            scheduler: RefCell::new(None),
        }
    }

    pub fn resume_with(&self, arg: R) -> GeneratorState<Y, R> {
        self.set_state(State::Running);
        let mut binding = self.sp.borrow_mut();
        let mut sp = Pin::new(binding.deref_mut());
        match sp.resume_with(arg) {
            GeneratorState::Yielded(y) => {
                if Suspender::<Y, R>::syscall_flag() {
                    self.set_state(State::SystemCall);
                    Suspender::<Y, R>::clean_syscall_flag();
                } else {
                    self.set_state(State::Suspend(Suspender::<Y, R>::delay_time()));
                    Suspender::<Y, R>::clean_delay();
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

impl<Y, R> Debug for ScopedCoroutine<'_, '_, Y, R> {
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
        let gen = {
            let shelf = Box::leak(Box::new(Shelf::new()));
            ScopedCoroutine::new(Box::from("test"), unsafe {
                Gen::new(shelf, |co| {
                    Box::new(Box::pin(async move {
                        println!("{}", f(co).await);
                    }))
                })
            })
        };
        assert_eq!(gen.resume_with(()), GeneratorState::Yielded(10));
        assert_eq!(gen.resume_with(()), GeneratorState::Yielded(20));
        assert_eq!(gen.resume_with(()), GeneratorState::Complete(()));
    }
}
