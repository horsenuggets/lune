/*!
    Global registry of weak doms that instances live in.

    Historically all instances lived in a single global "internal dom", and
    instances were *transferred* into it on parse and *cloned* out on save.
    That model made it impossible to tell two instances from different place
    files apart when their referents collided (see issue #356), and it never
    freed memory since orphaned instances lived at the root forever.

    Instead, every parsed document and the shared scratch dom used for manually
    created orphans is registered here under a unique [`DomId`]. An [`Instance`]
    is identified by the pair `(dom_id, dom_ref)` and the `dom_id` disambiguates
    referents that happen to collide between separate doms.
*/

use std::{
    collections::{HashMap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{LazyLock, Mutex, MutexGuard, PoisonError},
};

use rbx_dom_weak::{
    InstanceBuilder as DomInstanceBuilder, PropertyPool, WeakDom, types::Ref as DomRef,
};

pub type DomId = u64;

struct RegisteredDom {
    dom: WeakDom,
    pool_work: usize,
}

struct Registry {
    doms: HashMap<DomId, RegisteredDom>,
    next_id: DomId,
    default_id: Option<DomId>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry {
        doms: HashMap::new(),
        next_id: 1,
        default_id: None,
    })
});

static PROPERTY_POOL: LazyLock<Mutex<PropertyPool>> =
    LazyLock::new(|| Mutex::new(PropertyPool::default()));

/*
    Both locks recover from poisoning. Every operation leaves the registry and pool
    consistent before it returns, and closures run under the registry lock are kept
    from poisoning it, so a panic on one thread (for example in one actor) must never
    break every other thread's instances.
*/

fn property_pool() -> MutexGuard<'static, PropertyPool> {
    PROPERTY_POOL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn lock() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/**
    Runs a closure while the registry lock is held, releasing the lock before resuming
    any panic of the closure, so that the panic fails only the calling thread.
*/
fn run_released<R>(reg: MutexGuard<'static, Registry>, f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut reg = reg;
    let result = catch_unwind(AssertUnwindSafe(|| f(&mut reg)));
    drop(reg);
    result.unwrap_or_else(|payload| resume_unwind(payload))
}

/**
    Registers a new dom and returns its freshly allocated id.
*/
pub fn register(mut dom: WeakDom) -> DomId {
    // Pool only immutable property payloads, never instances or document identities.
    // Finish this work and release its mutex before entering the instance registry.
    let pool_work = dom.share_properties(&mut property_pool());
    crate::reclaim_native_document_memory();
    let mut reg = lock();

    let id = reg.next_id;

    reg.next_id += 1;
    reg.doms.insert(id, RegisteredDom { dom, pool_work });

    id
}

/**
    Returns the id of the shared default scratch dom, creating it if needed.
*/
fn default_dom_locked(reg: &mut Registry) -> DomId {
    if let Some(id) = reg.default_id
        && reg.doms.contains_key(&id)
    {
        return id;
    }

    let id = reg.next_id;

    reg.next_id += 1;
    reg.doms.insert(
        id,
        RegisteredDom {
            dom: WeakDom::new(DomInstanceBuilder::new("ROOT")),
            pool_work: 0,
        },
    );
    reg.default_id = Some(id);

    id
}

/**
    Inserts a new orphaned instance at the root of the shared default scratch dom,
    creating that dom if needed, and returns where the instance now lives.

    Manually created orphan instances (e.g. `Instance.new(...)`) live there until
    they are parented into another dom. Looking up the scratch dom and inserting into
    it happen under one registry lock, so another thread destroying the last orphan
    (and with it the scratch dom) can never remove the dom in between.
*/
pub fn insert_orphan(class_name: &str) -> (DomId, DomRef) {
    let mut reg = lock();
    let id = default_dom_locked(&mut reg);
    let dom = &mut reg
        .doms
        .get_mut(&id)
        .expect("the default dom was registered above")
        .dom;
    let dom_root = dom.root_ref();
    let dom_ref = dom.insert(dom_root, DomInstanceBuilder::new(class_name));
    (id, dom_ref)
}

/**
    Runs a closure with shared access to the dom with the given id.

    Returns `None` if no such dom exists (e.g. it was already torn down).

    **NOTE:** The closure must not re-enter the registry (no nested
    [`with`]/[`with_mut`] calls), as the registry lock is held for its duration.
    Collect referents inside the closure and build [`Instance`]s from the
    `&WeakDom` already in hand instead.
*/
pub fn with<R>(dom_id: DomId, f: impl FnOnce(&WeakDom) -> R) -> Option<R> {
    run_released(lock(), |reg| {
        reg.doms.get(&dom_id).map(|entry| f(&entry.dom))
    })
}

