// Copyright 2015 The Servo Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use ipc::OpaqueIpcSender;
use ipc::{self, IpcOneShotServer, IpcReceiver, IpcReceiverSet, IpcSender, IpcSharedMemory};
use libc;
use router::ROUTER;
use std::io::Error;
use std::iter;
use std::ptr;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;

///XXXjdm Windows' libc doesn't include fork.
#[cfg(not(windows))]
// I'm not actually sure invoking this is indeed unsafe -- but better safe than sorry...
pub unsafe fn fork<F: FnOnce()>(child_func: F) -> libc::pid_t {
    match libc::fork() {
        -1 => panic!("Fork failed: {}", Error::last_os_error()),
        0 => {
            child_func();
            unreachable!()
        }
        pid => pid,
    }
}

#[cfg(not(windows))]
pub trait Wait {
    fn wait(self);
}

#[cfg(not(windows))]
impl Wait for libc::pid_t {
    fn wait(self) {
        unsafe {
            libc::waitpid(self, ptr::null_mut(), 0);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Person {
    name: String,
    age: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct PersonAndSender {
    person: Person,
    sender: IpcSender<Person>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PersonAndOpaqueSender {
    person: Person,
    sender: OpaqueIpcSender,
}

#[derive(Serialize, Deserialize)]
struct PersonAndReceiver {
    person: Person,
    receiver: IpcReceiver<Person>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PersonAndSharedMemory {
    person: Person,
    shared_memory: IpcSharedMemory,
}

#[test]
fn simple() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (tx, rx) = ipc::channel().unwrap();
    tx.send(person.clone()).unwrap();
    let received_person = rx.recv().unwrap();
    assert_eq!(person, received_person);
}

#[test]
fn embedded_senders() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (sub_tx, sub_rx) = ipc::channel().unwrap();
    let person_and_sender = PersonAndSender {
        person: person.clone(),
        sender: sub_tx,
    };
    let (super_tx, super_rx) = ipc::channel().unwrap();
    super_tx.send(person_and_sender).unwrap();
    let received_person_and_sender = super_rx.recv().unwrap();
    assert_eq!(received_person_and_sender.person, person);
    received_person_and_sender.sender.send(person.clone()).unwrap();
    let received_person = sub_rx.recv().unwrap();
    assert_eq!(received_person, person);
}

#[test]
fn embedded_receivers() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (sub_tx, sub_rx) = ipc::channel().unwrap();
    let person_and_receiver = PersonAndReceiver {
        person: person.clone(),
        receiver: sub_rx,
    };
    let (super_tx, super_rx) = ipc::channel().unwrap();
    super_tx.send(person_and_receiver).unwrap();
    let received_person_and_receiver = super_rx.recv().unwrap();
    assert_eq!(received_person_and_receiver.person, person);
    sub_tx.send(person.clone()).unwrap();
    let received_person = received_person_and_receiver.receiver.recv().unwrap();
    assert_eq!(received_person, person);
}

#[test]
fn select() {
    let (tx0, rx0) = ipc::channel().unwrap();
    let (tx1, rx1) = ipc::channel().unwrap();
    let mut rx_set = IpcReceiverSet::new().unwrap();
    let rx0_id = rx_set.add(rx0).unwrap();
    let rx1_id = rx_set.add(rx1).unwrap();

    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    tx0.send(person.clone()).unwrap();
    let (received_id, received_data) = rx_set.select()
                                             .unwrap()
                                             .into_iter()
                                             .next()
                                             .unwrap()
                                             .unwrap();
    let received_person: Person = received_data.to().unwrap();
    assert_eq!(received_id, rx0_id);
    assert_eq!(received_person, person);

    tx1.send(person.clone()).unwrap();
    let (received_id, received_data) = rx_set.select()
                                             .unwrap()
                                             .into_iter()
                                             .next()
                                             .unwrap()
                                             .unwrap();
    let received_person: Person = received_data.to().unwrap();
    assert_eq!(received_id, rx1_id);
    assert_eq!(received_person, person);

    tx0.send(person.clone()).unwrap();
    tx1.send(person.clone()).unwrap();
    let (mut received0, mut received1) = (false, false);
    while !received0 || !received1 {
        for result in rx_set.select().unwrap().into_iter() {
            let (received_id, received_data) = result.unwrap();
            let received_person: Person = received_data.to().unwrap();
            assert_eq!(received_person, person);
            assert!(received_id == rx0_id || received_id == rx1_id);
            if received_id == rx0_id {
                assert!(!received0);
                received0 = true;
            } else if received_id == rx1_id {
                assert!(!received1);
                received1 = true;
            }
        }
    }
}

#[test]
///XXXjdm Windows' libc doesn't include fork.
#[cfg(not(windows))]
fn cross_process_embedded_senders() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (server0, server0_name) = IpcOneShotServer::new().unwrap();
    let (server2, server2_name) = IpcOneShotServer::new().unwrap();
    let child_pid = unsafe {
        fork(|| {
            let (tx1, rx1): (IpcSender<Person>, IpcReceiver<Person>) = ipc::channel().unwrap();
            let tx0 = IpcSender::connect(server0_name).unwrap();
            tx0.send(tx1).unwrap();
            rx1.recv().unwrap();
            let tx2: IpcSender<Person> = IpcSender::connect(server2_name).unwrap();
            tx2.send(person.clone()).unwrap();
            libc::exit(0);
        })
    };
    let (_, tx1): (_, IpcSender<Person>) = server0.accept().unwrap();
    tx1.send(person.clone()).unwrap();
    let (_, received_person): (_, Person) = server2.accept().unwrap();
    child_pid.wait();
    assert_eq!(received_person, person);
}

#[test]
fn router_simple() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (tx, rx) = ipc::channel().unwrap();
    tx.send(person.clone()).unwrap();

    let (callback_fired_sender, callback_fired_receiver) = mpsc::channel::<Person>();
    ROUTER.add_route(rx.to_opaque(),
                     Box::new(move |person| {
                         callback_fired_sender.send(person.to().unwrap()).unwrap()
                     }));
    let received_person = callback_fired_receiver.recv().unwrap();
    assert_eq!(received_person, person);
}

#[test]
fn router_routing_to_new_mpsc_receiver() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (tx, rx) = ipc::channel().unwrap();
    tx.send(person.clone()).unwrap();

