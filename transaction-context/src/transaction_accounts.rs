#[cfg(feature = "dev-context-only-utils")]
use qualifier_attr::qualifiers;
use {
    crate::{
        IndexOfAccount, MAX_ACCOUNT_DATA_GROWTH_PER_TRANSACTION, MAX_ACCOUNT_DATA_LEN,
        SUBACCOUNT_MARKER,
        vm_addresses::{GUEST_ACCOUNT_PAYLOAD_BASE_ADDRESS, GUEST_REGION_SIZE},
        vm_slice::VmSlice,
    },
    solana_account::{AccountSharedData, ReadableAccount, WritableAccount},
    solana_instruction::error::InstructionError,
    solana_pubkey::Pubkey,
    std::{
        cell::{Cell, RefCell, UnsafeCell},
        ops::{Deref, DerefMut},
        ptr,
        sync::Arc,
    },
};

/// This struct is shared with programs. Do not alter its fields.
#[repr(C)]
#[derive(Debug, PartialEq)]
struct AccountSharedFields {
    key: Pubkey,
    owner: Pubkey,
    lamports: u64,
    // The payload is going to be filled with the guest virtual address of the account payload
    // vector.
    payload: VmSlice<u8>,
}

#[derive(Debug, PartialEq)]
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
struct AccountPrivateFields {
    rent_epoch: u64,
    executable: bool,
    payload: Arc<Vec<u8>>,
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl AccountPrivateFields {
    fn payload_len(&self) -> usize {
        self.payload.len()
    }
}

#[derive(Debug, PartialEq)]
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
pub struct TransactionAccountView<'a> {
    abi_account: &'a AccountSharedFields,
    private_fields: &'a AccountPrivateFields,
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl ReadableAccount for TransactionAccountView<'_> {
    fn lamports(&self) -> u64 {
        self.abi_account.lamports
    }

    fn data(&self) -> &[u8] {
        self.private_fields.payload.as_slice()
    }

    fn owner(&self) -> &Pubkey {
        &self.abi_account.owner
    }

    fn executable(&self) -> bool {
        self.private_fields.executable
    }

    fn rent_epoch(&self) -> u64 {
        self.private_fields.rent_epoch
    }
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl PartialEq<AccountSharedData> for TransactionAccountView<'_> {
    fn eq(&self, other: &AccountSharedData) -> bool {
        other.lamports() == self.lamports()
            && other.data() == self.data()
            && other.owner() == self.owner()
            && other.executable() == self.executable()
            && other.rent_epoch() == self.rent_epoch()
    }
}

