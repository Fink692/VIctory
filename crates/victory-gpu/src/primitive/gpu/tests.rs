//! Authored rectangle inputs, actual title memory copies and native draws.
//! Compare complete bytes (including padding) with the existing CPU oracle,
//! then consume the expansion through the production title draw path.

use super::*;
use crate::object::{self, This, slot};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, PCSTR};

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
            // SAFETY: the stable boxed context lives until unregistration;
            // D3D12 owns the description for this callback's duration.
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
        let mut validation = Self {
            queue: device
                .cast()
                .expect("rectangle fixtures require diagnostics and VICTORY_D3D12_DEBUG=1"),
            messages: Box::default(),
            cookie: 0,
        };
        // SAFETY: the context stays boxed until Drop unregisters it. This
        // catches errors even when Host::execute drains the info queue.
        unsafe {
            validation
                .queue
                .RegisterMessageCallback(
                    Some(message),
                    D3D12_MESSAGE_CALLBACK_IGNORE_FILTERS,
                    (&raw mut *validation.messages).cast(),
                    &mut validation.cookie,
                )
                .unwrap();
        }
        validation
    }

    fn check(&self, phase: &str) {
        let messages = self.messages.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            messages.is_empty(),
            "{phase}: native validation errors: {messages:?}"
        );
    }
}

impl Drop for Validation {
    fn drop(&mut self) {
        // SAFETY: this registration and its context are still live.
        if unsafe { self.queue.UnregisterMessageCallback(self.cookie) }.is_err() {
            Box::leak(std::mem::take(&mut self.messages));
        }
    }
}

struct Memory(Box<[u8]>);

impl Memory {
    fn new(size: usize, fill: u8) -> Self {
        let memory = Self(vec![fill; size].into_boxed_slice());
        crate::shadow::register(memory.address() as usize, memory.address(), size as u64);
        memory
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let mut memory = Self::new(bytes.len(), 0);
        memory.0.copy_from_slice(bytes);
        memory
    }

    fn address(&self) -> u64 {
        self.0.as_ptr() as u64
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        crate::shadow::unregister(self.address() as usize, self.address());
    }
}

struct TitleList {
    title: This,
    allocator: This,
    newer: bool,
}

impl TitleList {
    fn new() -> Self {
        let null = std::ptr::null();
        let mut allocator = null;
        let mut title = null;
        let iid = crate::resource::PAGEABLE_IIDS[1];
        let newer = crate::device::newer();
        assert_eq!(
            crate::list::create_allocator(null, 0, &iid, &mut allocator),
            object::S_OK
        );
        assert_eq!(
            crate::list::create_list(null, 0, 0, allocator, null, &iid, &mut title),
            object::S_OK
        );
        Self {
            title,
            allocator,
            newer,
        }
    }

    fn host(&self) -> &HostList {
        crate::list::host_list(self.title).expect("title list has its native list")
    }

    fn memory_copy(&self, to: u64, from: u64, bytes: u64) {
        let index = if self.newer { 62 } else { 60 };
        slot::<extern "system" fn(This, u64, u64, u64, u32)>(self.title, index * 8)(
            self.title, to, from, bytes, 0,
        );
    }

    fn close(&self) {
        assert_eq!(
            slot::<extern "system" fn(This) -> i32>(self.title, 0x48)(self.title),
            object::S_OK
        );
    }

    fn reset(&self) {
        assert_eq!(
            slot::<extern "system" fn(This, This, This) -> i32>(self.title, 0x50)(
                self.title,
                self.allocator,
                std::ptr::null(),
            ),
            object::S_OK,
        );
    }

    fn pipeline(&self, pipeline: This) {
        slot::<extern "system" fn(This, This)>(self.title, 0xc8)(self.title, pipeline);
    }

    fn topology(&self, topology: u32) {
        slot::<extern "system" fn(This, u32)>(self.title, 0xa0)(self.title, topology);
    }

    fn vertices(&self, address: u64, size: u32, stride: u32) {
        #[repr(C)]
        struct View {
            address: u64,
            size: u32,
            stride: u32,
        }
        let view = View {
            address,
            size,
            stride,
        };
        slot::<extern "system" fn(This, u32, u32, *const View)>(self.title, 0x180)(
            self.title, 0, 1, &view,
        );
    }