    let mpsc_receiver = ROUTER.route_ipc_receiver_to_new_mpsc_receiver(rx);
    let received_person = mpsc_receiver.recv().unwrap();
    assert_eq!(received_person, person);
}

#[test]
fn router_multiplexing() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (tx0, rx0) = ipc::channel().unwrap();
    tx0.send(person.clone()).unwrap();
    let (tx1, rx1) = ipc::channel().unwrap();
    tx1.send(person.clone()).unwrap();

    let mpsc_rx_0 = ROUTER.route_ipc_receiver_to_new_mpsc_receiver(rx0);
    let mpsc_rx_1 = ROUTER.route_ipc_receiver_to_new_mpsc_receiver(rx1);
    let received_person_0 = mpsc_rx_0.recv().unwrap();
    let received_person_1 = mpsc_rx_1.recv().unwrap();
    assert_eq!(received_person_0, person);
    assert_eq!(received_person_1, person);
}

#[test]
fn router_multithreaded_multiplexing() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };

    let person_for_thread = person.clone();
    let (tx0, rx0) = ipc::channel().unwrap();
    thread::spawn(move || tx0.send(person_for_thread).unwrap());
    let person_for_thread = person.clone();
    let (tx1, rx1) = ipc::channel().unwrap();
    thread::spawn(move || tx1.send(person_for_thread).unwrap());

    let mpsc_rx_0 = ROUTER.route_ipc_receiver_to_new_mpsc_receiver(rx0);
    let mpsc_rx_1 = ROUTER.route_ipc_receiver_to_new_mpsc_receiver(rx1);
    let received_person_0 = mpsc_rx_0.recv().unwrap();
    let received_person_1 = mpsc_rx_1.recv().unwrap();
    assert_eq!(received_person_0, person);
    assert_eq!(received_person_1, person);
}

#[test]
fn router_drops_callbacks_on_sender_shutdown() {
    struct Dropper {
        sender: Sender<i32>,
    }

    impl Drop for Dropper {
        fn drop(&mut self) {
            self.sender.send(42).unwrap()
        }
    }

    let (tx0, rx0) = ipc::channel::<()>().unwrap();
    let (drop_tx, drop_rx) = mpsc::channel();
    let dropper = Dropper { sender: drop_tx };

    ROUTER.add_route(rx0.to_opaque(), Box::new(move |_| drop(&dropper)));
    drop(tx0);
    assert_eq!(drop_rx.recv(), Ok(42));
}

#[test]
fn router_drops_callbacks_on_cloned_sender_shutdown() {
    struct Dropper {
        sender: Sender<i32>,
    }

    impl Drop for Dropper {
        fn drop(&mut self) {
            self.sender.send(42).unwrap()
        }
    }

    let (tx0, rx0) = ipc::channel::<()>().unwrap();
    let (drop_tx, drop_rx) = mpsc::channel();
    let dropper = Dropper { sender: drop_tx };

    ROUTER.add_route(rx0.to_opaque(), Box::new(move |_| drop(&dropper)));
    let txs = vec![tx0.clone(), tx0.clone(), tx0.clone()];
    drop(txs);
    drop(tx0);
    assert_eq!(drop_rx.recv(), Ok(42));
}