#[derive(Debug)]
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
pub struct TransactionAccountViewMut<'a> {
    abi_account: &'a mut AccountSharedFields,
    private_fields: &'a mut AccountPrivateFields,
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl TransactionAccountViewMut<'_> {
    fn data_mut(&mut self) -> &mut Vec<u8> {
        Arc::make_mut(&mut self.private_fields.payload)
    }

    pub fn resize(&mut self, new_len: usize, value: u8) {
        self.data_mut().resize(new_len, value);
        // SAFETY: We are synchronizing the lengths.
        unsafe {
            self.abi_account.payload.set_len(new_len as u64);
        }
    }

    #[cfg_attr(feature = "dev-context-only-utils", qualifiers(pub))]
    pub(crate) fn set_data_from_slice(&mut self, new_data: &[u8]) {
        // If the buffer isn't shared, we're going to memcpy in place.
        let Some(data) = Arc::get_mut(&mut self.private_fields.payload) else {
            // If the buffer is shared, the cheapest thing to do is to clone the
            // incoming slice and replace the buffer.
            self.private_fields.payload = Arc::new(new_data.to_vec());
            // SAFETY: We are synchronizing the lengths.
            unsafe {
                self.abi_account.payload.set_len(new_data.len() as u64);
            }
            return;
        };

        let new_len = new_data.len();

        // Reserve additional capacity if needed. Here we make the assumption
        // that growing the current buffer is cheaper than doing a whole new
        // allocation to make `new_data` owned.
        //
        // This assumption holds true during CPI, especially when the account
        // size doesn't change but the account is only changed in place. And
        // it's also true when the account is grown by a small margin (the
        // realloc limit is quite low), in which case the allocator can just
        // update the allocation metadata without moving.
        //
        // Shrinking and copying in place is always faster than making
        // `new_data` owned, since shrinking boils down to updating the Vec's
        // length.

        data.reserve(new_len.saturating_sub(data.len()));

        // Safety:
        // We just reserved enough capacity. We set data::len to 0 to avoid
        // possible UB on panic (dropping uninitialized elements), do the copy,
        // finally set the new length once everything is initialized.
        #[allow(clippy::uninit_vec)]
        // this is a false positive, the lint doesn't currently special case set_len(0)
        unsafe {
            data.set_len(0);
            ptr::copy_nonoverlapping(new_data.as_ptr(), data.as_mut_ptr(), new_len);
            data.set_len(new_len);
            self.abi_account.payload.set_len(new_len as u64);
        };
    }

    pub(crate) fn extend_from_slice(&mut self, data: &[u8]) {
        self.data_mut().extend_from_slice(data);
        // SAFETY: We are synchronizing the lengths.
        unsafe {
            self.abi_account
                .payload
                .set_len(self.private_fields.payload_len() as u64);
        }
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        if let Some(data) = Arc::get_mut(&mut self.private_fields.payload) {
            data.reserve(additional)
        } else {
            let mut data =
                Vec::with_capacity(self.private_fields.payload_len().saturating_add(additional));
            data.extend_from_slice(self.private_fields.payload.as_slice());
            self.private_fields.payload = Arc::new(data);
        }
    }

    #[cfg_attr(feature = "dev-context-only-utils", qualifiers(pub))]
    pub(crate) fn is_shared(&self) -> bool {
        Arc::strong_count(&self.private_fields.payload) > 1
    }
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl ReadableAccount for TransactionAccountViewMut<'_> {
    fn lamports(&self) -> u64 {
        self.abi_account.lamports
    }

    fn data(&self) -> &[u8] {
        self.private_fields.payload.as_slice()
    }

    fn owner(&self) -> &Pubkey {
        &self.abi_account.owner
    }

    fn executable(&self) -> bool {
        self.private_fields.executable
    }

    fn rent_epoch(&self) -> u64 {
        self.private_fields.rent_epoch
    }
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl WritableAccount for TransactionAccountViewMut<'_> {
    fn set_lamports(&mut self, lamports: u64) {
        self.abi_account.lamports = lamports;
    }

    fn data_as_mut_slice(&mut self) -> &mut [u8] {
        Arc::make_mut(&mut self.private_fields.payload).as_mut_slice()
    }

    fn set_owner(&mut self, owner: Pubkey) {
        self.abi_account.owner = owner;
    }

    fn copy_into_owner_from_slice(&mut self, source: &[u8]) {
        self.abi_account.owner.as_mut().copy_from_slice(source);
    }

    fn set_executable(&mut self, executable: bool) {
        self.private_fields.executable = executable;
    }

    fn set_rent_epoch(&mut self, epoch: u64) {
        self.private_fields.rent_epoch = epoch;
    }

    fn create(
        _lamports: u64,
        _data: Vec<u8>,
        _owner: Pubkey,
        _executable: bool,
        _rent_epoch: u64,
    ) -> Self {
        panic!("It is not possible to create a TransactionAccountMutView");
    }
}

/// An account key and the matching account
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
pub type KeyedAccountSharedData = (Pubkey, AccountSharedData);
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
pub(crate) type DeconstructedTransactionAccounts =
    (Vec<KeyedAccountSharedData>, Box<[Cell<bool>]>, Cell<i64>);

