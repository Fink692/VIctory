//! Authored raw-address copies, including GPU producers and consumers.

use super::*;
use crate::object::slot;
use windows::Win32::Graphics::Direct3D12::*;
use windows::core::{Interface, PCSTR};

type Messages = std::sync::Mutex<Vec<String>>;

/// Host::execute drains the debug queue; retain errors at their source.
struct Validation {
    queue: Option<ID3D12InfoQueue1>,
    messages: Box<Messages>,
    cookie: u32,
}

impl Validation {
    fn new(device: &ID3D12Device) -> Self {
        unsafe extern "system" fn message(
            _: D3D12_MESSAGE_CATEGORY,
            severity: D3D12_MESSAGE_SEVERITY,
            id: D3D12_MESSAGE_ID,
            description: PCSTR,
            context: *mut std::ffi::c_void,
        ) {
            if severity.0 <= D3D12_MESSAGE_SEVERITY_ERROR.0 {
                // SAFETY: the boxed context outlives this registration;
                // D3D12 supplies the description for the callback's span.
                let (messages, text) = unsafe {
                    (
                        &*context.cast::<Messages>(),
                        std::ffi::CStr::from_ptr(description.0.cast()).to_string_lossy(),
                    )
                };
                messages
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(format!("id={} severity={} {text}", id.0, severity.0));
            }
        }
        let mut result = Self {
            queue: None,
            messages: Box::default(),
            cookie: 0,
        };
        if cfg!(feature = "diagnostics") && std::env::var_os("VICTORY_D3D12_DEBUG").is_some() {
            let queue = device
                .cast::<ID3D12InfoQueue1>()
                .expect("the requested debug layer must support message capture");
            // SAFETY: the stable boxed context remains live until Drop
            // unregisters this callback. The callback never reenters D3D.
            unsafe {
                queue
                    .RegisterMessageCallback(
                        Some(message),
                        D3D12_MESSAGE_CALLBACK_IGNORE_FILTERS,
                        (&raw mut *result.messages).cast(),
                        &mut result.cookie,
                    )
                    .unwrap();
            }
            result.queue = Some(queue);
        }
        result
    }

    fn check(&self, phase: &str) {
        let messages = std::mem::take(&mut *self.messages.lock().unwrap());
        assert!(messages.is_empty(), "{phase}: native errors: {messages:?}");
    }
}

impl Drop for Validation {
    fn drop(&mut self) {
        if let Some(queue) = &self.queue {
            // SAFETY: registration and context are still live.
            if let Err(error) = unsafe { queue.UnregisterMessageCallback(self.cookie) } {
                eprintln!("could not unregister memory-copy validation callback: {error}");
                Box::leak(std::mem::take(&mut self.messages));
            }
        }
    }
}

struct Memory {
    bytes: Box<[u8]>,
    registered: bool,
}

impl Memory {
    fn new(value: u8, registered: bool) -> Self {
        let memory = Self {
            bytes: vec![value; 512].into_boxed_slice(),
            registered,
        };
        if registered {
            crate::shadow::register(memory.address() as usize, memory.address(), 512);
        }
        memory
    }

    fn address(&self) -> u64 {
        self.bytes.as_ptr() as u64
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        if self.registered {
            crate::shadow::unregister(self.address() as usize, self.address());
        }
    }
}

struct Recorded {
    title: This,
    allocator: This,
}

impl Recorded {
    fn new() -> Self {
        let null = std::ptr::null();
        let mut allocator = null;
        assert_eq!(
            create_allocator(null, 0, &ALLOCATOR_IIDS[0], &mut allocator),
            S_OK
        );
        let mut title = null;
        assert_eq!(
            create_list(null, 0, 0, allocator, null, &LIST_IIDS[0], &mut title),
            S_OK
        );
        Self { title, allocator }
    }

    fn host(&self) -> &crate::host::HostList {
        host_list(self.title).expect("the title list must have its host list")
    }

    fn memory_copy(&self, to: u64, from: u64, len: u64) {
        // Exercise the title-facing vtable. Its own table, rather than
        // the mutable global layout setting, selects the console slot.
        // SAFETY: this wrapper owns a live title List until its Drop.
        let table = unsafe { *self.title.cast::<*const *const ()>() };
        let index = if table == NEWER_LIST_FUNCTIONS.0.as_ptr() {
            62
        } else {
            60
        };
        let copy = slot::<extern "system" fn(This, u64, u64, u64, u32)>(self.title, index * 8);
        copy(self.title, to, from, len, 0);
    }

    fn close(&self) {
        assert_eq!(close(self.title), S_OK);
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        assert_eq!(list_release(self.title), 0);
        assert_eq!(allocator_release(self.allocator), 0);
    }
}

fn expected(guard: u8, offset: usize, values: &[u8]) -> Vec<u8> {
    let mut bytes = vec![guard; 512];
    bytes[offset..offset + values.len()].copy_from_slice(values);
    bytes
}

