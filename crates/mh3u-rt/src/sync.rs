//! Threads, mutexes, events, semaphores, message queues and sleeping: the system functions that may have to wait.
//!
//! Arguments are in r3 to r10 and the result in r3. A function returns `Step::Wait` when its condition does not hold yet; it is run
//! again when its thread gets the baton back, so it must not change anything before it knows it can finish.

use crate::Ctx;
use crate::kernel::{Kernel, KernelHandler, Life, MutexState, Queue, Step, Thread};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const TRUE: u32 = 1;
const FALSE: u32 = 0;
/// The time base's rate, in ticks per second.
const TICKS_PER_SECOND: u64 = crate::cafe::TICKS_PER_SECOND;

fn ticks_to_duration(ticks: u64) -> Duration {
    // `ticks * 1e9` in u64 saturates after a few minutes (~18e9 ticks at this rate)
    Duration::from_secs(ticks / TICKS_PER_SECOND) + Duration::from_nanos((ticks % TICKS_PER_SECOND) * 1_000_000_000 / TICKS_PER_SECOND)
}

pub fn find(name: &str) -> Option<KernelHandler> {
    Some(match name {
        "coreinit:OSGetCurrentThread" => |k, c| {
            c.r[3] = k.threads[k.current].addr;
            Step::Done
        },
        "coreinit:OSCreateThread" => create_thread,
        "coreinit:OSResumeThread" => |k, c| {
            let Some(&t) = k.by_addr.get(&c.r[3]) else { return done(c, 0) };
            let before = k.threads[t].suspend;
            if before > 0 {
                k.threads[t].suspend -= 1;
            }
            if k.threads[t].suspend == 0 && k.threads[t].life == Life::Suspended {
                k.threads[t].life = Life::Ready;
                if !k.threads[t].started {
                    k.threads[t].started = true;
                    crate::game::spawn(k, t);
                }
            }
            done(c, before)
        },
        "coreinit:OSSuspendThread" => |k, c| {
            if let Some(&t) = k.by_addr.get(&c.r[3]) {
                k.threads[t].suspend += 1;
            }
            done(c, 0)
        },
        "coreinit:OSExitThread" => |k, c| {
            k.finish_thread(c.r[3]);
            Step::Exit
        },
        "coreinit:OSJoinThread" => |k, c| {
            let Some(&t) = k.by_addr.get(&c.r[3]) else { return done(c, FALSE) };
            if k.threads[t].life != Life::Done {
                return Step::Wait;
            }
            if c.r[4] != 0 {
                k.guest.put_u32(c.r[4], k.threads[t].exit_code);
            }
            done(c, TRUE)
        },
        "coreinit:OSIsThreadTerminated" => |k, c| {
            let ended = k.by_addr.get(&c.r[3]).is_none_or(|&t| k.threads[t].life == Life::Done);
            done(c, u32::from(ended))
        },
        "coreinit:exit" | "coreinit:_Exit" | "coreinit:abort" => |_, c| Step::Stop(format!("the game called exit({})", c.r[3] as i32)),
        // modules loaded at run time are all "found"; what they export are functions that do nothing and return 0
        "coreinit:OSDynLoad_Acquire" => |k, c| {
            if k.report.trace {
                eprintln!("[dynload] module {}", k.guest.cstr(c.r[3]));
            }
            k.guest.put_u32(c.r[4], 1);
            done(c, 0)
        },
        "coreinit:OSDynLoad_FindExport" => |k, c| {
            let name = k.guest.cstr(c.r[5]);
            let n = k.dynamic.iter().position(|d| *d == name).unwrap_or_else(|| {
                if k.report.trace {
                    eprintln!("[dynload] {name} -> stored at {:#x}", c.r[6]);
                }
                k.dynamic.push(name);
                k.dynamic.len() - 1
            });
            k.guest.put_u32(c.r[6], crate::kernel::DYNAMIC_BASE + 4 * n as u32);
            done(c, 0)
        },
        // alarms: timers whose handlers run on a thread of their own (see `game::spawn_alarms`)
        "coreinit:OSCreateAlarm" => |k, c| {
            k.alarms.remove(&c.r[3]);
            Step::Done
        },
        "coreinit:OSSetAlarm" => |k, c| {
            let ticks = (u64::from(c.r[4]) << 32) | u64::from(c.r[5]);
            let delay = ticks_to_duration(ticks);
            k.alarms.insert(
                c.r[3],
                crate::kernel::Alarm {
                    due: std::time::Instant::now() + delay,
                    period: None,
                    handler: c.r[6],
                    tag: 0,
                },
            );
            if !k.alarm_thread {
                k.alarm_thread = true;
                crate::game::spawn_alarms(k);
            }
            done(c, 1)
        },
        "coreinit:OSCancelAlarm" => |k, c| {
            k.alarms.remove(&c.r[3]);
            Step::Done
        },
        "coreinit:OSCancelAlarms" => |k, c| {
            let tag = c.r[3];
            k.alarms.retain(|_, a| a.tag != tag);
            Step::Done
        },
        "coreinit:OSSetAlarmUserData" => |k, c| {
            k.alarm_user_data.insert(c.r[3], c.r[4]);
            Step::Done
        },
        "coreinit:OSGetAlarmUserData" => |k, c| {
            let v = k.alarm_user_data.get(&c.r[3]).copied().unwrap_or(0);
            done(c, v)
        },
        "coreinit:OSDynLoad_Release" => |_, _| Step::Done,
        // the application starts in the background and is told to acquire the foreground by its first call of ProcUIProcessMessages
        "proc_ui:ProcUIRegisterCallback" => |k, c| {
            k.procui_callbacks.push((c.r[3], c.r[4], c.r[5], c.r[6]));
            done(c, 0)
        },
        "proc_ui:ProcUIProcessMessages" => |k, c| {
            c.r[3] = 0;
            if k.procui_acquired {
                return Step::Done;
            }
            k.procui_acquired = true;
            let mut acquire: Vec<_> = k.procui_callbacks.iter().filter(|cb| cb.0 == 0).copied().collect();
            acquire.sort_by_key(|cb| cb.3);
            Step::Calls(acquire.iter().map(|cb| (cb.1, cb.2)).collect())
        },
        // the audio system starts its frames (and the thread that mixes them) when it is set up or given a frame callback
        "snd_core.rpl:AXInit" | "snd_core.rpl:AXInitEx" => |k, c| {
            k.os.ax.rate = 32_000;
            crate::game::spawn_audio(k);
            done(c, 0)
        },
        "snd_core.rpl:AXInitWithParams" => |k, c| {
            // the renderer's rate: 0 for 32 kHz, 1 for 48 kHz
            k.os.ax.rate = if k.guest.u32(c.r[3]) == 1 { 48_000 } else { 32_000 };
            crate::game::spawn_audio(k);
            done(c, 0)
        },
        "snd_core.rpl:AXRegisterFrameCallback" | "snd_core.rpl:AXRegisterAppFrameCallback" => |k, c| {
            crate::game::spawn_audio(k);
            if c.r[3] != 0 && !k.ax_callbacks.contains(&c.r[3]) {
                k.ax_callbacks.push(c.r[3]);
            }
            done(c, 0)
        },
        "snd_core.rpl:AXDeregisterAppFrameCallback" => |k, c| {
            k.ax_callbacks.retain(|&cb| cb != c.r[3]);
            done(c, 0)
        },
        "coreinit:OSYieldThread" => |_, _| Step::Yield,
        "coreinit:OSSleepTicks" => |k, c| {
            let me = k.current;
            let ticks = (u64::from(c.r[3]) << 32) | u64::from(c.r[4]);
            let until = *k.threads[me]
                .sleep_until
                .get_or_insert_with(|| Instant::now() + ticks_to_duration(ticks));
            if Instant::now() >= until {
                k.threads[me].sleep_until = None;
                Step::Done
            } else {
                Step::Wait
            }
        },
        "coreinit:OSInitMutex" | "coreinit:OSInitMutexEx" => |k, c| {
            k.mutexes.insert(c.r[3], MutexState::default());
            k.guest.fill(c.r[3], 0, 0x2c);
            Step::Done
        },
        "coreinit:OSLockMutex" => |k, c| {
            let me = k.current;
            let m = k.mutexes.entry(c.r[3]).or_default();
            if m.owner.is_none() || m.owner == Some(me) {
                m.owner = Some(me);
                m.count += 1;
                Step::Done
            } else {
                Step::Wait
            }
        },
        "coreinit:OSTryLockMutex" => |k, c| {
            let me = k.current;
            let m = k.mutexes.entry(c.r[3]).or_default();
            if m.owner.is_none() || m.owner == Some(me) {
                m.owner = Some(me);
                m.count += 1;
                done(c, TRUE)
            } else {
                done(c, FALSE)
            }
        },
        "coreinit:OSUnlockMutex" => |k, c| {
            let me = k.current;
            if let Some(m) = k.mutexes.get_mut(&c.r[3]).filter(|m| m.owner == Some(me)) {
                m.count -= 1;
                if m.count == 0 {
                    m.owner = None;
                }
            }
            Step::Done
        },
        // the mode: 0 is manual reset, 1 resets when a thread passes
        "coreinit:OSInitEvent" => |k, c| {
            k.events.insert(c.r[3], (c.r[4] != 0, c.r[5] == 1));
            Step::Done
        },
        "coreinit:OSSignalEvent" => |k, c| {
            k.events.entry(c.r[3]).or_default().0 = true;
            Step::Done
        },
        "coreinit:OSResetEvent" => |k, c| {
            k.events.entry(c.r[3]).or_default().0 = false;
            Step::Done
        },
        "coreinit:OSWaitEvent" => |k, c| {
            let e = k.events.entry(c.r[3]).or_default();
            if e.0 {
                if e.1 {
                    e.0 = false;
                }
                Step::Done
            } else {
                Step::Wait
            }
        },
        "coreinit:OSInitSemaphore" => |k, c| {
            k.semaphores.insert(c.r[3], c.r[4] as i32);
            Step::Done
        },
        "coreinit:OSWaitSemaphore" => |k, c| {
            let s = k.semaphores.entry(c.r[3]).or_default();
            if *s > 0 {
                *s -= 1;
                done(c, 1)
            } else {
                Step::Wait
            }
        },
        "coreinit:OSTryWaitSemaphore" => |k, c| {
            let s = k.semaphores.entry(c.r[3]).or_default();
            let got = *s > 0;
            if got {
                *s -= 1;
            }
            done(c, u32::from(got))
        },
        "coreinit:OSSignalSemaphore" => |k, c| {
            let s = k.semaphores.entry(c.r[3]).or_default();
            *s += 1;
            done(c, (*s - 1) as u32)
        },
        "coreinit:OSInitMessageQueue" | "coreinit:OSInitMessageQueueEx" => |k, c| {
            k.queues.insert(
                c.r[3],
                Queue {
                    capacity: c.r[5],
                    items: VecDeque::new(),
                },
            );
            Step::Done
        },
        // flags: 1 blocks when the queue is full (or empty, for receiving)
        "coreinit:OSSendMessage" => |k, c| send(k, c, false),
        "coreinit:OSJamMessage" => |k, c| send(k, c, true),
        "coreinit:OSReceiveMessage" => |k, c| {
            let g = k.guest;
            let Some(q) = k.queues.get_mut(&c.r[3]) else {
                return done(c, FALSE);
            };
            match q.items.pop_front() {
                Some(m) => {
                    for (i, w) in m.iter().enumerate() {
                        g.put_u32(c.r[4] + 4 * i as u32, *w);
                    }
                    done(c, TRUE)
                }
                None if c.r[5] & 1 != 0 => Step::Wait,
                None => done(c, FALSE),
            }
        },
        "coreinit:OSPeekMessage" => |k, c| {
            let g = k.guest;
            let Some(m) = k.queues.get(&c.r[3]).and_then(|q| q.items.front().copied()) else {
                return done(c, FALSE);
            };
            for (i, w) in m.iter().enumerate() {
                g.put_u32(c.r[4] + 4 * i as u32, *w);
            }
            done(c, TRUE)
        },
        // names, priorities and affinities do not change how the threads are run here
        "coreinit:OSSetThreadName" | "coreinit:OSSetThreadAffinity" | "coreinit:OSSetThreadCancelState" => |_, c| done(c, TRUE),
        "coreinit:OSSetThreadPriority" => |_, c| done(c, TRUE),
        "coreinit:OSGetThreadPriority" => |_, c| done(c, 16),
        _ => return None,
    })
}

