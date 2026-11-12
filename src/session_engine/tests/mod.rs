//! Synthetic, offline acceptance vectors for the engine.

mod events_hooks;
mod forks;
mod policy;
mod tail;

use crate::domain::{
    Binding, ClockSample, ClockSource, Generations, MonotonicStamp, NativeProcessIdentity,
    RuntimeEpoch, SessionId, Slot, SlotMap,
};

fn session(number: u128) -> SessionId {
    SessionId::from_uuid(uuid::Uuid::from_u128(number))
}

fn epoch(number: u128) -> RuntimeEpoch {
    RuntimeEpoch::from_uuid(uuid::Uuid::from_u128(number))
}

fn slot(number: u8) -> Slot {
    Slot::new(number).expect("synthetic slot")
}

fn process(number: u32) -> NativeProcessIdentity {
    NativeProcessIdentity {
        pid: number,
        creation_time_100ns: u64::from(number) * 100,
        session_id: 7,
    }
}

fn clock(ticks: u64, frequency: u64) -> ClockSample {
    ClockSample {
        source: ClockSource::LinuxMonotonic,
        stamp: MonotonicStamp {
            epoch: epoch(1),
            ticks,
            frequency,
        },
    }
}

fn generations() -> Generations {
    Generations {
        connection: 10,
        profile: 20,
        selection: 30,
        binding: 40,
        presentation: 50,
    }
}

fn slot_map(binding: Binding) -> SlotMap {
    let mut map = SlotMap::default();
    map.bind(slot(1), binding, 0, None, None)
        .expect("synthetic binding");
    map
}