/**
    Runs a closure with mutable access to the dom with the given id.

    Returns `None` if no such dom exists. Same re-entrancy rules as [`with`].
*/
pub fn with_mut<R>(dom_id: DomId, f: impl FnOnce(&mut WeakDom) -> R) -> Option<R> {
    run_released(lock(), |reg| {
        reg.doms.get_mut(&dom_id).map(|entry| f(&mut entry.dom))
    })
}

fn collect_subtree(dom: &WeakDom, root: DomRef) -> Vec<DomRef> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([root]);

    while let Some(referent) = queue.pop_front() {
        out.push(referent);
        if let Some(inst) = dom.get_by_ref(referent) {
            queue.extend(inst.children().iter().copied());
        }
    }

    out
}

/**
    Reparents `child_ref` (currently in `child_dom`) under an optional parent.

    - `parent` is `None`: the instance is orphaned within its current dom (moved
      to that dom's root).
    - `parent` is in the same dom: a cheap `transfer_within`.
    - `parent` is in a different dom: the instance and all of its descendants
      are transferred into the parent's dom. The list of moved referents is
      returned so that callers can re-key any cached userdata; for same-dom
      moves an empty vec is returned.
*/
pub fn reparent(
    child_dom: DomId,
    child_ref: DomRef,
    parent: Option<(DomId, DomRef)>,
) -> Vec<DomRef> {
    // The native transfers panic on invalid referents, so they must not poison the lock.
    run_released(lock(), |reg| match parent {
        None => {
            if let Some(entry) = reg.doms.get_mut(&child_dom) {
                let dom_root = entry.dom.root_ref();
                entry.dom.transfer_within(child_ref, dom_root);
            }
            Vec::new()
        }
        Some((parent_dom, parent_ref)) if parent_dom == child_dom => {
            if let Some(entry) = reg.doms.get_mut(&child_dom) {
                entry.dom.transfer_within(child_ref, parent_ref);
            }
            Vec::new()
        }
        Some((parent_dom, parent_ref)) => {
            let moved = match reg.doms.get(&child_dom) {
                Some(entry) => collect_subtree(&entry.dom, child_ref),
                None => return Vec::new(),
            };
            // Since we want to use get_disjoint_mut we cannot store doms in a BTreeMap. Unfortunate
            let [src, dst] = reg.doms.get_disjoint_mut([&child_dom, &parent_dom]);
            if let (Some(src), Some(dst)) = (src, dst) {
                src.dom.transfer(child_ref, &mut dst.dom, parent_ref);
                // Move existing cleanup credit, rather than minting more on every arrival.
                let pool_work = moved.len().min(src.pool_work);
                src.pool_work -= pool_work;
                dst.pool_work += pool_work;
                moved
            } else {
                Vec::new()
            }
        }
    })
}

