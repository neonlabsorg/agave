use std::{cell::{Cell, UnsafeCell}, fmt::Debug, marker::PhantomPinned, pin::Pin, ptr::NonNull};

struct ListLink<T, const N: usize>(UnsafeCell<Option<NonNull<ListNode<T, N>>>>);

impl<T, const N: usize> ListLink<T, N> {
    fn link(&self) -> Option<NonNull<ListNode<T, N>>> {
        unsafe {
            *self.0.get()
        }
    }

    fn link_mut(&mut self) -> Option<NonNull<ListNode<T, N>>> {
        unsafe {
            *self.0.get()
        }
    }

    fn is_some(&self) -> bool {
        self.link().is_some()
    }
}

impl<T, const N: usize> From<Option<NonNull<ListNode<T, N>>>> for ListLink<T, N> {
    fn from(value: Option<NonNull<ListNode<T, N>>>) -> Self {
        Self(UnsafeCell::new(value))
    }
}

impl<T, const N: usize> From<*mut ListNode<T, N>> for ListLink<T, N> {
    fn from(value: *mut ListNode<T, N>) -> Self {
        NonNull::new(value).into()
    }
}

impl<T, const N: usize> PartialEq for ListLink<T, N> {
    fn eq(&self, rhs: &Self) -> bool {
        unsafe {
            *self.0.get() == *rhs.0.get()
        }
    }
}

impl<T, const N: usize> Eq for ListLink<T, N> {
}

impl<T, const N: usize> Clone for ListLink<T, N> {
    fn clone(&self) -> Self {
        Self(unsafe {*self.0.get()}.into())
    }
}

impl<T, const N: usize> Default for ListLink<T, N> {
    fn default() -> Self {
        Self(UnsafeCell::new(None))
    }
}

struct ListNode<T, const N: usize> {
    payload: Option<T>,
    borrows: Cell<usize>,
    next: [ListLink<T, N>; N],
    prev: [ListLink<T, N>; N],
    _marker: PhantomPinned
}

impl<T, const N: usize> ListNode<T, N> {
    fn new(payload: T) -> Self {
        Self {
            payload: Some(payload),
            borrows: Cell::new(1),
            next: std::array::from_fn(|_| ListLink::default()),
            prev: std::array::from_fn(|_| ListLink::default()),
            _marker: PhantomPinned
        }
    }

    fn new_empty() -> Self {
        Self {
            payload: None,
            borrows: Cell::new(1),
            next: std::array::from_fn(|_| ListLink::default()),
            prev: std::array::from_fn(|_| ListLink::default()),
            _marker: PhantomPinned
        }
    }
}

/// `DELETE_ALL` selects how items leave the list when it is dropped or items are popped:
/// `false` unlinks each item only from this list's index `I`, leaving it in any
/// other lists it belongs to; `true` unlinks each item from *every* index, so
/// dropping this list evicts its items from all lists at once.
pub struct IntrusiveList<T, const N: usize, const I: usize, const DELETE_ALL: bool> {
    hub: Option<Pin<Box<ListNode<T, N>>>>
}

impl<T, const N: usize, const I: usize, const DELETE_ALL: bool> Drop
    for IntrusiveList<T, N, I, DELETE_ALL>
{
    fn drop(&mut self) {
        if let Some(head) = self.private_head_mut() {
            while let Some(mut cur) = head.next() {
                if DELETE_ALL {
                    ItemHolder::from(cur).unlink();
                } else {
                    cur.unlink();
                }
            }
        }
    }
}

pub struct Cursor<T, const N: usize, const I: usize> {
    cur: NonNull<ListNode<T, N>>,
}

pub struct ItemHolder<T, const N: usize> {
    cur: NonNull<ListNode<T, N>>,
}

impl<T: Debug, const N: usize> ItemHolder<T, N> {
    fn contained(&self) -> &T {
        unsafe { &self.cur.as_ref().payload.as_ref().unwrap() }
    }
}

impl<T: Debug, const N: usize> Debug for ItemHolder<T, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        T::fmt(self.contained(), f)
    }
}

impl<T, const N: usize> ItemHolder<T, N> {
    pub fn new(payload: T) -> Self {
        Self {
            cur: NonNull::new(Box::into_raw(ListNode::new(payload).into())).unwrap()
        }
    }

    fn leak(self) -> NonNull<ListNode<T, N>> {
        let cur = self.cur;
        std::mem::forget(self);
        cur
    }
}

impl<T, const N: usize, const I: usize> From<Cursor<T, N, I>> for ItemHolder<T, N> {
    fn from(value: Cursor<T, N, I>) -> Self {
        Self { cur: value.leak() }
    }
}

impl<T, const N: usize, const I: usize> Into<Cursor<T, N, I>> for ItemHolder<T, N> {
    fn into(self) -> Cursor<T, N, I> {
        Cursor::<T, N, I> {
            cur: self.leak()
        }
    }
}

