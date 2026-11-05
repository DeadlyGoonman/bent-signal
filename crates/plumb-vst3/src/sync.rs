//! Link groups: Plumb instances in one process whose Link parameter (id 7) is the same dot 1..=5
//! share their DSP parameters (key, scale, retune speed, flex tune, reference, mode). An edit in
//! one (the editor, or the host writing a parameter) is copied into the others and reported to
//! the host for each, so the DAW sees every instance move. An instance that joins a group takes
//! the values the group already has. Safe code only (the host calls are in `bentsignal_ui::host`).
//!
//! Threading: every call here runs on the UI thread (controller calls and editor edits). The
//! registry lock is never held while calling into a host, because the host may call back into
//! `changed` for the instance it was just told about.

use crate::params::{self, DSP_COUNT, MODE, SPEED, SYNC, Shared};
use bentsignal_ui::host::{flush_restart, notify_host};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use vst3::Steinberg::Vst::RestartFlags_;

/// What the registry knows of one plugin instance.
pub struct Peer {
    pub shared: Arc<Shared>,
    /// The latency last reported to the host (samples).
    pub latency: AtomicU32,
}

static PEERS: Mutex<Vec<Weak<Peer>>> = Mutex::new(Vec::new());

/// Adds an instance (at creation).
pub fn register(peer: &Arc<Peer>) {
    if let Ok(mut v) = PEERS.lock() {
        v.retain(|w| w.strong_count() > 0);
        v.push(Arc::downgrade(peer));
    }
}

/// Removes an instance (at drop).
pub fn unregister(peer: &Peer) {
    if let Ok(mut v) = PEERS.lock() {
        v.retain(|w| w.upgrade().is_some_and(|p| !std::ptr::eq(&*p, peer)));
    }
}

/// The group of an instance: 0 is Off.
pub fn group_of(shared: &Shared) -> u32 {
    params::to_plain(SYNC, shared.get(SYNC)).round().clamp(0.0, f64::from(params::SYNC_GROUPS)) as u32
}

fn alive() -> Vec<Arc<Peer>> {
    PEERS.lock().map(|v| v.iter().filter_map(Weak::upgrade).collect()).unwrap_or_default()
}

/// The other instances in `group` (never `of` itself).
fn others(of: &Shared, group: u32) -> Vec<Arc<Peer>> {
    alive().into_iter().filter(|p| !std::ptr::eq(&*p.shared, of) && group_of(&p.shared) == group).collect()
}

/// Sets `id` to `n` in `peer` (a no-op when it already has it): the processor re-applies the
/// values at its next block, the latency is re-reported when it changed, and the host is told.
fn push(peer: &Peer, id: u32, n: f64) {
    if (peer.shared.get(id) - n).abs() < 1e-12 {
        return;
    }
    peer.shared.set(id, n);
    peer.shared.generation.fetch_add(1, Ordering::Release);
    if id == SPEED || id == MODE {
        let now = params::latency_of(&peer.shared);
        if peer.latency.swap(now, Ordering::AcqRel) != now {
            peer.shared.restart.fetch_or(RestartFlags_::kLatencyChanged as i32, Ordering::AcqRel);
            flush_restart(&peer.shared);
        }
    }
    notify_host(&peer.shared, id, n);
}

/// Parameter `id` of `shared` is now `n` (already stored): mirror it into its group, or, when
/// the Link parameter changed, take on the group's values.
pub fn changed(shared: &Shared, id: u32, n: f64) {
    let group = group_of(shared);
    if id == SYNC {
        join(shared, group);
        return;
    }
    if group == 0 || id as usize >= DSP_COUNT {
        return;
    }
    for p in others(shared, group) {
        push(&p, id, n);
    }
}

/// `shared` just entered `group`: it takes the values of a member already there.
fn join(shared: &Shared, group: u32) {
    if group == 0 {
        return;
    }
    let Some(first) = others(shared, group).into_iter().next() else { return };
    let Some(me) = alive().into_iter().find(|p| std::ptr::eq(&*p.shared, shared)) else { return };
    for id in 0..DSP_COUNT as u32 {
        push(&me, id, first.shared.get(id));
    }
}

/// The audio thread saw parameter `id` change to `n` (already stored in `shared`): mirrors it
/// into the group's values, or, for the Link parameter, takes the group's values. Atomics only:
/// no host calls, no waiting (a busy registry just skips this change; the UI-thread path still
/// covers hosts that also call the controller). The followers' processors re-apply at their next
/// block.
pub fn rt_changed(shared: &Shared, id: u32, n: f64) {
    let group = group_of(shared);
    if group == 0 {
        return;
    }
    let Ok(peers) = PEERS.try_lock() else { return };
    let mut first = true;
    for p in peers.iter().filter_map(Weak::upgrade) {
        if std::ptr::eq(&*p.shared, shared) || group_of(&p.shared) != group {
            continue;
        }
        if id == SYNC {
            // Joining: copy the first member's values, once.
            if first {
                first = false;
                for i in 0..DSP_COUNT as u32 {
                    shared.set(i, p.shared.get(i));
                }
                shared.generation.fetch_add(1, Ordering::Release);
            }
        } else if (id as usize) < DSP_COUNT && (p.shared.get(id) - n).abs() >= 1e-12 {
            p.shared.set(id, n);
            p.shared.generation.fetch_add(1, Ordering::Release);
            if id == SPEED || id == MODE {
                let now = params::latency_of(&p.shared);
                if p.latency.swap(now, Ordering::AcqRel) != now {
                    p.shared.restart.fetch_or(RestartFlags_::kLatencyChanged as i32, Ordering::AcqRel);
                }
            }
        }
    }
}

