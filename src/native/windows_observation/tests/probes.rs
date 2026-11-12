use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
};

#[derive(Clone)]
struct Node {
    process_id: i32,
    metadata: UiNodeMetadata,
    child: Option<usize>,
    sibling: Option<usize>,
}

struct ConsoleState {
    lists: VecDeque<Vec<u32>>,
    windows: VecDeque<Option<u64>>,
    identities: HashMap<u32, ProcessIdentity>,
    attaches: usize,
    detaches: usize,
    fail_detach: bool,
}

struct Probe {
    tick: u64,
    identities: HashMap<u32, ProcessIdentity>,
    security_calls: usize,
    image_calls: usize,
    ui_calls: usize,
    metadata_calls: usize,
    unsafe_security: bool,
    fail_security: bool,
    security_delay_ticks: u64,
    fail_tree: bool,
    nodes: Vec<Node>,
    foreground: VecDeque<bool>,
    existing_console: Option<u64>,
    console: Rc<RefCell<ConsoleState>>,
    drop_count: Rc<Cell<usize>>,
}

fn helper_id() -> ProcessIdentity {
    ProcessIdentity {
        pid: 777,
        creation_time_100ns: 789,
        session_id: 3,
    }
}

fn request() -> ProcessRequest {
    ProcessRequest {
        target: identity(),
        requested_at: stamp(100, 1_000),
    }
}

fn token() -> TokenContext {
    TokenContext {
        session_id: 3,
        integrity_rid: 0x2000,
        elevated: false,
        ui_access: false,
        app_container: false,
    }
}

fn node() -> Node {
    Node {
        process_id: 123,
        metadata: UiNodeMetadata {
            control_type: 50_020,
            keyboard_focus: false,
            offscreen: false,
        },
        child: None,
        sibling: None,
    }
}

fn probe() -> Probe {
    let identities = [(123, identity()), (777, helper_id())]
        .into_iter()
        .collect::<HashMap<_, _>>();
    Probe {
        tick: 102,
        identities: identities.clone(),
        security_calls: 0,
        image_calls: 0,
        ui_calls: 0,
        metadata_calls: 0,
        unsafe_security: false,
        fail_security: false,
        security_delay_ticks: 0,
        fail_tree: false,
        nodes: vec![node()],
        foreground: [true, true].into(),
        existing_console: None,
        console: Rc::new(RefCell::new(ConsoleState {
            lists: [vec![777, 123], vec![123, 777]].into(),
            windows: [Some(81), Some(81)].into(),
            identities,
            attaches: 0,
            detaches: 0,
            fail_detach: false,
        })),
        drop_count: Rc::new(Cell::new(0)),
    }
}

impl ProcessProbeBackend for Probe {
    type Held = ProcessIdentity;
    fn stamp(&mut self) -> PlatformResult<MonotonicStamp> {
        self.tick += 1;
        Ok(stamp(self.tick, 1_000))
    }
    fn current_pid(&mut self) -> u32 {
        777
    }
    fn hold(&mut self, pid: u32) -> PlatformResult<ProcessIdentity> {
        self.identities
            .get(&pid)
            .copied()
            .ok_or_else(|| error(ErrorKind::ProcessExited))
    }
    fn evidence(&mut self, process: &ProcessIdentity) -> PlatformResult<ProcessEvidence> {
        Ok(evidence(*process, ExecutableClass::NativeHost))
    }
    fn check_running(&mut self, _: &ProcessIdentity) -> PlatformResult<()> {
        Ok(())
    }
    fn security(
        &mut self,
        _: &ProcessIdentity,
        _: &ProcessIdentity,
    ) -> PlatformResult<SecurityEvidence> {
        self.security_calls += 1;
        self.tick += self.security_delay_ticks;
        if self.fail_security {
            return Err(error(ErrorKind::AccessDenied));
        }
        let target = if self.unsafe_security {
            TokenContext {
                elevated: true,
                ..token()
            }
        } else {
            token()
        };
        Ok(SecurityEvidence {
            same_user: true,
            same_logon: true,
            observer: token(),
            target,
        })
    }
}