impl<T, const N: usize, const I: usize, const DELETE_ALL: bool> Default
    for IntrusiveList<T, N, I, DELETE_ALL>
{
    fn default() -> Self {
        Self { hub: None }
    }
}

impl<T, const N: usize, const I: usize, const DELETE_ALL: bool>
    IntrusiveList<T, N, I, DELETE_ALL>
{
    pub const fn new() -> Self {
        Self { hub: None }
    }

    fn init(&mut self) {
        if self.hub.is_none() {
            self.hub = Some(Box::pin(ListNode::new_empty()));
            let pin = self.hub.as_mut().unwrap().as_mut();
            let ptr = unsafe { pin.get_unchecked_mut() as *mut ListNode<T, N> };
            unsafe {
                (*ptr).next[I] = ptr.into();
                (*ptr).prev[I] = ptr.into();
            }
        }
    }

    fn private_head(&self) -> Option<Cursor<T, N, I>> {
        self.hub.as_ref().map(|hub| {
            let ptr = hub.as_ref().get_ref() as *const ListNode<T, N>;
            unsafe {Cursor::new(std::ptr::NonNull::new(ptr.cast_mut()).unwrap())}
        })
    }

    fn private_head_mut(&mut self) -> Option<Cursor<T, N, I>> {
        self.hub.as_mut().map(|hub| {
            let ptr = unsafe { hub.as_mut().get_unchecked_mut() as *mut ListNode<T, N> };
            unsafe { Cursor::new(NonNull::new(ptr).unwrap()) }
        })
    }

    pub fn push_front(&mut self, cur: &mut Cursor<T, N, I>) {
        cur.unlink();
        self.init();
        self.private_head_mut().unwrap().insert_after(cur);
    }

    pub fn push_back(&mut self, cur: &mut Cursor<T, N, I>) {
        cur.unlink();
        self.init();
        self.private_head_mut().unwrap().prev_item().unwrap().insert_after(cur);
    }

    pub fn is_empty(&self) -> bool {
        match self.hub.as_ref() {
            Some(hub) => {
                let ptr = hub.as_ref().get_ref() as *const ListNode<T, N>;
                hub.as_ref().next[I] == ptr.cast_mut().into()
            },
            None => true
        }
    }

    pub fn pop_front(&mut self) -> Option<Cursor<T, N, I>> {
        match self.front() {
            Some(mut begin) => {
                if DELETE_ALL {
                    let mut begin = ItemHolder::from(begin);
                    begin.unlink();
                    Some(begin.into())
                } else {
                    begin.unlink();
                    Some(begin)
                }
            }
            None => None
        }
    }

    pub fn pop_back(&mut self) -> Option<Cursor<T, N, I>> {
        match self.tail() {
            Some(mut tail) => {
                if DELETE_ALL {
                    let mut tail = ItemHolder::from(tail);
                    tail.unlink();
                    Some(tail.into())
                } else {
                    tail.unlink();
                    Some(tail)
                }
            }
            None => None
        }
    }

    pub fn front(&self) -> Option<Cursor<T, N, I>> {
        self.private_head().and_then(|x| x.next())
    }

    pub fn tail(&self) -> Option<Cursor<T, N, I>> {
        self.private_head().and_then(|x| x.prev())
    }
}

unsafe fn drop_impl<T, const N: usize>(ptr: NonNull<ListNode<T, N>>) {
    let remaining = {
        let node = unsafe { ptr.as_ref() };
        let remaining = node.borrows.get() - 1;
        node.borrows.set(remaining);
        remaining
    };
    if remaining == 0 {
        for i in 0..N {
            if unsafe {ptr.as_ref().next[i].is_some()} {
                return;
            }
        }
        std::mem::drop(unsafe { Box::from_raw(ptr.as_ptr() ) });
    }
}

impl<T, const N: usize, const I: usize> Drop for Cursor<T, N, I> {
    fn drop(&mut self) {
        unsafe { drop_impl(self.cur.clone()) };
    }
}

impl<T, const N: usize> Drop for ItemHolder<T, N> {
    fn drop(&mut self) {
        unsafe { drop_impl(self.cur.clone()) };
    }
}

impl<T, const N: usize> Clone for ItemHolder<T, N> {
    fn clone(&self) -> Self {
        let cur = self.cur;
        let node = unsafe { cur.as_ref() };
        node.borrows.set(node.borrows.get() + 1);
        Self { cur }
    }
}

impl<T, const N: usize, const I: usize> Clone for Cursor<T, N, I> {
    fn clone(&self) -> Self {
        unsafe {Self::new(self.cur)}
    }
}