    fn draw(&self, count: u32, first: u32) {
        slot::<extern "system" fn(This, u32, u32, u32, u32)>(self.title, 0x60)(
            self.title, count, 1, first, 0,
        );
    }

    fn dispatch(&self) {
        slot::<extern "system" fn(This, u32, u32, u32)>(self.title, 0x70)(self.title, 1, 1, 1);
    }
}

impl Drop for TitleList {
    fn drop(&mut self) {
        for object in [self.title, self.allocator] {
            assert_eq!(
                slot::<extern "system" fn(This) -> u32>(object, 0x10)(object),
                0
            );
        }
    }
}

#[derive(Clone)]
struct Fixture {
    label: String,
    bytes: Vec<u8>,
    address_offset: usize,
    count: u32,
    first: u32,
    stride: u32,
    elements: Vec<(u32, u32)>,
}

impl Fixture {
    fn size(&self) -> u32 {
        (self.bytes.len() - self.address_offset) as u32
    }

    fn expected(&self) -> Vec<u8> {
        let start = self.address_offset + (self.first * self.stride) as usize;
        let mut result = vec![0; (self.count / 3 * 6 * self.stride) as usize];
        super::super::expand_rects(
            &self.bytes[start..],
            &mut result,
            self.stride as usize,
            &self.elements,
        );
        result
    }

    fn produce(&self, list: &TitleList, input: &Memory, working: &Memory) {
        list.memory_copy(
            working.address() + self.address_offset as u64,
            input.address() + self.address_offset as u64,
            u64::from(self.size()),
        );
    }

    fn check_memory(&self, input: &Memory, working: &Memory) {
        assert_eq!(&*input.0, self.bytes, "{}: source changed", self.label);
        let mut wanted = self.bytes.clone();
        wanted[..self.address_offset].fill(0xe7);
        assert_eq!(
            &*working.0, wanted,
            "{}: raw copy/leading guard",
            self.label
        );
    }
}

/// A native copy consumes the expansion, with untouched sentinels on both
/// sides. Keep the expansion as well to check the final partial DWORD.
struct Output {
    expanded: Expanded,
    _buffer: ID3D12Resource,
    data: *mut u8,
    size: usize,
}

impl Output {
    fn record(list: &TitleList, working: &Memory, fixture: &Fixture) -> Self {
        let expanded = expand(
            list.host(),
            working.address() + fixture.address_offset as u64,
            fixture.count,
            fixture.first,
            fixture.size(),
            fixture.stride,
            &fixture.elements,
        )
        .unwrap_or_else(|error| panic!("{}: expansion failed: {error}", fixture.label))
        .expect("a complete rectangle produces a buffer");
        let size = fixture.expected().len();
        assert_eq!(expanded.view.SizeInBytes as usize, size);
        assert_eq!(expanded.view.StrideInBytes, fixture.stride);
        // SAFETY: a live native resource.
        assert_eq!(expanded.view.BufferLocation, unsafe {
            expanded.buffer.GetGPUVirtualAddress()
        });
        let (buffer, data) = host().unwrap().shared_buffer((size + 64) as u64).unwrap();
        // SAFETY: fresh mapped bytes, not yet submitted.
        unsafe { std::ptr::write_bytes(data, 0xa5, size + 64) };
        list.host()
            .buffer_state(&expanded.buffer, D3D12_RESOURCE_STATE_COPY_SOURCE);
        list.host()
            .buffer_state(&buffer, D3D12_RESOURCE_STATE_COPY_DEST);
        // SAFETY: the source/view and destination ranges are complete and
        // both resources stay alive through execution.
        unsafe {
            list.host()
                .list
                .CopyBufferRegion(&buffer, 32, &expanded.buffer, 0, size as u64)
        };
        Self {
            expanded,
            _buffer: buffer,
            data,
            size,
        }
    }