#[derive(Debug)]
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
pub struct TransactionAccounts {
    shared_account_fields: Box<[UnsafeCell<AccountSharedFields>]>,
    private_account_fields: Box<[UnsafeCell<AccountPrivateFields>]>,
    borrow_counters: Box<[BorrowCounter]>,
    touched_flags: Box<[Cell<bool>]>,
    resize_delta: Cell<i64>,
    lamports_delta: Cell<i128>,
    /// F10 W10: running sum of lamports introduced by `add_subaccount`
    /// (i.e. by the `sol_create_subaccount` syscall) for accounts that did
    /// not exist in the original transaction account list.
    dynamic_accounts_lamports_sum: Cell<u128>,
    /// F10 subaccount lane — split-storage mirror of the main-account
    /// representation. Append-only; each element is boxed so heap addresses of
    /// the inner cells are stable across `Vec::push` reallocations, keeping
    /// outstanding `AccountRef`/`AccountRefMut` borrows sound.
    #[allow(clippy::vec_box)]
    subaccount_shared_fields: RefCell<Vec<Box<UnsafeCell<AccountSharedFields>>>>,
    #[allow(clippy::vec_box)]
    subaccount_private_fields: RefCell<Vec<Box<UnsafeCell<AccountPrivateFields>>>>,
    #[allow(clippy::vec_box)]
    subaccount_borrow_counters: RefCell<Vec<Box<BorrowCounter>>>,
    touched_subaccounts: RefCell<Vec<bool>>,
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl TransactionAccounts {
    pub(crate) fn new(accounts: Vec<KeyedAccountSharedData>) -> TransactionAccounts {
        let touched_flags = vec![Cell::new(false); accounts.len()].into_boxed_slice();
        let borrow_counters = vec![BorrowCounter::default(); accounts.len()].into_boxed_slice();
        let (shared_accounts, private_fields) = accounts
            .into_iter()
            .enumerate()
            .map(|(idx, item)| {
                (
                    UnsafeCell::new(AccountSharedFields {
                        key: item.0,
                        owner: *item.1.owner(),
                        lamports: item.1.lamports(),
                        payload: VmSlice::new(
                            GUEST_ACCOUNT_PAYLOAD_BASE_ADDRESS
                                .saturating_add(GUEST_REGION_SIZE.saturating_mul(idx as u64)),
                            item.1.data().len() as u64,
                        ),
                    }),
                    UnsafeCell::new(AccountPrivateFields {
                        rent_epoch: item.1.rent_epoch(),
                        executable: item.1.executable(),
                        payload: item.1.data_clone(),
                    }),
                )
            })
            .collect::<(
                Vec<UnsafeCell<AccountSharedFields>>,
                Vec<UnsafeCell<AccountPrivateFields>>,
            )>();

        TransactionAccounts {
            shared_account_fields: shared_accounts.into_boxed_slice(),
            private_account_fields: private_fields.into_boxed_slice(),
            borrow_counters,
            touched_flags,
            resize_delta: Cell::new(0),
            lamports_delta: Cell::new(0),
            subaccount_shared_fields: RefCell::new(Vec::new()),
            subaccount_private_fields: RefCell::new(Vec::new()),
            subaccount_borrow_counters: RefCell::new(Vec::new()),
            touched_subaccounts: RefCell::new(Vec::new()),
            dynamic_accounts_lamports_sum: Cell::new(0),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.shared_account_fields.len()
    }

    pub fn touch(&self, index: IndexOfAccount) -> Result<(), InstructionError> {
        if index & SUBACCOUNT_MARKER != 0 {
            let subaccount_index = (index & !SUBACCOUNT_MARKER) as usize;
            let mut touched = self.touched_subaccounts.borrow_mut();
            *touched
                .get_mut(subaccount_index)
                .ok_or(InstructionError::NotEnoughAccountKeys)? = true;
            Ok(())
        } else {
            self.touched_flags
                .get(index as usize)
                .ok_or(InstructionError::MissingAccount)?
                .set(true);
            Ok(())
        }
    }

    /// F10: append a new subaccount into the split-storage lane. Populates the
    /// three parallel Vecs plus `touched_subaccounts`. Caller
    /// (`TransactionContext::add_subaccount`) is responsible for dedup.
    #[allow(dead_code)] // wired in by later F10 waves (syscalls + TransactionContext helpers)
    pub(crate) fn add_subaccount(
        &self,
        pubkey: Pubkey,
        account: AccountSharedData,
    ) -> IndexOfAccount {
        let lamports = account.lamports();
        let mut shared = self.subaccount_shared_fields.borrow_mut();
        let mut private = self.subaccount_private_fields.borrow_mut();
        let mut counters = self.subaccount_borrow_counters.borrow_mut();
        let mut touched = self.touched_subaccounts.borrow_mut();
        let index = shared.len() as IndexOfAccount;
        shared.push(Box::new(UnsafeCell::new(AccountSharedFields {
            key: pubkey,
            owner: *account.owner(),
            lamports,
            payload: VmSlice::new(0, account.data().len() as u64),
        })));
        private.push(Box::new(UnsafeCell::new(AccountPrivateFields {
            rent_epoch: account.rent_epoch(),
            executable: account.executable(),
            payload: account.data_clone(),
        })));
        counters.push(Box::new(BorrowCounter::default()));
        touched.push(false);
        self.dynamic_accounts_lamports_sum.set(
            self.dynamic_accounts_lamports_sum
                .get()
                .saturating_add(lamports as u128),
        );
        index
    }

    /// F10 W10: lamports introduced by `add_subaccount` for accounts not
    /// present in the original tx account list.
    pub fn get_dynamic_accounts_lamports_sum(&self) -> u128 {
        self.dynamic_accounts_lamports_sum.get()
    }

    pub fn number_of_subaccounts(&self) -> IndexOfAccount {
        self.subaccount_shared_fields.borrow().len() as IndexOfAccount
    }

    pub fn find_index_of_subaccount(&self, pubkey: &Pubkey) -> Option<IndexOfAccount> {
        let shared = self.subaccount_shared_fields.borrow();
        shared
            .iter()
            .position(|boxed| {
                // SAFETY: append-only lane; `key` never mutated after construction.
                unsafe { (*boxed.get()).key == *pubkey }
            })
            .map(|i| i as IndexOfAccount)
    }

    pub fn subaccount_key(&self, index: IndexOfAccount) -> Option<Pubkey> {
        let shared = self.subaccount_shared_fields.borrow();
        // SAFETY: `key` is set at construction and never mutated.
        shared
            .get(index as usize)
            .map(|boxed| unsafe { (*boxed.get()).key })
    }

    /// F10: borrow a subaccount (immutable) via the split-storage format.
    pub fn try_borrow_subaccount(
        &self,
        index: IndexOfAccount,
    ) -> Result<AccountRef<'_>, InstructionError> {
        let (shared_ptr, private_ptr, counter_ptr) = self.subaccount_raw_ptrs(index)?;

        // SAFETY: pointers from Box-owned entries in append-only Vecs — stable
        // for the life of `self`. Borrow counter excludes a live AccountRefMut.
        let borrow_counter = unsafe { &*counter_ptr };
        borrow_counter.try_borrow()?;
        let abi_account = unsafe { &*(*shared_ptr).get() };
        let private_fields = unsafe { &*(*private_ptr).get() };
        let account = TransactionAccountView {
            abi_account,
            private_fields,
        };
        Ok(AccountRef {
            account,
            borrow_counter,
        })
    }

    /// F10: borrow a subaccount (mutable) via the split-storage format.
    pub fn try_borrow_mut_subaccount(
        &self,
        index: IndexOfAccount,
    ) -> Result<AccountRefMut<'_>, InstructionError> {
        let (shared_ptr, private_ptr, counter_ptr) = self.subaccount_raw_ptrs(index)?;

        // SAFETY: see `try_borrow_subaccount`; counter excludes concurrent borrows.
        let borrow_counter = unsafe { &*counter_ptr };
        borrow_counter.try_borrow_mut()?;
        let abi_account = unsafe { &mut *(*shared_ptr).get() };
        let private_fields = unsafe { &mut *(*private_ptr).get() };
        let account = TransactionAccountViewMut {
            abi_account,
            private_fields,
        };
        Ok(AccountRefMut {
            account,
            borrow_counter,
        })
    }

