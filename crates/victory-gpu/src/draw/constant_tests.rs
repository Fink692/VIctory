//! Authored constant-buffer programs. Read real native CBVs after GPU
//! producers, including table reuse, reverse recording and list replay.

use super::*;
use crate::object::{guid, slot};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::core::{PCSTR, s};

type Messages = std::sync::Mutex<Vec<String>>;

struct Validation {
    queue: ID3D12InfoQueue1,
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
            if severity.0 > D3D12_MESSAGE_SEVERITY_ERROR.0 {
                return;
            }
            // SAFETY: registration retains the boxed context and D3D12
            // keeps the description alive for this callback.
            let (messages, text) = unsafe {
                (
                    &*context.cast::<Messages>(),
                    std::ffi::CStr::from_ptr(description.0.cast())
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            messages
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{}: {text}", id.0));
        }
        let queue = device
            .cast::<ID3D12InfoQueue1>()
            .expect("constant fixtures require diagnostics and VICTORY_D3D12_DEBUG=1");
        let mut result = Self {
            queue,
            messages: Box::default(),
            cookie: 0,
        };
        // SAFETY: a stable boxed context, unregistered before release.
        unsafe {
            result
                .queue
                .RegisterMessageCallback(
                    Some(message),
                    D3D12_MESSAGE_CALLBACK_IGNORE_FILTERS,
                    (&raw mut *result.messages).cast(),
                    &mut result.cookie,
                )
                .unwrap();
        }
        result
    }

    fn check(&self) {
        let messages = self.messages.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            messages.is_empty(),
            "native validation errors: {messages:?}"
        );
    }
}

impl Drop for Validation {
    fn drop(&mut self) {
        // SAFETY: this registration and its boxed context are still live.
        if unsafe { self.queue.UnregisterMessageCallback(self.cookie) }.is_err() {
            Box::leak(std::mem::take(&mut self.messages));
        }
    }
}

fn host() -> &'static crate::host::Host {
    crate::host::on_failure(|message| {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "VICTORY_CONSTANT_FAILURE {message}");
        let _ = stderr.flush();
        std::process::abort();
    });
    crate::host::host().expect("a D3D12 adapter is required")
}

/// Ordinary authored CPU memory registered as a title buffer. No title
/// executable, captures or expected game data are part of these fixtures.
struct TitleBuffer(Box<[u8]>);

impl TitleBuffer {
    fn new(size: usize, value: u8) -> Self {
        let result = Self(vec![value; size].into_boxed_slice());
        crate::shadow::register(result.address() as usize, result.address(), size as u64);
        result
    }

    fn address(&self) -> u64 {
        self.0.as_ptr() as u64
    }

    fn set(&mut self, offset: usize, words: [u32; 4]) {
        for (bytes, word) in self.0[offset..offset + 16]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(words)
        {
            bytes.copy_from_slice(&word.to_le_bytes());
        }
    }
}

impl Drop for TitleBuffer {
    fn drop(&mut self) {
        crate::shadow::unregister(self.address() as usize, self.address());
    }
}

struct Heap(This);

impl Heap {
    fn new() -> Self {
        // The descriptor is followed by the console placement hint.
        #[repr(C)]
        struct Desc {
            fields: [u32; 4],
            placement: [u64; 2],
        }
        let desc = Desc {
            fields: [0, 8, 1, 0],
            placement: [0; 2],
        };
        let iid = guid(
            0x8efb471d,
            0x616c,
            0x4f49,
            [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51],
        );
        let mut heap = std::ptr::null();
        assert_eq!(
            crate::descriptor::create_heap(
                std::ptr::null(),
                (&raw const desc).cast(),
                &iid,
                &mut heap,
            ),
            0
        );
        Self(heap)
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        slot::<extern "system" fn(This) -> u32>(self.0, 0x10)(self.0);
    }
}

#[derive(Clone, Copy, Debug)]
enum Binding {
    Root,
    Table,
}

struct Program {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
}

