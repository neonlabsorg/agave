use {
    agave_scheduler_bindings::{
        worker_message_types::{
            self, CheckResponse, ExecutionResponse, CHECK_RESPONSE, EXECUTION_RESPONSE,
        },
        TransactionResponseRegion,
    },
    rts_alloc::Allocator,
};

/// Prepare a [`TransactionResponseRegion`] with [`ExecutionResponse`].
pub fn execution_responses_from_iter(
    allocator: &Allocator,
    iter: impl ExactSizeIterator<Item = ExecutionResponse>,
) -> Option<TransactionResponseRegion> {
    // SAFETY: EXECUTION_RESPONSE -> ExecutionResponse
    unsafe { from_iterator(allocator, EXECUTION_RESPONSE, iter) }
}

/// Prepare a [`TransactionResponseRegion`] with [`CheckResponse`].
pub fn check_responses_from_iter(
    allocator: &Allocator,
    iter: impl ExactSizeIterator<Item = CheckResponse>,
) -> Option<TransactionResponseRegion> {
    // SAFETY: CHECK_RESPONSE -> CheckResponse
    unsafe { from_iterator(allocator, CHECK_RESPONSE, iter) }
}

/// Prepare a [`TransactionResponseRegion`] from an iterator.
///
/// # Safety
/// - T must be a valid response type
/// - `tag` must match the `T`
unsafe fn from_iterator<T: Sized>(
    allocator: &Allocator,
    tag: u8,
    iter: impl ExactSizeIterator<Item = T>,
) -> Option<TransactionResponseRegion> {
    let num_transaction_responses = iter.len();
    let size = num_transaction_responses.wrapping_mul(core::mem::size_of::<T>());
    let response_ptr = allocator.allocate(size as u32)?.cast::<T>();

    for (index, response) in iter.enumerate() {
        debug_assert!(
            response_ptr.is_aligned(),
            "allocator should guarantee alignment for the response types of interest"
        );

        // SAFETY: `response_ptr` is sufficiently sized to fit the response vector.
        unsafe { response_ptr.add(index).write(response) };
    }

    // SAFETY: `response_ptr` was allocated from the allocator.
    let transaction_responses_offset = unsafe { allocator.offset(response_ptr.cast()) };

    Some(TransactionResponseRegion {
        tag,
        num_transaction_responses: num_transaction_responses as u8,
        transaction_responses_offset,
    })
}

pub struct CheckResponsesPtr<'a> {
    ptr: NonNull<CheckResponse>,
    count: usize,
    allocator: &'a Allocator,
}

impl<'a> CheckResponsesPtr<'a> {
    /// Constructs the pointer from a [`TransactionResponseRegion`].
    ///
    /// # Safety
    ///
    /// - The provided [`TransactionResponseRegion`] must be of type
    ///   [`worker_message_types::CHECK_RESPONSE`].
    /// - The allocation pointed to by this region must not have previously been freed.
    /// - Pointer must be exclusive so that calling [`Self::free`] is safe.
    pub unsafe fn from_transaction_response_region(
        transaction_response_region: &TransactionResponseRegion,
        allocator: &'a Allocator,
    ) -> Self {
        debug_assert!(transaction_response_region.tag == worker_message_types::CHECK_RESPONSE);

        Self {
            ptr: allocator
                .ptr_from_offset(transaction_response_region.transaction_responses_offset)
                .cast(),
            count: transaction_response_region.num_transaction_responses as usize,
            allocator,
        }
    }

    /// The number of responses in this batch.
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether the batch is empty.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate the responses within the batch.
    pub fn iter(&self) -> impl Iterator<Item = &CheckResponse> {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.count) }.iter()
    }

    /// Free the batch's allocation.
    pub fn free(self) {
        unsafe { self.allocator.free(self.ptr.cast()) }
    }
}

pub struct ExecutionResponsesPtr<'a> {
    ptr: NonNull<ExecutionResponse>,
    count: usize,
    allocator: &'a Allocator,
}

impl<'a> ExecutionResponsesPtr<'a> {
    /// Constructs the pointer from a [`TransactionResponseRegion`].
    ///
    /// # Safety
    ///
    /// - The provided [`TransactionResponseRegion`] must be of type
    ///   [`worker_message_types::EXECUTION_RESPONSE`].
    /// - The allocation pointed to by this region must not have previously been freed.
    /// - Pointer must be exclusive so that calling [`Self::free`] is safe.
    pub unsafe fn from_transaction_response_region(
        transaction_response_region: &TransactionResponseRegion,
        allocator: &'a Allocator,
    ) -> Self {
        debug_assert!(transaction_response_region.tag == worker_message_types::EXECUTION_RESPONSE);

        Self {
            ptr: allocator
                .ptr_from_offset(transaction_response_region.transaction_responses_offset)
                .cast(),
            count: transaction_response_region.num_transaction_responses as usize,
            allocator,
        }
    }

    /// The number of responses in this batch.
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Whether the batch is empty.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate the responses within the batch.
    pub fn iter(&self) -> impl Iterator<Item = &ExecutionResponse> {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.count) }.iter()
    }

    /// Free the batch's allocation.
    pub fn free(self) {
        unsafe { self.allocator.free(self.ptr.cast()) }
    }
}