    /// Extracts stable raw pointers to the three split-storage cells of the
    /// subaccount at `index`. RefCell borrows are dropped before returning
    /// because Box-owned heap addresses are stable for the life of `self`.
    fn subaccount_raw_ptrs(
        &self,
        index: IndexOfAccount,
    ) -> Result<
        (
            *mut UnsafeCell<AccountSharedFields>,
            *mut UnsafeCell<AccountPrivateFields>,
            *const BorrowCounter,
        ),
        InstructionError,
    > {
        let shared = self.subaccount_shared_fields.borrow();
        let private = self.subaccount_private_fields.borrow();
        let counters = self.subaccount_borrow_counters.borrow();
        let shared_box = shared
            .get(index as usize)
            .ok_or(InstructionError::MissingAccount)?;
        let private_box = private
            .get(index as usize)
            .ok_or(InstructionError::MissingAccount)?;
        let counter_box = counters
            .get(index as usize)
            .ok_or(InstructionError::MissingAccount)?;
        let shared_ptr: *mut UnsafeCell<AccountSharedFields> = &**shared_box as *const _ as *mut _;
        let private_ptr: *mut UnsafeCell<AccountPrivateFields> =
            &**private_box as *const _ as *mut _;
        let counter_ptr: *const BorrowCounter = &**counter_box;
        Ok((shared_ptr, private_ptr, counter_ptr))
    }

