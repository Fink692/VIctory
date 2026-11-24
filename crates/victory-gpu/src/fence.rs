//! Fences: a 64-bit value and the CPU/event waiters completed by a signal.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::object::{self, FENCE, HRESULT, Iid, Object, S_OK, Table, This, guid};
use crate::resource::PAGEABLE_IIDS;

pub(crate) const FENCE_IIDS: [Iid; 4] = [
    guid(
        0x0a753dcf,
        0xc4d8,
        0x4b91,
        [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
struct Fence {
    object: Object,
    /// Titles also read it straight from memory, at the fence's GPU
    /// address.
    value: AtomicU64,
    flags: u32,
    /// Waiters registered before the value reaches their target.
    waiters: Mutex<Vec<(u64, Waiter)>>,
}

enum Waiter {
    Event(usize),
    Blocking(Arc<BlockingWait>),
}

/// Completion belongs to each registered wait, not the fence's current
/// value: another signal may rewind that value before a waiter wakes.
#[derive(Default)]
struct BlockingWait {
    reached: Mutex<bool>,
    ready: Condvar,
}

impl BlockingWait {
    fn complete(&self) {
        *self.reached.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.ready.notify_one();
    }

    fn wait(&self) {
        let mut reached = self.reached.lock().unwrap_or_else(|e| e.into_inner());
        while !*reached {
            reached = self.ready.wait(reached).unwrap_or_else(|e| e.into_inner());
        }
    }
}

fn fence<'a>(this: This) -> &'a Fence {
    // SAFETY: called from fence methods only.
    unsafe { &*this.cast::<Fence>() }
}

pub(crate) extern "system" fn create(
    _: This,
    initial: u64,
    flags: u32,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    tracing::debug!(initial, flags, "fence");
    let fence = Box::new(Fence {
        object: Object::new(&FUNCTIONS),
        value: AtomicU64::new(initial),
        flags,
        waiters: Mutex::new(Vec::new()),
    });
    object::hand_out(FENCE, fence, iid, &FENCE_IIDS, out)
}

fn set_event(event: usize) {
    #[cfg(windows)]
    // SAFETY: the title passed an event handle.
    unsafe {
        windows_sys::Win32::System::Threading::SetEvent(event as *mut c_void)
    };
    #[cfg(not(windows))]
    let _ = event;
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    object::answer(
        FENCE,
        iid,
        object::is(iid, &FENCE_IIDS).then_some(this),
        out,
    )
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Fence>(this)
}
extern "system" fn completed(this: This) -> u64 {
    fence(this).value.load(Ordering::Acquire)
}
extern "system" fn set_event_on_completion(this: This, value: u64, event: *mut c_void) -> HRESULT {
    let fence = fence(this);
    let mut waiters = fence.waiters.lock().unwrap_or_else(|e| e.into_inner());
    if fence.value.load(Ordering::Acquire) >= value {
        if !event.is_null() {
            set_event(event as usize);
        }
        return S_OK;
    }
    if event.is_null() {
        let completion = Arc::new(BlockingWait::default());
        waiters.push((value, Waiter::Blocking(completion.clone())));
        // Registration and the current-value check share the signal lock,
        // so a concurrent signal cannot be lost before the CPU sleeps.
        drop(waiters);
        completion.wait();
    } else {
        waiters.push((value, Waiter::Event(event as usize)));
    }
    S_OK
}
pub(crate) extern "system" fn signal(this: This, value: u64) -> HRESULT {
    let fence = fence(this);
    let mut waiters = fence.waiters.lock().unwrap_or_else(|e| e.into_inner());
    fence.value.store(value, Ordering::Release);
    waiters.retain(|(wanted, waiter)| {
        let reached = *wanted <= value;
        if reached {
            match waiter {
                Waiter::Event(event) => set_event(*event),
                Waiter::Blocking(completion) => completion.complete(),
            }
        }
        !reached
    });
    S_OK
}

extern "system" fn creation_flags(this: This) -> u32 {
    fence(this).flags
}
/// Where the value lives (GPU addresses are CPU addresses).
extern "system" fn gpu_address(this: This) -> u64 {
    (&raw const fence(this).value) as u64
}

/// IUnknown 3, object 4, device child 1, fence 3, fence 1 (2, a console
/// one among them).
static FUNCTIONS: Table<24> = {
    let mut f = object::d3d12_object(object::missing_table::<FENCE, 24>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    f[8] = completed as *const (); // 0x40
    f[9] = set_event_on_completion as *const (); // 0x48
    f[10] = signal as *const (); // 0x50
    f[11] = creation_flags as *const (); // 0x58
    f[12] = gpu_address as *const (); // 0x60
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::slot;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn make(initial: u64) -> This {
        let mut result = std::ptr::null();
        assert_eq!(
            create(std::ptr::null(), initial, 0, &FENCE_IIDS[0], &mut result),
            S_OK
        );
        result
    }

    fn blocking_thread(
        this: This,
        target: u64,
    ) -> (std::thread::JoinHandle<()>, mpsc::Receiver<HRESULT>) {
        // The caller owns this fence until the thread joins. COM fence
        // methods are thread-safe; an integer carries its pointer across.
        let address = this as usize;
        let (sent, received) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let this = address as This;
            let wait = slot::<extern "system" fn(This, u64, *mut c_void) -> HRESULT>(this, 0x48);
            let result = wait(this, target, std::ptr::null_mut());
            let _ = sent.send(result);
        });
        (thread, received)
    }

    fn await_registered(this: This, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fence(this).waiters.lock().unwrap().len() == count {
                return;
            }
            assert!(Instant::now() < deadline, "CPU wait did not register");
            std::thread::yield_now();
        }
    }

    #[test]
    fn null_event_waits_for_its_target() {
        let f = make(2);
        assert_eq!(set_event_on_completion(f, 2, std::ptr::null_mut()), S_OK);
        let (waiting, result) = blocking_thread(f, 9);
        await_registered(f, 1);
        assert_eq!(signal(f, 8), S_OK);
        assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(signal(f, 9), S_OK);
        assert_eq!(result.recv_timeout(Duration::from_secs(5)).unwrap(), S_OK);
        waiting.join().unwrap();
        assert!(fence(f).waiters.lock().unwrap().is_empty());
        assert_eq!(release(f), 0);
    }

    #[test]
    fn blocking_waiters_complete_independently_after_rewinds() {
        let f = make(0);
        let (lower, lower_result) = blocking_thread(f, 5);
        let (higher, higher_result) = blocking_thread(f, 9);
        await_registered(f, 2);
        assert_eq!(signal(f, 5), S_OK);
        assert_eq!(signal(f, 1), S_OK);
        assert_eq!(
            lower_result.recv_timeout(Duration::from_secs(5)).unwrap(),
            S_OK
        );
        lower.join().unwrap();
        assert!(matches!(
            higher_result.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        // A new wait after the rewind must not inherit the older wait's
        // completion, even though that target was reached in the past.
        let (later, later_result) = blocking_thread(f, 5);
        await_registered(f, 2);
        assert!(matches!(
            later_result.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(signal(f, 9), S_OK);
        assert_eq!(
            higher_result.recv_timeout(Duration::from_secs(5)).unwrap(),
            S_OK
        );
        assert_eq!(
            later_result.recv_timeout(Duration::from_secs(5)).unwrap(),
            S_OK
        );
        higher.join().unwrap();
        later.join().unwrap();
        assert_eq!(release(f), 0);
    }

    #[test]
    fn signal_racing_with_blocking_registration_is_not_lost() {
        for target in 1..=64 {
            let f = make(0);
            let (waiting, result) = blocking_thread(f, target);
            assert_eq!(signal(f, target), S_OK);
            assert_eq!(result.recv_timeout(Duration::from_secs(5)).unwrap(), S_OK);
            waiting.join().unwrap();
            assert_eq!(release(f), 0);
        }
    }

    #[test]
    fn completed_wait_remains_ready_before_the_cpu_sleeps() {
        let f = make(0);
        let completion = Arc::new(BlockingWait::default());
        fence(f)
            .waiters
            .lock()
            .unwrap()
            .push((5, Waiter::Blocking(completion.clone())));
        assert_eq!(signal(f, 5), S_OK);
        assert_eq!(signal(f, 1), S_OK);
        assert_eq!(completed(f), 1);
        // Deterministically exercise signal + rewind before the wait
        // begins; the public threaded tests also cover concurrent calls.
        completion.wait();
        assert_eq!(release(f), 0);
    }

    #[cfg(windows)]
    #[test]
    fn signals_set_waiting_events() {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

        let mut f = std::ptr::null();
        assert_eq!(create(std::ptr::null(), 1, 0, &FENCE_IIDS[0], &mut f), S_OK);
        assert_eq!(slot::<extern "system" fn(This) -> u64>(f, 0x40)(f), 1);
        // SAFETY: plain auto-reset event.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        assert!(!event.is_null());
        let wait = slot::<extern "system" fn(This, u64, *mut c_void) -> HRESULT>(f, 0x48);
        let signal = slot::<extern "system" fn(This, u64) -> HRESULT>(f, 0x50);
        assert_eq!(wait(f, 5, event), S_OK);
        // SAFETY: a valid event.
        assert_ne!(unsafe { WaitForSingleObject(event, 0) }, WAIT_OBJECT_0);
        assert_eq!(signal(f, 5), S_OK);
        // SAFETY: as above.
        assert_eq!(unsafe { WaitForSingleObject(event, 0) }, WAIT_OBJECT_0);
        assert_eq!(wait(f, 5, event), S_OK, "already reached");
        // SAFETY: as above.
        assert_eq!(unsafe { WaitForSingleObject(event, 0) }, WAIT_OBJECT_0);
        assert_eq!(wait(f, 5, std::ptr::null_mut()), S_OK);
        // Grand Theft Auto VI reads the value at the fence's address.
        let at = slot::<extern "system" fn(This) -> u64>(f, 0x60)(f);
        // SAFETY: the fence's value.
        assert_eq!(unsafe { *(at as *const u64) }, 5);
        // SAFETY: ours.
        unsafe { CloseHandle(event) };
        assert_eq!(slot::<extern "system" fn(This) -> u32>(f, 0x10)(f), 0);
    }
}