impl Program {
    fn new(binding: Binding) -> Self {
        let parameter = match binding {
            Binding::Root => "CBV(b0)",
            Binding::Table => "DescriptorTable(CBV(b0))",
        };
        let source = format!(
            r#"
cbuffer Values : register(b0) {{ uint4 values[16]; }};
cbuffer Parameters : register(b1) {{ uint destination; uint element; }};
RWByteAddressBuffer output : register(u0);
[RootSignature("{parameter}, UAV(u0), RootConstants(num32BitConstants=2, b1)")]
[numthreads(1, 1, 1)]
void main() {{ output.Store4(destination, values[element]); }}
"#
        );
        let mut code: Option<ID3DBlob> = None;
        let mut errors: Option<ID3DBlob> = None;
        // SAFETY: authored source, static names and valid output pointers.
        let compiled = unsafe {
            D3DCompile(
                source.as_ptr().cast(),
                source.len(),
                s!("constant-buffer-regression"),
                None,
                None,
                s!("main"),
                s!("cs_5_1"),
                0,
                0,
                &mut code,
                Some(&mut errors),
            )
        };
        if let Err(error) = compiled {
            let message = errors.map(|blob| {
                // SAFETY: the live compiler blob owns this byte range.
                String::from_utf8_lossy(unsafe {
                    std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
                })
                .into_owned()
            });
            panic!("authored constant shader failed: {error}; {message:?}");
        }
        let code = code.expect("compiled constant shader");
        let device = &host().device;
        // SAFETY: the live bytecode contains its authored root signature;
        // native creation copies the descriptions and shader bytes.
        unsafe {
            let bytes =
                std::slice::from_raw_parts(code.GetBufferPointer().cast(), code.GetBufferSize());
            let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes).unwrap();
            let pso = device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    CS: D3D12_SHADER_BYTECODE {
                        pShaderBytecode: bytes.as_ptr().cast(),
                        BytecodeLength: bytes.len(),
                    },
                    ..Default::default()
                })
                .unwrap();
            Self { root, pso }
        }
    }
}

struct Output {
    buffer: ID3D12Resource,
    data: *mut u8,
    slots: usize,
}

impl Output {
    fn new(slots: usize) -> Self {
        let (buffer, data) = host().shared_buffer(((slots + 2) * 16) as u64).unwrap();
        // SAFETY: newly allocated mapped bytes, not yet used by the GPU.
        unsafe { std::ptr::write_bytes(data, 0xa5, (slots + 2) * 16) };
        Self {
            buffer,
            data,
            slots,
        }
    }