unsafe fn unlink_impl<T, const N: usize>(mut ptr: NonNull<ListNode<T, N>>, i: usize) {
    let next = unsafe {ptr.as_mut().next[i].link_mut()};
    let prev = unsafe {ptr.as_mut().prev[i].link_mut()};

    match (next, prev) {
        (Some(mut next), Some(mut prev)) => unsafe {
            next.as_mut().prev[i] = Some(prev).into();
            prev.as_mut().next[i] = Some(next).into();

            ptr.as_mut().next[i] = None.into();
            ptr.as_mut().prev[i] = None.into();
        }
        (None, None) => {}
        _ => panic!("broken list links")
    }
}

impl<T, const N: usize> ItemHolder<T, N> {
    pub fn unlink(&mut self) {
        for i in 0..N {
            unsafe {unlink_impl(self.cur, i);}
        }
    }
}

impl<T, const N: usize, const I: usize> Cursor<T, N, I> {
    unsafe fn new(cur: NonNull<ListNode<T, N>>) -> Self {
        let node = unsafe { cur.as_ref() };
        node.borrows.set(node.borrows.get() + 1);
        Self { cur }
    }

    fn leak(self) -> NonNull<ListNode<T, N>> {
        let cur = self.cur;
        std::mem::forget(self);
        cur
    }

    pub fn switch<const J: usize>(self) -> Cursor<T, N, J> {
        Cursor::<T, N, J> {
            cur: self.leak()
        }
    }

    pub fn unlink(&mut self) {
        unsafe {unlink_impl(self.cur, I);}
    }

    pub fn insert_after(&mut self, other: &mut Cursor<T, N, I>) {
        other.unlink();
        unsafe {
            let next = self.cur.as_mut().next[I].link_mut();
            self.cur.as_mut().next[I] = Some(other.cur).into();
            other.cur.as_mut().prev[I] = Some(self.cur).into();

            other.cur.as_mut().next[I] = next.into();
            if let Some(mut next) = next {
                next.as_mut().prev[I] = Some(other.cur).into();
            } else {
                panic!("we don not allow building cursor list outside of List struct yet");
            }
        }
    }

    fn filter_guard(self) -> Option<Self> {
        if unsafe { self.cur.as_ref().payload.is_some() } {
            Some(self)
        } else {
            None
        }
    }

    fn next_item(&self) -> Option<Cursor<T, N, I>> {
        unsafe { self.cur.as_ref().next[I].link().map(|next| Self::new(next)) }
    }

    pub fn next(&self) -> Option<Cursor<T, N, I>> {
        self.next_item().and_then(|x| x.filter_guard())
    }

    fn prev_item(&self) -> Option<Cursor<T, N, I>> {
        unsafe { self.cur.as_ref().prev[I].link().map(|next| Self::new(next)) }
    }

    fn prev(&self) -> Option<Cursor<T, N, I>> {
        self.prev_item().and_then(|x| x.filter_guard())
    }

    // we do not expose guards to the public api
    pub fn contained(&self) -> &T {
        unsafe { &self.cur.as_ref().payload.as_ref().unwrap() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// Payload that bumps a shared counter on drop, so tests can assert that
    /// every node is freed exactly once (catches leaks and double-frees).
    struct Probe {
        id: i32,
        drops: Rc<Cell<usize>>,
    }

    impl Probe {
        fn new(id: i32, drops: &Rc<Cell<usize>>) -> Self {
            Self { id, drops: drops.clone() }
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            self.drops.set(self.drops.get() + 1);
        }
    }

    /// Build a `Cursor` over a fresh node and push it to the front of `list`.
    /// The cursor is dropped on return; the node survives because it is linked.
    fn push_probe(list: &mut IntrusiveList<Probe, 1, 0, false>, id: i32, drops: &Rc<Cell<usize>>) {
        let mut c: Cursor<Probe, 1, 0> = ItemHolder::new(Probe::new(id, drops)).into();
        list.push_front(&mut c);
    }

    /// Build a `Cursor` over a fresh node and push it to the back of `list`.
    fn push_back_probe(list: &mut IntrusiveList<Probe, 1, 0, false>, id: i32, drops: &Rc<Cell<usize>>) {
        let mut c: Cursor<Probe, 1, 0> = ItemHolder::new(Probe::new(id, drops)).into();
        list.push_back(&mut c);
    }

    /// Collect the ids of a single-index list by walking `begin()` -> `next()`.
    fn ids(list: &IntrusiveList<Probe, 1, 0, false>) -> Vec<i32> {
        let mut out = Vec::new();
        let mut cur = list.front();
        while let Some(c) = cur {
            out.push(c.contained().id);
            cur = c.next();
        }
        out
    }

    #[test]
    fn empty_list_has_no_begin_or_tail() {
        let list = IntrusiveList::<Probe, 1, 0, false>::new();
        assert!(list.front().is_none());
        assert!(list.tail().is_none());
        assert_eq!(ids(&list), Vec::<i32>::new());
    }

    #[test]
    fn push_single_element() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_probe(&mut list, 42, &drops);

        assert_eq!(list.front().unwrap().contained().id, 42);
        assert_eq!(list.tail().unwrap().contained().id, 42);
        // single element: its `next` walks back to the hub -> None
        assert!(list.front().unwrap().next().is_none());
    }

    #[test]
    fn push_is_front_insertion_lifo() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_probe(&mut list, 1, &drops);
        push_probe(&mut list, 2, &drops);
        push_probe(&mut list, 3, &drops);

        // most-recently pushed is at the front
        assert_eq!(ids(&list), vec![3, 2, 1]);
        // tail is the first one pushed
        assert_eq!(list.tail().unwrap().contained().id, 1);
    }