/// The edit hook installed on every instance's `Shared`: edits from the editor.
pub fn on_edit(shared: &Shared, id: u32, n: f64) {
    changed(shared, id, n);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{FLEX, KEY, SCALE, new_shared};

    /// The registry is process-wide, so the tests that use it run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn peer() -> Arc<Peer> {
        let shared = Arc::new(new_shared());
        let p = Arc::new(Peer { shared: shared.clone(), latency: AtomicU32::new(params::latency_of(&shared)) });
        register(&p);
        p
    }

    fn set_group(p: &Peer, g: f64) {
        let n = params::to_normalized(SYNC, g);
        p.shared.set(SYNC, n);
        changed(&p.shared, SYNC, n);
    }

    fn edit(p: &Peer, id: u32, plain: f64) {
        let n = params::to_normalized(id, plain);
        p.shared.set(id, n);
        changed(&p.shared, id, n);
    }

    fn plain(p: &Peer, id: u32) -> f64 {
        params::to_plain(id, p.shared.get(id))
    }

    #[test]
    fn instances_in_one_group_follow_each_other_and_others_do_not() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (a, b, c, d) = (peer(), peer(), peer(), peer());
        set_group(&a, 1.0);
        set_group(&b, 1.0);
        set_group(&c, 5.0);
        // d stays Off.
        edit(&a, KEY, 9.0);
        edit(&a, FLEX, 40.0);
        assert_eq!((plain(&b, KEY), plain(&b, FLEX)), (9.0, 40.0), "same group follows");
        assert_eq!(plain(&c, KEY), 0.0, "another group is untouched");
        assert_eq!(plain(&d, KEY), 0.0, "Off is untouched");
        // And back the other way.
        edit(&b, SCALE, 2.0);
        assert_eq!(plain(&a, SCALE), 2.0);
        // Off does not push.
        edit(&d, KEY, 3.0);
        assert_eq!(plain(&a, KEY), 9.0);
        // Bypass and the Link value itself are not mirrored.
        edit(&a, params::BYPASS, 1.0);
        assert_eq!(plain(&b, params::BYPASS), 0.0);
        for p in [&a, &b, &c, &d] {
            unregister(p);
        }
    }

    #[test]
    fn joining_takes_the_groups_values_and_the_processor_is_told() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (a, b) = (peer(), peer());
        set_group(&a, 2.0);
        edit(&a, KEY, 7.0);
        edit(&a, params::SPEED, 35.0);
        edit(&b, KEY, 1.0);
        let before = b.shared.generation.load(Ordering::Relaxed);
        set_group(&b, 2.0);
        assert_eq!((plain(&b, KEY), plain(&b, params::SPEED)), (7.0, 35.0));
        assert!(b.shared.generation.load(Ordering::Relaxed) > before, "the processor re-applies");
        // The first member of a group keeps its own values.
        let c = peer();
        edit(&c, KEY, 4.0);
        set_group(&c, 3.0);
        assert_eq!(plain(&c, KEY), 4.0);
        for p in [&a, &b, &c] {
            unregister(p);
        }
    }

    #[test]
    fn audio_thread_changes_reach_the_group_without_the_host() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (a, b, c) = (peer(), peer(), peer());
        for (p, g) in [(&a, 2.0), (&b, 2.0), (&c, 4.0)] {
            let n = params::to_normalized(SYNC, g);
            p.shared.set(SYNC, n);
            rt_changed(&p.shared, SYNC, n);
        }
        let n = params::to_normalized(KEY, 6.0);
        a.shared.set(KEY, n);
        rt_changed(&a.shared, KEY, n);
        assert_eq!(plain(&b, KEY), 6.0);
        assert_eq!(plain(&c, KEY), 0.0, "another group");
        // A late joiner takes the group's values.
        let d = peer();
        let g = params::to_normalized(SYNC, 2.0);
        d.shared.set(SYNC, g);
        let before = d.shared.generation.load(Ordering::Relaxed);
        rt_changed(&d.shared, SYNC, g);
        assert_eq!(plain(&d, KEY), 6.0);
        assert!(d.shared.generation.load(Ordering::Relaxed) > before);
        for p in [&a, &b, &c, &d] {
            unregister(p);
        }
    }

    #[test]
    fn latency_follows_a_synced_speed_change() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let (a, b) = (peer(), peer());
        set_group(&a, 4.0);
        set_group(&b, 4.0);
        let l0 = b.latency.load(Ordering::Relaxed);
        edit(&a, params::SPEED, -3.0);
        assert!(b.latency.load(Ordering::Relaxed) > l0, "Lock adds lookahead in the follower too");
        edit(&a, MODE, 1.0);
        assert_eq!(b.latency.load(Ordering::Relaxed), 24, "Record is 0.5 ms");
        for p in [&a, &b] {
            unregister(p);
        }
    }

    #[test]
    fn dropped_instances_leave_the_registry() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let a = peer();
        {
            let b = peer();
            set_group(&b, 1.0);
        }
        set_group(&a, 1.0);
        edit(&a, KEY, 2.0);
        assert_eq!(plain(&a, KEY), 2.0);
        unregister(&a);
    }
}
