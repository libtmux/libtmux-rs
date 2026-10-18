//! Control-mode block framing, reachable from `fuzz/` without becoming public.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use tokio::sync::oneshot;

use super::BlockResult;
use super::actor::{ReplyFence, ReplySlots};
use super::protocol::{Line, read_line_within};
use crate::{Error, TmuxText};

/// One queued request: how many blocks it is owed, and its caller's end.
struct Queued {
    owed: usize,
    fence: ReplyFence,
    reply: Option<oneshot::Receiver<Result<BlockResult, Error>>>,
}

/// Frame arbitrary control-mode output into replies, and check each reply.
///
/// The first byte sets the line budget and how many requests are queued; the
/// next byte per request says how many blocks precede its fence and whether its
/// caller has gone. The rest is what tmux wrote. Lines are read the way the
/// connection actor reads them, and every closed block goes to the actor's
/// reply slots.
///
/// # Panics
///
/// When a line inside a block is anything but output or that block's own
/// terminator, or when a reply holds other than the blocks it was owed: the
/// next ones in order, including failures followed by successes.
#[doc(hidden)]
pub fn __fuzz_control_blocks(data: &[u8]) {
    let Some((&header, rest)) = data.split_first() else {
        return;
    };
    let limit = 32 * (1 + usize::from(header >> 4));
    let queued_count = usize::from(header & 0b111).min(rest.len());
    let (requests, mut stdout) = rest.split_at(queued_count);

    let mut slots = ReplySlots::default();
    let queued: Vec<Queued> = requests
        .iter()
        .map(|&request| {
            let owed = usize::from(request & 0b11) + 1;
            let (sender, receiver) = oneshot::channel();
            // A caller that stopped waiting leaves a tombstone, which must
            // still consume its blocks.
            let reply = (request & 0x80 == 0).then_some(receiver);
            let fence = slots.push(sender, None);
            Queued { owed, fence, reply }
        })
        .collect();

    let mut blocks = Vec::new();
    let mut request_index = 0;
    let mut request_blocks = 0;
    let mut pending = Vec::new();
    let mut open: Option<(u64, u64, Vec<TmuxText>)> = None;
    loop {
        let within = open.as_ref().map(|(number, _, _)| *number);
        let read = ready(read_line_within(&mut stdout, &mut pending, limit, within));
        // A line is consumed whole, or refused whole when it breaks the budget.
        assert!(pending.is_empty(), "{} bytes left pending", pending.len());
        let Ok(Some(line)) = read else {
            break;
        };
        match (open.take(), line) {
            (None, Line::BlockStart { number, flags }) => open = Some((number, flags, Vec::new())),
            (None, _) => {}
            (Some((number, flags, mut output)), Line::Text(text)) => {
                output.push(text);
                open = Some((number, flags, output));
            }
            (
                Some((number, flags, output)),
                Line::BlockEnd {
                    number: end,
                    succeeded,
                },
            ) if end == number => {
                let block = BlockResult {
                    number,
                    succeeded,
                    failed_lines: vec![!succeeded; output.len()],
                    output,
                    sensitive_input: false,
                };
                if flags & 1 == 0 {
                    continue;
                }
                blocks.push(block.clone());
                assert!(
                    slots.consume(block, usize::MAX).is_ok(),
                    "unbounded fuzz reply fits"
                );
                if let Some(request) = queued.get(request_index) {
                    request_blocks += 1;
                    if request_blocks == request.owed {
                        assert!(
                            slots.consume(request.fence.block(), usize::MAX).is_ok(),
                            "fuzz fence completes"
                        );
                        request_index += 1;
                        request_blocks = 0;
                    }
                }
            }
            (Some((number, _, _)), line) => unreachable!("inside block {number}, read {line:?}"),
        }
    }

    check_replies(queued, &blocks);
}

/// Compare each reply with the blocks it was owed.
fn check_replies(queued: Vec<Queued>, blocks: &[BlockResult]) {
    let mut next = 0;
    for Queued { owed, reply, .. } in queued {
        let start = next;
        while next < blocks.len() && next - start < owed {
            next += 1;
        }
        let taken = &blocks[start..next];
        let finished = taken.len() == owed;

        let Some(mut reply) = reply else {
            continue;
        };
        let received = reply.try_recv();
        let (Some((last, _)), true) = (taken.split_last(), finished) else {
            assert!(
                received.is_err(),
                "answered after {} of {owed} blocks",
                taken.len()
            );
            continue;
        };
        assert!(
            matches!(received, Ok(Ok(_))),
            "no reply after {owed} blocks: {received:?}"
        );
        let Ok(Ok(result)) = received else {
            continue;
        };
        let expected: Vec<&TmuxText> = taken.iter().flat_map(BlockResult::output).collect();
        assert_eq!(result.output().iter().collect::<Vec<_>>(), expected);
        assert_eq!(result.number(), last.number());
        assert_eq!(result.succeeded(), taken.iter().all(BlockResult::succeeded));
        let (stdout, stderr) = result.split_by_outcome();
        let expected_stdout: Vec<_> = taken
            .iter()
            .filter(|block| block.succeeded())
            .flat_map(BlockResult::output)
            .collect();
        let expected_stderr: Vec<_> = taken
            .iter()
            .filter(|block| !block.succeeded())
            .flat_map(BlockResult::output)
            .collect();
        assert_eq!(stdout.collect::<Vec<_>>(), expected_stdout, "stdout");
        assert_eq!(stderr.collect::<Vec<_>>(), expected_stderr, "stderr");
    }
}

/// Poll a future that cannot wait: every read here is from memory.
fn ready<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => unreachable!("an in-memory reader never waits"),
    }
}
