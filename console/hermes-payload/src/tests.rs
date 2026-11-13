//! The session loop against a scripted console and host.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use hermes_wire::{BUILD_ID_LEN, DIGEST_LEN, MAX_FRAME, TOKEN_LEN};

use super::*;

const CFG: Config = Config {
    version: VERSION,
    host: [10, 0, 0, 2],
    port: 40_000,
    token: [7; TOKEN_LEN],
    build_id: [9; BUILD_ID_LEN],
};

enum Step {
    Bytes(Vec<u8>),
    /// One receive wait passes with no data.
    Wait,
}

#[derive(Default)]
struct Shared {
    steps: RefCell<VecDeque<Step>>,
    out: RefCell<Vec<u8>>,
    clock: Cell<u64>,
}

struct MockConn(Rc<Shared>);

impl Conn for MockConn {
    fn recv(&mut self, buf: &mut [u8], wait_ms: u32) -> Result<usize, SysError> {
        let mut steps = self.0.steps.borrow_mut();
        match steps.pop_front() {
            None => Ok(0),
            Some(Step::Wait) => {
                self.0.clock.set(self.0.clock.get() + u64::from(wait_ms));
                Err(SysError::TimedOut)
            }
            Some(Step::Bytes(mut b)) => {
                let n = b.len().min(buf.len());
                buf[..n].copy_from_slice(&b[..n]);
                if n < b.len() {
                    steps.push_front(Step::Bytes(b.split_off(n)));
                }
                Ok(n)
            }
        }
    }

    fn send_all(&mut self, data: &[u8]) -> Result<(), SysError> {
        self.0.out.borrow_mut().extend_from_slice(data);
        Ok(())
    }
}

struct Mock {
    shared: Rc<Shared>,
    connect: Result<(), SysError>,
    register: Result<u64, RegisterError>,
    polls: VecDeque<Result<Polled, SysError>>,
}

impl Mock {
    fn new(steps: Vec<Step>) -> Mock {
        let shared = Rc::new(Shared::default());
        shared.steps.borrow_mut().extend(steps);
        Mock {
            shared,
            connect: Ok(()),
            register: Ok(0xABC),
            polls: VecDeque::new(),
        }
    }

    /// Every frame the payload sent, decoded.
    fn sent(&self) -> Vec<Message<'static>> {
        let out: &'static [u8] = Box::leak(self.shared.out.borrow().clone().into_boxed_slice());
        let mut rest = out;
        let mut frames = Vec::new();
        while !rest.is_empty() {
            let h = decode_header(rest[..HEADER_LEN].try_into().unwrap()).unwrap();
            let end = HEADER_LEN + h.body_len as usize;
            frames.push(Message::decode(h.kind, &rest[HEADER_LEN..end]).unwrap());
            rest = &rest[end..];
        }
        frames
    }
}

impl Console for Mock {
    type Conn = MockConn;

    fn connect(&mut self, host: [u8; 4], port: u16) -> Result<MockConn, SysError> {
        assert_eq!((host, port), (CFG.host, CFG.port));
        self.connect.map(|()| MockConn(Rc::clone(&self.shared)))
    }

    fn now_ms(&mut self) -> Result<u64, SysError> {
        Ok(self.shared.clock.get())
    }

    fn register(&mut self, job: &Job<'_>) -> Result<u64, RegisterError> {
        assert_eq!(job.name, "Title");
        self.register
    }

    fn poll(&mut self, _task_id: u64) -> Result<Polled, SysError> {
        self.polls
            .pop_front()
            .unwrap_or(Err(SysError::NotImplemented(Spike::S4)))
    }
}

fn frame(m: &Message<'_>) -> Step {
    let mut out = vec![0u8; m.encoded_len().unwrap()];
    m.encode(&mut out).unwrap();
    Step::Bytes(out)
}

fn job(index: u32) -> Step {
    frame(&Message::Job(Job {
        index,
        manifest_url: "http://10.0.0.2:1/t/0/package.json",
        name: "Title",
        content_id: "UP0000-TEST00000_00-0000000000000000",
        package_type: "PS4GD",
        size: 1,
        digest: [0; DIGEST_LEN],
        icon: None,
    }))
}

fn hello() -> Message<'static> {
    Message::Hello(Hello {
        version: VERSION,
        token: CFG.token,
        build_id: CFG.build_id,
    })
}

