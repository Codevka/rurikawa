use async_trait::async_trait;
use dashmap::DashMap;
use futures::{pin_mut, Future, FutureExt};
use std::{
    num::NonZeroUsize, sync::atomic::AtomicBool, sync::atomic::AtomicUsize, sync::atomic::Ordering,
    sync::Arc, sync::Weak, task::Poll, task::Waker,
};

/// A handle for controlling cancellation tokens.
///
/// This struct can be cloned to control the same tokens from different places.
///
/// This implementation of `CancellationToken` is very inefficient, and should be
/// replaced with `tokio_util::sync::CancellationToken` once it is released
/// along `tokio_util` v0.4.0.
///
/// # Structure
///
/// ```plaintext
///  -> Waker 1               -> Waker 3
/// |-> Waker 2              |-> Waker 4
/// |                        |
/// * root_inner_token ----> * child_inner_token
/// |  ^---------------------|
/// parent_token - - - - - > child_token
/// ```
#[derive(Debug, Clone)]
pub struct CancellationTokenHandle {
    token_ref: Option<Arc<InnerCToken>>,
}

impl CancellationTokenHandle {
    /// Generate a new handle for cancelling stuff
    pub fn new() -> CancellationTokenHandle {
        CancellationTokenHandle {
            token_ref: Some(Arc::new(InnerCToken::new())),
        }
    }

    pub fn new_with_parent(parent: &CancellationTokenHandle) -> CancellationTokenHandle {
        if let Some(parent) = parent.token_ref.clone() {
            let token_ref = InnerCToken::new_with_parent(parent);
            CancellationTokenHandle {
                token_ref: Some(token_ref),
            }
        } else {
            Default::default()
        }
    }

    /// Send a cancel signal for all tokens currently connected to this token
    pub fn cancel(&self) {
        if let Some(r) = self.token_ref.as_ref() {
            r.wake_all();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.token_ref
            .as_ref()
            .map(|r| r.is_cancelled())
            .unwrap_or(false)
    }

    pub fn create_child(&self) -> CancellationTokenHandle {
        Self::new_with_parent(self)
    }

    /// Get a new token from this handle.
    pub fn get_token(&self) -> CancellationToken {
        CancellationToken {
            token_ref: self.token_ref.clone(),
            waker_id: None,
        }
    }

    /// Generate an empty handle that does nothing
    pub fn empty() -> CancellationTokenHandle {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.token_ref.is_none()
    }
}

impl Default for CancellationTokenHandle {
    /// Generate an empty handle that does nothing
    fn default() -> Self {
        CancellationTokenHandle { token_ref: None }
    }
}

impl Drop for CancellationTokenHandle {
    fn drop(&mut self) {
        if let Some(x) = self.token_ref.as_ref() {
            if let Some((id, parent)) = x.parent.as_ref() {
                parent.drop_child(*id);
            }
        }
    }
}

#[derive(Debug)]
struct InnerCToken {
    cancelled: AtomicBool,
    counter: AtomicUsize,
    wakers: DashMap<NonZeroUsize, Waker>,
    children: DashMap<NonZeroUsize, Weak<InnerCToken>>,
    parent: Option<(NonZeroUsize, Arc<InnerCToken>)>,
}

impl InnerCToken {
    pub fn new() -> Self {
        InnerCToken {
            cancelled: AtomicBool::new(false),
            counter: AtomicUsize::new(1),
            wakers: DashMap::new(),
            children: DashMap::new(),
            parent: None,
        }
    }

    pub fn new_with_parent(parent: Arc<InnerCToken>) -> Arc<Self> {
        let this = Arc::new(Self::new());
        this.cancelled
            .store(parent.cancelled.load(Ordering::SeqCst), Ordering::SeqCst);
        let child_id = parent.store_child(&this);
        let this_ptr = Arc::into_raw(this.clone());
        unsafe {
            // * HI, UNSAFE!
            //
            // This code is safe because the inner value only has two references
            // (`this` and `parent.children`). It's pretty much a custom
            // `OnceCell` without all those clutter.
            let this_ptr = this_ptr as *mut InnerCToken;
            (*this_ptr).parent = Some((child_id, parent));
            let _ = Arc::from_raw(this_ptr);
        }
        this
    }

    /// Store a waker reference generated by a context for waking up afterwards
    pub fn store_waker(&self, waker: Waker) -> NonZeroUsize {
        let id = NonZeroUsize::new(self.counter.fetch_add(1, Ordering::SeqCst)).unwrap();
        self.wakers.insert(id, waker);
        id
    }

    /// Drop the waker reference specified by this ID
    pub fn drop_waker(&self, id: NonZeroUsize) -> Option<Waker> {
        self.wakers.remove(&id).map(|(_id, waker)| waker)
    }

    /// Store a child reference generated by a context for waking up afterwards
    pub fn store_child(&self, child: &Arc<InnerCToken>) -> NonZeroUsize {
        let id = NonZeroUsize::new(self.counter.fetch_add(1, Ordering::SeqCst)).unwrap();
        self.children.insert(id, Arc::downgrade(child));
        id
    }

