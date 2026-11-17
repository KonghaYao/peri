// ─── 最小暂存端口（测试局部 fixture） ──────────────────────────────

/// 进程内暂存端口：`create` 写入内存缓冲，`publish` 后可按 id 领取。
struct MemStore {
    threshold: usize,
    next: AtomicU64,
    self_ref: std::sync::OnceLock<std::sync::Weak<MemStore>>,
    objects: Mutex<HashMap<ObjectId, Bytes>>,
    in_flight: Mutex<HashMap<ObjectId, Arc<Mutex<Vec<u8>>>>>,
}

impl crate::spool::sealed::Sealed for MemStore {}

impl MemStore {
    fn new(threshold: usize) -> Arc<Self> {
        let store = Arc::new(Self {
            threshold,
            next: AtomicU64::new(1),
            self_ref: std::sync::OnceLock::new(),
            objects: Mutex::new(HashMap::new()),
            in_flight: Mutex::new(HashMap::new()),
        });
        let _ = store.self_ref.set(Arc::downgrade(&store));
        store
    }

    fn handle(&self) -> Arc<dyn SpoolStore> {
        let store = self
            .self_ref
            .get()
            .expect("self handle")
            .upgrade()
            .expect("store alive");
        let handle: Arc<dyn SpoolStore> = store;
        handle
    }

    fn allocate(&self) -> ObjectId {
        let counter = self.next.fetch_add(1, Ordering::Relaxed);
        let mut raw = [0u8; 16];
        raw[..8].copy_from_slice(&counter.to_be_bytes());
        ObjectId::from_bytes(raw)
    }

    fn bytes(&self, id: ObjectId) -> SpoolResult<Bytes> {
        self.objects
            .lock()
            .expect("objects")
            .get(&id)
            .cloned()
            .ok_or(SpoolError::NotFound)
    }

    fn meta(&self, id: ObjectId) -> ObjectMeta {
        let len = self
            .objects
            .lock()
            .expect("objects")
            .get(&id)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or_default();
        ObjectMeta {
            version: 1,
            id,
            kind: ObjectKind::Export,
            route: RouteFingerprint::from_bytes([9; 32]),
            event_count: 0,
            encoding: crate::config::Compression::None,
            raw_len: len,
            stored_len: len,
            sha256: [0; 32],
            accepted_at_ms: 0,
            expires_at_ms: 60_000,
            attempts: 0,
            next_attempt_at_ms: None,
            state: SpoolState::Ready,
        }
    }
}

impl SpoolStore for MemStore {
    fn reserve(&self, bytes: u64) -> SpoolResult<Box<dyn QuotaReservation>> {
        struct Reserved(u64);
        impl QuotaReservation for Reserved {
            fn bytes(&self) -> u64 {
                self.0
            }
        }
        Ok(Box::new(Reserved(bytes)))
    }

    fn create(
        &self,
        quota: Box<dyn QuotaReservation>,
        _kind: ObjectKind,
        cleanup: CleanupRegistration,
    ) -> SpoolResult<Box<dyn TemporaryObject>> {
        let id = self.allocate();
        cleanup.slot.bind(id, quota.bytes())?;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        self.in_flight
            .lock()
            .expect("in flight")
            .insert(id, Arc::clone(&buffer));
        Ok(Box::new(MemObject {
            id,
            handle: self.handle(),
            threshold: self.threshold,
            buffer,
            cleanup: Some(cleanup),
        }))
    }

    fn inspect(&self, id: ObjectId) -> SpoolResult<ObjectMeta> {
        self.bytes(id).map(|_| self.meta(id))
    }

    fn open(&self, id: ObjectId) -> SpoolResult<SpoolReader> {
        Ok(Box::new(std::io::Cursor::new(self.bytes(id)?)))
    }

    fn list(&self, _after: Option<PageCursor>, _limit: usize) -> SpoolResult<SpoolPage> {
        Err(SpoolError::Unavailable)
    }

    fn claim(&self, id: ObjectId) -> SpoolResult<Option<Box<dyn ObjectClaim>>> {
        let mut held = self.in_flight.lock().expect("in flight");
        if let Some(buffer) = held.remove(&id) {
            let bytes = Bytes::from(buffer.lock().expect("buffer").clone());
            return Ok(Some(Box::new(MemClaim {
                meta: self.meta(id),
                bytes,
            })));
        }
        Ok(Some(Box::new(MemClaim {
            meta: self.meta(id),
            bytes: self.bytes(id)?,
        })))
    }

    fn delete(&self, id: ObjectId) -> SpoolResult<()> {
        self.objects
            .lock()
            .expect("objects")
            .remove(&id)
            .map(|_| ())
            .ok_or(SpoolError::NotFound)
    }

    fn usage(&self) -> SpoolResult<SpoolUsage> {
        let objects = self.objects.lock().expect("objects");
        let stored_bytes = objects.values().map(|bytes| bytes.len() as u64).sum();
        Ok(SpoolUsage {
            reserved_bytes: stored_bytes,
            stored_bytes,
            objects: objects.len() as u64,
        })
    }
}

struct MemObject {
    id: ObjectId,
    handle: Arc<dyn SpoolStore>,
    threshold: usize,
    buffer: Arc<Mutex<Vec<u8>>>,
    cleanup: Option<CleanupRegistration>,
}

impl Write for MemObject {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len()
            > self
                .threshold
                .saturating_sub(self.buffer.lock().expect("buffer").len())
        {
            return Err(std::io::Error::other("fake spool quota exceeded"));
        }
        self.buffer.lock().expect("buffer").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl TemporaryObject for MemObject {
    fn id(&self) -> ObjectId {
        self.id
    }

    fn written_len(&self) -> u64 {
        self.buffer.lock().expect("buffer").len() as u64
    }

    fn publish(mut self: Box<Self>, meta: ObjectMeta) -> SpoolResult<SpoolObject> {
        let bytes = Bytes::from(std::mem::take(&mut *self.buffer.lock().expect("buffer")));
        let claim: Box<dyn ObjectClaim> = Box::new(MemClaim {
            meta: meta.clone(),
            bytes,
        });
        let cleanup = self.cleanup.take().ok_or(SpoolError::Unavailable)?;
        SpoolObject::from_claim(self.handle.clone(), claim, cleanup)
    }

    fn abort(self: Box<Self>) -> SpoolResult<()> {
        Ok(())
    }
}

struct MemClaim {
    meta: ObjectMeta,
    bytes: Bytes,
}

impl ObjectClaim for MemClaim {
    fn meta(&self) -> &ObjectMeta {
        &self.meta
    }

    fn open(&self) -> SpoolResult<SpoolReader> {
        Ok(Box::new(std::io::Cursor::new(self.bytes.clone())))
    }

    fn checkpoint(&mut self, _meta: ObjectMeta) -> SpoolResult<()> {
        Ok(())
    }

    fn delete(&mut self) -> SpoolResult<()> {
        Ok(())
    }
}
