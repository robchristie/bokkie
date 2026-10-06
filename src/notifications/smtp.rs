//! Bounded, unauthenticated submission to an operator-selected private relay.
//! Message-ID is a trace identity. SMTP does not promise deduplication.
use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use super::{
    NotificationConfig, NotificationIntent, NotificationOutcome, NotificationSender,
    NotificationTransport,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};

const MAX_MESSAGE_BYTES: usize = 100_000;

impl NotificationSender for NotificationConfig {
    fn transport(&self) -> Option<NotificationTransport> {
        Some(NotificationTransport {
            relay_host: self.relay_host.clone(),
            relay_port: self.relay_port,
            from_address: self.from_address.clone(),
            destination: self.destination.clone(),
        })
    }
    fn send(&self, intent: &NotificationIntent) -> NotificationOutcome {
        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms.min(3000));
        let rejected = |detail: String| NotificationOutcome::Rejected {
            retryable: false,
            detail,
        };
        if let Err(error) = self.validate() {
            return rejected(error);
        }
        if intent.destination != self.destination || intent.transport != self.transport() {
            return rejected("Notification configuration differs from the immutable destination or bound relay/sender; restore the reviewed configuration before retrying".into());
        }
        let message = match encode_message(intent, &self.from_address) {
            Ok(message) => message,
            Err(error) => return rejected(error),
        };
        let stream = match connect(&self.relay_host, self.relay_port, deadline) {
            Ok(stream) => stream,
            Err(error) => {
                return NotificationOutcome::Rejected {
                    retryable: true,
                    detail: format!("SMTP connection failed before DATA: {error}"),
                };
            }
        };
        let mut session = Session {
            stream,
            deadline,
            possible_acceptance: false,
        };
        let result = session.submit(&self.from_address, &intent.destination, &message);
        match result {
            Ok(reply) if session.possible_acceptance && (200..300).contains(&reply.code) => {
                NotificationOutcome::Accepted {
                    detail: format!(
                        "Private relay accepted {}; delivery to the recipient mailbox is not proved",
                        intent.message_id
                    ),
                }
            }
            Ok(reply) if (400..500).contains(&reply.code) => NotificationOutcome::Rejected {
                retryable: true,
                detail: format!("SMTP {} rejected the message: {}", reply.code, reply.text),
            },
            Ok(reply) if (500..600).contains(&reply.code) => NotificationOutcome::Rejected {
                retryable: false,
                detail: format!(
                    "SMTP {} rejected the message; operator intervention required: {}",
                    reply.code, reply.text
                ),
            },
            Ok(reply) => failure(
                session.possible_acceptance,
                format!("Unexpected SMTP {} reply: {}", reply.code, reply.text),
            ),
            Err(error) => failure(
                session.possible_acceptance,
                format!("SMTP dialogue failed: {error}"),
            ),
        }
    }
}

fn failure(possible_acceptance: bool, detail: String) -> NotificationOutcome {
    if possible_acceptance {
        NotificationOutcome::Uncertain {
            detail: format!(
                "{detail}; acceptance after DATA is uncertain, reconcile before retrying"
            ),
        }
    } else {
        NotificationOutcome::Rejected {
            retryable: true,
            detail: format!("{detail}; no message DATA was dispatched"),
        }
    }
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    let duration = deadline.saturating_duration_since(Instant::now());
    if duration.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "total notification deadline elapsed",
        ))
    } else {
        Ok(duration)
    }
}
struct ResolveRequest {
    host: String,
    port: u16,
    deadline: Instant,
    reply: mpsc::SyncSender<Result<Vec<SocketAddr>, String>>,
}
static RESOLVER: OnceLock<Result<mpsc::SyncSender<ResolveRequest>, String>> = OnceLock::new();
fn resolve(host: &str, port: u16, deadline: Instant) -> io::Result<Vec<SocketAddr>> {
    let resolver = RESOLVER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<ResolveRequest>(1);
            thread::Builder::new()
                .name("bokkie-smtp-resolver".into())
                .spawn(move || {
                    for request in receiver {
                        // One resolver and one queued request bound resource use even
                        // when the OS resolver stalls. It never connects or sends mail.
                        let result = if request.deadline <= Instant::now() {
                            Err("relay resolution request expired".into())
                        } else {
                            (request.host.as_str(), request.port)
                                .to_socket_addrs()
                                .map(|addresses| addresses.take(16).collect::<Vec<_>>())
                                .map_err(|e| e.to_string())
                        };
                        let _ = request.reply.send(result);
                    }
                })
                .map_err(|e| e.to_string())?;
            Ok(sender)
        })
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    resolver
        .try_send(ResolveRequest {
            host: host.into(),
            port,
            deadline,
            reply: sender,
        })
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("bounded relay resolver unavailable: {e}"),
            )
        })?;
    receiver
        .recv_timeout(remaining(deadline)?)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("bounded relay resolution failed: {e}"),
            )
        })?
        .map_err(io::Error::other)
}
fn connect(host: &str, port: u16, deadline: Instant) -> io::Result<TcpStream> {
    let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        resolve(host, port, deadline)?
    };
    let mut error = io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "relay has no usable address",
    );
    for address in addresses {
        match TcpStream::connect_timeout(&address, remaining(deadline)?) {
            Ok(stream) => return Ok(stream),
            Err(failed) => error = failed,
        }
    }
    Err(error)
}

