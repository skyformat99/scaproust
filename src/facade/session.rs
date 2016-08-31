// Copyright 2016 Benoît Labaere (benoit.labaere@gmail.com)
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or http://www.apache.org/licenses/LICENSE-2.0>
// or the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// This file may not be copied, modified, or distributed except according to those terms.

use std::io;
use std::thread;
use std::sync::mpsc;

use mio;

use super::*;
use reactor;
use reactor::dispatcher;
use core::session::{Request, Reply};
use core::socket::{Protocol, ProtocolCtor};
use core;
use io_error::*;

type ReplyReceiver = mpsc::Receiver<Reply>;

struct RequestSender {
    req_tx: EventLoopRequestSender
}

impl RequestSender {
    fn new(tx: EventLoopRequestSender) -> RequestSender {
        RequestSender { req_tx: tx }
    }
    fn child_sender(&self, socket_id: core::SocketId) -> socket::RequestSender {
        socket::RequestSender::new(self.req_tx.clone(), socket_id)
    }
    fn send(&self, req: Request) -> io::Result<()> {
        self.req_tx.send(reactor::Request::Session(req)).map_err(from_send_error)
    }
}

pub struct SessionBuilder;

impl SessionBuilder {

    pub fn build() -> io::Result<Session> {

        let (reply_tx, reply_rx) = mpsc::channel();
        let (request_tx, request_rx) = mio::channel::channel();
        let session = Session::new(RequestSender::new(request_tx), reply_rx);

        thread::spawn(move || {
            let mut dispatcher = dispatcher::Dispatcher::new(request_rx, reply_tx);

            dispatcher.run()
        });
        /*let mut builder = mio::deprecated::EventLoopBuilder::new();

        builder.
            notify_capacity(4_096).
            messages_per_tick(256).
            timer_tick(time::Duration::from_millis(15)).
            timer_wheel_size(1_024).
            timer_capacity(4_096);

        let event_loop = try!(builder.build());
        let request_tx = RequestSender::new(event_loop.channel());
        let session = Session::new(request_tx, reply_rx);

        thread::spawn(move || reactor::run_event_loop(event_loop, reply_tx));*/

        Ok(session)
    }}

pub struct Session {
    request_sender: RequestSender,
    reply_receiver: ReplyReceiver
}

impl Session {

    fn new(request_tx: RequestSender, reply_rx: ReplyReceiver) -> Session {
        Session {
            request_sender: request_tx,
            reply_receiver: reply_rx
        }
    }

    pub fn create_socket<T>(&mut self) -> io::Result<socket::Socket>
    where T : Protocol + From<mpsc::Sender<core::socket::Reply>> + 'static
    {
        let protocol_ctor = Session::create_protocol_ctor::<T>();
        let request = Request::CreateSocket(protocol_ctor);

        self.call(request, |reply| self.on_create_socket_reply(reply))
    }

    fn create_protocol_ctor<T>() -> ProtocolCtor 
    where T : Protocol + From<mpsc::Sender<core::socket::Reply>> + 'static
    {
        Box::new(move |sender: mpsc::Sender<core::socket::Reply>| {
            Box::new(T::from(sender)) as Box<Protocol>
        })
    }

    fn on_create_socket_reply(&self, reply: Reply) -> io::Result<socket::Socket> {
        match reply {
            Reply::SocketCreated(id, rx) => {
                let sender = self.request_sender.child_sender(id);
                let sock = socket::Socket::new(sender, rx);
                
                Ok(sock)
            },
            Reply::Err(e) => Err(e),
            _ => self.unexpected_reply()
        }
    }

    fn unexpected_reply<T>(&self) -> io::Result<T> {
        Err(other_io_error("unexpected reply"))
    }

    fn call<T, F : FnOnce(Reply) -> io::Result<T>>(&self, request: Request, process: F) -> io::Result<T> {
        self.execute_request(request).and_then(process)
    }

    fn execute_request(&self, request: Request) -> io::Result<Reply> {
        self.send_request(request).and_then(|_| self.recv_reply())
    }

    fn send_request(&self, request: Request) -> io::Result<()> {
        self.request_sender.send(request)
    }

    fn recv_reply(&self) -> io::Result<Reply> {
        self.reply_receiver.receive()
    }
}
