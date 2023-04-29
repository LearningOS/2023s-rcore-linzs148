//! Process management syscalls
//!
use alloc::sync::Arc;

use crate::{
    config::MAX_SYSCALL_NUM,
    fs::{open_file, OpenFlags},
    mm::{
        translated_refmut, translated_str, virtual_page_mapped, write_to_physical, VPNRange,
        VirtAddr,
    },
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next, insert_to_memset,
        remove_from_memset, suspend_current_and_run_next, TaskStatus,
    },
    timer::{get_time_ms, get_time_us},
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// Task information
#[allow(dead_code)]
pub struct TaskInfo {
    /// Task status in it's life cycle
    status: TaskStatus,
    /// The numbers of syscall called by task
    syscall_times: [u32; MAX_SYSCALL_NUM],
    /// Total running time of task
    time: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel:pid[{}] sys_get_time", current_task().unwrap().pid.0);
    let us = get_time_us();
    let content = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let token = current_user_token();
    write_to_physical(token, content, ts);
    0
}

/// YOUR JOB: Finish sys_task_info to pass testcases
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TaskInfo`] is splitted by two pages ?
pub fn sys_task_info(ti: *mut TaskInfo) -> isize {
    trace!(
        "kernel:pid[{}] sys_task_info",
        current_task().unwrap().pid.0
    );
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    let first_scheduled_time = inner.get_first_scheduled_time();
    let syscall_counter = inner.get_syscall_counter().clone();
    let content = TaskInfo {
        status: TaskStatus::Running,
        time: get_time_ms() - first_scheduled_time,
        syscall_times: syscall_counter,
    };
    let token = current_user_token();
    write_to_physical(token, content, ti);
    0
}

/// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, _len: usize, _port: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap", current_task().unwrap().pid.0);
    if _port & !0x7 != 0 || _port & 0x7 == 0 {
        error!("sys_mmap: _port is not valid");
        return -1;
    }
    let va_start = VirtAddr::from(start);
    let va_end = VirtAddr::from(start + _len);
    if !va_start.aligned() {
        error!("sys_mmap: start is not aligned");
        return -1;
    }
    let vpn_start = va_start.floor();
    let vpn_end = va_end.ceil();
    let vpn_range = VPNRange::new(vpn_start, vpn_end);
    let token = current_user_token();
    for vpn in vpn_range {
        if virtual_page_mapped(token, vpn) {
            error!(
                "sys_mmap: virtual page {:?} has been mapped to physival page",
                vpn
            );
            return -1;
        }
    }
    insert_to_memset(va_start, va_end, _port as u8);
    0
}

/// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, _len: usize) -> isize {
    trace!("kernel:pid[{}] sys_munmap", current_task().unwrap().pid.0);
    let va_start = VirtAddr::from(start);
    let va_end = VirtAddr::from(start + _len);
    if !va_start.aligned() {
        error!("sys_munmap: start is not aligned");
        return -1;
    }
    let vpn_start = va_start.floor();
    let vpn_end = va_end.ceil();
    let vpn_range = VPNRange::new(vpn_start, vpn_end);
    let token = current_user_token();
    for vpn in vpn_range {
        if !virtual_page_mapped(token, vpn) {
            error!(
                "sys_munmap: virtual page {:?} has not been mapped to physival page",
                vpn
            );
            return -1;
        }
    }
    remove_from_memset(va_start, va_end);
    0
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let data = app_inode.read_all();
        let current_task = current_task().unwrap();
        let child_task = current_task.vfork(data.as_slice());
        let child_pid = child_task.pid.0;
        let trap_cx = child_task.inner_exclusive_access().get_trap_cx();
        trap_cx.x[10] = 0;
        add_task(child_task);
        child_pid as isize
    } else {
        -1
    }
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority",
        current_task().unwrap().pid.0
    );
    if prio > 1 {
        let task = current_task().unwrap();
        let mut inner = task.inner_exclusive_access();
        inner.priority = prio as u8;
        prio
    } else {
        -1
    }
}
