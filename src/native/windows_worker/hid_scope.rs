//! Windows HID authority exists only for a validated live child callback.

use std::{cell::Cell, marker::PhantomData, rc::Rc};

use uuid::Uuid;

use super::WorkerErrorKind;
use crate::native::hid::{HidError, UnavailableReason};

thread_local! {
    static SCOPE: Cell<Option<(bool, Uuid)>> = const { Cell::new(None) };
}

pub(super) struct HidWorkerScope;

impl HidWorkerScope {
    pub(super) fn enter(
        live_hid: bool,
        nonce: Uuid,
        verify_containment: impl FnOnce() -> bool,
    ) -> Result<Self, WorkerErrorKind> {
        if !verify_containment() {
            return Err(WorkerErrorKind::Containment);
        }
        SCOPE.with(|scope| {
            if scope.get().is_some() {
                return Err(WorkerErrorKind::Containment);
            }
            scope.set(Some((live_hid, nonce)));
            Ok(Self)
        })
    }
}

impl Drop for HidWorkerScope {
    fn drop(&mut self) {
        SCOPE.with(|scope| scope.set(None));
    }
}

#[derive(Clone)]
pub(crate) struct HidWorkerCapability {
    pid: u32,
    nonce: Uuid,
    _thread_bound: PhantomData<Rc<()>>,
}

impl HidWorkerCapability {
    fn authorize_with(&self, verify_containment: impl FnOnce() -> bool) -> Result<(), HidError> {
        if self.pid == std::process::id()
            && SCOPE.with(|scope| scope.get() == Some((true, self.nonce)))
            && verify_containment()
        {
            Ok(())
        } else {
            Err(HidError::Unavailable(UnavailableReason::UnsupervisedAccess))
        }
    }

    #[cfg(all(windows, any(feature = "micro-hid", feature = "akp03-hid")))]
    pub(crate) fn authorize(&self) -> Result<(), HidError> {
        self.authorize_with(super::windows::verify_hid_containment)
    }

    #[cfg(all(windows, any(feature = "micro-hid", feature = "akp03-hid")))]
    pub(crate) fn valid(&self) -> bool {
        self.authorize().is_ok()
    }
}

fn capability_with(verify_containment: impl FnOnce() -> bool) -> Option<HidWorkerCapability> {
    let (live_hid, nonce) = SCOPE.with(Cell::get)?;
    if !live_hid {
        return None;
    }
    let capability = HidWorkerCapability {
        pid: std::process::id(),
        nonce,
        _thread_bound: PhantomData,
    };
    capability.authorize_with(verify_containment).ok()?;
    Some(capability)
}

#[cfg(all(windows, any(feature = "micro-hid", feature = "akp03-hid")))]
pub(crate) fn hid_worker_capability() -> Option<HidWorkerCapability> {
    capability_with(super::windows::verify_hid_containment)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_live_scopes_issue_authority_and_stale_tokens_never_revive() {
        assert!(capability_with(|| panic!("unscoped native call")).is_none());
        assert!(HidWorkerScope::enter(true, Uuid::new_v4(), || false).is_err());
        {
            let _health = HidWorkerScope::enter(false, Uuid::new_v4(), || true).unwrap();
            assert!(capability_with(|| panic!("health native call")).is_none());
        }
        let capability;
        {
            let _live = HidWorkerScope::enter(true, Uuid::new_v4(), || true).unwrap();
            capability = capability_with(|| true).unwrap();
            capability.authorize_with(|| true).unwrap();
            let mut wrong_process = capability.clone();
            wrong_process.pid = wrong_process.pid.wrapping_add(1);
            assert!(
                wrong_process
                    .authorize_with(|| panic!("wrong-process native call"))
                    .is_err()
            );
            assert!(capability.authorize_with(|| false).is_err());
            assert!(capability_with(|| false).is_none());
            assert!(HidWorkerScope::enter(true, Uuid::new_v4(), || true).is_err());
        }
        assert!(
            capability
                .authorize_with(|| panic!("expired native call"))
                .is_err()
        );
        let _next = HidWorkerScope::enter(true, Uuid::new_v4(), || true).unwrap();
        assert!(
            capability
                .authorize_with(|| panic!("revived native call"))
                .is_err()
        );
    }
}
