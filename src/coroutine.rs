use crate::scheduler::Scheduler;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};
use std::mem::{ManuallyDrop, MaybeUninit};
use uuid::Uuid;

pub use corosensei::stack::*;
pub use corosensei::*;

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Status {
    ///协程被创建
    Created,
    ///等待运行
    Ready,
    ///运行中
    Running,
    ///被挂起
    Suspend,
    ///执行系统调用
    SystemCall,
    ///栈扩/缩容时
    CopyStack,
    ///调用用户函数完成，但未退出
    Finished,
    ///已退出
    Exited,
}

thread_local! {
    static COROUTINE: Box<RefCell<*const c_void>> = Box::new(RefCell::new(std::ptr::null()));
    static YIELDER: Box<RefCell<*const c_void>> = Box::new(RefCell::new(std::ptr::null()));
}

#[repr(C)]
pub struct OpenCoroutine<'a, 's, Param, Yield, Return> {
    name: &'a str,
    sp: RefCell<ScopedCoroutine<'a, Param, Yield, Return, DefaultStack>>,
    status: Cell<Status>,
    //调用用户函数的参数
    param: RefCell<Param>,
    result: RefCell<MaybeUninit<ManuallyDrop<Return>>>,
    scheduler: RefCell<Option<&'a Scheduler<'s>>>,
}

impl<'a, 's, Param, Yield, Return> Drop for OpenCoroutine<'a, 's, Param, Yield, Return> {
    fn drop(&mut self) {
        self.status.set(Status::Exited);
    }
}

impl<'a, 's, Param, Yield, Return> OpenCoroutine<'a, 's, Param, Yield, Return> {
    pub fn new<F>(f: F, param: Param, size: usize) -> std::io::Result<Self>
    where
        F: FnOnce(&Yielder<Param, Yield>, Param) -> Return,
        F: 'a,
    {
        OpenCoroutine::with_name(None, f, param, size)
    }

    pub fn with_name<F>(
        name: Option<&'a str>,
        f: F,
        param: Param,
        size: usize,
    ) -> std::io::Result<Self>
    where
        F: FnOnce(&Yielder<Param, Yield>, Param) -> Return,
        F: 'a,
    {
        let stack = DefaultStack::new(size)?;
        let sp = ScopedCoroutine::with_stack(stack, f);
        Ok(OpenCoroutine {
            name: name.unwrap_or_else(|| Box::leak(Box::new(Uuid::new_v4().to_string()))),
            sp: RefCell::new(sp),
            status: Cell::new(Status::Created),
            param: RefCell::new(param),
            result: RefCell::new(MaybeUninit::uninit()),
            scheduler: RefCell::new(None),
        })
    }

    pub fn resume_with(&self, val: Param) -> (Param, CoroutineResult<Yield, Return>) {
        let previous = self.param.replace(val);
        (previous, self.resume())
    }

    pub fn resume(&self) -> CoroutineResult<Yield, Return> {
        self.set_status(Status::Running);
        OpenCoroutine::init_current(self);
        let param = unsafe { std::ptr::read_unaligned(self.param.as_ptr()) };
        match self.sp.borrow_mut().resume(param) {
            CoroutineResult::Return(r) => {
                self.set_status(Status::Finished);
                self.result.replace(MaybeUninit::new(ManuallyDrop::new(r)));
                CoroutineResult::Return(self.get_result().unwrap())
            }
            CoroutineResult::Yield(y) => CoroutineResult::Yield(y),
        }
    }

    fn init_yielder(yielder: &Yielder<Param, Yield>) {
        YIELDER.with(|boxed| {
            *boxed.borrow_mut() = yielder as *const _ as *const c_void;
        });
    }

    pub fn yielder() -> *const Yielder<Param, Yield> {
        YIELDER.with(|boxed| unsafe { std::mem::transmute(*boxed.borrow_mut()) })
    }

