//! Task management implementation
//!
//! Everything about task management, like starting and switching tasks is
//! implemented here.
//!
//! A single global instance of [`TaskManager`] called `TASK_MANAGER` controls
//! all the tasks in the operating system.
//!
//! Be careful when you see `__switch` ASM function in `switch.S`. Control flow around this function
//! might not be what you expect.

mod context;
mod switch;
#[allow(clippy::module_inception)]
mod task;

use crate::config::MAX_SYSCALL_NUM;
use crate::loader::{get_app_data, get_num_app};
use crate::mm::{MapPermission, VirtAddr};
use crate::sync::UPSafeCell;
use crate::timer::get_time_ms;
use crate::trap::TrapContext;
use alloc::vec::Vec;
use lazy_static::*;
use switch::__switch;
pub use task::{TaskControlBlock, TaskStatus};

pub use context::TaskContext;

/// The task manager, where all the tasks are managed.
///
/// Functions implemented on `TaskManager` deals with all task state transitions
/// and task context switching. For convenience, you can find wrappers around it
/// in the module level.
///
/// Most of `TaskManager` are hidden behind the field `inner`, to defer
/// borrowing checks to runtime. You can see examples on how to use `inner` in
/// existing functions on `TaskManager`.
pub struct TaskManager {
    /// total number of tasks
    num_app: usize,
    /// use inner value to get mutable access
    inner: UPSafeCell<TaskManagerInner>,
}

/// The task manager inner in 'UPSafeCell'
struct TaskManagerInner {
    /// task list
    tasks: Vec<TaskControlBlock>,
    /// id of current `Running` task
    current: usize,
    /// task infi list
    infos: Vec<TaskInfo>,
}

/// infomation about task
#[derive(Copy, Clone)]
pub struct TaskInfo {
    /// task first scheduled time
    pub first_scheduled_time: Option<usize>,
    /// task syscall counter
    pub syscall_counter: [u32; MAX_SYSCALL_NUM],
}

impl TaskInfo {
    fn new() -> Self {
        TaskInfo {
            first_scheduled_time: None,
            syscall_counter: [0; MAX_SYSCALL_NUM],
        }
    }
}

lazy_static! {
    /// a `TaskManager` global instance through lazy_static!
    pub static ref TASK_MANAGER: TaskManager = {
        println!("init TASK_MANAGER");
        let num_app = get_num_app();
        println!("num_app = {}", num_app);
        let mut tasks: Vec<TaskControlBlock> = Vec::new();
        let mut infos: Vec<TaskInfo> = Vec::new();
        for i in 0..num_app {
            tasks.push(TaskControlBlock::new(get_app_data(i), i));
            infos.push(TaskInfo::new());
        }
        TaskManager {
            num_app,
            inner: unsafe {
                UPSafeCell::new(TaskManagerInner {
                    tasks,
                    current: 0,
                    infos
                })
            },
        }
    };
}

impl TaskManager {
    /// Run the first task in task list.
    ///
    /// Generally, the first task in task list is an idle task (we call it zero process later).
    /// But in ch4, we load apps statically, so the first task is a real app.
    fn run_first_task(&self) -> ! {
        let mut inner = self.inner.exclusive_access();
        let task0 = &mut inner.tasks[0];
        task0.task_status = TaskStatus::Running;
        let next_task_cx_ptr = &task0.task_cx as *const TaskContext;
        let info0 = &mut inner.infos[0];
        info0.first_scheduled_time = Some(get_time_ms());
        drop(inner);
        let mut _unused = TaskContext::zero_init();
        // before this, we should drop local variables that must be dropped manually
        unsafe {
            __switch(&mut _unused as *mut _, next_task_cx_ptr);
        }
        panic!("unreachable in run_first_task!");
    }

    /// Change the status of current `Running` task into `Ready`.
    fn mark_current_suspended(&self) {
        let mut inner = self.inner.exclusive_access();
        let current = inner.current;
        inner.tasks[current].task_status = TaskStatus::Ready;
    }

    /// Change the status of current `Running` task into `Exited`.
    fn mark_current_exited(&self) {
        let mut inner = self.inner.exclusive_access();
        let current = inner.current;
        inner.tasks[current].task_status = TaskStatus::Exited;
    }

    /// Find next task to run and return task id.
    ///
    /// In this case, we only return the first `Ready` task in task list.
    fn find_next_task(&self) -> Option<usize> {
        let inner = self.inner.exclusive_access();
        let current = inner.current;
        (current + 1..current + self.num_app + 1)
            .map(|id| id % self.num_app)
            .find(|id| inner.tasks[*id].task_status == TaskStatus::Ready)
    }

    /// Get the current 'Running' task's token.
    fn get_current_token(&self) -> usize {
        let inner = self.inner.exclusive_access();
        inner.tasks[inner.current].get_user_token()
    }

