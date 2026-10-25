//! Byte-accounted, nonblocking UI admission. Charges include allocation
//! capacity and message overhead until the writer finishes (not just recv).
use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Condvar, Mutex};

pub(crate) const INPUT_BYTES: usize = 16 * 1024 * 1024;
const INPUT_RESERVE: usize = 64 * 1024;
const MAX_MESSAGES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    Full,
    TooLarge,
    Closed,
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Full => "Terminal input queue is full; input was not sent. Wait for the program to read, then retry.",
            Self::TooLarge => "Paste exceeds the 16 MiB input budget; nothing was sent. Paste a smaller selection.",
            Self::Closed => "Terminal input is closed; input was not sent.",
        })
    }
}

#[derive(Default)]
struct State {
    queue: VecDeque<Vec<u8>>,
    bytes: usize,
    messages: usize,
    closed: bool,
    aborted: bool,
    notice: Option<String>,
    reply_waiting: bool,
    reply_retry: bool,
}

struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    wake: Box<dyn Fn() + Send + Sync>,
    budget: usize,
    reserve: usize,
}

pub(crate) struct InputSender(Arc<Shared>);
pub(crate) struct InputReceiver(Arc<Shared>);

pub(crate) fn channel(wake: impl Fn() + Send + Sync + 'static) -> (InputSender, InputReceiver) {
    channel_with_budget(INPUT_BYTES, INPUT_RESERVE, wake)
}

fn channel_with_budget(
    budget: usize,
    reserve: usize,
    wake: impl Fn() + Send + Sync + 'static,
) -> (InputSender, InputReceiver) {
    let shared = Arc::new(Shared {
        state: Mutex::new(State::default()),
        ready: Condvar::new(),
        wake: Box::new(wake),
        budget,
        reserve,
    });
    (InputSender(shared.clone()), InputReceiver(shared))
}

fn cost(data: &Vec<u8>) -> usize {
    data.capacity().saturating_add(size_of::<Vec<u8>>())
}

impl InputSender {
    /// Closing a tab is cancellation, not a graceful drain of queued paste.
    pub(super) fn cancel(&self) {
        self.0.cancel();
    }

    #[cfg(test)]
    pub(super) fn observe(&self) -> impl Fn() -> (usize, usize) + use<> {
        let shared = self.0.clone();
        move || {
            let state = shared.state.lock().unwrap();
            (state.bytes, state.queue.len())
        }
    }

    pub fn send(&self, data: Vec<u8>, bulk: bool) -> Result<(), AdmissionError> {
        self.admit(data, bulk, false).map_err(|(error, _)| error)
    }

    /// A parser reply may pause that tab's output drain, never disappear
    /// merely because user input filled the queue. At most one parsed chunk
    /// can be waiting outside the queue; writer progress re-arms its wake.
    pub fn reply(&self, data: Vec<u8>) -> Option<Vec<u8>> {
        match self.admit(data, false, true) {
            Err((AdmissionError::Full, data)) => Some(data),
            _ => None,
        }
    }

    pub fn reply_blocked(&self) -> bool {
        let state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.reply_waiting && !state.reply_retry && !state.closed
    }

    fn admit(
        &self,
        data: Vec<u8>,
        bulk: bool,
        reply: bool,
    ) -> Result<(), (AdmissionError, Vec<u8>)> {
        if data.is_empty() {
            return Ok(());
        }
        let charge = cost(&data);
        // While a parser reply waits, keep its fixed headroom available so
        // pending keyboard input is not rejected merely because the parser
        // is paused for FIFO reply delivery.
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        let limit = self.0.budget
            + if !bulk && (reply || !state.reply_waiting) {
                self.0.reserve
            } else {
                0
            };
        let error = if state.closed {
            Some(AdmissionError::Closed)
        } else if charge > limit {
            Some(AdmissionError::TooLarge)
        } else if state.bytes.saturating_add(charge) > limit
            || state.messages >= MAX_MESSAGES + if bulk { 0 } else { 1024 }
        {
            Some(AdmissionError::Full)
        } else {
            None
        };
        if let Some(error) = error {
            if reply && error == AdmissionError::Full {
                state.reply_waiting = true;
                state.reply_retry = false;
                return Err((error, data));
            }
            let notice = error.to_string();
            let changed = state.notice.as_ref() != Some(&notice);
            state.notice = Some(notice);
            drop(state);
            if changed {
                (self.0.wake)();
            }
            return Err((error, data));
        }
        if reply {
            state.reply_waiting = false;
            state.reply_retry = false;
        }
        state.bytes += charge;
        state.messages += 1;
        state.queue.push_back(data);
        self.0.ready.notify_one();
        Ok(())
    }

