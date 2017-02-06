extern crate libc;

use std::fs;
use std::os::unix;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::{FromRawFd, IntoRawFd};

#[link(name = "c")]
extern "C" {
  fn kqueue() -> libc::c_int;  
  fn kevent(fd: i32, changelist: *const KEvent, changes: i32, 
    eventlist: *const KEvent, events: i32, ts: *const ()) -> i32;
  fn close(fd: libc::c_int);
}

#[repr(C)]
struct KEvent {
  ident: *const (),
  filter: i16,
  flags: u16,
  fflags: u32,
  data: *const (),
  udata: *const ()
}

impl KEvent {

  fn for_read(stream: unix::io::RawFd) -> KEvent {
    KEvent {
      ident: stream as *const _,
      filter: libc::EVFILT_READ,
      flags: libc::EV_ADD | libc::EV_ONESHOT,
      fflags: 0,
      data: std::ptr::null(),
      udata: std::ptr::null()
    }
  }
}

#[derive(Clone)]
enum OpState {
  Listen,
  Read,
  Write
}

#[derive(Clone)]
struct Op {
  fd: libc::c_int,
  state: OpState
}

struct QueueHandle {
  handle: libc::c_int
}

impl QueueHandle {
  fn new() -> Option<QueueHandle> {
    match unsafe { kqueue() } {
      fd if fd >= 0 => Some(QueueHandle { handle: fd }),
      _ => None
    }
  }
}

impl Drop for QueueHandle {
  fn drop(&mut self) {
    unsafe { close(self.handle) }
  }
}

struct AsyncQueue {
  os_queue: QueueHandle,
  events: std::vec::Vec<Op>
}

impl AsyncQueue {
  fn new() -> Option<AsyncQueue> {
    if let Some(h) = QueueHandle::new() {
      Some(AsyncQueue {
        os_queue: h,
        events: std::vec::Vec::new()
      })
    }
    else {
      None
    }
  }

  fn add_op(&mut self, op: Op) {
    
    let op = Box::new(op);
    let mut ev = KEvent::for_read(op.fd);

    unsafe {
      let ptr = Box::into_raw(op) as *mut _;
      ev.udata = ptr;

      match kevent(self.os_queue.handle, &ev as *const KEvent, 1, std::ptr::null(), 0, 
        std::ptr::null()) {
        err if err < 0 => { 
          drop(Box::from_raw(ptr));
          panic!("Couldn't add async operation - {}", unsafe { *libc::__error() }) 
        },
        _ => {}
      }
    }
  }

  fn read_async<S>(&mut self, stream: S)
    where S: unix::io::IntoRawFd
  {
    self.add_op(Op {
      fd: stream.into_raw_fd(),
      state: OpState::Read
    });
  }

  fn listen_async<S>(&mut self, stream: S) 
    where S: unix::io::IntoRawFd
  {
    self.add_op(Op {
      fd: stream.into_raw_fd(),
      state: OpState::Listen
    });
  }

  fn each_event<F: FnMut(Op, &mut AsyncQueue) -> Option<Op>>(&mut self, mut func: F) {
    loop {
      unsafe {
        let ev_buff: [KEvent; 16] = std::mem::uninitialized();

        let n = kevent(self.os_queue.handle, std::ptr::null(), 0,
          &ev_buff[0] as *const KEvent, 16, std::ptr::null());

        if n <= 0 {
          break;
        }
        
        for e in &ev_buff[..n as usize] {
          let op = Box::from_raw(e.udata as *mut () as *mut Op);
          if let Some(next) = func(*op, self) {
            self.add_op(next);
          }
        }
      }
    }

  }

  fn run<H: Handler>(mut self, mut handler: H) {
    self.each_event(|op, q| handler.handle(op, q));
  }
}

trait Handler {
  fn handle(&mut self, op: Op, queue: &mut AsyncQueue) -> Option<Op>;
}

struct MyHandler;

impl Handler for MyHandler {
  fn handle(&mut self, op: Op, _: &mut AsyncQueue) -> Option<Op> {
    let next = {
      match op.state {
        OpState::Listen => {
          let listener = unsafe { TcpListener::from_raw_fd(op.fd) };
          let (socket, addr) = listener.accept().unwrap();

          println!("Accepted connection for {:?}", addr);
          socket.set_nonblocking(true).unwrap();

          Some(Op { fd: socket.into_raw_fd(), state: OpState::Read })
        },
        OpState::Read => {
          let mut buf: [u8; 32] = [0x00; 32];
          let mut n = 0;
          let mut socket = unsafe { TcpStream::from_raw_fd(op.fd) };

          loop {
            match socket.read(&mut buf[n..]) {
              Ok(read) if read > 0 => n += read,
              Ok(_) => break,
              Err(_) => break
            }
          }

          println!("Data received: {}", std::str::from_utf8(&buf[..n]).unwrap());

          None
        },
        _ => None
      }
    };

    next
  }
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:8081").unwrap();
    listener.set_nonblocking(true).unwrap();

    let mut queue = AsyncQueue::new().unwrap();

    queue.listen_async(listener);
    queue.run(MyHandler{});
}

#[cfg(test)]
mod tests {

  use super::{AsyncQueue, MyHandler};

  #[test]
  fn test_create_queue() {
    if let Some(AsyncQueue{ os_queue: val, .. }) = AsyncQueue::new() {
      assert!(0 <= val.handle);
    }
    else {
      panic!("No queue returned from ::new");
    }
  }
}