#[test]
#[ignore = "requires D3D12; run with VICTORY_D3D12_WARP=1"]
fn raw_memory_copies_observe_gpu_writes_and_preserve_guards() {
    crate::host::on_failure(|message| {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "VICTORY_MEMORY_COPY_FAILURE {message}");
        let _ = stderr.flush();
        std::process::abort();
    });
    let host = crate::host::host().expect("a D3D12 adapter is required");
    let validation = Validation::new(&host.device);

    let mut input = Memory::new(0x11, true);
    let working = Memory::new(0x44, true);
    let output = Memory::new(0x77, true);
    let after = Memory::new(0xaa, true);
    {
        let list = Recorded::new();
        assert!(copy_buffers(
            list.host(),
            working.address() + 13,
            input.address() + 7,
            73
        ));
        list.memory_copy(output.address() + 21, working.address() + 13, 73);
        assert!(copy_buffers(
            list.host(),
            after.address() + 35,
            output.address() + 21,
            73
        ));
        list.close();
        assert!(working.bytes.iter().all(|&v| v == 0x44));
        assert!(output.bytes.iter().all(|&v| v == 0x77));
        for pass in 0..2 {
            // Changes made after recording must be observed on both
            // executions of the very same command list.
            let values: Vec<u8> = (0..73).map(|i| (i * 3 + pass * 17) as u8).collect();
            input.bytes[7..80].copy_from_slice(&values);
            host.execute(&[list.host()]);
            validation.check("same-list/resubmission");
            assert_eq!(&*working.bytes, expected(0x44, 13, &values));
            assert_eq!(&*output.bytes, expected(0x77, 21, &values));
            assert_eq!(&*after.bytes, expected(0xaa, 35, &values));
        }
    }
    {
        // Record the dependent list before its producer. Preparation
        // order must follow execution, and no CPU copy may run ahead.
        let consumer = Recorded::new();
        consumer.memory_copy(output.address() + 21, working.address() + 13, 73);
        assert!(copy_buffers(
            consumer.host(),
            after.address() + 35,
            output.address() + 21,
            73
        ));
        consumer.close();
        let producer = Recorded::new();
        assert!(copy_buffers(
            producer.host(),
            working.address() + 13,
            input.address() + 7,
            73
        ));
        producer.close();
        input.bytes[7..80].fill(0xc3);
        host.execute(&[producer.host(), consumer.host()]);
        validation.check("reverse-recorded-lists");
        assert_eq!(&*output.bytes, expected(0x77, 21, &[0xc3; 73]));
        assert_eq!(&*after.bytes, expected(0xaa, 35, &[0xc3; 73]));
    }
    {
        let overlap = Memory::new(0x39, true);
        let result = Memory::new(0xf0, true);
        let list = Recorded::new();
        assert!(copy_buffers(
            list.host(),
            overlap.address() + 16,
            input.address() + 7,
            73
        ));
        list.memory_copy(overlap.address() + 29, overlap.address() + 16, 73);
        list.memory_copy(overlap.address() + 8, overlap.address() + 29, 73);
        assert!(copy_buffers(
            list.host(),
            result.address() + 3,
            overlap.address(),
            128
        ));
        list.close();
        let mut wanted = expected(0x39, 16, &[0xc3; 73]);
        wanted.copy_within(16..89, 29);
        wanted.copy_within(29..102, 8);
        host.execute(&[list.host()]);
        validation.check("overlapping-regions");
        assert_eq!(&*overlap.bytes, wanted);
        assert_eq!(&*result.bytes, expected(0xf0, 3, &wanted[..128]));
    }
    {
        // Raw GPU addresses need not belong to a resource object.
        let loose = Memory::new(0x5e, false);
        let result = Memory::new(0xb4, true);
        let list = Recorded::new();
        assert!(copy_buffers(
            list.host(),
            loose.address() + 9,
            input.address() + 7,
            73
        ));
        list.memory_copy(result.address() + 31, loose.address() + 9, 73);
        list.close();
        host.execute(&[list.host()]);
        validation.check("unregistered-memory");
        assert_eq!(&*loose.bytes, expected(0x5e, 9, &[0xc3; 73]));
        assert_eq!(&*result.bytes, expected(0xb4, 31, &[0xc3; 73]));
    }
    {
        let result = Memory::new(0x62, true);
        let list = Recorded::new();
        list.memory_copy(result.address(), input.address(), 0);
        list.memory_copy(result.address(), result.address(), 512);
        list.memory_copy(result.address(), 0, 16);
        list.memory_copy(0, input.address(), 16);
        list.memory_copy(result.address(), u64::MAX - 7, 16);
        list.memory_copy(u64::MAX - 7, input.address(), 16);
        list.close();
        host.execute(&[list.host()]);
        validation.check("no-op-and-invalid-ranges");
        assert!(result.bytes.iter().all(|&v| v == 0x62));
    }
    validation.check("complete");
    println!("VICTORY_MEMORY_COPY_RESULT=executed cases=6");
    println!(
        "VICTORY_MEMORY_COPY_VALIDATION={} unexpected_errors=0",
        if validation.queue.is_some() {
            "checked"
        } else {
            "disabled"
        }
    );
}
