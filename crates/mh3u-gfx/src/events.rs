//! Game events from translation-time hooks (`docs/hooks.md`) for the overlay: the hooks' callbacks only queue an event (no memory
//! reads besides their registers), and the overlay reads the live save again when events come, instead of every frame.

use mh3u_rt::hooks::{Hook, Registry};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    Pouch,
    Box,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    ItemGain { item: u16, to: Dest },
    PouchRemove,
    Money(i32),
    Craft { jewel: bool },
    QuestEnd,
}

/// Where `item_gain` puts the item, by its return address (runs 30, 31, 33).
pub fn dest_of(lr: u32) -> Dest {
    match lr {
        0x0219_b080 => Dest::Pouch,
        0x0219_950c | 0x0219_9574 => Dest::Box,
        _ => Dest::Unknown,
    }
}

const MOST_QUEUED: usize = 64;
static QUEUE: Mutex<VecDeque<Event>> = Mutex::new(VecDeque::new());
/// Whether any of the overlay's hooks is in the build (then the save is read on events and every 2 s; else every frame).
static HOOKED: AtomicBool = AtomicBool::new(false);

pub fn push(e: Event) {
    let mut q = QUEUE.lock().unwrap_or_else(|p| p.into_inner());
    if q.len() == MOST_QUEUED {
        q.pop_front();
    }
    q.push_back(e);
}

pub fn drain() -> Vec<Event> {
    QUEUE.lock().unwrap_or_else(|p| p.into_inner()).drain(..).collect()
}

pub fn hooked() -> bool {
    HOOKED.load(Relaxed)
}

/// Registers the overlay's callbacks on the hooks the build has; gives how many were registered.
pub fn register(r: &mut Registry) -> usize {
    let mut n = 0;
    let mut on = |name: &str, f: fn(&Hook) -> Event| {
        n += usize::from(r.on(name, Box::new(move |h: &mut Hook| push(f(h)))));
    };
    on("item_gain", |h| Event::ItemGain {
        item: h.ctx.r[5] as u16,
        to: dest_of(h.ctx.lr),
    });
    on("pouch_remove_slot", |_| Event::PouchRemove);
    on("money_add", |h| Event::Money(h.ctx.r[4] as i32));
    on("craft_equipment", |_| Event::Craft { jewel: false });
    on("craft_jewel", |_| Event::Craft { jewel: true });
    on("quest_end", |_| Event::QuestEnd);
    HOOKED.store(n > 0, Relaxed);
    n
}

/// A line for the overlay's event feed.
pub fn describe(e: &Event, item_name: &dyn Fn(u16) -> String) -> Option<String> {
    Some(match *e {
        Event::ItemGain { item, to } => format!(
            "+ {} {}",
            item_name(item),
            match to {
                Dest::Pouch => "-> pouch",
                Dest::Box => "-> box",
                Dest::Unknown => "",
            }
        )
        .trim_end()
        .to_string(),
        Event::PouchRemove => "a pouch item went to the box".into(),
        Event::Money(d) => format!("{d:+}z"),
        Event::Craft { jewel: true } => "crafted a jewel".into(),
        Event::Craft { jewel: false } => "crafted equipment".into(),
        Event::QuestEnd => "quest over".into(),
    })
}

/// When the overlay reads the live save: without hooks, every frame (as before); with them, on the frames after an event (the
/// hook runs at the function's entry, before it writes, and the snapshot is taken at the frame's end) and every 2 s besides.
#[derive(Debug)]
pub struct Refresh {
    last: Option<Instant>,
    pending: u8,
}

pub const FALLBACK: Duration = Duration::from_secs(2);
/// Frames read after an event.
const AFTER_EVENT: u8 = 3;

impl Refresh {
    pub fn new() -> Refresh {
        Refresh { last: None, pending: 0 }
    }

    pub fn due(&mut self, now: Instant, hooked: bool, events: bool) -> bool {
        if events {
            self.pending = AFTER_EVENT;
        }
        let due = !hooked || self.pending > 0 || self.last.is_none_or(|l| now.duration_since(l) >= FALLBACK);
        if due {
            self.pending = self.pending.saturating_sub(1);
            self.last = Some(now);
        }
        due
    }
}

impl Default for Refresh {
    fn default() -> Refresh {
        Refresh::new()
    }
}