impl ExactProcessBackend for Probe {
    fn image_file(
        &mut self,
        _: &ProcessIdentity,
        _: &ProcessIdentity,
    ) -> PlatformResult<ImageFileEvidence> {
        self.image_calls += 1;
        Ok(ImageFileEvidence {
            same_file_as_observer: true,
            path_file_stable_during_probe: true,
            provenance: Provenance::Unverified,
        })
    }
    fn foreground(&mut self) -> PlatformResult<ForegroundEvidence> {
        Err(error(ErrorKind::NoForeground))
    }
    fn desktop(&mut self) -> PlatformResult<DesktopContext> {
        Err(error(ErrorKind::AccessDenied))
    }
}

impl UiBackend for Probe {
    type Element = usize;
    fn is_root_window(&mut self, window: u64) -> PlatformResult<bool> {
        self.ui_calls += 1;
        Ok(window == 42)
    }
    fn window_owner(&mut self, _: u64) -> PlatformResult<(u32, u32)> {
        Ok((50, 123))
    }
    fn is_foreground(&mut self, _: u64) -> PlatformResult<bool> {
        Ok(self.foreground.pop_front().unwrap())
    }
    fn root_element(&mut self, _: u64) -> PlatformResult<usize> {
        Ok(0)
    }
    fn root_native_window(&mut self, _: &usize) -> PlatformResult<u64> {
        Ok(42)
    }
    fn process_id(&mut self, node: &usize) -> PlatformResult<i32> {
        Ok(self.nodes[*node].process_id)
    }
    fn metadata(&mut self, node: &usize) -> PlatformResult<UiNodeMetadata> {
        self.metadata_calls += 1;
        Ok(self.nodes[*node].metadata)
    }
    fn first_child(&mut self, node: &usize) -> PlatformResult<Option<usize>> {
        if self.fail_tree {
            Err(error(ErrorKind::NativeFailure))
        } else {
            Ok(self.nodes[*node].child)
        }
    }
    fn next_sibling(&mut self, node: &usize) -> PlatformResult<Option<usize>> {
        Ok(self.nodes[*node].sibling)
    }
}

struct FakeAttachment {
    state: Rc<RefCell<ConsoleState>>,
    dropped: Rc<Cell<usize>>,
    attached: bool,
}

impl Drop for FakeAttachment {
    fn drop(&mut self) {
        if self.attached {
            self.dropped.set(self.dropped.get() + 1);
        }
    }
}

impl ConsoleAttachment for FakeAttachment {
    type Held = ProcessIdentity;
    fn process_ids(&mut self) -> PlatformResult<Vec<u32>> {
        Ok(self.state.borrow_mut().lists.pop_front().unwrap())
    }
    fn console_window(&mut self) -> PlatformResult<Option<u64>> {
        Ok(self.state.borrow_mut().windows.pop_front().unwrap())
    }
    fn hold(&mut self, pid: u32) -> PlatformResult<ProcessIdentity> {
        self.state
            .borrow()
            .identities
            .get(&pid)
            .copied()
            .ok_or_else(|| error(ErrorKind::ProcessExited))
    }
    fn evidence(&mut self, process: &ProcessIdentity) -> PlatformResult<ProcessEvidence> {
        Ok(evidence(*process, ExecutableClass::Other))
    }
    fn check_running(&mut self, _: &ProcessIdentity) -> PlatformResult<()> {
        Ok(())
    }
    fn detach(mut self) -> PlatformResult<()> {
        self.state.borrow_mut().detaches += 1;
        if self.state.borrow().fail_detach {
            return Err(error(ErrorKind::NativeFailure));
        }
        self.attached = false;
        Ok(())
    }
}

impl ConsoleBackend for Probe {
    type Attachment = FakeAttachment;
    fn existing_console_window(&mut self) -> PlatformResult<Option<u64>> {
        Ok(self.existing_console)
    }
    fn attach(&mut self, _: u32) -> PlatformResult<FakeAttachment> {
        self.console.borrow_mut().attaches += 1;
        Ok(FakeAttachment {
            state: self.console.clone(),
            dropped: self.drop_count.clone(),
            attached: true,
        })
    }
}

#[test]
fn exact_target_mismatch_aborts_before_sensitive_subsections() {
    let mut backend = probe();
    let mut request = request();
    request.target.creation_time_100ns += 1;
    assert_eq!(
        collect_exact_process(&mut backend, &request)
            .unwrap_err()
            .operation,
        "exact_process_lifetime"
    );
    assert_eq!(backend.security_calls, 0);
    assert_eq!(backend.image_calls, 0);
    request.target = ProcessIdentity {
        session_id: 4,
        ..identity()
    };
    assert_eq!(
        collect_exact_process(&mut backend, &request)
            .unwrap_err()
            .operation,
        "exact_process_lifetime"
    );
    assert_eq!(backend.security_calls, 0);
}