fn done(c: &mut Ctx, result: u32) -> Step {
    c.r[3] = result;
    Step::Done
}

fn send(k: &mut Kernel, c: &mut Ctx, front: bool) -> Step {
    let g = k.guest;
    let Some(q) = k.queues.get_mut(&c.r[3]) else {
        return done(c, FALSE);
    };
    if q.items.len() as u32 >= q.capacity {
        return if c.r[5] & 1 != 0 { Step::Wait } else { done(c, FALSE) };
    }
    let m = [g.u32(c.r[4]), g.u32(c.r[4] + 4), g.u32(c.r[4] + 8), g.u32(c.r[4] + 12)];
    if front {
        q.items.push_front(m);
    } else {
        q.items.push_back(m);
    }
    done(c, TRUE)
}

/// `OSCreateThread(thread, entry, argc, argv, stack, stack_size, priority, attributes)`: the thread starts suspended.
fn create_thread(k: &mut Kernel, c: &mut Ctx) -> Step {
    let addr = c.r[3];
    let thread = Thread {
        addr,
        life: Life::Suspended,
        suspend: 1,
        exit_code: 0,
        sleep_until: None,
        entry: c.r[4],
        argc: c.r[5],
        argv: c.r[6],
        stack_top: c.r[7],
        started: false,
        last: None,
    };
    match k.by_addr.get(&addr) {
        Some(&t) => k.threads[t] = thread,
        None => {
            k.by_addr.insert(addr, k.threads.len());
            k.threads.push(thread);
        }
    }
    done(c, TRUE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_ticks_convert_correctly_after_fifty_minutes() {
        let ticks = 50 * 60 * TICKS_PER_SECOND;
        assert_eq!(ticks_to_duration(ticks), Duration::from_secs(50 * 60));
    }

    #[test]
    fn sleep_ticks_keep_the_fraction_past_the_old_u64_overflow() {
        let ticks = 5 * 60 * TICKS_PER_SECOND + TICKS_PER_SECOND / 2;
        assert_eq!(
            ticks_to_duration(ticks),
            Duration::from_secs(5 * 60) + Duration::from_nanos(500_000_000)
        );
    }
}