struct Reply {
    code: u16,
    text: String,
}
struct Session {
    stream: TcpStream,
    deadline: Instant,
    possible_acceptance: bool,
}
impl Session {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.stream
                .set_write_timeout(Some(remaining(self.deadline)?))?;
            let count = self.stream.write(bytes)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "relay closed while writing",
                ));
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }
    fn reply(&mut self) -> io::Result<Reply> {
        let mut first_code = None;
        for _ in 0..64 {
            let mut line = Vec::new();
            loop {
                self.stream
                    .set_read_timeout(Some(remaining(self.deadline)?))?;
                let mut byte = [0];
                self.stream.read_exact(&mut byte)?;
                line.push(byte[0]);
                if line.len() > 512 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "SMTP response exceeds line bound",
                    ));
                }
                if byte[0] == b'\n' {
                    break;
                }
            }
            let bare = line.len() == 5 && line.ends_with(b"\r\n");
            if line.len() < 5
                || !line.ends_with(b"\r\n")
                || !line[..3].iter().all(u8::is_ascii_digit)
                || (!bare
                    && (!matches!(line[3], b' ' | b'-')
                        || line[4..line.len() - 2]
                            .iter()
                            .any(|b| !(32..=126).contains(b))))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "malformed SMTP response",
                ));
            }
            let code = ((line[0] - b'0') as u16) * 100
                + ((line[1] - b'0') as u16) * 10
                + (line[2] - b'0') as u16;
            if !(200..600).contains(&code) || first_code.is_some_and(|first| first != code) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "inconsistent SMTP response code",
                ));
            }
            first_code = Some(code);
            if bare || line[3] == b' ' {
                return Ok(Reply {
                    code,
                    text: if bare {
                        String::new()
                    } else {
                        String::from_utf8_lossy(&line[4..line.len() - 2]).into_owned()
                    },
                });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SMTP response exceeds multiline bound",
        ))
    }
    fn command(&mut self, command: &str) -> io::Result<Reply> {
        self.write(command.as_bytes())?;
        self.reply()
    }
    fn submit(&mut self, from: &str, to: &str, message: &[u8]) -> io::Result<Reply> {
        let greeting = self.reply()?;
        if greeting.code != 220 {
            return Ok(greeting);
        }
        let ehlo = self.command("EHLO bokkie.local\r\n")?;
        if ehlo.code != 250 {
            return Ok(ehlo);
        }
        let mail = self.command(&format!("MAIL FROM:<{from}>\r\n"))?;
        if mail.code != 250 {
            return Ok(mail);
        }
        let recipient = self.command(&format!("RCPT TO:<{to}>\r\n"))?;
        if !matches!(recipient.code, 250 | 251) {
            return Ok(recipient);
        }
        let data = self.command("DATA\r\n")?;
        if data.code != 354 {
            return Ok(data);
        }
        // Once message bytes may have reached the relay, lost replies are never
        // classified as safe retry. Persisted dispatch precedes this boundary.
        self.possible_acceptance = true;
        self.write(message)?;
        let response = self.reply()?;
        let _ = self.write(b"QUIT\r\n");
        Ok(response)
    }
}