    pub(crate) fn update_accounts_resize_delta(
        &self,
        old_len: usize,
        new_len: usize,
    ) -> Result<(), InstructionError> {
        let accounts_resize_delta = self.resize_delta.get();
        self.resize_delta.set(
            accounts_resize_delta.saturating_add((new_len as i64).saturating_sub(old_len as i64)),
        );
        Ok(())
    }

    pub(crate) fn can_data_be_resized(
        &self,
        old_len: usize,
        new_len: usize,
    ) -> Result<(), InstructionError> {
        // The new length can not exceed the maximum permitted length
        if new_len > MAX_ACCOUNT_DATA_LEN as usize {
            return Err(InstructionError::InvalidRealloc);
        }
        // The resize can not exceed the per-transaction maximum
        let length_delta = (new_len as i64).saturating_sub(old_len as i64);
        if self.resize_delta.get().saturating_add(length_delta)
            > MAX_ACCOUNT_DATA_GROWTH_PER_TRANSACTION
        {
            return Err(InstructionError::MaxAccountsDataAllocationsExceeded);
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev-context-only-utils", qualifiers(pub))]
    pub(crate) fn try_borrow_mut(
        &self,
        index: IndexOfAccount,
    ) -> Result<AccountRefMut<'_>, InstructionError> {
        // F10: route subaccount-lane indices (high bit set) into the subaccount
        // storage. Without this, `access_violation_handler` silently bails when a
        // write hits a shared subaccount, surfacing as InvalidRealloc.
        if index & SUBACCOUNT_MARKER != 0 {
            return self.try_borrow_mut_subaccount(index & !SUBACCOUNT_MARKER);
        }
        let borrow_counter = self
            .borrow_counters
            .get(index as usize)
            .ok_or(InstructionError::MissingAccount)?;
        borrow_counter.try_borrow_mut()?;

        // SAFETY: The borrow counter guarantees this is the only mutable borrow of this account.
        // The unwrap is safe because accounts.len() == borrow_counters.len(), so the missing
        // account error should have been returned above.
        let svm_account = unsafe {
            &mut *self
                .shared_account_fields
                .get(index as usize)
                .unwrap()
                .get()
        };

        let private_fields = unsafe {
            &mut *self
                .private_account_fields
                .get(index as usize)
                .unwrap()
                .get()
        };

        let account = TransactionAccountViewMut {
            abi_account: svm_account,
            private_fields,
        };

        Ok(AccountRefMut {
            account,
            borrow_counter,
        })
    }

    pub fn try_borrow(&self, index: IndexOfAccount) -> Result<AccountRef<'_>, InstructionError> {
        let borrow_counter = self
            .borrow_counters
            .get(index as usize)
            .ok_or(InstructionError::MissingAccount)?;
        borrow_counter.try_borrow()?;

        // SAFETY: The borrow counter guarantees there are no mutable borrow of this account.
        // The unwrap is safe because accounts.len() == borrow_counters.len(), so the missing
        // account error should have been returned above.
        let svm_account = unsafe {
            &*self
                .shared_account_fields
                .get(index as usize)
                .unwrap()
                .get()
        };

        let private_fields = unsafe {
            &*self
                .private_account_fields
                .get(index as usize)
                .unwrap()
                .get()
        };

        let account = TransactionAccountView {
            abi_account: svm_account,
            private_fields,
        };

        Ok(AccountRef {
            account,
            borrow_counter,
        })
    }