    /// Get the current 'Running' task's trap contexts.
    fn get_current_trap_cx(&self) -> &'static mut TrapContext {
        let inner = self.inner.exclusive_access();
        inner.tasks[inner.current].get_trap_cx()
    }

    /// Change the current 'Running' task's program break
    pub fn change_current_program_brk(&self, size: i32) -> Option<usize> {
        let mut inner = self.inner.exclusive_access();
        let current = inner.current;
        inner.tasks[current].change_program_brk(size)
    }

    /// Switch current `Running` task to the task we have found,
    /// or there is no `Ready` task and we can exit with all applications completed
    fn run_next_task(&self) {
        if let Some(next) = self.find_next_task() {
            let mut inner = self.inner.exclusive_access();
            let current = inner.current;
            inner.tasks[next].task_status = TaskStatus::Running;
            inner.infos[next]
                .first_scheduled_time
                .get_or_insert(get_time_ms());
            inner.current = next;
            let current_task_cx_ptr = &mut inner.tasks[current].task_cx as *mut TaskContext;
            let next_task_cx_ptr = &inner.tasks[next].task_cx as *const TaskContext;
            drop(inner);
            // before this, we should drop local variables that must be dropped manually
            unsafe {
                __switch(current_task_cx_ptr, next_task_cx_ptr);
            }
            // go back to user mode
        } else {
            panic!("All applications completed!");
        }
    }

    fn increase_syscall_counter(&self, syscall_id: usize) {
        let mut inner = self.inner.exclusive_access();
        let current = inner.current;
        inner.infos[current].syscall_counter[syscall_id] += 1;
    }

    fn get_task_info(&self) -> TaskInfo {
        let inner = self.inner.exclusive_access();
        let current = inner.current;
        inner.infos[current]
    }

    fn insert_to_memset(&self, va_start: VirtAddr, va_end: VirtAddr, flags: u8) {
        let mut inner = self.inner.exclusive_access();
        let current = inner.current;
        let memset = &mut inner.tasks[current].memory_set;
        let mut perm = MapPermission::U;
        if (flags & 0x1) > 0 {
            perm |= MapPermission::R;
        }
        if (flags & 0x2) > 0 {
            perm |= MapPermission::W;
        }
        if (flags & 0x4) > 0 {
            perm |= MapPermission::X;
        }
        memset.insert_framed_area(va_start, va_end, perm);
    }

    fn remove_from_memset(&self, va_start: VirtAddr, va_end: VirtAddr) {
        let mut inner = self.inner.exclusive_access();
        let current = inner.current;
        let memset = &mut inner.tasks[current].memory_set;
        memset.remove_framed_area(va_start, va_end);
    }
}

/// Run the first task in task list.
pub fn run_first_task() {
    TASK_MANAGER.run_first_task();
}

/// Switch current `Running` task to the task we have found,
/// or there is no `Ready` task and we can exit with all applications completed
fn run_next_task() {
    TASK_MANAGER.run_next_task();
}

/// Change the status of current `Running` task into `Ready`.
fn mark_current_suspended() {
    TASK_MANAGER.mark_current_suspended();
}

/// Change the status of current `Running` task into `Exited`.
fn mark_current_exited() {
    TASK_MANAGER.mark_current_exited();
}

/// Suspend the current 'Running' task and run the next task in task list.
pub fn suspend_current_and_run_next() {
    mark_current_suspended();
    run_next_task();
}

/// Exit the current 'Running' task and run the next task in task list.
pub fn exit_current_and_run_next() {
    mark_current_exited();
    run_next_task();
}

/// increase current syscall counter
pub fn increase_syscall_counter(syscall_id: usize) {
    TASK_MANAGER.increase_syscall_counter(syscall_id);
}

/// get current syscall info
pub fn get_task_info() -> TaskInfo {
    TASK_MANAGER.get_task_info()
}

/// Get the current 'Running' task's token.
pub fn current_user_token() -> usize {
    TASK_MANAGER.get_current_token()
}

/// Get the current 'Running' task's trap contexts.
pub fn current_trap_cx() -> &'static mut TrapContext {
    TASK_MANAGER.get_current_trap_cx()
}

/// Change the current 'Running' task's program break
pub fn change_program_brk(size: i32) -> Option<usize> {
    TASK_MANAGER.change_current_program_brk(size)
}

/// insert virtual srea to memory set
pub fn insert_to_memset(va_start: VirtAddr, va_end: VirtAddr, flags: u8) {
    TASK_MANAGER.insert_to_memset(va_start, va_end, flags);
}

/// remove virtual srea from memory set
pub fn remove_from_memset(va_start: VirtAddr, va_end: VirtAddr) {
    TASK_MANAGER.remove_from_memset(va_start, va_end)
}