fn encoded_subject(subject: &str) -> String {
    let mut words = Vec::new();
    let mut part = String::new();
    for c in subject.chars() {
        if part.len() + c.len_utf8() > 42 {
            words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(part.as_bytes())));
            part.clear();
        }
        part.push(c);
    }
    if !part.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(part.as_bytes())));
    }
    words.join("\r\n ")
}
fn dot_stuff(message: &str) -> Vec<u8> {
    let mut result = Vec::new();
    for line in message.split("\r\n") {
        if line.starts_with('.') {
            result.push(b'.');
        }
        result.extend_from_slice(line.as_bytes());
        result.extend_from_slice(b"\r\n");
    }
    result.extend_from_slice(b".\r\n");
    result
}
fn encode_message(intent: &NotificationIntent, from: &str) -> Result<Vec<u8>, String> {
    super::validate_address(from)?;
    super::validate_address(&intent.destination)?;
    if intent.subject.is_empty()
        || intent.subject.chars().count() > 200
        || intent.subject.contains('\0')
        || intent.body.chars().count() > 16384
        || intent.body.contains('\0')
        || intent.message_id.len() > 256
        || !intent.message_id.starts_with('<')
        || !intent.message_id.ends_with('>')
        || intent.message_id.len() < 5
        || intent.message_id[1..intent.message_id.len() - 1]
            .bytes()
            .any(|c| !c.is_ascii_alphanumeric() && !b".-@".contains(&c))
    {
        return Err(
            "notification message exceeds finite bounds or contains an invalid header identity"
                .into(),
        );
    }
    let date = chrono::DateTime::from_timestamp(intent.created_at, 0)
        .ok_or_else(|| "notification has an invalid creation timestamp".to_owned())?
        .to_rfc2822();
    let body = intent
        .body
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let encoded = STANDARD.encode(body.as_bytes());
    let body = encoded
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let message = format!(
        "From: {from}\r\nTo: {}\r\nSubject: {}\r\nDate: {date}\r\nMessage-ID: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}",
        intent.destination,
        encoded_subject(&intent.subject),
        intent.message_id
    );
    let message = dot_stuff(&message);
    if message.len() > MAX_MESSAGE_BYTES {
        return Err("encoded notification exceeds the finite message size".into());
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
    };

    enum Peer {
        Final(u16),
        FinalReceipt(mpsc::Sender<Vec<u8>>),
        BeforeDataClose,
        AfterDataClose,
        Silent(mpsc::Receiver<()>),
        BadFinal,
        BadGreeting,
    }
    fn fixture(
        peer: Peer,
    ) -> (
        NotificationConfig,
        NotificationIntent,
        thread::JoinHandle<Vec<u8>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let join = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            if matches!(peer, Peer::BadGreeting) {
                socket.write_all(b"250 not a greeting\r\n").unwrap();
                return Vec::new();
            }
            socket.write_all(b"220 local synthetic relay\r\n").unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "EHLO bokkie.local\r\n");
            line.clear();
            socket
                .write_all(b"250-local synthetic relay\r\n250 SIZE 100000\r\n")
                .unwrap();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "MAIL FROM:<bokkie@example.org>\r\n");
            line.clear();
            socket.write_all(b"250 sender accepted\r\n").unwrap();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "RCPT TO:<reader@example.org>\r\n");
            line.clear();
            socket.write_all(b"250 recipient accepted\r\n").unwrap();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "DATA\r\n");
            line.clear();
            if matches!(peer, Peer::BeforeDataClose) {
                return Vec::new();
            }
            socket.write_all(b"354 send message\r\n").unwrap();
            let mut wire = Vec::new();
            loop {
                let count = reader.read_line(&mut line).unwrap();
                assert!(count > 0);
                wire.extend_from_slice(line.as_bytes());
                if line == ".\r\n" {
                    break;
                }
                assert!(wire.len() < MAX_MESSAGE_BYTES);
                line.clear();
            }
            match peer {
                Peer::Final(code) => socket
                    .write_all(format!("{code} synthetic final response\r\n").as_bytes())
                    .unwrap(),
                Peer::FinalReceipt(receipt) => {
                    socket
                        .write_all(b"250 synthetic final response\r\n")
                        .unwrap();
                    receipt.send(wire.clone()).unwrap();
                }
                Peer::Silent(release) => {
                    let _ = release.recv_timeout(Duration::from_secs(2));
                }
                Peer::BadFinal => socket
                    .write_all(b"250-inconsistent reply\r\n550 final mismatch\r\n")
                    .unwrap(),
                _ => {}
            }
            wire
        });
        let config = NotificationConfig {
            relay_host: "127.0.0.1".into(),
            relay_port: port,
            from_address: "bokkie@example.org".into(),
            destination: "reader@example.org".into(),
            timeout_ms: 1000,
        };
        let intent = NotificationIntent {
            id: "delivery-fixture".into(),
            task_id: "task-fixture".into(),
            source_obligation_id: "run-fixture".into(),
            destination: config.destination.clone(),
            subject: "Synthetic reminder".into(),
            body: "Synthetic notification only".into(),
            message_id: "<delivery-fixture@bokkie.local>".into(),
            created_at: 0,
            transport: config.transport(),
            push: None,
        };
        (config, intent, join)
    }
    #[test]
    fn final_success_temporary_and_permanent_replies_have_distinct_outcomes() {
        for code in [250, 451, 550] {
            let (config, intent, peer) = fixture(Peer::Final(code));
            let outcome = config.send(&intent);
            match code {
                250 => assert!(matches!(outcome, NotificationOutcome::Accepted { .. })),
                451 => assert!(matches!(
                    outcome,
                    NotificationOutcome::Rejected {
                        retryable: true,
                        ..
                    }
                )),
                550 => assert!(matches!(
                    outcome,
                    NotificationOutcome::Rejected {
                        retryable: false,
                        ..
                    }
                )),
                _ => unreachable!(),
            }
            assert!(!peer.join().unwrap().is_empty());
        }
    }

    #[test]
    fn concrete_scheduler_delivers_a_dated_reminder_without_a_browser_and_never_resends() {
        use crate::{
            ManualClock, NewObligation, ObligationState, Store, UnixClock,
            service::{Scheduler, SchedulerConfig, ServiceFakeOutcome},
        };
        use bokkie_operator_api::{
            ManagedCapabilityProfile, ManagedTaskDefinition, ManagedTrigger,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct CountingRelay {
            config: NotificationConfig,
            calls: AtomicUsize,
        }
        impl NotificationSender for CountingRelay {
            fn transport(&self) -> Option<NotificationTransport> {
                self.config.transport()
            }
            fn send(&self, intent: &NotificationIntent) -> NotificationOutcome {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.config.send(intent)
            }
        }
        fn wait_for(mut predicate: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !predicate() {
                assert!(
                    Instant::now() < deadline,
                    "service did not reconcile synthetic work"
                );
                thread::yield_now();
            }
        }
        let (receipt_sender, receipt_receiver) = mpsc::channel();
        let (config, _, peer) = fixture(Peer::FinalReceipt(receipt_sender));
        let temporary = tempfile::TempDir::new().unwrap();
        let database = temporary.path().join("closed-browser.sqlite");
        let clock = Arc::new(ManualClock::new(0));
        let mut store = Store::open(&database).unwrap();
        let mut definition = ManagedTaskDefinition::reminder(
            "Dated reminder",
            "Closed browser synthetic reminder",
            config.destination(),
        );
        definition.trigger = ManagedTrigger::Once {
            local_datetime: "1970-01-01T00:01:00".into(),
            timezone: "UTC".into(),
        };
        let task = store
            .managed_create("create", &definition, clock.now())
            .unwrap()
            .task_id;
        let profiles = [ManagedCapabilityProfile::reminder(config.destination())];
        let review = store
            .managed_preview(&task, "session", &profiles, clock.now())
            .unwrap();
        store
            .managed_activate("activate", &review, "session", &profiles, clock.now())
            .unwrap();
        let sender = Arc::new(CountingRelay {
            config,
            calls: AtomicUsize::new(0),
        });
        let scheduler = Scheduler::start_with_notification_clock(
            SchedulerConfig {
                database: database.clone(),
                poll_interval: Duration::from_millis(5),
                lease_seconds: 5,
                ordinary_concurrency: 1,
                fake_delay: Duration::ZERO,
                fake_outcome: ServiceFakeOutcome::Succeed,
            },
            None,
            false,
            Some(sender.clone()),
            clock.clone(),
        )
        .unwrap();
        assert!(
            store.managed_detail(&task).unwrap().runs[0]
                .result
                .is_none()
        );
        clock.set(60);
        let wire = receipt_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert_eq!(wire, peer.join().unwrap());
        wait_for(|| {
            store.managed_detail(&task).unwrap().runs[0]
                .delivery
                .as_ref()
                .is_some_and(|delivery| delivery.status == "accepted_by_relay")
        });
        for tick in [61, 120, 3600] {
            let id = format!("tick-observed-{tick}");
            store
                .create(
                    NewObligation {
                        id: id.clone(),
                        description: "Synthetic tick observation".into(),
                        scheduled_at: tick,
                        recurrence: None,
                        approval_required: false,
                        retry: Default::default(),
                    },
                    clock.now(),
                )
                .unwrap();
            clock.set(tick);
            wait_for(|| store.get(&id).unwrap().unwrap().state == ObligationState::Completed);
        }
        scheduler.shutdown().unwrap();
        assert_eq!(sender.calls.load(Ordering::SeqCst), 1);
        let saved = store.managed_detail(&task).unwrap();
        assert_eq!(saved.runs.len(), 1);
        assert_eq!(
            saved.runs[0].result.as_deref(),
            Some("Closed browser synthetic reminder")
        );
        assert_eq!(
            saved.runs[0].delivery.as_ref().unwrap().status,
            "accepted_by_relay"
        );
        assert_eq!(saved.runs[0].delivery.as_ref().unwrap().attempts.len(), 1);
        assert!(saved.next_wake_at.is_none());
        assert_eq!(
            wire.windows(3).filter(|bytes| *bytes == b".\r\n").count(),
            1
        );
    }
    #[test]
    fn synthetic_relay_outcomes_reconcile_the_durable_delivery_without_repeating_the_result() {
        use crate::Store;
        use bokkie_operator_api::{ManagedCapabilityProfile, ManagedTaskDefinition};
        for code in [250, 451, 550] {
            let (config, _, peer) = fixture(Peer::Final(code));
            let mut store = Store::open_in_memory().unwrap();
            let task = store
                .managed_create(
                    "create",
                    &ManagedTaskDefinition::reminder(
                        "Reminder",
                        "Saved text",
                        config.destination(),
                    ),
                    0,
                )
                .unwrap()
                .task_id;
            let profiles = [ManagedCapabilityProfile::reminder(config.destination())];
            let review = store
                .managed_preview(&task, "session", &profiles, 0)
                .unwrap();
            store
                .managed_activate("activate", &review, "session", &profiles, 0)
                .unwrap();
            assert!(crate::managed::run_one_reminder(&mut store, 0).unwrap());
            let run = store.managed_detail(&task).unwrap().runs[0].clone();
            let id = run.delivery.unwrap().id;
            let claim = store
                .claim_due_notifications(0, 5, 1)
                .unwrap()
                .pop()
                .unwrap();
            let intent = store
                .begin_notification_send_with_transport(&claim, config.transport().as_ref(), 0)
                .unwrap();
            store
                .complete_notification_send(&claim, config.send(&intent), 0)
                .unwrap();
            let detail = store.managed_detail(&task).unwrap();
            assert_eq!(detail.runs.len(), 1);
            assert_eq!(detail.runs[0].result.as_deref(), Some("Saved text"));
            let delivery = store.notification_delivery(&id).unwrap();
            assert_eq!(
                delivery.status,
                match code {
                    250 => "accepted_by_relay",
                    451 => "retry_scheduled",
                    550 => "needs_attention",
                    _ => unreachable!(),
                }
            );
            assert_eq!(delivery.attempts.len(), 1);
            assert!(delivery.recovery.is_none());
            assert!(store.claim_due_notifications(0, 5, 1).unwrap().is_empty());
            assert!(!peer.join().unwrap().is_empty());
        }
    }
    #[test]
    fn disconnect_and_malformed_reply_after_data_are_uncertain() {
        for mode in [Peer::AfterDataClose, Peer::BadFinal] {
            let (config, intent, peer) = fixture(mode);
            assert!(matches!(
                config.send(&intent),
                NotificationOutcome::Uncertain { .. }
            ));
            assert!(!peer.join().unwrap().is_empty());
        }
    }
    #[test]
    fn disconnect_before_data_and_unexpected_greeting_cannot_claim_acceptance() {
        for mode in [Peer::BeforeDataClose, Peer::BadGreeting] {
            let (config, intent, peer) = fixture(mode);
            assert!(matches!(
                config.send(&intent),
                NotificationOutcome::Rejected {
                    retryable: true,
                    ..
                }
            ));
            assert!(peer.join().unwrap().is_empty());
        }
    }
    #[test]
    fn total_deadline_after_data_returns_uncertain_without_waiting_for_the_peer() {
        let (release_sender, release_receiver) = mpsc::channel();
        let (mut config, intent, peer) = fixture(Peer::Silent(release_receiver));
        config.timeout_ms = 100;
        let started = Instant::now();
        assert!(matches!(
            config.send(&intent),
            NotificationOutcome::Uncertain { .. }
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        release_sender.send(()).unwrap();
        assert!(!peer.join().unwrap().is_empty());
    }
    #[test]
    fn encoding_preserves_text_without_header_or_data_injection() {
        let (config, mut intent, peer) = fixture(Peer::Final(250));
        intent.subject = "Résumé 🦘\r\nBcc: injected@example.org".into();
        intent.body = ".first\r\n.\r\nMAIL FROM:<injected@example.org>\nUnicode 🦘".into();
        assert!(matches!(
            config.send(&intent),
            NotificationOutcome::Accepted { .. }
        ));
        let wire = String::from_utf8(peer.join().unwrap()).unwrap();
        assert!(wire.contains("Message-ID: <delivery-fixture@bokkie.local>\r\n"));
        assert!(!wire.contains("\r\nBcc:"));
        assert!(!wire.contains("\r\nMAIL FROM:"));
        assert_eq!(wire.matches("\r\n.\r\n").count(), 1);
        assert!(wire.split("\r\n").all(|line| line.len() < 998));
        let encoded = wire
            .split_once("\r\n\r\n")
            .unwrap()
            .1
            .trim_end_matches(".\r\n")
            .replace("\r\n", "");
        assert_eq!(
            String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap(),
            ".first\r\n.\r\nMAIL FROM:<injected@example.org>\r\nUnicode 🦘"
        );
        assert_eq!(
            dot_stuff(".first\r\n..second\r\nlast"),
            b"..first\r\n...second\r\nlast\r\n.\r\n"
        );
    }
    #[test]
    fn finite_message_bounds_and_sender_config_mismatch_fail_before_network() {
        let config = NotificationConfig {
            relay_host: "127.0.0.1".into(),
            relay_port: 1,
            from_address: "bokkie@example.org".into(),
            destination: "reader@example.org".into(),
            timeout_ms: 100,
        };
        let mut intent = NotificationIntent {
            id: "bounded".into(),
            task_id: "task".into(),
            source_obligation_id: "run".into(),
            destination: config.destination.clone(),
            subject: "Subject".into(),
            body: "🦘".repeat(16384),
            message_id: "<bounded@bokkie.local>".into(),
            created_at: 0,
            transport: config.transport(),
            push: None,
        };
        assert!(encode_message(&intent, &config.from_address).unwrap().len() < MAX_MESSAGE_BYTES);
        intent.body.push('x');
        assert!(matches!(
            config.send(&intent),
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
        intent.body = "safe".into();
        intent.message_id = "<bad\r\nBcc: injected@example.org>".into();
        assert!(matches!(
            config.send(&intent),
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
        intent.message_id = "<bounded@bokkie.local>".into();
        intent.transport.as_mut().unwrap().from_address = "different@example.org".into();
        assert!(matches!(
            config.send(&intent),
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
        intent.transport = config.transport();
        intent.destination = "different@example.org".into();
        assert!(matches!(
            config.send(&intent),
            NotificationOutcome::Rejected {
                retryable: false,
                ..
            }
        ));
    }
}