    pub(crate) fn add_lamports_delta(&self, balance: i128) -> Result<(), InstructionError> {
        let delta = self.lamports_delta.get();
        self.lamports_delta.set(
            delta
                .checked_add(balance)
                .ok_or(InstructionError::ArithmeticOverflow)?,
        );
        Ok(())
    }

    pub(crate) fn get_lamports_delta(&self) -> i128 {
        self.lamports_delta.get()
    }

    fn deconstruct_into_keyed_account_shared_data(&mut self) -> Vec<KeyedAccountSharedData> {
        let shared_account_fields = std::mem::take(&mut self.shared_account_fields);
        let private_account_fields = std::mem::take(&mut self.private_account_fields);
        let mut accounts: Vec<_> = shared_account_fields
            .into_iter()
            .zip(private_account_fields)
            .map(|(shared_fields_cell, private_fields_cell)| {
                let shared_fields = shared_fields_cell.into_inner();
                let private_fields = private_fields_cell.into_inner();
                (
                    shared_fields.key,
                    AccountSharedData::create_from_existing_shared_data(
                        shared_fields.lamports,
                        private_fields.payload.clone(),
                        shared_fields.owner,
                        private_fields.executable,
                        private_fields.rent_epoch,
                    ),
                )
            })
            .collect();
        // F10 W10: drain the subaccount lane and append each entry under its
        // derived *storage* address `hashv(&[&[1u8], pda])`. Without this,
        // subaccount state created by `sol_create_subaccount` is dropped at tx
        // commit and accounts-db never receives it.
        let sub_shared = std::mem::take(&mut *self.subaccount_shared_fields.borrow_mut());
        let sub_private = std::mem::take(&mut *self.subaccount_private_fields.borrow_mut());
        for (shared_box, private_box) in sub_shared.into_iter().zip(sub_private.into_iter()) {
            let shared = (*shared_box).into_inner();
            let private = (*private_box).into_inner();
            let storage_address = Pubkey::new_from_array(
                solana_sha256_hasher::hashv(&[&[1u8], shared.key.as_ref()]).to_bytes(),
            );
            accounts.push((
                storage_address,
                AccountSharedData::create_from_existing_shared_data(
                    shared.lamports,
                    private.payload,
                    shared.owner,
                    private.executable,
                    private.rent_epoch,
                ),
            ));
        }
        accounts
    }

    pub(crate) fn deconstruct_into_account_shared_data(&mut self) -> Vec<AccountSharedData> {
        let shared_account_fields = std::mem::take(&mut self.shared_account_fields);
        let private_account_fields = std::mem::take(&mut self.private_account_fields);
        let mut accounts: Vec<_> = shared_account_fields
            .into_iter()
            .zip(private_account_fields)
            .map(|(shared_fields_cell, private_fields_cell)| {
                let shared_fields = shared_fields_cell.into_inner();
                let private_fields = private_fields_cell.into_inner();
                AccountSharedData::create_from_existing_shared_data(
                    shared_fields.lamports,
                    private_fields.payload.clone(),
                    shared_fields.owner,
                    private_fields.executable,
                    private_fields.rent_epoch,
                )
            })
            .collect();
        // F10 W10: mirror the keyed variant so mock_process_instruction sees
        // subaccount state instead of dropping it.
        let sub_shared = std::mem::take(&mut *self.subaccount_shared_fields.borrow_mut());
        let sub_private = std::mem::take(&mut *self.subaccount_private_fields.borrow_mut());
        for (shared_box, private_box) in sub_shared.into_iter().zip(sub_private.into_iter()) {
            let shared = (*shared_box).into_inner();
            let private = (*private_box).into_inner();
            accounts.push(AccountSharedData::create_from_existing_shared_data(
                shared.lamports,
                private.payload,
                shared.owner,
                private.executable,
                private.rent_epoch,
            ));
        }
        accounts
    }