#[test]
fn exact_report_keeps_independent_errors_and_false_qualification() {
    let mut backend = probe();
    backend.fail_security = true;
    let report = collect_exact_process(&mut backend, &request()).unwrap();
    assert!(
        matches!(report.security, Observation::Unavailable(ref e) if e.kind == ErrorKind::AccessDenied)
    );
    assert!(matches!(report.image_file, Observation::Observed(_)));
    assert!(matches!(report.foreground, Observation::Unavailable(_)));
    assert!(matches!(report.desktop, Observation::Unavailable(_)));
    assert!(!report.command_ready && !report.w2_qualified && report.read_only);
    let json = serde_json::to_string(&report).unwrap();
    for secret in ["SID", "LUID", "username", "\\\\", "token_id"] {
        assert!(!json.contains(secret));
    }
}

#[test]
fn helpers_reject_self_and_unsafe_tokens_before_native_work() {
    let mut backend = probe();
    let self_request = ProcessRequest {
        target: helper_id(),
        ..request()
    };
    let ui_request = UiRequest {
        process: self_request.clone(),
        window: 42,
    };
    assert_eq!(
        collect_ui_structure(&mut backend, &ui_request)
            .unwrap_err()
            .operation,
        "helper_must_not_be_target"
    );
    assert_eq!(
        collect_console_membership(&mut backend, &self_request)
            .unwrap_err()
            .operation,
        "helper_must_not_be_target"
    );
    assert_eq!(backend.security_calls, 0);
    assert_eq!(backend.ui_calls, 0);
    assert_eq!(backend.console.borrow().attaches, 0);
    backend.unsafe_security = true;
    let ui_request = UiRequest {
        process: request(),
        window: 42,
    };
    assert_eq!(
        collect_ui_structure(&mut backend, &ui_request)
            .unwrap_err()
            .operation,
        "helper_security_context"
    );
    assert_eq!(backend.ui_calls, 0);
    assert_eq!(
        collect_console_membership(&mut backend, &request())
            .unwrap_err()
            .operation,
        "helper_security_context"
    );
    assert_eq!(backend.console.borrow().attaches, 0);

    let mut stale = probe();
    stale.security_delay_ticks = 12_001;
    assert_eq!(
        collect_ui_structure(&mut stale, &ui_request)
            .unwrap_err()
            .operation,
        "process_request_expired"
    );
    assert_eq!(stale.ui_calls, 0);
    let mut stale = probe();
    stale.security_delay_ticks = 12_001;
    assert_eq!(
        collect_console_membership(&mut stale, &request())
            .unwrap_err()
            .operation,
        "process_request_expired"
    );
    assert_eq!(stale.console.borrow().attaches, 0);
}