    #[test]
    fn iterate_visits_every_element_once() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        for i in 0..10 {
            push_probe(&mut list, i, &drops);
        }
        let seen = ids(&list);
        assert_eq!(seen.len(), 10);
        let mut sorted = seen.clone();
        sorted.sort();
        assert_eq!(sorted, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn unlink_first_element() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_probe(&mut list, 1, &drops);
        push_probe(&mut list, 2, &drops);
        push_probe(&mut list, 3, &drops); // front

        let mut first = list.front().unwrap();
        assert_eq!(first.contained().id, 3);
        first.unlink();
        drop(first); // last handle gone + unlinked => freed

        assert_eq!(drops.get(), 1);
        assert_eq!(ids(&list), vec![2, 1]);
    }

    #[test]
    fn unlink_middle_element() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_probe(&mut list, 1, &drops);
        push_probe(&mut list, 2, &drops);
        push_probe(&mut list, 3, &drops); // order: 3, 2, 1

        // walk to the middle element (id == 2)
        let mut mid = list.front().unwrap().next().unwrap();
        assert_eq!(mid.contained().id, 2);
        mid.unlink();
        drop(mid);

        assert_eq!(drops.get(), 1);
        assert_eq!(ids(&list), vec![3, 1]);
    }

    #[test]
    fn unlink_everything_leaves_empty_list() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        for i in 0..5 {
            push_probe(&mut list, i, &drops);
        }
        while let Some(mut c) = list.front() {
            c.unlink();
        }
        assert!(list.front().is_none());
        assert!(list.tail().is_none());
        assert_eq!(drops.get(), 5, "every unlinked node should be freed");
    }

    #[test]
    fn dropping_list_frees_all_nodes() {
        let drops = Rc::new(Cell::new(0));
        {
            let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
            for i in 0..7 {
                push_probe(&mut list, i, &drops);
            }
            assert_eq!(drops.get(), 0, "nothing freed while list is alive");
        }
        assert_eq!(drops.get(), 7, "dropping the list must free every node exactly once");
    }

    #[test]
    fn cursor_keeps_node_alive_after_list_drop() {
        let drops = Rc::new(Cell::new(0));
        let held: Option<Cursor<Probe, 1, 0>>;
        {
            let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
            push_probe(&mut list, 99, &drops);
            // keep an outside handle to the only node
            held = list.front();
        }
        // list is gone, but our cursor still holds the node
        assert_eq!(drops.get(), 0, "node still referenced by a live cursor");
        assert_eq!(held.as_ref().unwrap().contained().id, 99);
        drop(held);
        assert_eq!(drops.get(), 1, "node freed once the last cursor drops");
    }

    #[test]
    fn clone_cursor_shares_node() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_probe(&mut list, 5, &drops);

        let a = list.front().unwrap();
        let b = a.clone();
        assert_eq!(a.contained().id, b.contained().id);
        drop(a);
        assert_eq!(drops.get(), 0, "still linked + still borrowed by b");
        drop(b);
        assert_eq!(drops.get(), 0, "still linked in the list, must not be freed");
        assert_eq!(ids(&list), vec![5]);
    }

    #[test]
    fn node_can_live_in_two_lists_simultaneously() {
        let drops = Rc::new(Cell::new(0));
        {
            let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();
            let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();

            let holder = ItemHolder::<Probe, 2>::new(Probe::new(7, &drops));
            let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
            let mut c1: Cursor<Probe, 2, 1> = holder.into();
            l0.push_front(&mut c0);
            l1.push_front(&mut c1);
            drop(c0);
            drop(c1);

            // visible from both lists
            assert_eq!(l0.front().unwrap().contained().id, 7);
            assert_eq!(l1.front().unwrap().contained().id, 7);
            assert_eq!(drops.get(), 0);
        }
        assert_eq!(drops.get(), 1, "node shared by two lists must be freed exactly once");
    }

    #[test]
    fn switch_addresses_other_index_and_preserves_refcount() {
        let drops = Rc::new(Cell::new(0));
        {
            let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();
            let holder = ItemHolder::<Probe, 2>::new(Probe::new(8, &drops));
            let mut c0: Cursor<Probe, 2, 0> = holder.into();
            l0.push_front(&mut c0);

            // a cursor pointing at the same node, but addressing list index 1
            let c1: Cursor<Probe, 2, 1> = c0.clone().switch::<1>();
            assert_eq!(c1.contained().id, 8, "switch keeps pointing at the same node");
            // index 1 was never linked, so this node has no index-1 neighbours
            assert!(c1.next().is_none());

            // switch must not lose a borrow: dropping both handles is sound and
            // must not free the node while it is still linked in l0.
            drop(c1);
            drop(c0);
            assert_eq!(drops.get(), 0, "node still linked in l0");
        }
        assert_eq!(drops.get(), 1, "freed exactly once when l0 drops");
    }

    #[test]
    fn cursor_itemholder_roundtrip_keeps_node_alive() {
        let drops = Rc::new(Cell::new(0));
        let holder = ItemHolder::<Probe, 1>::new(Probe::new(1, &drops));
        let c: Cursor<Probe, 1, 0> = holder.into();
        assert_eq!(c.contained().id, 1);

        // Cursor -> ItemHolder must transfer ownership, not free the node.
        let holder2 = ItemHolder::from(c);
        assert_eq!(drops.get(), 0, "round-trip must not free the node");

        // ...and back again.
        let c2: Cursor<Probe, 1, 0> = holder2.into();
        assert_eq!(c2.contained().id, 1);
        assert_eq!(drops.get(), 0);

        // node was never linked, so the last handle dropping frees it once
        drop(c2);
        assert_eq!(drops.get(), 1, "freed once when the last handle drops");
    }

    #[test]
    fn itemholder_drop_frees_unlinked_node() {
        let drops = Rc::new(Cell::new(0));
        {
            let _holder = ItemHolder::<Probe, 1>::new(Probe::new(1, &drops));
            assert_eq!(drops.get(), 0);
        }
        assert_eq!(drops.get(), 1, "dropping an ItemHolder frees its unlinked node");
    }

    #[test]
    fn itemholder_clone_shares_then_frees_once() {
        let drops = Rc::new(Cell::new(0));
        let h1 = ItemHolder::<Probe, 1>::new(Probe::new(1, &drops));
        let h2 = h1.clone();
        drop(h1);
        assert_eq!(drops.get(), 0, "second holder keeps the node alive");
        drop(h2);
        assert_eq!(drops.get(), 1, "freed once the last holder drops");
    }

    #[test]
    fn default_constructs_empty_list() {
        let list: IntrusiveList<Probe, 1, 0, false> = IntrusiveList::default();
        assert!(list.front().is_none());
        assert!(list.tail().is_none());
    }

    #[test]
    fn insert_after_inserts_in_the_middle() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_probe(&mut list, 1, &drops);
        push_probe(&mut list, 2, &drops); // order: 2, 1

        let mut anchor = list.front().unwrap(); // id 2
        let mut newc: Cursor<Probe, 1, 0> =
            ItemHolder::new(Probe::new(99, &drops)).into();
        anchor.insert_after(&mut newc);
        drop(newc);
        drop(anchor);

        assert_eq!(ids(&list), vec![2, 99, 1]);
    }

    #[test]
    fn re_push_relocates_node_between_lists() {
        let drops = Rc::new(Cell::new(0));
        let mut l0 = IntrusiveList::<Probe, 1, 0, false>::new();
        let mut l1 = IntrusiveList::<Probe, 1, 0, false>::new();

        let mut c: Cursor<Probe, 1, 0> = ItemHolder::new(Probe::new(1, &drops)).into();
        l0.push_front(&mut c);
        assert_eq!(ids(&l0), vec![1]);
        assert!(l1.front().is_none());

        // pushing the same cursor again must unlink it from l0 first
        // (exercises unlink's (Some, Some) relink arm)
        l1.push_front(&mut c);
        assert!(l0.front().is_none(), "node moved out of l0");
        assert_eq!(ids(&l1), vec![1]);

        drop(c);
        assert_eq!(drops.get(), 0, "still linked in l1");
    }

    /// Walk any list (any `N`/`I`/`DELETE_ALL`) front-to-back, collecting ids.
    fn collect<const N: usize, const I: usize, const D: bool>(
        list: &IntrusiveList<Probe, N, I, D>,
    ) -> Vec<i32> {
        let mut out = Vec::new();
        let mut cur = list.front();
        while let Some(c) = cur {
            out.push(c.contained().id);
            cur = c.next();
        }
        out
    }

    #[test]
    fn node_in_three_lists_freed_once_when_all_drop() {
        let drops = Rc::new(Cell::new(0));
        {
            let mut l0 = IntrusiveList::<Probe, 3, 0, false>::new();
            let mut l1 = IntrusiveList::<Probe, 3, 1, false>::new();
            let mut l2 = IntrusiveList::<Probe, 3, 2, false>::new();

            let holder = ItemHolder::<Probe, 3>::new(Probe::new(5, &drops));
            let mut c0: Cursor<Probe, 3, 0> = holder.clone().into();
            let mut c1: Cursor<Probe, 3, 1> = holder.clone().into();
            let mut c2: Cursor<Probe, 3, 2> = holder.into(); // consume last handle
            l0.push_front(&mut c0);
            l1.push_front(&mut c1);
            l2.push_front(&mut c2);
            drop(c0);
            drop(c1);
            drop(c2);

            // the single node is reachable from all three lists
            assert_eq!(l0.front().unwrap().contained().id, 5);
            assert_eq!(l1.front().unwrap().contained().id, 5);
            assert_eq!(l2.front().unwrap().contained().id, 5);
            assert_eq!(drops.get(), 0);
        }
        // l2, then l1, then l0 drop: each unlinks its own index, the last frees
        assert_eq!(drops.get(), 1, "node shared by three lists is freed exactly once");
    }

    #[test]
    fn list_drop_unlinks_only_its_own_index() {
        let drops = Rc::new(Cell::new(0));
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();
        {
            let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();
            let holder = ItemHolder::<Probe, 2>::new(Probe::new(7, &drops));
            let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
            let mut c1: Cursor<Probe, 2, 1> = holder.into();
            l0.push_front(&mut c0);
            l1.push_front(&mut c1);
            drop(c0);
            drop(c1);

            assert_eq!(l0.front().unwrap().contained().id, 7);
            assert_eq!(l1.front().unwrap().contained().id, 7);
            // l0 is dropped here, at the end of the block
        }

        // l0's Drop unlinked the node from index 0 only; it lives on in l1
        assert_eq!(drops.get(), 0, "dropping l0 must not free a node still in l1");
        assert_eq!(l1.front().unwrap().contained().id, 7);

        // removing it from the last remaining list frees it
        drop(l1);
        assert_eq!(drops.get(), 1, "freed once excluded from every list");
    }

    #[test]
    fn other_list_stays_intact_when_one_list_drops() {
        let drops = Rc::new(Cell::new(0));
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();
        {
            let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();
            for id in [10, 20] {
                let holder = ItemHolder::<Probe, 2>::new(Probe::new(id, &drops));
                let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
                let mut c1: Cursor<Probe, 2, 1> = holder.into();
                l0.push_front(&mut c0);
                l1.push_front(&mut c1);
            }
            assert_eq!(collect(&l0), vec![20, 10]);
            assert_eq!(collect(&l1), vec![20, 10]);
        }

        // l0 gone; l1 is untouched, both nodes still alive and correctly ordered
        assert_eq!(drops.get(), 0, "nodes still owned by l1");
        assert_eq!(collect(&l1), vec![20, 10]);

        drop(l1);
        assert_eq!(drops.get(), 2, "both freed once l1 (the last owner) drops");
    }

    #[test]
    fn itemholder_unlink_all_excludes_from_every_list_and_frees() {
        let drops = Rc::new(Cell::new(0));
        let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();

        let mut holder = ItemHolder::<Probe, 2>::new(Probe::new(3, &drops));
        let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
        let mut c1: Cursor<Probe, 2, 1> = holder.clone().into();
        l0.push_front(&mut c0);
        l1.push_front(&mut c1);
        drop(c0);
        drop(c1);

        // node lives in both lists, kept alive by `holder`
        assert_eq!(l0.front().unwrap().contained().id, 3);
        assert_eq!(l1.front().unwrap().contained().id, 3);

        // unlink-all removes it from every list at once
        holder.unlink();
        assert!(l0.front().is_none(), "removed from l0");
        assert!(l1.front().is_none(), "removed from l1");
        assert_eq!(drops.get(), 0, "holder still owns the (now unlinked) node");

        // excluded from all lists + last handle dropped => freed
        drop(holder);
        assert_eq!(drops.get(), 1, "exclusion from all lists drops the item");
    }

    #[test]
    fn itemholder_unlink_all_on_partially_linked_node() {
        let drops = Rc::new(Cell::new(0));
        let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();

        // node pushed only into the index-0 list; index 1 is never linked
        let mut holder = ItemHolder::<Probe, 2>::new(Probe::new(4, &drops));
        let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
        l0.push_front(&mut c0);
        drop(c0);
        assert_eq!(l0.front().unwrap().contained().id, 4);

        // unlink-all must skip the never-linked index 1 gracefully (no-op there)
        holder.unlink();
        assert!(l0.front().is_none());
        assert_eq!(drops.get(), 0);

        drop(holder);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn itemholder_unlink_all_is_idempotent() {
        let drops = Rc::new(Cell::new(0));
        let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();

        let mut holder = ItemHolder::<Probe, 2>::new(Probe::new(6, &drops));
        let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
        l0.push_front(&mut c0);
        drop(c0);

        holder.unlink();
        // calling it again on a fully-unlinked node hits only (None, None) arms
        holder.unlink();
        assert!(l0.front().is_none());
        assert_eq!(drops.get(), 0);

        drop(holder);
        assert_eq!(drops.get(), 1);
    }

    // ----- DELETE_ALL policy -----

    #[test]
    fn delete_all_evicts_node_from_every_list_on_drop() {
        let drops = Rc::new(Cell::new(0));
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();
        {
            // l0 is configured to delete its items from ALL lists on drop
            let mut l0 = IntrusiveList::<Probe, 2, 0, true>::new();
            let holder = ItemHolder::<Probe, 2>::new(Probe::new(7, &drops));
            let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
            let mut c1: Cursor<Probe, 2, 1> = holder.into();
            l0.push_front(&mut c0);
            l1.push_front(&mut c1);
            drop(c0);
            drop(c1);

            assert_eq!(collect(&l0), vec![7]);
            assert_eq!(collect(&l1), vec![7]);
            // l0 dropped here
        }

        // delete-all evicted the node from l1 too, and (no cursors left) freed it
        assert!(l1.front().is_none(), "delete-all must evict the node from l1");
        assert_eq!(drops.get(), 1, "node freed once it left every list");
    }

    #[test]
    fn delete_one_keeps_node_in_other_lists_on_drop() {
        let drops = Rc::new(Cell::new(0));
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();
        {
            // l0 deletes only from its own index (the default policy)
            let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();
            let holder = ItemHolder::<Probe, 2>::new(Probe::new(7, &drops));
            let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
            let mut c1: Cursor<Probe, 2, 1> = holder.into();
            l0.push_front(&mut c0);
            l1.push_front(&mut c1);
            drop(c0);
            drop(c1);
            // l0 dropped here
        }

        // contrast with delete-all: the node survives in l1
        assert_eq!(drops.get(), 0, "delete-one must leave the node alive in l1");
        assert_eq!(l1.front().unwrap().contained().id, 7);
        drop(l1);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn delete_all_evicts_every_item_from_other_lists() {
        let drops = Rc::new(Cell::new(0));
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();
        {
            let mut l0 = IntrusiveList::<Probe, 2, 0, true>::new();
            for id in [1, 2, 3] {
                let holder = ItemHolder::<Probe, 2>::new(Probe::new(id, &drops));
                let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
                let mut c1: Cursor<Probe, 2, 1> = holder.into();
                l0.push_front(&mut c0);
                l1.push_front(&mut c1);
            }
            assert_eq!(collect(&l1), vec![3, 2, 1]);
            // l0 dropped here
        }

        assert!(l1.front().is_none(), "every item evicted from l1");
        assert_eq!(drops.get(), 3, "all three freed");
    }

    #[test]
    fn delete_all_evicts_but_defers_free_while_a_cursor_is_held() {
        let drops = Rc::new(Cell::new(0));
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();
        let held: Cursor<Probe, 2, 1>;
        {
            let mut l0 = IntrusiveList::<Probe, 2, 0, true>::new();
            let holder = ItemHolder::<Probe, 2>::new(Probe::new(9, &drops));
            let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
            let mut c1: Cursor<Probe, 2, 1> = holder.into();
            l0.push_front(&mut c0);
            l1.push_front(&mut c1);
            drop(c0);
            // keep an independent live handle to the node
            held = l1.front().unwrap();
            drop(c1);
            // l0 dropped here
        }

        // delete-all unlinked the node from l1, but a live cursor still pins it
        assert!(l1.front().is_none(), "node removed from l1 by delete-all");
        assert_eq!(drops.get(), 0, "must not free while a cursor still holds it");
        assert_eq!(held.contained().id, 9);

        drop(held);
        assert_eq!(drops.get(), 1, "freed once the last handle drops");
    }

    #[test]
    fn delete_all_on_single_list_behaves_like_delete_one() {
        // With N == 1 there is only one index, so both policies coincide.
        let drops = Rc::new(Cell::new(0));
        {
            let mut list = IntrusiveList::<Probe, 1, 0, true>::new();
            let mut c: Cursor<Probe, 1, 0> =
                ItemHolder::new(Probe::new(1, &drops)).into();
            list.push_front(&mut c);
            drop(c);
            assert_eq!(collect(&list), vec![1]);
        }
        assert_eq!(drops.get(), 1, "dropping a delete-all list still frees its node");
    }

    // ----- deque-like methods -----

    #[test]
    fn push_back_appends_in_fifo_order() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_back_probe(&mut list, 1, &drops);
        push_back_probe(&mut list, 2, &drops);
        push_back_probe(&mut list, 3, &drops);

        // push_back keeps insertion order front-to-back
        assert_eq!(ids(&list), vec![1, 2, 3]);
        assert_eq!(list.front().unwrap().contained().id, 1);
        assert_eq!(list.tail().unwrap().contained().id, 3);
    }

    #[test]
    fn push_back_into_empty_list() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_back_probe(&mut list, 42, &drops);

        assert_eq!(ids(&list), vec![42]);
        assert_eq!(list.front().unwrap().contained().id, 42);
        assert_eq!(list.tail().unwrap().contained().id, 42);
    }

    #[test]
    fn push_front_and_push_back_mixed() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_back_probe(&mut list, 2, &drops); // [2]
        push_probe(&mut list, 1, &drops); // push_front -> [1, 2]
        push_back_probe(&mut list, 3, &drops); // [1, 2, 3]
        push_probe(&mut list, 0, &drops); // push_front -> [0, 1, 2, 3]

        assert_eq!(ids(&list), vec![0, 1, 2, 3]);
    }

    #[test]
    fn pop_front_returns_front_in_order() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_back_probe(&mut list, 1, &drops);
        push_back_probe(&mut list, 2, &drops);
        push_back_probe(&mut list, 3, &drops); // [1, 2, 3]

        let popped = list.pop_front().unwrap();
        assert_eq!(popped.contained().id, 1, "pop_front returns the head");
        assert_eq!(ids(&list), vec![2, 3], "head removed from the list");
        assert_eq!(drops.get(), 0, "popped node kept alive by the returned cursor");

        drop(popped);
        assert_eq!(drops.get(), 1, "freed once the popped cursor drops");
    }

    #[test]
    fn pop_back_returns_tail_in_order() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_back_probe(&mut list, 1, &drops);
        push_back_probe(&mut list, 2, &drops);
        push_back_probe(&mut list, 3, &drops); // [1, 2, 3]

        let popped = list.pop_back().unwrap();
        assert_eq!(popped.contained().id, 3, "pop_back returns the tail");
        assert_eq!(ids(&list), vec![1, 2], "tail removed from the list");
        assert_eq!(drops.get(), 0);

        drop(popped);
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn pop_on_empty_list_returns_none() {
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        assert!(list.pop_front().is_none());
        assert!(list.pop_back().is_none());
    }

    #[test]
    fn pop_drains_list_from_both_ends() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        for i in 1..=4 {
            push_back_probe(&mut list, i, &drops);
        } // [1, 2, 3, 4]

        assert_eq!(list.pop_front().unwrap().contained().id, 1);
        assert_eq!(list.pop_back().unwrap().contained().id, 4);
        assert_eq!(list.pop_front().unwrap().contained().id, 2);
        assert_eq!(list.pop_back().unwrap().contained().id, 3);

        assert!(list.pop_front().is_none(), "list is now empty");
        assert!(list.front().is_none());
        assert!(list.tail().is_none());
        // each popped cursor was dropped at the end of its statement
        assert_eq!(drops.get(), 4, "every drained node freed exactly once");
    }

    #[test]
    fn popped_cursor_can_be_re_pushed() {
        let drops = Rc::new(Cell::new(0));
        let mut list = IntrusiveList::<Probe, 1, 0, false>::new();
        push_back_probe(&mut list, 1, &drops);
        push_back_probe(&mut list, 2, &drops); // [1, 2]

        // pop the head, then push it back to the tail (a rotate)
        let mut popped = list.pop_front().unwrap(); // returns 1, list = [2]
        list.push_back(&mut popped);
        drop(popped);

        assert_eq!(ids(&list), vec![2, 1], "rotated head to the back");
        assert_eq!(drops.get(), 0, "nothing freed: node re-linked, not dropped");
    }

    #[test]
    fn push_back_uses_only_its_own_index() {
        // push_back walks `prev` on index I; with N == 2 it must not be confused
        // by the node's links in the other list.
        let drops = Rc::new(Cell::new(0));
        let mut l0 = IntrusiveList::<Probe, 2, 0, false>::new();
        let mut l1 = IntrusiveList::<Probe, 2, 1, false>::new();

        for id in [1, 2, 3] {
            let holder = ItemHolder::<Probe, 2>::new(Probe::new(id, &drops));
            let mut c0: Cursor<Probe, 2, 0> = holder.clone().into();
            let mut c1: Cursor<Probe, 2, 1> = holder.into();
            l0.push_back(&mut c0); // FIFO on index 0
            l1.push_front(&mut c1); // LIFO on index 1
        }

        assert_eq!(collect(&l0), vec![1, 2, 3], "index 0 ordered by push_back");
        assert_eq!(collect(&l1), vec![3, 2, 1], "index 1 ordered by push_front");
    }
}