    pub(crate) fn take(mut self) -> DeconstructedTransactionAccounts {
        let shared_data = self.deconstruct_into_keyed_account_shared_data();
        (shared_data, self.touched_flags, self.resize_delta)
    }

    pub fn resize_delta(&self) -> i64 {
        self.resize_delta.get()
    }

    pub(crate) fn account_key(&self, index: IndexOfAccount) -> Option<&Pubkey> {
        // F10: subaccount-marker bit redirects to the subaccount lane.
        if index & SUBACCOUNT_MARKER != 0 {
            let sub_index = (index & !SUBACCOUNT_MARKER) as usize;
            let shared = self.subaccount_shared_fields.borrow();
            let boxed = shared.get(sub_index)?;
            // SAFETY: subaccount keys are set at `add_subaccount` and never
            // mutated; `Box<UnsafeCell<_>>` gives a stable heap address, so
            // extending the Ref-scoped borrow to `&self` is sound (append-only).
            let key_ptr: *const Pubkey = unsafe { &(*boxed.get()).key };
            drop(shared);
            return Some(unsafe { &*key_ptr });
        }
        // SAFETY: We never modify an account key, so returning a reference to it is safe.
        unsafe {
            self.shared_account_fields
                .get(index as usize)
                .map(|acc| &(*acc.get()).key)
        }
    }

    pub(crate) fn account_keys_iter(&self) -> impl Iterator<Item = &Pubkey> {
        // SAFETY: We never modify account keys, so returning an immutable reference to them is safe.
        unsafe {
            self.shared_account_fields
                .iter()
                .map(|item| &(*item.get()).key)
        }
    }
}

#[derive(Default, Debug, Clone)]
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
struct BorrowCounter {
    counter: Cell<i8>,
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl BorrowCounter {
    #[inline]
    fn is_writing(&self) -> bool {
        self.counter.get() < 0
    }

    #[inline]
    fn is_reading(&self) -> bool {
        self.counter.get() > 0
    }

    #[inline]
    fn try_borrow(&self) -> Result<(), InstructionError> {
        if self.is_writing() {
            return Err(InstructionError::AccountBorrowFailed);
        }

        if let Some(counter) = self.counter.get().checked_add(1) {
            self.counter.set(counter);
            return Ok(());
        }

        Err(InstructionError::AccountBorrowFailed)
    }

    #[inline]
    fn try_borrow_mut(&self) -> Result<(), InstructionError> {
        if self.is_writing() || self.is_reading() {
            return Err(InstructionError::AccountBorrowFailed);
        }

        self.counter.set(self.counter.get().saturating_sub(1));

        Ok(())
    }

    #[inline]
    fn release_borrow(&self) {
        self.counter.set(self.counter.get().saturating_sub(1));
    }

    #[inline]
    fn release_borrow_mut(&self) {
        self.counter.set(self.counter.get().saturating_add(1));
    }
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
pub struct AccountRef<'a> {
    account: TransactionAccountView<'a>,
    borrow_counter: &'a BorrowCounter,
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl Drop for AccountRef<'_> {
    fn drop(&mut self) {
        self.borrow_counter.release_borrow();
    }
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl<'a> Deref for AccountRef<'a> {
    type Target = TransactionAccountView<'a>;
    fn deref(&self) -> &Self::Target {
        &self.account
    }
}

#[derive(Debug)]
#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
pub struct AccountRefMut<'a> {
    account: TransactionAccountViewMut<'a>,
    borrow_counter: &'a BorrowCounter,
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl Drop for AccountRefMut<'_> {
    fn drop(&mut self) {
        // SAFETY: We are synchronizing the lengths.
        unsafe {
            self.account
                .abi_account
                .payload
                .set_len(self.account.private_fields.payload_len() as u64);
        }
        self.borrow_counter.release_borrow_mut();
    }
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl<'a> Deref for AccountRefMut<'a> {
    type Target = TransactionAccountViewMut<'a>;
    fn deref(&self) -> &Self::Target {
        &self.account
    }
}

#[cfg(not(any(target_arch = "bpf", target_arch = "sbf")))]
impl DerefMut for AccountRefMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.account
    }
}