    fn check(&self, fixture: &Fixture) {
        let expected = fixture.expected();
        let mut guarded = vec![0xa5; self.size + 64];
        guarded[32..32 + self.size].copy_from_slice(&expected);
        // SAFETY: Host::execute has waited; the mapped native buffer owns
        // this complete region and no GPU work is outstanding.
        let actual = unsafe { std::slice::from_raw_parts(self.data, self.size + 64) };
        assert_eq!(
            actual, guarded,
            "{}: GPU expansion/consumer/guards",
            fixture.label
        );
        let mut mapped = std::ptr::null_mut();
        let padded = self.size.next_multiple_of(4);
        // SAFETY: the expansion allocated the padded size and execution
        // finished. Balance this additional mapping after the copy.
        unsafe {
            self.expanded
                .buffer
                .Map(
                    0,
                    Some(&D3D12_RANGE {
                        Begin: 0,
                        End: padded,
                    }),
                    Some(&mut mapped),
                )
                .unwrap();
            let bytes = std::slice::from_raw_parts(mapped.cast::<u8>(), padded);
            assert_eq!(
                &bytes[..self.size],
                expected,
                "{}: native expansion",
                fixture.label
            );
            assert!(
                bytes[self.size..].iter().all(|&v| v == 0),
                "{}: padded tail",
                fixture.label
            );
            self.expanded
                .buffer
                .Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
        }
    }
}

fn format_fixture(format: u32, components: usize, kind: u8) -> Fixture {
    let width = match kind {
        0 => 4,
        1 | 3 => 2,
        _ => 1,
    };
    // Odd stride and a one-byte element offset exercise DWORD crossings.
    let stride = (1 + components * width) as u32;
    let mut fixture = Fixture {
        label: format!("format-{format}"),
        bytes: vec![0x93; 3 + 7 * stride as usize],
        address_offset: 3,
        count: 6,
        first: 1,
        stride,
        elements: vec![(1, format)],
    };
    for vertex in 0..7 {
        for component in 0..components {
            let at = 3 + vertex * stride as usize + 1 + component * width;
            let corner = vertex.saturating_sub(1) % 3;
            let bytes = match kind {
                0 => ([-1.0f32, -2.0, 1.0][corner] + component as f32 * 0.25)
                    .to_le_bytes()
                    .to_vec(),
                1 => ([0x3c00u16, 0x4000, 0x4200][corner] + component as u16 * 0x80)
                    .to_le_bytes()
                    .to_vec(),
                2 => vec![
                    [[10u8, 200, 40, 240], [50, 10, 180, 50], [100, 80, 250, 250]][corner]
                        [component],
                ],
                _ => ([
                    [1000u16, 60000, 9000, 65000],
                    [9000, 1000, 50000, 9000],
                    [20000, 20000, 60000, 65000],
                ][corner][component])
                    .to_le_bytes()
                    .to_vec(),
            };
            fixture.bytes[at..at + width].copy_from_slice(&bytes);
        }
    }
    fixture
}

fn positions(rectangles: u32, first: u32, remainder: u32) -> Fixture {
    let count = rectangles * 3 + remainder;
    let stride = 13;
    let mut fixture = Fixture {
        label: "float2-color".into(),
        bytes: vec![0x96; 3 + ((first + count) * stride) as usize],
        address_offset: 3,
        count,
        first,
        stride,
        elements: vec![(1, 16), (9, 28)],
    };
    for vertex in 0..first + count {
        let i = vertex.saturating_sub(first);
        let (x, y) = [(-1.0f32, 1.0f32), (-1.0, -1.0), (1.0, -1.0)][(i % 3) as usize];
        let at = 3 + vertex as usize * 13;
        fixture.bytes[at + 1..at + 5].copy_from_slice(&(x + (i / 3) as f32 * 2.0).to_le_bytes());
        fixture.bytes[at + 5..at + 9].copy_from_slice(&y.to_le_bytes());
        fixture.bytes[at + 9..at + 13].copy_from_slice(
            &[[0, 50, 200, 255], [100, 150, 50, 255], [150, 200, 100, 255]][(i % 3) as usize],
        );
    }
    // An independent hand-computed corner checks the oracle's convention.
    let expected = fixture.expected();
    assert_eq!(&expected[66..70], &1.0f32.to_le_bytes());
    assert_eq!(&expected[70..74], &1.0f32.to_le_bytes());
    assert_eq!(&expected[74..78], &[50, 100, 250, 255]);
    fixture
}