#[test]
fn ui_tree_counts_are_bounded_and_not_a_pane_claim() {
    let mut backend = probe();
    backend.nodes = vec![node(), node(), node()];
    backend.nodes[0].child = Some(1);
    backend.nodes[1].sibling = Some(2);
    backend.nodes[1].metadata.keyboard_focus = true;
    backend.nodes[2].metadata.offscreen = true;
    backend.foreground = [true, false].into();
    let report = collect_ui_structure(
        &mut backend,
        &UiRequest {
            process: request(),
            window: 42,
        },
    )
    .unwrap();
    assert_eq!(
        (
            report.visited_nodes,
            report.text_nodes,
            report.focused_nodes,
            report.offscreen_nodes
        ),
        (3, 3, 1, 1)
    );
    assert!(!report.foreground_at_both_checks);
    assert_eq!(report.relationship, "unproven");
    assert!(!report.command_ready && !report.w2_qualified);
    let mut foreign = probe();
    foreign.nodes[0].process_id = 999;
    assert_eq!(
        collect_ui_structure(
            &mut foreign,
            &UiRequest {
                process: request(),
                window: 42
            }
        )
        .unwrap_err()
        .operation,
        "uia_element_process"
    );
    assert_eq!(foreign.metadata_calls, 0);
    let mut failed = probe();
    failed.fail_tree = true;
    assert_eq!(
        collect_ui_structure(
            &mut failed,
            &UiRequest {
                process: request(),
                window: 42
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::NativeFailure
    );
}

#[test]
fn ui_fails_on_257th_node_and_depth_17() {
    let mut at_bound = probe();
    at_bound.nodes = (0..256).map(|_| node()).collect();
    at_bound.nodes[0].child = Some(1);
    for i in 1..255 {
        at_bound.nodes[i].sibling = Some(i + 1);
    }
    assert_eq!(
        collect_ui_structure(
            &mut at_bound,
            &UiRequest {
                process: request(),
                window: 42
            }
        )
        .unwrap()
        .visited_nodes,
        256
    );

    let mut at_depth = probe();
    at_depth.nodes = (0..17).map(|_| node()).collect();
    for i in 0..16 {
        at_depth.nodes[i].child = Some(i + 1);
    }
    assert_eq!(
        collect_ui_structure(
            &mut at_depth,
            &UiRequest {
                process: request(),
                window: 42
            }
        )
        .unwrap()
        .visited_nodes,
        17
    );

    let mut wide = probe();
    wide.nodes = (0..257).map(|_| node()).collect();
    wide.nodes[0].child = Some(1);
    for i in 1..256 {
        wide.nodes[i].sibling = Some(i + 1);
    }
    assert_eq!(
        collect_ui_structure(
            &mut wide,
            &UiRequest {
                process: request(),
                window: 42
            }
        )
        .unwrap_err()
        .operation,
        "uia_tree_bound"
    );
    let mut deep = probe();
    deep.nodes = (0..18).map(|_| node()).collect();
    for i in 0..17 {
        deep.nodes[i].child = Some(i + 1);
    }
    assert_eq!(
        collect_ui_structure(
            &mut deep,
            &UiRequest {
                process: request(),
                window: 42
            }
        )
        .unwrap_err()
        .operation,
        "uia_tree_bound"
    );
}

#[test]
fn console_membership_is_sorted_stable_and_detaches_owned_attachment() {
    let mut backend = probe();
    let report = collect_console_membership(&mut backend, &request()).unwrap();
    assert_eq!(report.members, [identity(), helper_id()]);
    assert_eq!(report.console_window, Some(81));
    assert_eq!(report.relationship, "unproven");
    assert_eq!(backend.console.borrow().detaches, 1);
    assert_eq!(backend.drop_count.get(), 0);
    assert!(!report.command_ready && !report.w2_qualified);

    let mut changed = probe();
    changed.console.borrow_mut().lists = [vec![123, 777], vec![123, 777, 888]].into();
    assert_eq!(
        collect_console_membership(&mut changed, &request())
            .unwrap_err()
            .operation,
        "console_membership"
    );
    assert_eq!(changed.drop_count.get(), 1);

    let mut duplicate = probe();
    duplicate.console.borrow_mut().lists = [vec![123, 123, 777]].into();
    assert_eq!(
        collect_console_membership(&mut duplicate, &request())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidData
    );
    assert_eq!(duplicate.drop_count.get(), 1);

    let mut excessive = probe();
    excessive.console.borrow_mut().lists = [(1..=129).collect()].into();
    assert_eq!(
        collect_console_membership(&mut excessive, &request())
            .unwrap_err()
            .kind,
        ErrorKind::LimitExceeded
    );
    assert_eq!(excessive.drop_count.get(), 1);

    let mut changed_window = probe();
    changed_window.console.borrow_mut().windows = [Some(81), Some(82)].into();
    assert_eq!(
        collect_console_membership(&mut changed_window, &request())
            .unwrap_err()
            .operation,
        "console_membership"
    );
    assert_eq!(changed_window.drop_count.get(), 1);

    let mut existing = probe();
    existing.existing_console = Some(1);
    assert_eq!(
        collect_console_membership(&mut existing, &request())
            .unwrap_err()
            .operation,
        "helper_already_has_console"
    );
    assert_eq!(existing.console.borrow().attaches, 0);

    let mut failed = probe();
    failed.console.borrow_mut().fail_detach = true;
    assert_eq!(
        collect_console_membership(&mut failed, &request())
            .unwrap_err()
            .kind,
        ErrorKind::NativeFailure
    );
    assert_eq!(failed.drop_count.get(), 1);
}