    /// Drop the child reference specified by this ID
    pub fn drop_child(&self, id: NonZeroUsize) {
        self.children.remove(&id).map(|(_id, child)| child);
    }

    /// Trigger all wakers and clean them up
    pub fn wake_all(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.wakers
            .iter()
            .for_each(|pair| pair.value().wake_by_ref());
        self.children.iter().for_each(|child| {
            if let Some(x) = child.value().upgrade() {
                x.wake_all()
            }
        });
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// A cancellation token, also a future that can be awaited.
///
/// This future resolves once the task is being cancelled.
#[derive(Debug)]
pub struct CancellationToken {
    token_ref: Option<Arc<InnerCToken>>,
    waker_id: Option<NonZeroUsize>,
}

impl CancellationToken {
    pub fn is_cancelled(&self) -> bool {
        self.token_ref
            .as_ref()
            .map(|r| r.is_cancelled())
            .unwrap_or(false)
    }

    pub fn is_token_of(&self, handle: &CancellationTokenHandle) -> bool {
        handle.token_ref.as_ref().map_or(false, |r| {
            self.token_ref.as_ref().map_or(false, |s| Arc::ptr_eq(r, s))
        })
    }
}

impl Clone for CancellationToken {
    /// Create a new cancellation token connected to the same handle instance.
    ///
    /// This method is essentially the same as `CancellationTokenHandle::get_token`,
    /// just you don't need to go to the handle to get new tokens.
    ///
    /// This method is very cheap.
    fn clone(&self) -> Self {
        CancellationToken {
            token_ref: self.token_ref.clone(),
            waker_id: None,
        }
    }
}

impl Future for CancellationToken {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        if let Some(token_ref) = self.token_ref.clone() {
            if token_ref.cancelled.load(Ordering::Acquire) {
                if let Some(id) = self.waker_id.take() {
                    token_ref.drop_waker(id);
                }
                return Poll::Ready(());
            }
            if let Some(_id) = self.waker_id.as_ref() {
                // noop
            } else {
                let id = token_ref.store_waker(cx.waker().clone());
                self.waker_id = Some(id);
            }
            Poll::Pending
        } else {
            log::info!("eternity");
            Poll::Pending
        }
    }
}

impl Drop for CancellationToken {
    fn drop(&mut self) {
        if let Some(token_ref) = self.token_ref.as_ref() {
            if let Some(id) = self.waker_id.take() {
                token_ref.drop_waker(id);
            }
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        CancellationToken {
            token_ref: None,
            waker_id: None,
        }
    }
}

#[async_trait]
pub trait CancelFutureExt {
    type Output;

    /// Execute this task with the given cancellation token, returning `None`
    /// if the task is being cancelled and `Some(output)` otherwise.
    async fn with_cancel<C>(self, mut cancel: C) -> Option<Self::Output>
    where
        C: ICancellationToken;
}

#[async_trait]
impl<T> CancelFutureExt for T
where
    T: Future + Send,
{
    type Output = T::Output;

    async fn with_cancel<C>(self, cancel: C) -> Option<T::Output>
    where
        C: ICancellationToken,
    {
        let self_ = self.fuse();
        pin_mut!(self_);

        futures::select! {
            abort = cancel.fuse() => None,
            fut = self_ => Some(fut),
            complete => None
        }
    }
}

pub trait ICancellationToken: Future<Output = ()> + Send + Unpin {}

impl ICancellationToken for CancellationToken {}

#[cfg(test)]
mod test {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cancel_token_should_not_be_triggered() {
        let handle = CancellationTokenHandle::new();
        let res = tokio_test::block_on(async move {
            let token = handle.get_token();
            let awaiter = tokio::time::delay_for(Duration::from_secs(5));
            awaiter.with_cancel(token).await
        });
        assert_eq!(res, Some(()))
    }

    #[test]
    fn cancel_token_being_triggered() {
        let handle = CancellationTokenHandle::new();
        let res = tokio_test::block_on(async move {
            let token = handle.get_token();
            let awaiter = tokio::time::delay_for(Duration::from_secs(3600));
            futures::join!(awaiter.with_cancel(token), async { handle.cancel() })
        });
        assert_eq!(res, (None, ()))
    }

    #[test]
    fn multiple_cancel_token_being_triggered() {
        let handle = CancellationTokenHandle::new();
        let res = tokio_test::block_on(async move {
            let token = handle.get_token();
            let token2 = handle.get_token();
            let awaiter = tokio::time::delay_for(Duration::from_secs(3600));
            let awaiter2 = tokio::time::delay_for(Duration::from_secs(3600));
            futures::join!(
                awaiter.with_cancel(token),
                awaiter2.with_cancel(token2),
                async { handle.cancel() }
            )
        });
        assert_eq!(res, (None, None, ()))
    }

    #[test]
    fn child_token_being_triggered() {
        let handle = CancellationTokenHandle::new();
        let res = tokio_test::block_on(async move {
            let child_handle = handle.create_child();
            let token = child_handle.get_token();
            let awaiter = tokio::time::delay_for(Duration::from_secs(3600));
            futures::join!(awaiter.with_cancel(token), async move {
                let child = child_handle;
                handle.cancel()
            })
        });
        assert_eq!(res, (None, ()))
    }
}
