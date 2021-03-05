use crate::{
    error::Error,
    ffi::{
        c4dbobs_create, c4dbobs_free, c4dbobs_getChanges, c4dbobs_releaseChanges, C4DatabaseChange,
        C4DatabaseObserver,
    },
    fl_slice::fl_slice_to_str_unchecked,
    Database, Result,
};
use log::error;
use std::{mem::MaybeUninit, os::raw::c_void, panic::catch_unwind, process::abort, ptr::NonNull};

pub(crate) struct DatabaseObserver {
    inner: NonNull<C4DatabaseObserver>,
    free_callback_f: unsafe extern "C" fn(_: *mut c_void),
    boxed_callback_f: NonNull<c_void>,
}

impl Drop for DatabaseObserver {
    fn drop(&mut self) {
        unsafe {
            c4dbobs_free(self.inner.as_ptr());
            (self.free_callback_f)(self.boxed_callback_f.as_ptr());
        }
    }
}

impl DatabaseObserver {
    pub(crate) fn new<F>(db: &Database, callback_f: F) -> Result<DatabaseObserver>
    where
        F: FnMut(*const C4DatabaseObserver) + Send + 'static,
    {
        unsafe extern "C" fn call_boxed_closure<F>(
            obs: *mut C4DatabaseObserver,
            context: *mut c_void,
        ) where
            F: FnMut(*const C4DatabaseObserver) + Send,
        {
            let r = catch_unwind(|| {
                let boxed_f = context as *mut F;
                assert!(
                    !boxed_f.is_null(),
                    "DatabaseObserver: Internal error - null function pointer"
                );
                (*boxed_f)(obs);
            });
            if r.is_err() {
                error!("DatabaseObserver::call_boxed_closure catch panic aborting");
                abort();
            }
        }
        let boxed_f: *mut F = Box::into_raw(Box::new(callback_f));
        let obs = unsafe {
            c4dbobs_create(
                db.inner.0.as_ptr(),
                Some(call_boxed_closure::<F>),
                boxed_f as *mut c_void,
            )
        };
        NonNull::new(obs)
            .map(|inner| DatabaseObserver {
                inner,
                free_callback_f: free_boxed_value::<F>,
                boxed_callback_f: unsafe { NonNull::new_unchecked(boxed_f as *mut c_void) },
            })
            .ok_or_else(|| {
                unsafe { free_boxed_value::<F>(boxed_f as *mut c_void) };
                Error::LogicError("c4dbobs_create return null".to_string())
            })
    }

    pub(crate) fn match_obs_ptr(&self, obs_ptr: usize) -> bool {
        self.inner.as_ptr() as usize == obs_ptr
    }
    pub(crate) fn changes_iter(&self) -> DbChangesIter {
        DbChangesIter { obs: self }
    }
}

unsafe extern "C" fn free_boxed_value<T>(p: *mut c_void) {
    drop(Box::from_raw(p as *mut T));
}

pub(crate) struct DbChangesIter<'obs> {
    obs: &'obs DatabaseObserver,
}

#[derive(Debug)]
pub struct DbChange {
    inner: C4DatabaseChange,
    external: bool,
}

impl DbChange {
    #[inline]
    pub fn external(&self) -> bool {
        self.external
    }
    #[inline]
    pub fn doc_id(&self) -> &str {
        unsafe { fl_slice_to_str_unchecked(self.inner.docID) }
    }
    #[inline]
    pub fn revision_id(&self) -> &str {
        unsafe { fl_slice_to_str_unchecked(self.inner.revID) }
    }
    #[inline]
    pub fn body_size(&self) -> u32 {
        self.inner.bodySize
    }
}

impl Drop for DbChange {
    fn drop(&mut self) {
        unsafe { c4dbobs_releaseChanges(&mut self.inner, 1) };
    }
}

impl<'obs> Iterator for DbChangesIter<'obs> {
    type Item = DbChange;

    fn next(&mut self) -> Option<Self::Item> {
        let mut item = MaybeUninit::<C4DatabaseChange>::uninit();
        let mut out_external = false;
        let n = unsafe {
            c4dbobs_getChanges(
                self.obs.inner.as_ptr(),
                item.as_mut_ptr(),
                1,
                &mut out_external,
            )
        };
        if n > 0 {
            let item = unsafe { item.assume_init() };
            Some(DbChange {
                inner: item,
                external: out_external,
            })
        } else {
            None
        }
    }
}