#[test]
fn hello_jobs_then_bye() {
    let mut m = Mock::new(vec![job(0), Step::Wait, job(1), frame(&Message::Bye)]);
    assert_eq!(run(&mut m, &CFG), Exit::Bye);
    let reg = |index| {
        Message::Registered(Registered {
            index,
            task_id: 0xABC,
        })
    };
    assert_eq!(m.sent(), [hello(), reg(0), reg(1)]);
}

#[test]
fn frames_split_across_reads_are_reassembled() {
    let Step::Bytes(bytes) = job(0) else {
        unreachable!()
    };
    let mut steps: Vec<Step> = bytes.chunks(3).map(|c| Step::Bytes(c.to_vec())).collect();
    steps.insert(2, Step::Wait);
    steps.push(frame(&Message::Bye));
    let mut m = Mock::new(steps);
    assert_eq!(run(&mut m, &CFG), Exit::Bye);
    assert_eq!(m.sent().len(), 2);
}

#[test]
fn idle_timeout_ends_the_session() {
    let waits = (IDLE_TIMEOUT_MS / u64::from(RECV_WAIT_MS)) as usize;
    let mut steps: Vec<Step> = (0..waits - 1).map(|_| Step::Wait).collect();
    steps.push(job(0));
    // The job resets the timer; then a full idle period passes.
    steps.extend((0..waits).map(|_| Step::Wait));
    let mut m = Mock::new(steps);
    assert_eq!(run(&mut m, &CFG), Exit::Idle);
    assert_eq!(m.sent().len(), 2, "Hello and Registered, then idle");
}

#[test]
fn host_closing_ends_the_session() {
    let mut m = Mock::new(vec![job(0)]);
    assert_eq!(run(&mut m, &CFG), Exit::Closed);
}

#[test]
fn installer_refusal_is_reported_raw() {
    let mut m = Mock::new(vec![job(3), frame(&Message::Bye)]);
    m.register = Err(RegisterError::Console(0x8099_0001));
    assert_eq!(run(&mut m, &CFG), Exit::Bye);
    assert_eq!(
        m.sent()[1],
        Message::Failed(Failed {
            index: 3,
            stage: Stage::Register,
            code: 0x8099_0001
        })
    );
}

#[test]
fn stubs_end_the_session_without_inventing_codes() {
    let mut m = Mock::new(vec![job(0)]);
    m.register = Err(RegisterError::Sys(SysError::NotImplemented(Spike::S3)));
    assert_eq!(
        run(&mut m, &CFG),
        Exit::Sys(SysError::NotImplemented(Spike::S3))
    );
    assert_eq!(m.sent(), [hello()], "no Failed frame with a made-up code");

    let mut m = Mock::new(vec![]);
    m.connect = Err(SysError::NotImplemented(Spike::S2));
    assert_eq!(
        run(&mut m, &CFG),
        Exit::Sys(SysError::NotImplemented(Spike::S2))
    );
}

#[test]
fn status_changes_are_reported_until_done() {
    let polled = |state| {
        Ok(Polled {
            state,
            bytes_done: Some(5),
            error_code: 0,
        })
    };
    let mut m = Mock::new(vec![
        job(0),
        Step::Wait,
        Step::Wait,
        Step::Wait,
        Step::Wait,
        frame(&Message::Bye),
    ]);
    m.polls.extend([
        polled(TaskState::Downloading),
        polled(TaskState::Downloading),
        polled(TaskState::Done),
    ]);
    assert_eq!(run(&mut m, &CFG), Exit::Bye);
    let states: Vec<TaskState> = m
        .sent()
        .into_iter()
        .filter_map(|f| match f {
            Message::Status(s) => Some(s.state),
            _ => None,
        })
        .collect();
    assert_eq!(states, [TaskState::Downloading, TaskState::Done]);
    assert!(m.polls.is_empty(), "polling stops after done");
}

#[test]
fn unexpected_and_oversized_frames_are_malformed() {
    let mut m = Mock::new(vec![frame(&hello())]);
    assert!(matches!(run(&mut m, &CFG), Exit::Malformed(_)));

    // A Job larger than the receive buffer (S1 decides the arena size).
    let len = (RECV_BUF_LEN as u32 + 2).min(MAX_FRAME * 2);
    let mut head = len.to_le_bytes().to_vec();
    head.push(Kind::Job as u8);
    let mut m = Mock::new(vec![Step::Bytes(head)]);
    assert!(matches!(
        run(&mut m, &CFG),
        Exit::Malformed(hermes_wire::Error::FrameTooLarge { .. })
    ));
}