/// Item gains not in the save yet: during a quest the game keeps the pickups elsewhere and merges them into the save at the quest's
/// end (PR #16 test). Each gain counts 1 until the save's count of that item goes up past what it was at the gain (then the save
/// has it) or the quest ends.
#[derive(Debug, Default)]
pub struct Provisional {
    pending: std::collections::BTreeMap<u16, (u32, u32)>,
}

impl Provisional {
    /// An item gained; `in_save` is the save's count of it now.
    pub fn gain(&mut self, item: u16, in_save: u32) {
        self.pending.entry(item).or_insert((in_save, 0)).1 += 1;
    }

    /// The save read again: gains the save now has are dropped.
    pub fn observe(&mut self, in_save: impl Fn(u16) -> u32) {
        self.pending.retain(|&item, (base, _)| in_save(item) <= *base);
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }

    pub fn extra(&self, item: u16) -> u32 {
        self.pending.get(&item).map_or(0, |p| p.1)
    }

    /// (item, gained) for the ones not in the save yet.
    pub fn items(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.pending.iter().map(|(&i, &(_, n))| (i, n))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quest_pickups_count_until_the_save_has_them_without_counting_twice() {
        let mut p = Provisional::default();
        p.gain(0x10, 3);
        p.gain(0x10, 3);
        p.gain(0x20, 0);
        assert_eq!((p.extra(0x10), p.extra(0x20), p.extra(0x30)), (2, 1, 0));
        // the save unchanged (on a quest): still counted
        p.observe(|i| if i == 0x10 { 3 } else { 0 });
        assert_eq!(p.extra(0x10), 2);
        // out of a quest the save gets the item at once: dropped, not counted twice
        p.observe(|i| if i == 0x20 { 1 } else { 3 });
        assert_eq!((p.extra(0x10), p.extra(0x20)), (2, 0));
        p.clear();
        assert_eq!(p.items().count(), 0);
    }

    use super::*;

    #[test]
    fn item_gain_destination_comes_from_the_lr() {
        assert_eq!(dest_of(0x0219_b080), Dest::Pouch);
        assert_eq!(dest_of(0x0219_950c), Dest::Box);
        assert_eq!(dest_of(0x1234), Dest::Unknown);
        let name = |id: u16| if id == 0x17a { "Spongy Hide".to_string() } else { format!("#{id}") };
        assert_eq!(
            describe(
                &Event::ItemGain {
                    item: 0x17a,
                    to: Dest::Pouch
                },
                &name
            )
            .unwrap(),
            "+ Spongy Hide -> pouch"
        );
        assert_eq!(describe(&Event::Money(-750), &name).unwrap(), "-750z");
        assert_eq!(describe(&Event::Money(1000), &name).unwrap(), "+1000z");
    }

    #[test]
    fn callbacks_queue_events_and_the_queue_is_bounded() {
        let list = [(0x0219_9438, "item_gain".to_string()), (0x0219_c720, "money_add".to_string())].into();
        let mut r = Registry::new(&list);
        assert_eq!(register(&mut r), 2);
        assert!(hooked());
        drain();
        let mut ctx = mh3u_rt::new_test_ctx();
        ctx.r[5] = 0x17a;
        ctx.lr = 0x0219_b080;
        r.run(0x0219_9438, &mut ctx);
        ctx.r[4] = (-750i32) as u32;
        r.run(0x0219_c720, &mut ctx);
        assert_eq!(
            drain(),
            vec![
                Event::ItemGain {
                    item: 0x17a,
                    to: Dest::Pouch
                },
                Event::Money(-750)
            ]
        );
        for _ in 0..100 {
            push(Event::QuestEnd);
        }
        assert_eq!(drain().len(), MOST_QUEUED);
    }

    #[test]
    fn the_save_is_read_after_events_and_every_two_seconds_with_hooks_and_every_frame_without() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut r = Refresh::new();
        assert!(r.due(ms(0), true, false), "the first time");
        assert!(!r.due(ms(16), true, false));
        assert!(r.due(ms(32), true, true), "an event");
        assert!(r.due(ms(48), true, false) && r.due(ms(64), true, false), "and the frames after it");
        assert!(!r.due(ms(80), true, false));
        assert!(r.due(ms(2100), true, false), "the fallback");
        let mut r = Refresh::new();
        assert!((0..5).all(|i| r.due(ms(i * 16), false, false)), "no hooks: every frame");
    }
}