    pub fn reject_large_paste(&self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .notice = Some(AdmissionError::TooLarge.to_string());
        (self.0.wake)();
    }

    pub fn take_notice(&self) -> Option<String> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .notice
            .take()
    }
}

impl Drop for InputSender {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed = true;
        self.0.ready.notify_one();
    }
}

impl Shared {
    fn cancel(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let queue = std::mem::take(&mut state.queue);
        state.bytes -= queue.iter().map(cost).sum::<usize>();
        state.messages -= queue.len();
        state.reply_waiting = false;
        state.reply_retry = false;
        state.closed = true;
        state.aborted = true;
        drop(state);
        self.ready.notify_all();
        // Free potentially large allocations outside the admission mutex.
        drop(queue);
    }
}

struct Message {
    data: Vec<u8>,
    charge: usize,
    shared: Arc<Shared>,
}
impl Drop for Message {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.data));
        let mut state = self.shared.state.lock().unwrap_or_else(|p| p.into_inner());
        state.bytes -= self.charge;
        state.messages -= 1;
        let wake = state.reply_waiting && !state.reply_retry && !state.aborted;
        if wake {
            state.reply_retry = true;
        }
        drop(state);
        if wake {
            (self.shared.wake)();
        }
    }
}

impl InputReceiver {
    fn recv(&self) -> Option<Message> {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        while state.queue.is_empty() && !state.closed {
            state = self.0.ready.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        state.queue.pop_front().map(|data| Message {
            charge: cost(&data),
            data,
            shared: self.0.clone(),
        })
    }

    pub fn run(self, mut writer: impl Write) {
        while let Some(message) = self.recv() {
            // A bulk message is one FIFO transaction: keys and replies may
            // follow it but never land inside bracketed paste boundaries.
            if let Err(error) = message
                .data
                .chunks(8192)
                .try_for_each(|chunk| {
                    if self
                        .0
                        .state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .aborted
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionAborted,
                            "PTY session closed",
                        ));
                    }
                    writer.write_all(chunk)
                })
                .and_then(|()| writer.flush())
            {
                let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
                if !state.aborted {
                    state.notice = Some(format!("Terminal write failed: {error}"));
                    drop(state);
                    (self.0.wake)();
                }
                break;
            }
        }
    }

    #[cfg(test)]
    pub fn try_recv(&self) -> Result<Vec<u8>, std::sync::mpsc::TryRecvError> {
        let data = self
            .0
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .queue
            .pop_front()
            .ok_or(std::sync::mpsc::TryRecvError::Empty)?;
        let mut message = Message {
            charge: cost(&data),
            data,
            shared: self.0.clone(),
        };
        Ok(std::mem::take(&mut message.data))
    }
}

impl Drop for InputReceiver {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn cancellation_discards_queue_but_charges_inflight_until_released() {
        let (tx, rx) = channel_with_budget(1024, 256, || {});
        tx.send(vec![b'a'; 600], true).unwrap();
        let message = rx.recv().unwrap();
        let inflight_cost = message.charge;
        tx.send(vec![b'b'; 200], true).unwrap();
        assert!(tx.reply(vec![b'r'; 600]).is_some());
        assert!(tx.reply_blocked());
        tx.cancel();
        tx.cancel();
        assert_eq!(tx.observe()(), (inflight_cost, 0));
        assert!(!tx.reply_blocked());
        assert!(rx.recv().is_none());
        assert_eq!(tx.0.state.lock().unwrap().queue.capacity(), 0);
        drop(message);
        assert_eq!(tx.observe()(), (0, 0));
        assert_eq!(tx.0.state.lock().unwrap().messages, 0);
        assert!(tx.take_notice().is_none());
        assert_eq!(tx.send(vec![1], false), Err(AdmissionError::Closed));
    }

