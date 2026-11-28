//! Control-mode block framing, reachable from `fuzz/` without becoming public.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use tokio::sync::oneshot;

use super::BlockResult;
use super::actor::ReplySlots;
use super::protocol::{Line, read_line_within};
use crate::{Error, TmuxText};

/// One queued request: how many blocks it is owed, and its caller's end.
struct Queued {
    owed: usize,
    reply: Option<oneshot::Receiver<Result<BlockResult, Error>>>,
}

/// Frame arbitrary control-mode output into replies, and check each reply.
///
/// The first byte sets the line budget and how many requests are queued; the
/// next byte per request says how many commands it chained and whether its
/// caller has gone. The rest is what tmux wrote. Lines are read the way the
/// connection actor reads them, and every closed block goes to the actor's
/// reply slots.
///
/// # Panics
///
/// When a line inside a block is anything but output or that block's own
/// terminator, or when a reply holds other than the blocks it was owed: the
/// next ones in order, stopping at the first failure.
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
            slots.push_chain(sender, owed);
            Queued { owed, reply }
        })
        .collect();

    let mut blocks = Vec::new();
    let mut pending = Vec::new();
    let mut open: Option<(u64, Vec<TmuxText>)> = None;
    loop {
        let within = open.as_ref().map(|(number, _)| *number);
        let read = ready(read_line_within(&mut stdout, &mut pending, limit, within));
        // A line is consumed whole, or refused whole when it breaks the budget.
        assert!(pending.is_empty(), "{} bytes left pending", pending.len());
        let Ok(Some(line)) = read else {
            break;
        };
        match (open.take(), line) {
            (None, Line::BlockStart(number)) => open = Some((number, Vec::new())),
            (None, _) => {}
            (Some((number, mut output)), Line::Text(text)) => {
                output.push(text);
                open = Some((number, output));
            }
            (
                Some((number, output)),
                Line::BlockEnd {
                    number: end,
                    succeeded,
                },
            ) if end == number => {
                let block = BlockResult {
                    number,
                    succeeded,
                    output,
                    sensitive_input: false,
                    chained: 0,
                };
                blocks.push(block.clone());
                slots.complete(block);
            }
            (Some((number, _)), line) => unreachable!("inside block {number}, read {line:?}"),
        }
    }

    check_replies(queued, &blocks);
}

/// Compare each reply with the blocks it was owed.
fn check_replies(queued: Vec<Queued>, blocks: &[BlockResult]) {
    let mut next = 0;
    for Queued { owed, reply } in queued {
        let start = next;
        while next < blocks.len() && next - start < owed {
            next += 1;
            if !blocks[next - 1].succeeded() {
                break;
            }
        }
        let taken = &blocks[start..next];
        let finished = taken
            .last()
            .is_some_and(|last| taken.len() == owed || !last.succeeded());

        let Some(mut reply) = reply else {
            continue;
        };
        let received = reply.try_recv();
        let (Some((last, earlier)), true) = (taken.split_last(), finished) else {
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
        assert_eq!(result.succeeded(), last.succeeded());
        if !result.succeeded() {
            let before: Vec<&TmuxText> = earlier.iter().flat_map(BlockResult::output).collect();
            let (stdout, stderr) = result.split_by_outcome();
            assert_eq!(stdout.iter().collect::<Vec<_>>(), before, "stdout");
            assert_eq!(stderr, last.output(), "stderr");
        }
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