fn shifted(fixture: &Fixture) -> Fixture {
    let mut changed = fixture.clone();
    changed.label.push_str("-changed");
    for vertex in changed.first..changed.first + changed.count {
        for (offset, delta) in [(1, 0.5), (5, -0.75)] {
            let at = changed.address_offset + (vertex * changed.stride) as usize + offset;
            let value = f32::from_le_bytes(changed.bytes[at..at + 4].try_into().unwrap()) + delta;
            changed.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    changed
}

fn run_fixture(fixture: &Fixture, validation: &Validation) {
    let input = Memory::from_bytes(&fixture.bytes);
    let working = Memory::new(fixture.bytes.len(), 0xe7);
    let list = TitleList::new();
    fixture.produce(&list, &input, &working);
    let output = Output::record(&list, &working, fixture);
    list.close();
    assert!(
        working.0.iter().all(|&v| v == 0xe7),
        "recording must not update title memory"
    );
    host().unwrap().execute(&[list.host()]);
    output.check(fixture);
    fixture.check_memory(&input, &working);
    validation.check(&fixture.label);
}

fn completed(cases: &mut u32, label: &str, validation: &Validation) {
    validation.check(label);
    *cases += 1;
    println!(
        "VICTORY_RECTANGLE_CASE=passed case={} label={label}",
        *cases
    );
}

#[test]
#[ignore = "requires D3D12; run with diagnostics and VICTORY_D3D12_WARP=1"]
fn gpu_rectangles_observe_copies_formats_and_rebind_draw_state() {
    crate::host::on_failure(|message| {
        use std::io::Write;
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "VICTORY_RECTANGLE_FAILURE {message}");
        let _ = stderr.flush();
        std::process::abort();
    });
    let host = host().expect("a D3D12 adapter is required");
    let validation = Validation::new(&host.device);
    assert!(
        host.log_messages().is_none(),
        "native initialization produced errors"
    );
    let mut cases = 0;
    for (format, components, kind) in [
        (2, 4, 0),
        (6, 3, 0),
        (16, 2, 0),
        (41, 1, 0),
        (10, 4, 1),
        (34, 2, 1),
        (54, 1, 1),
        (11, 4, 3),
        (35, 2, 3),
        (28, 4, 2),
        (29, 4, 2),
        (87, 4, 2),
        (91, 4, 2),
    ] {
        let fixture = format_fixture(format, components, kind);
        run_fixture(&fixture, &validation);
        completed(&mut cases, &fixture.label, &validation);
    }
    let mut mixed = positions(2, 1, 0);
    mixed.label = "overlapping-unknown-truncated-elements".into();
    mixed.elements.extend([
        (5, 34),
        (2, 35),
        (0, 0xffff),
        (12, 2),
        (13, 28),
        (u32::MAX, 2),
    ]);
    run_fixture(&mixed, &validation);
    completed(&mut cases, &mixed.label, &validation);

    let half_cases = [
        ([0x3e00u16, 0, 0x37fe], 0x3c00u16), // legacy mantissa-carry OR
        ([0x0001, 0, 0], 0),
        ([0x8001, 0, 0], 0x8000),
        ([0x7bff, 0, 0x7bff], 0x7c00),
        ([0xfbff, 0, 0xfbff], 0xfc00),
        ([0x8000, 0, 0x8000], 0x8000),
        ([0x3c00, 0x4000, 0x4200], 0x4000),
        ([0x0400, 0x0001, 0], 0),
    ];
    let mut half = Fixture {
        label: "half-signed-zero-subnormal-overflow-carry".into(),
        bytes: vec![0x71; 3 + half_cases.len() * 3 * 7],
        address_offset: 3,
        count: (half_cases.len() * 3) as u32,
        first: 0,
        stride: 7,
        elements: vec![(1, 54)],
    };
    for (rectangle, (values, _)) in half_cases.iter().enumerate() {
        for (vertex, value) in values.iter().enumerate() {
            let at = 3 + (rectangle * 3 + vertex) * 7 + 1;
            half.bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    for (rectangle, (_, value)) in half_cases.iter().enumerate() {
        let at = (rectangle * 6 + 5) * 7 + 1;
        assert_eq!(
            &half.expected()[at..at + 2],
            &value.to_le_bytes(),
            "independent half corner {rectangle}"
        );
    }
    run_fixture(&half, &validation);
    // Independent IEEE nearest-even truth table for v0 + v2 - v1.
    // Spell out the expected integer sign instead of computing it with
    // the same shader expression: only (-0, +0, -0) produces -0.
    let zero_cases = [
        ([0u32, 0, 0], 0u32),
        ([0, 0, 0x8000_0000], 0),
        ([0, 0x8000_0000, 0], 0),
        ([0, 0x8000_0000, 0x8000_0000], 0),
        ([0x8000_0000, 0, 0], 0),
        ([0x8000_0000, 0, 0x8000_0000], 0x8000_0000),
        ([0x8000_0000, 0x8000_0000, 0], 0),
        ([0x8000_0000, 0x8000_0000, 0x8000_0000], 0),
    ];
    for (format, width, shift) in [(54, 2usize, 16), (41, 4, 0)] {
        let stride = width + 3;
        let mut zeros = Fixture {
            label: format!("format-{format}-all-zero-sign-triples"),
            bytes: vec![0x5b; 3 + zero_cases.len() * 3 * stride],
            address_offset: 3,
            count: (zero_cases.len() * 3) as u32,
            first: 0,
            stride: stride as u32,
            elements: vec![(1, format)],
        };
        for (rectangle, (values, _)) in zero_cases.iter().enumerate() {
            for (vertex, value) in values.iter().enumerate() {
                let at = 3 + (rectangle * 3 + vertex) * stride + 1;
                zeros.bytes[at..at + width]
                    .copy_from_slice(&(*value >> shift).to_le_bytes()[..width]);
            }
        }
        let expected = zeros.expected();
        for (rectangle, (_, bits)) in zero_cases.iter().enumerate() {
            let at = (rectangle * 6 + 5) * stride + 1;
            assert_eq!(
                &expected[at..at + width],
                &(*bits >> shift).to_le_bytes()[..width],
                "independent zero sign: format={format} triple={rectangle}"
            );
        }
        run_fixture(&zeros, &validation);
    }
    completed(&mut cases, &half.label, &validation);
    let many = positions(65, 2, 0);
    run_fixture(&many, &validation);
    completed(&mut cases, "multiple-threadgroups", &validation);

    for separate in [false, true] {
        let fixture = positions(2, 1, 0);
        let input = Memory::from_bytes(&fixture.bytes);
        let working = Memory::new(fixture.bytes.len(), 0xe7);
        // The consumer is deliberately recorded before its producer.
        let consumer = TitleList::new();
        let output = Output::record(&consumer, &working, &fixture);
        consumer.close();
        let producer = TitleList::new();
        fixture.produce(&producer, &input, &working);
        producer.close();
        if separate {
            host.execute(&[producer.host()]);
            host.execute(&[consumer.host()]);
        } else {
            host.execute(&[producer.host(), consumer.host()]);
        }
        output.check(&fixture);
        fixture.check_memory(&input, &working);
        completed(
            &mut cases,
            if separate {
                "separate-submissions"
            } else {
                "reverse-recorded-batch"
            },
            &validation,
        );
    }
    {
        let fixture = positions(2, 1, 0);
        let changed = shifted(&fixture);
        let mut input = Memory::from_bytes(&fixture.bytes);
        let working = Memory::new(fixture.bytes.len(), 0xe7);
        let list = TitleList::new();
        fixture.produce(&list, &input, &working);
        let output = Output::record(&list, &working, &fixture);
        list.close();
        // Both executions get data changed after recording.
        for wanted in [&changed, &fixture] {
            input.0.copy_from_slice(&wanted.bytes);
            host.execute(&[list.host()]);
            output.check(wanted);
            wanted.check_memory(&input, &working);
            validation.check("list-replay");
        }
        completed(&mut cases, "list-replay", &validation);
        list.reset();
        input.0.copy_from_slice(&changed.bytes);
        changed.produce(&list, &input, &working);
        let reset_output = Output::record(&list, &working, &changed);
        list.close();
        host.execute(&[list.host()]);
        reset_output.check(&changed);
        changed.check_memory(&input, &working);
        completed(&mut cases, "list-reset", &validation);
    }
    {
        let list = TitleList::new();
        for count in 0..3 {
            assert!(
                expand(list.host(), 0, count, u32::MAX, 0, 0, &[])
                    .unwrap()
                    .is_none()
            );
        }
        list.close();
        host.execute(&[list.host()]);
        completed(&mut cases, "incomplete-rectangle-no-ops", &validation);
    }
    {
        let memory = Memory::new(8, 0x61);
        let list = TitleList::new();
        let address = memory.address();
        for (address, count, first, size, stride) in [
            (0, 3, 0, 24, 8),
            (address, 3, 0, 24, 0),
            (address, 3, u32::MAX, u32::MAX, 8),
            (u64::MAX - 3, 3, 0, 24, 8),
            (u64::MAX - 63, 3, 0, 48, 16), // loose-window rounding overflows
            (address, 3, 0, 23, 8),
            (address, u32::MAX, 0, u32::MAX, 1),
            (address, 3, 1, u32::MAX, u32::MAX),
            (address, 3, 0, 0, 8),
        ] {
            assert!(
                expand(list.host(), address, count, first, size, stride, &[(0, 16)]).is_err(),
                "invalid view accepted: address={address:#x} count={count} first={first} size={size} stride={stride}"
            );
        }
        list.close();
        host.execute(&[list.host()]);
        assert_eq!(&*memory.0, &[0x61; 8]);
        completed(&mut cases, "invalid-and-overflow-ranges", &validation);
    }
    let partial = positions(1, 2, 2);
    run_fixture(&partial, &validation);
    completed(&mut cases, "partial-final-rectangle", &validation);

    graphics_integration(&validation);
    completed(
        &mut cases,
        "title-draw-and-compute-cache-restoration",
        &validation,
    );
    assert_eq!(cases, 24);
    println!("VICTORY_RECTANGLE_RESULT=executed cases=24 validation_errors=0");
}

// Authored native programs wrapped in the ordinary title Pipeline object.
// This tests State::draw/dispatch and their real cache bookkeeping without
// storing or deriving any console shader binary from a title.
struct TitlePipeline(This);

impl Drop for TitlePipeline {
    fn drop(&mut self) {
        assert_eq!(
            slot::<extern "system" fn(This) -> u32>(self.0, 0x10)(self.0),
            0
        );
    }
}

const GRAPHICS_SOURCE: &str = r#"
[RootSignature("RootFlags(ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT)")]
float4 vs(float2 position : POSITION) : SV_Position {
    return float4(position, 0.5, 1);
}
float4 ps() : SV_Target { return float4(1, 0, 0, 1); }
[RootSignature("RootFlags(0)")]
[numthreads(1, 1, 1)]
void cs() {}
"#;

fn compile(entry: PCSTR, target: PCSTR) -> ID3DBlob {
    let mut code = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the authored source and static names outlive compilation.
    let result = unsafe {
        D3DCompile(
            GRAPHICS_SOURCE.as_ptr().cast(),
            GRAPHICS_SOURCE.len(),
            s!("rectangle-draw-regression"),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(error) = result {
        let detail = errors.map(|blob| {
            // SAFETY: the compiler blob owns this complete byte range.
            String::from_utf8_lossy(unsafe {
                std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize())
            })
            .into_owned()
        });
        panic!("authored rectangle shader: {error}; {detail:?}");
    }
    code.unwrap()
}

fn bytecode(blob: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
    // SAFETY: the bytecode description borrows a live compiler blob.
    unsafe {
        D3D12_SHADER_BYTECODE {
            pShaderBytecode: blob.GetBufferPointer(),
            BytecodeLength: blob.GetBufferSize(),
        }
    }
}

fn programs(device: &ID3D12Device) -> (TitlePipeline, TitlePipeline) {
    use crate::pipeline::host::{HostPipeline, Input};
    let vs = compile(s!("vs"), s!("vs_5_1"));
    let ps = compile(s!("ps"), s!("ps_5_1"));
    let cs = compile(s!("cs"), s!("cs_5_1"));
    let root = |blob: &ID3DBlob| {
        // SAFETY: authored shader bytes include a valid root signature.
        unsafe {
            device
                .CreateRootSignature::<ID3D12RootSignature>(
                    0,
                    std::slice::from_raw_parts(
                        blob.GetBufferPointer().cast(),
                        blob.GetBufferSize(),
                    ),
                )
                .unwrap()
        }
    };
    let graphics_root = root(&vs);
    let compute_root = root(&cs);
    let input = [D3D12_INPUT_ELEMENT_DESC {
        SemanticName: s!("POSITION"),
        Format: DXGI_FORMAT_R32G32_FLOAT,
        InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
        ..Default::default()
    }];
    let blend = D3D12_RENDER_TARGET_BLEND_DESC {
        SrcBlend: D3D12_BLEND_ONE,
        DestBlend: D3D12_BLEND_ZERO,
        BlendOp: D3D12_BLEND_OP_ADD,
        SrcBlendAlpha: D3D12_BLEND_ONE,
        DestBlendAlpha: D3D12_BLEND_ZERO,
        BlendOpAlpha: D3D12_BLEND_OP_ADD,
        LogicOp: D3D12_LOGIC_OP_NOOP,
        RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
        ..Default::default()
    };
    let keep = D3D12_DEPTH_STENCILOP_DESC {
        StencilFailOp: D3D12_STENCIL_OP_KEEP,
        StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
        StencilPassOp: D3D12_STENCIL_OP_KEEP,
        StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
    };
    // SAFETY: descriptions borrow live root signatures, shader blobs and
    // input elements through creation; native PSOs retain their own data.
    let (graphics_pso, compute_pso) = unsafe {
        (
            device
                .CreateGraphicsPipelineState(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&graphics_root),
                    VS: bytecode(&vs),
                    PS: bytecode(&ps),
                    BlendState: D3D12_BLEND_DESC {
                        RenderTarget: [blend; 8],
                        ..Default::default()
                    },
                    SampleMask: u32::MAX,
                    RasterizerState: D3D12_RASTERIZER_DESC {
                        FillMode: D3D12_FILL_MODE_SOLID,
                        CullMode: D3D12_CULL_MODE_NONE,
                        DepthClipEnable: true.into(),
                        ..Default::default()
                    },
                    DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                        DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                        DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                        StencilReadMask: 0xff,
                        StencilWriteMask: 0xff,
                        FrontFace: keep,
                        BackFace: keep,
                        ..Default::default()
                    },
                    InputLayout: D3D12_INPUT_LAYOUT_DESC {
                        pInputElementDescs: input.as_ptr(),
                        NumElements: 1,
                    },
                    PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                    NumRenderTargets: 1,
                    RTVFormats: [
                        DXGI_FORMAT_R8G8B8A8_UNORM,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_FORMAT_UNKNOWN,
                        DXGI_FORMAT_UNKNOWN,
                    ],
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    ..Default::default()
                })
                .unwrap(),
            device
                .CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&compute_root),
                    CS: bytecode(&cs),
                    ..Default::default()
                })
                .unwrap(),
        )
    };
    let graphics = crate::pipeline::from_host_for_test(HostPipeline {
        compute: false,
        mesh: false,
        pso: graphics_pso,
        root: graphics_root,
        stages: Vec::new(),
        slots: 1,
        inputs: vec![Input {
            slot: 0,
            offset: 0,
            format: 16,
            per_instance: false,
        }],
        name: "authored-rectangle-graphics".into(),
    });
    let compute = crate::pipeline::from_host_for_test(HostPipeline {
        compute: true,
        mesh: false,
        pso: compute_pso,
        root: compute_root,
        stages: Vec::new(),
        slots: 0,
        inputs: Vec::new(),
        name: "authored-rectangle-compute".into(),
    });
    (TitlePipeline(graphics), TitlePipeline(compute))
}

