use std::ptr;
use couchbase_sys::*;
use std::ffi::CString;
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::{park, JoinHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use futures::sync::oneshot::{channel, Sender};
use ::CouchbaseFuture;
use ::Document;
use ::CouchbaseError;
use std;

pub struct Bucket {
    instance: Arc<Mutex<SendPtr<lcb_t>>>,
    io_handle: Mutex<Option<JoinHandle<()>>>,
    io_running: Arc<AtomicBool>,
}

impl Bucket {
    pub fn new<'a>(cs: &'a str, pw: &'a str) -> Self {
        let mut instance: lcb_t = ptr::null_mut();

        let connstr = CString::new(cs).unwrap();
        let passstr = CString::new(pw).unwrap();

        let mut cropts = lcb_create_st {
            version: 3,
            v: unsafe { ::std::mem::zeroed() },
        };

        let boot_result = unsafe {
            cropts.v.v3.as_mut().connstr = connstr.as_ptr();
            cropts.v.v3.as_mut().passwd = passstr.as_ptr();

            lcb_create(&mut instance, &cropts);
            lcb_connect(instance);
            lcb_wait(instance);
            lcb_get_bootstrap_status(instance)
        };

        if boot_result != LCB_SUCCESS {
            panic!("Couldn't connect. Result {:?}", boot_result);
        }

        // install the generic callback
        unsafe { lcb_install_callback3(instance, LCB_CALLBACK_GET as i32, Some(get_callback)) };

        let mt_instance = Arc::new(Mutex::new(SendPtr { inner: Some(instance) }));
        let io_running = Arc::new(AtomicBool::new(true));

        let io_instance = mt_instance.clone();
        let still_running = io_running.clone();
        let handle = thread::Builder::new()
            .name("io".into())
            .spawn(move || {
                loop {
                    // println!("[io] Parking");
                    park();
                    // println!("[io] Unparked");
                    if !still_running.load(Ordering::Acquire) {
                        break;
                    }
                    // println!("[io] before Wait before inst");
                    let guard = io_instance.lock().unwrap();
                    let instance = guard.inner.unwrap();
                    // println!("[io] before Wait after inst");
                    unsafe { lcb_wait(instance) };
                    // println!("[io] Done Waiting");
                }
            })
            .unwrap();

        Bucket {
            instance: mt_instance,
            io_handle: Mutex::new(Some(handle)),
            io_running: io_running,
        }
    }

    fn unpark_io(&self) {
        let guard = self.io_handle.lock().unwrap();
        guard.as_ref().unwrap().thread().unpark();
    }

    pub fn get<'a>(&self, id: &'a str) -> CouchbaseFuture<Document, CouchbaseError> {
        let (tx, rx) = channel();

        let lcb_id = CString::new(id).unwrap();
        let mut cmd_get: lcb_CMDGET = unsafe { ::std::mem::zeroed() };
        cmd_get.key.type_ = LCB_KV_COPY;
        cmd_get.key.contig.bytes = lcb_id.into_raw() as *const std::os::raw::c_void;
        cmd_get.key.contig.nbytes = id.len() as usize;

        let tx_boxed = Box::new(tx);

        unsafe {
            let guard = self.instance.lock().unwrap();
            let instance = guard.inner.unwrap();
            lcb_get3(instance,
                     Box::into_raw(tx_boxed) as *const std::os::raw::c_void,
                     &cmd_get as *const lcb_CMDGET);
        }

        self.unpark_io();

        CouchbaseFuture { inner: rx }
    }
}

impl Drop for Bucket {
    fn drop(&mut self) {
        self.io_running.clone().store(false, Ordering::Release);
        self.unpark_io();

        let mut unlocked_handle = self.io_handle.lock().unwrap();
        unlocked_handle.take().unwrap().join().unwrap();
    }
}

struct SendPtr<T> {
    inner: Option<T>,
}

unsafe impl<T> Send for SendPtr<T> {}

unsafe extern "C" fn get_callback(_: lcb_t, _: i32, rb: *const lcb_RESPBASE) {
    let response = rb as *const lcb_RESPGET;
    let tx = Box::from_raw((*response).cookie as *mut Sender<Result<Document, CouchbaseError>>);
    tx.complete(Ok(Document {
        id: String::from("test"),
        cas: 0,
        content: String::from("test"),
        expiry: 0,
    }));
}