    #[test]
    fn cancellation_stops_writer_between_chunks_without_failure_notice() {
        struct CancelWriter<'a> {
            tx: &'a InputSender,
            written: usize,
        }
        impl Write for CancelWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.written += bytes.len();
                self.tx.cancel();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                panic!("cancelled transaction must not be flushed");
            }
        }
        let (tx, rx) = channel(|| {});
        tx.send(vec![b'x'; 16384], true).unwrap();
        tx.send(b"later key".to_vec(), false).unwrap();
        let mut writer = CancelWriter {
            tx: &tx,
            written: 0,
        };
        rx.run(&mut writer);
        assert_eq!(writer.written, 8192);
        assert_eq!(tx.observe()(), (0, 0));
        assert!(tx.take_notice().is_none());
    }

    #[test]
    fn bulk_admission_is_atomic_and_charges_inflight_capacity() {
        let (tx, rx) = channel_with_budget(1024, INPUT_RESERVE, || {});
        let mut bytes = Vec::with_capacity(900);
        bytes.push(b'x');
        tx.send(bytes, true).unwrap();
        let inflight = rx.recv().unwrap();
        assert_eq!(tx.send(vec![1; 200], true), Err(AdmissionError::Full));
        tx.send(b"key".to_vec(), false).unwrap();
        assert_eq!(inflight.data, b"x");
        drop(inflight);
        tx.send(vec![2; 900], true).unwrap();
        assert_eq!(rx.try_recv().unwrap(), b"key");
        assert_eq!(rx.try_recv().unwrap(), vec![2; 900]);
        assert_eq!(tx.0.state.lock().unwrap().bytes, 0);
        drop(rx);
        assert_eq!(tx.send(vec![3], false), Err(AdmissionError::Closed));
        assert!(tx.take_notice().is_some());
    }

    #[test]
    fn reply_keeps_headroom_without_rejecting_keyboard_and_wakes_on_progress() {
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_count = wakes.clone();
        let (tx, rx) = channel_with_budget(1024, 256, move || {
            wake_count.fetch_add(1, Ordering::Relaxed);
        });
        let mut fill = Vec::with_capacity(900);
        fill.push(b'a');
        tx.send(fill, false).unwrap();
        let mut reply = Vec::with_capacity(500);
        reply.push(b'r');
        assert!(tx.reply(reply).is_some());
        assert!(tx.reply_blocked());
        // Under the budget, a keystroke still gets through; the blocked
        // reply's reserved tail was never spent on ordinary input.
        assert!(tx.send(b"k".to_vec(), false).is_ok());
        assert_eq!(rx.try_recv().unwrap(), b"a");
        assert!(!tx.reply_blocked(), "writer progress re-arms the reply");
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert!(tx.reply(vec![b'r'; 500]).is_none());
        assert!(!tx.reply_blocked());
        assert_eq!(rx.try_recv().unwrap(), b"k");
        assert_eq!(rx.try_recv().unwrap(), vec![b'r'; 500]);
    }

    #[test]
    fn writer_preserves_transactions_and_handles_partial_writes_and_failure() {
        struct ShortWriter(Vec<u8>);
        impl Write for ShortWriter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                let n = data.len().min(3);
                self.0.extend_from_slice(&data[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (tx, rx) = channel(|| {});
        let paste = b"\x1b[200~paste\x1b[201~";
        tx.send(paste.to_vec(), true).unwrap();
        tx.send(b"key".to_vec(), false).unwrap();
        let shared = tx.0.clone();
        drop(tx);
        let mut writer = ShortWriter(Vec::new());
        rx.run(&mut writer);
        assert_eq!(writer.0, [paste.as_slice(), b"key"].concat());
        assert_eq!(shared.state.lock().unwrap().bytes, 0);

        let (tx, rx) = channel(|| {});
        tx.send(vec![1], false).unwrap();
        let zero: &mut [u8] = &mut [];
        rx.run(zero);
        assert!(tx.take_notice().unwrap().contains("failed"));
        assert_eq!(tx.0.state.lock().unwrap().bytes, 0);
    }
}
