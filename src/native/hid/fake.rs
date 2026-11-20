use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use super::*;

#[derive(Default)]
pub struct FakeState {
    pub enumerate_calls: usize,
    pub open_calls: usize,
    pub descriptor_calls: usize,
    pub exclusive_open_calls: usize,
    pub exclusive_open_options: Vec<u32>,
    pub written: Vec<Vec<u8>>,
    pub inputs: VecDeque<Vec<u8>>,
    /// Count reported back from `write`, overriding the true length.
    pub write_count: Option<usize>,
    /// Count reported back from `read`, overriding the true length.
    pub read_count: Option<usize>,
    pub writes_unsupported: bool,
    pub report_descriptor: Option<Vec<u8>>,
    pub exclusive_open_result: Option<ExclusiveOpenProbe>,
    pub route_open_result: Option<OutputRouteOpenProof>,
    pub route_open_calls: usize,
}

pub struct FakeBackend {
    pub devices: Vec<HidDescriptor>,
    pub state: Rc<RefCell<FakeState>>,
    pub ignore_matcher: bool,
}

impl FakeBackend {
    pub fn new(devices: Vec<HidDescriptor>) -> Self {
        Self {
            devices,
            state: Rc::default(),
            ignore_matcher: false,
        }
    }
}

impl HidBackend for FakeBackend {
    fn enumerate(&self, m: &HidMatcher, _limit: usize) -> Result<Vec<HidDescriptor>, HidError> {
        self.state.borrow_mut().enumerate_calls += 1;
        Ok(self
            .devices
            .iter()
            .filter(|d| self.ignore_matcher || m.matches(d))
            .cloned()
            .collect())
    }

    fn open(&self, d: &HidDescriptor) -> Result<Box<dyn HidTransport>, HidError> {
        self.state.borrow_mut().open_calls += 1;
        if !self.devices.contains(d) {
            return Err(HidError::NotFound);
        }
        Ok(Box::new(FakeTransport(self.state.clone())))
    }

    fn open_filtered(
        &self,
        d: &HidDescriptor,
        reports: &[(u8, usize)],
    ) -> Result<Box<dyn HidTransport>, HidError> {
        if reports.is_empty() || reports.iter().any(|(_, length)| *length == 0) {
            return Err(HidError::Protocol("invalid fake report filter"));
        }
        self.open(d)
    }

    fn original_report_descriptor(&self, d: &HidDescriptor) -> Result<Vec<u8>, HidError> {
        let mut state = self.state.borrow_mut();
        state.descriptor_calls += 1;
        if !self.devices.contains(d) {
            return Err(HidError::NotFound);
        }
        state
            .report_descriptor
            .clone()
            .ok_or(HidError::EvidenceUnavailable(
                EvidenceKind::RawReportDescriptor,
            ))
    }

    fn open_exclusive_probe(&self, d: &HidDescriptor) -> Result<ExclusiveOpenProbe, HidError> {
        let mut state = self.state.borrow_mut();
        state.exclusive_open_calls += 1;
        state.exclusive_open_options.push(1);
        if !self.devices.contains(d) {
            return Err(HidError::NotFound);
        }
        state.exclusive_open_result.ok_or(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform,
        ))
    }

    fn open_route_probe(&self, d: &HidDescriptor) -> Result<OutputRouteOpenProof, HidError> {
        let state = &mut *self.state.borrow_mut();
        state.route_open_calls += 1;
        if !self.devices.contains(d) {
            return Err(HidError::NotFound);
        }
        state.route_open_result.clone().ok_or(HidError::Unavailable(
            UnavailableReason::UnsupportedPlatform,
        ))
    }
}

struct FakeTransport(Rc<RefCell<FakeState>>);

impl HidTransport for FakeTransport {
    fn supports_write(&self) -> bool {
        !self.0.borrow().writes_unsupported
    }

    fn write(&mut self, report: &[u8]) -> Result<usize, HidError> {
        let mut s = self.0.borrow_mut();
        s.written.push(report.to_vec());
        Ok(s.write_count.unwrap_or(report.len()))
    }

    fn read(&mut self, buf: &mut [u8], _t: Duration) -> Result<usize, HidError> {
        let mut s = self.0.borrow_mut();
        let Some(r) = s.inputs.pop_front() else {
            return Err(HidError::Timeout);
        };
        let n = r.len().min(buf.len());
        buf[..n].copy_from_slice(&r[..n]);
        Ok(s.read_count.unwrap_or(n))
    }

    fn report_descriptor(&mut self) -> Result<Vec<u8>, HidError> {
        self.0
            .borrow()
            .report_descriptor
            .clone()
            .ok_or(HidError::EvidenceUnavailable(
                EvidenceKind::RawReportDescriptor,
            ))
    }
}
