//! Title query heaps backed by host D3D12 queries. Results stay on the
//! GPU until submission completes, so later GPU work can consume them.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use crate::object::{self, E_INVALIDARG, HRESULT, Iid, Object, QUERY_HEAP, Table, This, guid};
use crate::resource::PAGEABLE_IIDS;

const HEAP_IIDS: [Iid; 4] = [
    guid(
        0x0d9658ae,
        0xed45,
        0x469e,
        [0xa6, 0x1d, 0x97, 0x0e, 0xc5, 0x83, 0xca, 0xb4],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct Desc {
    kind: u32,
    count: u32,
    nodes: u32,
}

#[repr(C)]
struct Heap {
    object: Object,
    desc: Desc,
    #[cfg(windows)]
    host: Option<windows::Win32::Graphics::Direct3D12::ID3D12QueryHeap>,
}

#[cfg(any(windows, test))]
impl Desc {
    /// Query types are not heap types: binary occlusion shares heap 0,
    /// and a timestamp (type 2) belongs to heap 1 (or copy heap 5).
    fn result_size(self, kind: u32) -> Option<u64> {
        match (self.kind, kind) {
            (0, 0 | 1) | (1 | 5, 2) => Some(8),
            (2, 3) => Some(88),
            (3, 4..=7) => Some(16),
            _ => None,
        }
    }

    fn range(self, kind: u32, first: u32, count: u32) -> Option<u64> {
        if count == 0 || first.checked_add(count)? > self.count {
            return None;
        }
        self.result_size(kind)?.checked_mul(u64::from(count))
    }
}

#[cfg(any(windows, test))]
fn destination(offset: u64, bytes: u64, size: u64) -> bool {
    offset.is_multiple_of(8) && offset.checked_add(bytes).is_some_and(|end| end <= size)
}

/// Device method.
pub(crate) extern "system" fn create(
    _: This,
    desc: *const Desc,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    if desc.count == 0 || !matches!(desc.kind, 0..=3 | 5) {
        return E_INVALIDARG;
    }
    #[cfg(windows)]
    let host = match crate::host::host() {
        Some(host) => {
            use windows::Win32::Graphics::Direct3D12::*;
            let mut heap: Option<ID3D12QueryHeap> = None;
            // All title queues currently run on the host direct queue.
            let kind = if desc.kind == 5 { 1 } else { desc.kind };
            // SAFETY: an initialized descriptor and our out pointer.
            if let Err(e) = unsafe {
                host.device.CreateQueryHeap(
                    &D3D12_QUERY_HEAP_DESC {
                        Type: D3D12_QUERY_HEAP_TYPE(kind as i32),
                        Count: desc.count,
                        NodeMask: 0,
                    },
                    &mut heap,
                )
            } {
                tracing::warn!(error = %e, ?desc, "host query heap refused");
                return e.code().0;
            }
            heap
        }
        None => None,
    };
    tracing::debug!(?desc, "query heap");
    let heap = Box::new(Heap {
        object: Object::new(&FUNCTIONS),
        desc,
        #[cfg(windows)]
        host,
    });
    object::hand_out(QUERY_HEAP, heap, iid, &HEAP_IIDS, out)
}

#[cfg(windows)]
fn heap<'a>(this: This) -> Option<&'a Heap> {
    // SAFETY: the title passes a live query heap, or null.
    unsafe { this.cast::<Heap>().as_ref() }
}

/// Records Begin/EndQuery at its actual position in the command stream.
#[cfg(windows)]
pub(crate) fn mark(list: &crate::host::HostList, this: This, kind: u32, index: u32, begin: bool) {
    use windows::Win32::Graphics::Direct3D12::D3D12_QUERY_TYPE;
    let Some(heap) = heap(this) else { return };
    if heap.desc.range(kind, index, 1).is_none() || begin && kind == 2 {
        tracing::warn!(kind, index, begin, "invalid query marker");
        return;
    }
    let Some(host) = &heap.host else { return };
    list.keep(host);
    #[cfg(feature = "diagnostics")]
    list.note_op(|| {
        format!(
            "{} query {this:?} type {kind} index {index}",
            if begin { "begin" } else { "end" }
        )
    });
    // SAFETY: an open list and live heap; kind and index checked above.
    unsafe {
        if begin {
            list.list
                .BeginQuery(host, D3D12_QUERY_TYPE(kind as i32), index);
        } else {
            list.list
                .EndQuery(host, D3D12_QUERY_TYPE(kind as i32), index);
        }
    }
}

/// Resolves into the buffer's existing GPU copy. Its ordinary copy-back
/// path publishes the measured values before bottom-of-pipe signals.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve(
    list: &crate::host::HostList,
    this: This,
    kind: u32,
    first: u32,
    count: u32,
    data: *mut u8,
    size: u64,
    offset: u64,
) {
    use windows::Win32::Graphics::Direct3D12::*;
    let Some(heap) = heap(this) else { return };
    let Some(bytes) = heap.desc.range(kind, first, count) else {
        tracing::warn!(kind, first, count, "invalid query resolve range");
        return;
    };
    if !destination(offset, bytes, size) {
        tracing::warn!(
            offset,
            bytes,
            size,
            "query resolve outside its buffer or misaligned"
        );
        return;
    }
    let Some(host) = &heap.host else { return };
    let Some(address) = (data as u64).checked_add(offset) else {
        return;
    };
    let Some(shadow) = crate::shadow::find(address, bytes) else {
        return;
    };
    shadow.refresh(list, address, bytes);
    shadow.gpu_writes(list, address, bytes);
    list.buffer_state(&shadow.buffer, D3D12_RESOURCE_STATE_COPY_DEST);
    list.keep(host);
    #[cfg(feature = "diagnostics")]
    list.note_op(|| format!("resolve queries {this:?} type {kind} {first}+{count}"));
    // SAFETY: the list keeps the heap and shadow alive. The query and
    // output ranges were checked above; later consumers choose their own
    // state through the shared-buffer state tracker.
    unsafe {
        list.list.ResolveQueryData(
            host,
            D3D12_QUERY_TYPE(kind as i32),
            first,
            count,
            &shadow.buffer,
            shadow.offset(address),
        );
    }
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    object::answer(
        QUERY_HEAP,
        iid,
        object::is(iid, &HEAP_IIDS).then_some(this),
        out,
    )
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Heap>(this)
}

/// IUnknown 3, object 4, device child 1.
static FUNCTIONS: Table<24> = {
    let mut f = object::d3d12_object(object::missing_table::<QUERY_HEAP, 24>());
    f[0] = query as *const ();
    f[2] = release as *const ();
    f[7] = object::parent_device as *const (); // 0x38
    Table(f)
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_kinds_match_their_heaps_and_result_sizes() {
        let heap = |kind| Desc {
            kind,
            count: 8,
            nodes: 0,
        };
        assert_eq!(heap(0).range(0, 0, 2), Some(16));
        assert_eq!(heap(0).range(1, 0, 2), Some(16));
        assert_eq!(heap(1).range(2, 0, 2), Some(16));
        assert_eq!(heap(5).range(2, 0, 2), Some(16));
        assert_eq!(heap(2).range(3, 0, 2), Some(176));
        assert_eq!(heap(3).range(7, 0, 2), Some(32));
        assert_eq!(heap(0).range(2, 0, 2), None);
        assert_eq!(heap(1).range(1, 0, 2), None);
        assert_eq!(heap(2).range(99, 0, 2), None);
    }

    #[test]
    fn query_ranges_reject_overflow_and_out_of_bounds() {
        let heap = Desc {
            kind: 1,
            count: 8,
            nodes: 0,
        };
        assert_eq!(heap.range(2, 7, 1), Some(8));
        assert_eq!(heap.range(2, 7, 2), None);
        assert_eq!(heap.range(2, u32::MAX, 2), None);
        assert_eq!(heap.range(2, 0, 0), None);
        assert!(destination(8, 8, 16));
        assert!(!destination(9, 8, 32));
        assert!(!destination(16, 8, 16));
        assert!(!destination(u64::MAX - 7, 8, u64::MAX));
    }

    /// Exercises real recording, GPU resolve, copy-back, and heap lifetime.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires D3D12; run with diagnostics and VICTORY_D3D12_WARP=1"]
    fn timestamp_queries_execute_and_preserve_destination_guards() {
        let host = crate::host::host().expect("a D3D12 adapter is required");
        let memory = crate::resource::Memory::zeroed(65536, 65536).unwrap();
        let address = memory.address();
        let ptr = address as *mut u64;
        let guard = 0x1234_5678_9abc_def0u64;
        // SAFETY: four words within our allocation.
        unsafe { std::slice::from_raw_parts_mut(ptr, 4).fill(guard) };
        crate::shadow::register(address as usize, address, 32);
        let mut query_heap = std::ptr::null();
        assert_eq!(
            create(
                std::ptr::null(),
                &Desc {
                    kind: 1,
                    count: 2,
                    nodes: 0
                },
                &HEAP_IIDS[0],
                &mut query_heap
            ),
            object::S_OK
        );
        let list = host.new_list().unwrap();
        mark(&list, query_heap, 2, 0, false);
        mark(&list, query_heap, 2, 1, false);
        resolve(&list, query_heap, 2, 0, 2, ptr.cast(), 32, 8);
        // The list must keep the native heap alive through execution.
        release(query_heap);
        // SAFETY: our allocation remains live through the test.
        assert_eq!(unsafe { *ptr.add(1) }, guard);
        list.close();
        host.execute(&[&list]);
        // SAFETY: the submission completed; the words are still allocated.
        let values = unsafe { std::slice::from_raw_parts(ptr, 4) };
        assert_eq!((values[0], values[3]), (guard, guard));
        assert_ne!(values[1], guard);
        assert_ne!(values[2], guard);
        assert!(values[1] > 0 && values[2] >= values[1]);
        assert!(host.timestamp_frequency().unwrap() > 0);
        crate::shadow::unregister(address as usize, address);
    }

    /// Covers a same-buffer GPU copy (via staging), a second list recorded
    /// before its producer, and a read-to-query-write transition. Copying
    /// title memory before submission would propagate the guard instead.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires D3D12; run with diagnostics and VICTORY_D3D12_WARP=1"]
    fn resolved_queries_feed_gpu_copies_across_lists() {
        let host = crate::host::host().expect("a D3D12 adapter is required");
        let memory_a = crate::resource::Memory::zeroed(65536, 65536).unwrap();
        let memory_b = crate::resource::Memory::zeroed(65536, 65536).unwrap();
        let (a, b) = (memory_a.address(), memory_b.address());
        let guard = 0x1234_5678_9abc_def0u64;
        for address in [a, b] {
            // SAFETY: the allocation holds at least eight words.
            unsafe { std::slice::from_raw_parts_mut(address as *mut u64, 8).fill(guard) };
            crate::shadow::register(address as usize, address, 64);
        }
        let mut heap = std::ptr::null();
        assert_eq!(
            create(
                std::ptr::null(),
                &Desc {
                    kind: 1,
                    count: 1,
                    nodes: 0
                },
                &HEAP_IIDS[0],
                &mut heap
            ),
            object::S_OK
        );

        // Submission order, not recording order, determines start states.
        let second = host.new_list().unwrap();
        assert!(crate::list::copy_buffers(&second, b + 8, a + 32, 8));
        second.close();
        let first = host.new_list().unwrap();
        mark(&first, heap, 2, 0, false);
        resolve(&first, heap, 2, 0, 1, a as *mut u8, 64, 8);
        assert!(crate::list::copy_buffers(&first, a + 32, a + 8, 8));
        first.close();
        // SAFETY: the title allocations are live and not executing.
        assert_eq!(unsafe { *((b + 8) as *const u64) }, guard);
        host.execute(&[&first, &second]);
        let read = |address: u64| {
            // SAFETY: these addresses are within the completed allocations.
            unsafe { *(address as *const u64) }
        };
        let first_stamp = read(a + 8);
        assert!(first_stamp > 0 && first_stamp != guard);
        assert_eq!(read(b + 8), first_stamp);
        // Repeated title lists are serialized with completion waits;
        // reusing one native direct list in an in-flight batch is invalid.
        host.execute(&[&first, &second, &first, &second]);
        let stamp = read(a + 8);
        assert!(stamp > 0 && stamp != guard);
        assert_eq!(read(a + 32), stamp);
        assert_eq!(read(b + 8), stamp);

        let third = host.new_list().unwrap();
        assert!(crate::list::copy_buffers(&third, b + 16, a + 32, 8));
        mark(&third, heap, 2, 0, false);
        resolve(&third, heap, 2, 0, 1, a as *mut u8, 64, 32);
        release(heap);
        third.close();
        host.execute(&[&third]);
        assert_eq!(read(b + 16), stamp);
        assert!(read(a + 32) >= stamp);
        for offset in [0, 24, 40, 48, 56] {
            assert_eq!(read(a + offset), guard);
            assert_eq!(read(b + offset), guard);
        }
        for address in [a, b] {
            crate::shadow::unregister(address as usize, address);
        }
    }

    /// An empty measured interval has no visible samples or pipeline
    /// invocations; fabricated "always visible" results fail this test.
    #[cfg(windows)]
    #[test]
    #[ignore = "requires D3D12; run with diagnostics and VICTORY_D3D12_WARP=1"]
    fn empty_occlusion_and_pipeline_queries_return_measured_zeroes() {
        let host = crate::host::host().expect("a D3D12 adapter is required");
        for (heap_kind, query_kind, bytes) in [(0, 0, 8), (0, 1, 8), (2, 3, 88)] {
            let memory = crate::resource::Memory::zeroed(65536, 65536).unwrap();
            let address = memory.address();
            let ptr = address as *mut u64;
            let size = bytes + 16;
            let guard = 0x1234_5678_9abc_def0u64;
            // SAFETY: the result and two guard words fit our allocation.
            unsafe { std::slice::from_raw_parts_mut(ptr, size as usize / 8).fill(guard) };
            crate::shadow::register(address as usize, address, size);
            let mut query_heap = std::ptr::null();
            assert_eq!(
                create(
                    std::ptr::null(),
                    &Desc {
                        kind: heap_kind,
                        count: 1,
                        nodes: 0
                    },
                    &HEAP_IIDS[0],
                    &mut query_heap
                ),
                object::S_OK
            );
            let list = host.new_list().unwrap();
            mark(&list, query_heap, query_kind, 0, true);
            mark(&list, query_heap, query_kind, 0, false);
            resolve(&list, query_heap, query_kind, 0, 1, ptr.cast(), size, 8);
            release(query_heap);
            list.close();
            host.execute(&[&list]);
            // SAFETY: the allocation outlives the completed submission.
            let values = unsafe { std::slice::from_raw_parts(ptr, size as usize / 8) };
            assert_eq!((values[0], values[values.len() - 1]), (guard, guard));
            assert!(values[1..values.len() - 1].iter().all(|&v| v == 0));
            crate::shadow::unregister(address as usize, address);
        }
    }
}