#[cfg(all(test, not(target_arch = "sbf"), not(target_arch = "bpf")))]
mod tests {
    use {
        crate::transaction_accounts::TransactionAccounts, solana_account::AccountSharedData,
        solana_instruction::error::InstructionError, solana_pubkey::Pubkey,
    };

    #[test]
    fn test_missing_account() {
        let accounts = vec![
            (
                Pubkey::new_unique(),
                AccountSharedData::new(2, 1, &Pubkey::new_unique()),
            ),
            (
                Pubkey::new_unique(),
                AccountSharedData::new(2, 1, &Pubkey::new_unique()),
            ),
        ];

        let tx_accounts = TransactionAccounts::new(accounts);

        let res = tx_accounts.try_borrow(3);
        assert_eq!(res.err(), Some(InstructionError::MissingAccount));

        let res = tx_accounts.try_borrow_mut(3);
        assert_eq!(res.err(), Some(InstructionError::MissingAccount));
    }

    #[test]
    fn test_invalid_borrow() {
        let accounts = vec![
            (
                Pubkey::new_unique(),
                AccountSharedData::new(2, 1, &Pubkey::new_unique()),
            ),
            (
                Pubkey::new_unique(),
                AccountSharedData::new(2, 1, &Pubkey::new_unique()),
            ),
        ];

        let tx_accounts = TransactionAccounts::new(accounts);

        // Two immutable borrows are valid
        {
            let acc_1 = tx_accounts.try_borrow(0);
            assert!(acc_1.is_ok());

            let acc_2 = tx_accounts.try_borrow(1);
            assert!(acc_2.is_ok());

            let acc_1_new = tx_accounts.try_borrow(0);
            assert!(acc_1_new.is_ok());

            assert_eq!(acc_1.unwrap().account, acc_1_new.unwrap().account);
        }

        // Two mutable borrows are invalid
        {
            let acc_1 = tx_accounts.try_borrow_mut(0);
            assert!(acc_1.is_ok());

            let acc_2 = tx_accounts.try_borrow_mut(1);
            assert!(acc_2.is_ok());

            let acc_1_new = tx_accounts.try_borrow_mut(0);
            assert_eq!(acc_1_new.err(), Some(InstructionError::AccountBorrowFailed));
        }

        // Mutable after immutable must fail
        {
            let acc_1 = tx_accounts.try_borrow(0);
            assert!(acc_1.is_ok());

            let acc_2 = tx_accounts.try_borrow(1);
            assert!(acc_2.is_ok());

            let acc_1_new = tx_accounts.try_borrow_mut(0);
            assert_eq!(acc_1_new.err(), Some(InstructionError::AccountBorrowFailed));
        }

        // Immutable after mutable must fail
        {
            let acc_1 = tx_accounts.try_borrow_mut(0);
            assert!(acc_1.is_ok());

            let acc_2 = tx_accounts.try_borrow_mut(1);
            assert!(acc_2.is_ok());

            let acc_1_new = tx_accounts.try_borrow(0);
            assert_eq!(acc_1_new.err(), Some(InstructionError::AccountBorrowFailed));
        }

        // Different scopes are good
        {
            let acc_1 = tx_accounts.try_borrow_mut(0);
            assert!(acc_1.is_ok());
        }

        {
            let acc_1 = tx_accounts.try_borrow_mut(0);
            assert!(acc_1.is_ok());
        }
    }

    #[test]
    fn too_many_borrows() {
        let accounts = vec![
            (
                Pubkey::new_unique(),
                AccountSharedData::new(2, 1, &Pubkey::new_unique()),
            ),
            (
                Pubkey::new_unique(),
                AccountSharedData::new(2, 1, &Pubkey::new_unique()),
            ),
        ];

        let tx_accounts = TransactionAccounts::new(accounts);
        let mut borrows = Vec::new();
        for i in 0..129 {
            let acc = tx_accounts.try_borrow(1);
            if i < 127 {
                assert!(acc.is_ok());
                borrows.push(acc.unwrap());
            } else {
                assert_eq!(acc.err(), Some(InstructionError::AccountBorrowFailed));
            }
        }
    }
}