/**
    Drops the dom with the given id if its root instance has no children.

    Called after destroying an instance so that tearing down the root of a dom
    (e.g. a parsed `DataModel`) frees the entire dom and bounds memory use.
*/
pub fn drop_if_empty(dom_id: DomId) {
    let removed = {
        let mut reg = lock();

        let empty = reg.doms.get(&dom_id).is_none_or(|entry| {
            entry
                .dom
                .get_by_ref(entry.dom.root_ref())
                .is_none_or(|root| root.children().is_empty())
        });

        if empty {
            if reg.default_id == Some(dom_id) {
                reg.default_id = None;
            }
            reg.doms.remove(&dom_id)
        } else {
            None
        }
    };

    if let Some(RegisteredDom { dom, pool_work }) = removed {
        // Cleanup scales with this document, not every other retained place.
        drop(dom);
        // Physical property ownership, not registry membership or cleanup credit, decides
        // whether the last actual destructor can release all remaining pool metadata.
        property_pool().prune_after_drop(pool_work);
        crate::reclaim_native_document_memory();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpooled_round_trip_can_leave_live_pooled_properties_with_zero_cleanup_credit() {
        use rbx_dom_weak::ustr;

        let retained = register(WeakDom::new(DomInstanceBuilder::new("ROOT").with_children(
            (0..32).map(|value| DomInstanceBuilder::new("IntValue").with_property("Value", value)),
        )));
        let root = with(retained, WeakDom::root_ref).unwrap();
        let children = with(retained, |dom| dom.root().children().to_vec()).unwrap();
        let scratch = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
        // Model an isolated scratch DOM without altering the process-wide default DOM.
        lock().doms.get_mut(&scratch).unwrap().pool_work = 0;
        let scratch_root = with(scratch, WeakDom::root_ref).unwrap();
        let unpooled = with_mut(scratch, |dom| {
            dom.insert(
                scratch_root,
                DomInstanceBuilder::new("Folder")
                    .with_children((0..64).map(|_| DomInstanceBuilder::new("Folder"))),
            )
        })
        .unwrap();
        reparent(scratch, unpooled, Some((retained, root)));
        reparent(retained, unpooled, Some((scratch, scratch_root)));
        assert_eq!(lock().doms[&retained].pool_work, 0);
        with_mut(scratch, |dom| dom.destroy(unpooled));
        drop_if_empty(scratch);
        for _ in 0..64 {
            let tiny = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
            drop_if_empty(tiny);
        }
        assert_eq!(
            with(retained, |dom| dom
                .get_by_ref(children[0])
                .unwrap()
                .properties[&ustr("Value")]
                .clone()),
            Some(0.into())
        );
        with_mut(retained, |dom| {
            for child in children {
                dom.destroy(child);
            }
        });
        drop_if_empty(retained);
        assert!(with(retained, |_| ()).is_none());
    }

    #[test]
    fn cleanup_budget_tracks_the_document_and_transferred_instances() {
        let branch =
            DomInstanceBuilder::new("Folder")
                .with_children((0..32).map(|value| {
                    DomInstanceBuilder::new("IntValue").with_property("Value", value)
                }));
        let branch_ref = branch.referent();
        let source = register(WeakDom::new(
            DomInstanceBuilder::new("ROOT").with_child(branch),
        ));
        let destination = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
        assert_eq!(lock().doms[&source].pool_work, 34);
        assert_eq!(lock().doms[&destination].pool_work, 1);
        let root = with(destination, WeakDom::root_ref).unwrap();
        assert_eq!(
            reparent(source, branch_ref, Some((destination, root))).len(),
            33
        );
        assert_eq!(lock().doms[&destination].pool_work, 34);
        assert_eq!(lock().doms[&source].pool_work, 1);
        let source_root = with(source, WeakDom::root_ref).unwrap();
        for _ in 0..64 {
            reparent(destination, branch_ref, Some((source, source_root)));
            reparent(source, branch_ref, Some((destination, root)));
            let reg = lock();
            assert_eq!(reg.doms[&source].pool_work, 1);
            assert_eq!(reg.doms[&destination].pool_work, 34);
        }
        drop_if_empty(source);
        with_mut(destination, |dom| dom.destroy(branch_ref));
        drop_if_empty(destination);
        assert!(with(source, |_| ()).is_none());
        assert!(with(destination, |_| ()).is_none());
    }

    #[test]
    fn transfers_of_unpooled_nodes_conserve_existing_cleanup_credit() {
        let source = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
        let destination = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
        let source_root = with(source, WeakDom::root_ref).unwrap();
        let destination_root = with(destination, WeakDom::root_ref).unwrap();
        let branch = with_mut(source, |dom| {
            dom.insert(
                source_root,
                DomInstanceBuilder::new("Folder").with_children([
                    DomInstanceBuilder::new("Folder"),
                    DomInstanceBuilder::new("Folder"),
                ]),
            )
        })
        .unwrap();
        for _ in 0..64 {
            reparent(source, branch, Some((destination, destination_root)));
            {
                let reg = lock();
                assert_eq!(reg.doms[&source].pool_work, 0);
                assert_eq!(reg.doms[&destination].pool_work, 2);
            }
            reparent(destination, branch, Some((source, source_root)));
            let reg = lock();
            assert_eq!(reg.doms[&source].pool_work, 2);
            assert_eq!(reg.doms[&destination].pool_work, 0);
        }
        with_mut(source, |dom| dom.destroy(branch));
        drop_if_empty(source);
        drop_if_empty(destination);
    }

    #[test]
    fn shared_payloads_preserve_colliding_ids_reparenting_mutation_and_teardown() {
        use rbx_dom_weak::{types::Variant, ustr};

        let child = DomRef::new();
        let builder = || {
            DomInstanceBuilder::new("ROOT").with_child(
                DomInstanceBuilder::new("Folder")
                    .with_referent(child)
                    .with_property("Value", "original"),
            )
        };
        let first = register(WeakDom::new(builder()));
        let second = register(WeakDom::new(builder()));
        assert_ne!(first, second);
        let pointer = |dom: &WeakDom| {
            std::ptr::from_ref(
                dom.get_by_ref(child)
                    .unwrap()
                    .properties
                    .get(&ustr("Value"))
                    .unwrap(),
            ) as usize
        };
        assert_eq!(with(first, pointer), with(second, pointer));
        let third = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
        let third_root = with(third, WeakDom::root_ref).unwrap();
        assert_eq!(
            reparent(first, child, Some((third, third_root))),
            vec![child]
        );
        drop_if_empty(first);
        assert!(with(first, |_| ()).is_none());
        with_mut(third, |dom| {
            dom.get_by_ref_mut(child)
                .unwrap()
                .properties
                .insert(ustr("Value"), "changed".into());
        });
        assert_ne!(with(third, pointer), with(second, pointer));
        assert_eq!(
            with(second, |dom| dom.get_by_ref(child).unwrap().properties
                [&ustr("Value")]
                .clone()),
            Some(Variant::String("original".into()))
        );
        with_mut(second, |dom| dom.destroy(child));
        drop_if_empty(second);
        assert_eq!(
            with(third, |dom| dom.get_by_ref(child).unwrap().properties
                [&ustr("Value")]
                .clone()),
            Some(Variant::String("changed".into()))
        );
        with_mut(third, |dom| dom.destroy(child));
        drop_if_empty(third);
        assert!(with(third, |_| ()).is_none());
    }

    #[test]
    fn removes_only_the_empty_document_and_allows_repeated_cleanup() {
        let empty = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
        let mut other_dom = WeakDom::new(DomInstanceBuilder::new("ROOT"));
        let child = other_dom.insert(other_dom.root_ref(), DomInstanceBuilder::new("Folder"));
        let other = register(other_dom);

        drop_if_empty(empty);
        drop_if_empty(empty);

        assert!(with(empty, |_| ()).is_none());
        assert_eq!(with(other, |dom| dom.root().children().len()), Some(1));
        with_mut(other, |dom| dom.destroy(child));
        drop_if_empty(other);
        assert!(with(other, |_| ()).is_none());
    }

    #[test]
    fn keeps_the_document_until_its_last_root_child_is_destroyed() {
        let mut dom = WeakDom::new(DomInstanceBuilder::new("ROOT"));
        let first = dom.insert(dom.root_ref(), DomInstanceBuilder::new("Folder"));
        let second = dom.insert(dom.root_ref(), DomInstanceBuilder::new("Folder"));
        let id = register(dom);

        drop_if_empty(id);
        assert_eq!(with(id, |dom| dom.root().children().len()), Some(2));

        with_mut(id, |dom| dom.destroy(first));
        drop_if_empty(id);
        assert!(with(id, |dom| dom.get_by_ref(second).is_some()).unwrap());

        with_mut(id, |dom| dom.destroy(second));
        drop_if_empty(id);
        assert!(with(id, |_| ()).is_none());
    }

    #[test]
    fn concurrent_orphans_never_lose_their_scratch_dom() {
        // Each thread repeatedly empties the shared scratch dom, which drops it, while
        // the others keep creating orphans in it. Every orphan must still land in a live
        // dom, so a lookup and an insert can never be split by another thread's drop.
        let threads: Vec<_> = (0..6)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..20_000 {
                        let (id, referent) = insert_orphan("Folder");
                        let destroyed = with_mut(id, |dom| dom.destroy(referent));
                        assert!(destroyed.is_some(), "the orphan's dom must exist");
                        drop_if_empty(id);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("a thread lost its scratch dom");
        }
    }

    /// Serializes the tests that observe or set the registry's process-wide poison flag.
    static POISON_TESTS: Mutex<()> = Mutex::new(());

    #[test]
    fn a_panicking_closure_fails_only_its_own_thread() {
        // A panic while a dom is borrowed must release the registry lock before it
        // unwinds, so another thread can keep creating and reading instances.
        let _serial = POISON_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
        let id = register(WeakDom::new(DomInstanceBuilder::new("ROOT")));
        let panicked = std::thread::spawn(move || {
            with_mut(id, |_| panic!("controlled panic under the registry lock"));
        })
        .join();
        assert!(panicked.is_err(), "the closure's panic must still unwind");
        assert!(
            !REGISTRY.is_poisoned(),
            "the panic must not poison the registry"
        );
        let (orphan_id, referent) = insert_orphan("Folder");
        assert!(with(orphan_id, |dom| dom.get_by_ref(referent).is_some()).unwrap());
        with_mut(orphan_id, |dom| dom.destroy(referent));
        drop_if_empty(orphan_id);
        drop_if_empty(id);
    }

    #[test]
    fn a_poisoned_registry_keeps_serving_every_thread() {
        // Even if some future path panicked while holding the lock directly, a panic on
        // one thread (for example one actor) must never break instances for all others.
        let _serial = POISON_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = std::thread::spawn(|| {
            let _reg = lock();
            panic!("controlled panic while holding the registry lock");
        })
        .join();
        assert!(REGISTRY.is_poisoned(), "the setup must poison the registry");
        let (id, referent) = insert_orphan("Folder");
        let found = with(id, |dom| dom.get_by_ref(referent).is_some());
        with_mut(id, |dom| dom.destroy(referent));
        drop_if_empty(id);
        REGISTRY.clear_poison();
        assert_eq!(found, Some(true), "a poisoned registry must keep working");
    }
}