#[test]
fn router_big_data() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let people: Vec<_> = iter::repeat(person).take(64 * 1024).collect();
    let (tx, rx) = ipc::channel().unwrap();
    let people_for_subthread = people.clone();
    let thread = thread::spawn(move || {
        tx.send(people_for_subthread).unwrap();
    });

    let (callback_fired_sender, callback_fired_receiver) = mpsc::channel::<Vec<Person>>();
    ROUTER.add_route(rx.to_opaque(),
                     Box::new(move |people| {
                         callback_fired_sender.send(people.to().unwrap()).unwrap()
                     }));
    let received_people = callback_fired_receiver.recv().unwrap();
    assert_eq!(received_people, people);
    thread.join().unwrap();
}

#[test]
fn shared_memory() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let person_and_shared_memory = PersonAndSharedMemory {
        person: person,
        shared_memory: IpcSharedMemory::from_byte(0xba, 1024 * 1024),
    };
    let (tx, rx) = ipc::channel().unwrap();
    tx.send(person_and_shared_memory.clone()).unwrap();
    let received_person_and_shared_memory = rx.recv().unwrap();
    assert_eq!(received_person_and_shared_memory, person_and_shared_memory);
    assert!(person_and_shared_memory.shared_memory.iter().all(|byte| *byte == 0xba));
    assert!(received_person_and_shared_memory.shared_memory.iter().all(|byte| *byte == 0xba));
}

#[test]
fn opaque_sender() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (tx, rx) = ipc::channel().unwrap();
    let opaque_tx = tx.to_opaque();
    let tx: IpcSender<Person> = opaque_tx.to();
    tx.send(person.clone()).unwrap();
    let received_person = rx.recv().unwrap();
    assert_eq!(person, received_person);
}

#[test]
fn embedded_opaque_senders() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (sub_tx, sub_rx) = ipc::channel::<Person>().unwrap();
    let person_and_sender = PersonAndOpaqueSender {
        person: person.clone(),
        sender: sub_tx.to_opaque(),
    };
    let (super_tx, super_rx) = ipc::channel().unwrap();
    super_tx.send(person_and_sender).unwrap();
    let received_person_and_sender = super_rx.recv().unwrap();
    assert_eq!(received_person_and_sender.person, person);
    received_person_and_sender.sender.to::<Person>().send(person.clone()).unwrap();
    let received_person = sub_rx.recv().unwrap();
    assert_eq!(received_person, person);
}

#[test]
fn try_recv() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (tx, rx) = ipc::channel().unwrap();
    assert!(rx.try_recv().is_err());
    tx.send(person.clone()).unwrap();
    let received_person = rx.try_recv().unwrap();
    assert_eq!(person, received_person);
    assert!(rx.try_recv().is_err());
}

#[test]
fn multiple_paths_to_a_sender() {
    let person = Person {
        name: "Patrick Walton".to_owned(),
        age: 29,
    };
    let (sub_tx, sub_rx) = ipc::channel().unwrap();
    let person_and_sender = Arc::new(PersonAndSender {
        person: person.clone(),
        sender: sub_tx,
    });
    let send_data = vec![person_and_sender.clone(),
                         person_and_sender.clone(),
                         person_and_sender.clone()];
    let (super_tx, super_rx) = ipc::channel().unwrap();
    super_tx.send(send_data).unwrap();
    let received_data = super_rx.recv().unwrap();
    assert_eq!(received_data[0].person, person);
    assert_eq!(received_data[1].person, person);
    assert_eq!(received_data[2].person, person);
    received_data[0].sender.send(person.clone()).unwrap();
    let received_person = sub_rx.recv().unwrap();
    assert_eq!(received_person, person);
    received_data[1].sender.send(person.clone()).unwrap();
    let received_person = sub_rx.recv().unwrap();
    assert_eq!(received_person, person);
}

#[test]
fn bytes() {
    let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
    let (tx, rx) = ipc::bytes_channel().unwrap();
    tx.send(&bytes[..]).unwrap();
    let received_bytes = rx.recv().unwrap();
    assert_eq!(&bytes, &received_bytes[..]);
}

#[test]
fn embedded_bytes_receivers() {
    let (sub_tx, sub_rx) = ipc::bytes_channel().unwrap();
    let (super_tx, super_rx) = ipc::channel().unwrap();
    super_tx.send(sub_tx).unwrap();
    let sub_tx = super_rx.recv().unwrap();
    let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
    sub_tx.send(&bytes[..]).unwrap();
    let received_bytes = sub_rx.recv().unwrap();
    assert_eq!(&bytes, &received_bytes[..]);
}

#[test]
fn test_so_linger() {
    let (sender, receiver) = ipc::channel().unwrap();
    sender.send(42).unwrap();
    drop(sender);
    let val = match receiver.recv() {
        Ok(val) => val,
        Err(e) => {
            panic!("err: `{}`", e);
        }
    };
    assert_eq!(val, 42);
}