fn graphics_integration(validation: &Validation) {
    let host = host().unwrap();
    let (graphics, compute) = programs(&host.device);
    let corners = |left: f32, right: f32| {
        [(9.0f32, 9.0f32), (left, 1.0), (left, -1.0), (right, -1.0)]
            .into_iter()
            .flat_map(|(x, y)| [x.to_le_bytes(), y.to_le_bytes()].concat())
            .collect::<Vec<_>>()
    };
    let left = Memory::from_bytes(&corners(-1.0, 0.0));
    let right = Memory::from_bytes(&corners(0.0, 1.0));
    let working = Memory::new(32, 0);
    let mut color: Option<ID3D12Resource> = None;
    let mut readback: Option<ID3D12Resource> = None;
    // SAFETY: complete one-mip texture and readback descriptions.
    unsafe {
        host.device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: 4,
                    Height: 4,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                None,
                &mut color,
            )
            .unwrap();
        host.device
            .CreateCommittedResource(
                &D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_READBACK,
                    ..Default::default()
                },
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                    Width: 1024,
                    Height: 1,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &mut readback,
            )
            .unwrap();
    }
    let (color, readback) = (color.unwrap(), readback.unwrap());
    let rtv = host.rtv(&color, None);
    let title = TitleList::new();
    let list = title.host();
    list.keep(&color);
    list.keep(&readback);
    // Establish the compute root cache before the rectangle helper replaces
    // native compute state, then the graphics PSO cache before expansion.
    title.pipeline(compute.0);
    title.dispatch();
    title.pipeline(graphics.0);
    title.topology(4);
    title.vertices(working.address(), 32, 8);
    // SAFETY: an open native list and live RTV/viewport/scissor data.
    unsafe {
        list.list.OMSetRenderTargets(1, Some(&rtv), false, None);
        list.list.RSSetViewports(&[D3D12_VIEWPORT {
            Width: 4.0,
            Height: 4.0,
            MaxDepth: 1.0,
            ..Default::default()
        }]);
        list.list.RSSetScissorRects(&[RECT {
            left: 0,
            top: 0,
            right: 4,
            bottom: 4,
        }]);
    }
    title.draw(3, 1);
    // SAFETY: valid RTV in render-target state, following the first draw.
    unsafe {
        list.list
            .ClearRenderTargetView(rtv, &[0.0, 0.0, 0.0, 0.0], None)
    };
    title.topology(17);
    for input in [&left, &right] {
        title.memory_copy(working.address(), input.address(), 32);
        title.draw(3, 1);
    }
    title.pipeline(compute.0);
    title.dispatch();
    // Copy the actual render target after both rectangle draws. Each half
    // must be red; stale CPU vertices would leave the entire target clear.
    let source = D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: borrowed native pointer, never released by this struct.
        pResource: unsafe { std::mem::transmute_copy(&color) },
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: 0,
        },
    };
    let destination = D3D12_TEXTURE_COPY_LOCATION {
        // SAFETY: borrowed native pointer, never released by this struct.
        pResource: unsafe { std::mem::transmute_copy(&readback) },
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
            PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                Offset: 0,
                Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    Width: 4,
                    Height: 4,
                    Depth: 1,
                    RowPitch: 256,
                },
            },
        },
    };
    // SAFETY: complete copy footprint, correctly transitioned source and
    // immutable COPY_DEST readback heap; objects remain alive through wait.
    unsafe {
        list.list.ResourceBarrier(&[crate::host::transition(
            &color,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        )]);
        list.list
            .CopyTextureRegion(&destination, 0, 0, 0, &source, None);
    }
    title.close();
    host.execute(&[list]);
    validation.check("actual-title-draw-and-dispatch");
    let mut mapped = std::ptr::null_mut();
    // SAFETY: the synchronous submission has completed all texture writes.
    unsafe {
        readback
            .Map(
                0,
                Some(&D3D12_RANGE {
                    Begin: 0,
                    End: 1024,
                }),
                Some(&mut mapped),
            )
            .unwrap();
        let bytes = std::slice::from_raw_parts(mapped.cast::<u8>(), 1024);
        for y in 0..4 {
            for x in 0..4 {
                let at = y * 256 + x * 4;
                assert_eq!(
                    &bytes[at..at + 4],
                    &[255, 0, 0, 255],
                    "rectangle pixel ({x},{y})"
                );
            }
        }
        readback.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
    }
}