    fn check(&self, expected: &[[u32; 4]]) {
        assert_eq!(expected.len(), self.slots);
        let mut bytes = vec![0xa5; (self.slots + 2) * 16];
        for (slot, values) in expected.iter().enumerate() {
            for (i, value) in values.iter().enumerate() {
                let at = (slot + 1) * 16 + i * 4;
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        // SAFETY: callers wait for completion before reading the mapping.
        let actual = unsafe { std::slice::from_raw_parts(self.data, bytes.len()) };
        assert_eq!(actual, bytes, "constant reads or output guard bytes differ");
    }
}

struct Reader {
    binding: Binding,
    state: State,
    // State must release its dynamic slots before this heap drops.
    heap: Heap,
    program: Program,
}

impl Reader {
    fn new(binding: Binding) -> Self {
        let heap = Heap::new();
        let mut state = State::default();
        state.set_heaps(&[heap.0]);
        state.set_table(true, 0, crate::descriptor::heap(heap.0).start);
        Self {
            binding,
            state,
            heap,
            program: Program::new(binding),
        }
    }

    /// Uses the actual runtime root/table preparation, followed by a native
    /// shader consuming the resulting constant binding.
    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &mut self,
        list: &HostList,
        address: u64,
        size: u32,
        output: &Output,
        slot: usize,
        element: u32,
    ) -> u64 {
        list.begin_buffer_bindings();
        let binding = match self.binding {
            Binding::Root => {
                root_view(list, address, false, &mut self.state.root_constants).unwrap()
            }
            Binding::Table => {
                let heap = crate::descriptor::heap(self.heap.0);
                crate::descriptor::create_cbv(
                    std::ptr::null(),
                    &[address as u32, (address >> 32) as u32, size, 0],
                    heap.start,
                );
                let used = [UsedResource {
                    kind: ResourceType::Cbv,
                    root: RootType::Table,
                    table_offset_dwords: 0,
                    lower_bound: 0,
                    stride: 0,
                    dwords: 4,
                    user_data: 0,
                    slot: 0,
                }];
                let made = self.state.tables(list, &used, true);
                assert_eq!(made.len(), 1);
                assert_eq!(made[0].0, 0);
                let native = &heap.host().unwrap().heap;
                list.keep(native);
                // SAFETY: a live shader-visible heap, bound before its table.
                unsafe {
                    list.list.SetDescriptorHeaps(&[Some(native.clone())]);
                    native.GetGPUDescriptorHandleForHeapStart().ptr
                        + u64::from(made[0].1 / 32)
                            * u64::from(host().device.GetDescriptorHandleIncrementSize(
                                D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                            ))
                }
            }
        };
        list.bind_buffer(&output.buffer, true);
        assert!(list.finish_buffer_bindings());
        list.keep(&self.program.root);
        list.keep(&self.program.pso);
        let parameters = [((slot + 1) * 16) as u32, element];
        // SAFETY: resources, signature and pipeline are retained; output
        // slots fit the allocation and the constant descriptor is prepared.
        unsafe {
            list.list.SetComputeRootSignature(&self.program.root);
            list.list.SetPipelineState(&self.program.pso);
            match self.binding {
                Binding::Root => list.list.SetComputeRootConstantBufferView(0, binding),
                Binding::Table => list
                    .list
                    .SetComputeRootDescriptorTable(0, D3D12_GPU_DESCRIPTOR_HANDLE { ptr: binding }),
            }
            list.list
                .SetComputeRootUnorderedAccessView(1, output.buffer.GetGPUVirtualAddress());
            list.list
                .SetComputeRoot32BitConstants(2, 2, parameters.as_ptr().cast(), 0);
            list.list.Dispatch(1, 1, 1);
        }
        binding
    }
}

const FIRST: [u32; 4] = [0x1020_3040, 0x5566_7788, 0x90ab_cdef, 0x0102_0304];
const SECOND: [u32; 4] = [0xffee_ddcc, 0xbbaa_9988, 0x7654_3210, 0x1357_9bdf];

#[test]
#[ignore = "requires diagnostics, D3D12 and VICTORY_D3D12_DEBUG=1"]
fn constant_buffers_follow_gpu_writes_and_table_reuse() {
    let host = host();
    let validation = Validation::new(&host.device);
    let mut cases = 0;
    for binding in [Binding::Root, Binding::Table] {
        for offset in [0, 16, 240, 4080] {
            let mut input = TitleBuffer::new(4096, 0x82);
            input.set(0, FIRST);
            input.set(16, SECOND);
            let working = TitleBuffer::new(4096, 0x3c);
            let address = working.address() + offset;
            let padding = matches!(binding, Binding::Table) || offset == 4080;
            let output = Output::new(if padding { 3 } else { 2 });
            let mut reader = Reader::new(binding);
            let list = host.new_list().unwrap();
            assert!(crate::list::copy_buffers(
                &list,
                address,
                input.address(),
                16
            ));
            let first = reader.dispatch(&list, address, 16, &output, 0, 0);
            assert!(crate::list::copy_buffers(
                &list,
                address,
                input.address() + 16,
                16
            ));
            let second = reader.dispatch(&list, address, 16, &output, 1, 0);
            assert_eq!(
                first, second,
                "unchanged bindings reuse their native storage"
            );
            let mut expected = vec![FIRST, SECOND];
            if padding {
                reader.dispatch(&list, address, 16, &output, 2, 1);
                expected.push([0; 4]);
            }
            if matches!(binding, Binding::Table) {
                assert_eq!(
                    reader.state.fresh_tables.len(),
                    1,
                    "table descriptors must be reused"
                );
            }
            list.close();
            assert!(
                working.0.iter().all(|&byte| byte == 0x3c),
                "recording must not copy into CPU title memory"
            );
            host.execute(&[&list]);
            output.check(&expected);
            cases += expected.len();
            validation.check();
        }
    }

    // Invalid table ranges become typed null CBVs, never stale descriptors
    // or reads beyond the registered title buffer.
    let working = TitleBuffer::new(4096, 0x7b);
    let output = Output::new(3);
    let mut reader = Reader::new(Binding::Table);
    let list = host.new_list().unwrap();
    for (slot, (offset, size)) in [(0, 0), (0, 65537), (4088, 16)].into_iter().enumerate() {
        reader.dispatch(&list, working.address() + offset, size, &output, slot, 0);
    }
    list.close();
    host.execute(&[&list]);
    output.check(&[[0; 4]; 3]);
    validation.check();
    cases += 3;
    assert_eq!(cases, 24);
    eprintln!("VICTORY_CONSTANT_RESULT=executed cases={cases} validation_errors=0");
}

#[test]
#[ignore = "requires diagnostics, D3D12 and VICTORY_D3D12_DEBUG=1"]
fn constant_buffers_follow_submission_order_and_reexecute() {
    let host = host();
    let validation = Validation::new(&host.device);
    let mut cases = 0;
    for binding in [Binding::Root, Binding::Table] {
        let mut input = TitleBuffer::new(4096, 0x82);
        input.set(0, FIRST);
        let working = TitleBuffer::new(4096, 0x3c);
        let address = working.address() + 16;
        let output = Output::new(1);
        let mut reader = Reader::new(binding);
        // Deliberately record the consumer before its GPU producer.
        let consumer = host.new_list().unwrap();
        reader.dispatch(&consumer, address, 16, &output, 0, 0);
        consumer.close();
        let producer = host.new_list().unwrap();
        assert!(crate::list::copy_buffers(
            &producer,
            address,
            input.address(),
            16
        ));
        producer.close();
        assert!(working.0.iter().all(|&byte| byte == 0x3c));
        host.execute(&[&producer, &consumer]);
        output.check(&[FIRST]);
        validation.check();
        cases += 1;

        // Identical recorded lists read current CPU input and consume the
        // new GPU write, rather than reusing their first submission's CBV.
        input.set(0, SECOND);
        host.execute(&[&producer, &consumer]);
        output.check(&[SECOND]);
        validation.check();
        cases += 1;

        // Reset releases the old bindings; a new source and table allocation
        // must still have the right lifetime through the next submission.
        consumer.reset();
        reader.state.reset(std::ptr::null());
        reader.state.set_heaps(&[reader.heap.0]);
        reader
            .state
            .set_table(true, 0, crate::descriptor::heap(reader.heap.0).start);
        reader.dispatch(&consumer, address, 16, &output, 0, 0);
        consumer.close();
        input.set(0, FIRST);
        host.execute(&[&producer, &consumer]);
        output.check(&[FIRST]);
        validation.check();
        cases += 1;

        // Identical CBV descriptor words can name newly registered backing
        // at the same title address. Earlier commands must keep the old
        // host storage, while the next table/root use selects the new one.
        input.set(16, SECOND);
        let replacement = host.new_list().unwrap();
        let output = Output::new(2);
        let mut reader = Reader::new(binding);
        assert!(crate::list::copy_buffers(
            &replacement,
            address,
            input.address(),
            16
        ));
        let before = reader.dispatch(&replacement, address, 16, &output, 0, 0);
        crate::shadow::unregister(working.address() as usize, working.address());
        crate::shadow::register(
            working.address() as usize,
            working.address(),
            working.0.len() as u64,
        );
        assert!(crate::list::copy_buffers(
            &replacement,
            address,
            input.address() + 16,
            16
        ));
        let after = reader.dispatch(&replacement, address, 16, &output, 1, 0);
        assert_ne!(
            before, after,
            "replaced backing must get a new native constant binding"
        );
        replacement.close();
        host.execute(&[&replacement]);
        output.check(&[FIRST, SECOND]);
        validation.check();
        cases += 2;
    }
    assert_eq!(cases, 10);
    eprintln!("VICTORY_CONSTANT_RESULT=executed cases={cases} validation_errors=0");
}
