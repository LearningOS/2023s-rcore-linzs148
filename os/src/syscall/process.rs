//! Process management syscalls
use crate::{
    config::MAX_SYSCALL_NUM,
    mm::{virtual_page_mapped, write_to_physical, VPNRange, VirtAddr},
    task::{
        change_program_brk, current_user_token, exit_current_and_run_next, get_task_info,
        insert_to_memset, remove_from_memset, suspend_current_and_run_next, TaskStatus,
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

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(_ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    let ts = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    write_to_physical(current_user_token(), ts, _ts);
    0
}

/// YOUR JOB: Finish sys_task_info to pass testcases
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TaskInfo`] is splitted by two pages ?
pub fn sys_task_info(_ti: *mut TaskInfo) -> isize {
    trace!("kernel: sys_task_info");
    let task_info = get_task_info();
    let first_scheduled_time = task_info.first_scheduled_time.unwrap();
    let ti = TaskInfo {
        status: TaskStatus::Running,
        time: get_time_ms() - first_scheduled_time,
        syscall_times: task_info.syscall_counter,
    };
    write_to_physical(current_user_token(), ti, _ti);
    0
}

// YOUR JOB: Implement mmap.
pub fn sys_mmap(_start: usize, _len: usize, _port: usize) -> isize {
    trace!("kernel: sys_mmap");
    if _port & !0x7 != 0 || _port & 0x7 == 0 {
        error!("sys_mmap: _port is not valid");
        return -1;
    }
    let va_start = VirtAddr::from(_start);
    let va_end = VirtAddr::from(_start + _len);
    if !va_start.aligned() {
        error!("sys_mmap: _start is not aligned");
        return -1;
    }
    let vpn_start = va_start.floor();
    let vpn_end = va_end.ceil();
    let vpn_range = VPNRange::new(vpn_start, vpn_end);
    for vpn in vpn_range {
        if virtual_page_mapped(current_user_token(), vpn) {
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

// YOUR JOB: Implement munmap.
pub fn sys_munmap(_start: usize, _len: usize) -> isize {
    trace!("kernel: sys_munmap");
    let va_start = VirtAddr::from(_start);
    let va_end = VirtAddr::from(_start + _len);
    if !va_start.aligned() {
        error!("sys_munmap: _start is not aligned");
        return -1;
    }
    let vpn_start = va_start.floor();
    let vpn_end = va_end.ceil();
    let vpn_range = VPNRange::new(vpn_start, vpn_end);
    for vpn in vpn_range {
        if !virtual_page_mapped(current_user_token(), vpn) {
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
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