    fn clean_yielder() {
        YIELDER.with(|boxed| *boxed.borrow_mut() = std::ptr::null())
    }

    fn init_current(coroutine: &OpenCoroutine<'a, 's, Param, Yield, Return>) {
        COROUTINE.with(|boxed| {
            *boxed.borrow_mut() = coroutine as *const _ as *const c_void;
        })
    }

    pub fn current() -> Option<&'a OpenCoroutine<'a, 's, Param, Yield, Return>> {
        COROUTINE.with(|boxed| {
            let ptr = *boxed.borrow_mut();
            if ptr.is_null() {
                None
            } else {
                Some(unsafe { &*(ptr as *const OpenCoroutine<'a, 's, Param, Yield, Return>) })
            }
        })
    }

    fn clean_current() {
        COROUTINE.with(|boxed| *boxed.borrow_mut() = std::ptr::null())
    }

    pub fn get_name(&self) -> &str {
        self.name
    }

    pub fn get_status(&self) -> Status {
        self.status.get()
    }

    pub fn set_status(&self, status: Status) {
        self.status.set(status);
    }

    pub fn is_finished(&self) -> bool {
        self.get_status() == Status::Finished
    }

    pub fn get_result(&self) -> Option<Return> {
        if self.is_finished() {
            unsafe {
                let mut m = self.result.borrow().assume_init_read();
                Some(ManuallyDrop::take(&mut m))
            }
        } else {
            None
        }
    }

    pub fn get_scheduler(&self) -> Option<&'a Scheduler<'s>> {
        *self.scheduler.borrow()
    }

    pub(crate) fn set_scheduler(&self, scheduler: &'s Scheduler<'s>) {
        self.scheduler.replace(Some(scheduler));
    }
}

impl<'a, 's, Param, Yield, Return> Debug for OpenCoroutine<'a, 's, Param, Yield, Return> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenCoroutine")
            .field("name", &self.name)
            .field("status", &self.status)
            .field("scheduler", &self.scheduler)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_return() {
        let coroutine = OpenCoroutine::new(
            |_yielder: &Yielder<i32, i32>, param| {
                assert_eq!(0, param);
                1
            },
            0,
            2048,
        )
        .expect("create coroutine failed !");
        assert_eq!(1, coroutine.resume().as_return().unwrap());
    }

    // #[test]
    // fn test_yield_once() {
    //     // will panic
    //     let coroutine = OpenCoroutine::new(
    //         |yielder: &Yielder<i32, i32>, input| {
    //             assert_eq!(1, input);
    //             assert_eq!(3, yielder.suspend(2));
    //             6
    //         },
    //         1,
    //         2048,
    //     )
    //     .expect("create coroutine failed !");
    //     assert_eq!(2, coroutine.resume().as_yield().unwrap());
    // }

    #[test]
    fn test_yield() {
        let coroutine = OpenCoroutine::new(
            |yielder, input| {
                assert_eq!(1, input);
                assert_eq!(3, yielder.suspend(2));
                assert_eq!(5, yielder.suspend(4));
                6
            },
            1,
            2048,
        )
        .expect("create coroutine failed !");
        assert_eq!(2, coroutine.resume().as_yield().unwrap());
        assert_eq!(4, coroutine.resume_with(3).1.as_yield().unwrap());
        assert_eq!(6, coroutine.resume_with(5).1.as_return().unwrap());
    }

    #[test]
    fn test_current() {
        assert!(OpenCoroutine::<i32, i32, i32>::current().is_none());
        let coroutine = OpenCoroutine::new(
            |_yielder: &Yielder<i32, i32>, input| {
                assert_eq!(0, input);
                assert!(OpenCoroutine::<i32, i32, i32>::current().is_some());
                1
            },
            0,
            2048,
        )
        .expect("create coroutine failed !");
        assert_eq!(coroutine.resume().as_return().unwrap(), 1);
    }
}
