//! 段文件：创建、追加、滚动，压缩与后台维护。

use super::*;

pub(super) fn seg_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("{id:08}.seg"))
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

pub(super) fn create_segment(dir: &Path, id: u32) -> Result<Arc<File>> {
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(seg_path(dir, id))?;
    write_all_at(&f, SEG_MAGIC, 0)?;
    f.sync_all()?;
    sync_dir(dir)?;
    Ok(Arc::new(f))
}

impl Store {
    pub(super) fn append(&self, w: &mut Writer, raw: &[u8], tomb: bool) -> Result<(u32, u32)> {
        let len = raw.len() as u64;
        if w.len > SEG_HEADER && w.len + len > self.0.cfg.max_segment_bytes {
            self.roll(w)?;
        }
        if let Err(e) = write_all_at(&w.file, raw, w.len) {
            let _ = w.file.set_len(w.len);
            return Err(e.into());
        }
        let off = w.len as u32;
        w.len += len;
        match self.0.cfg.sync {
            SyncPolicy::Always => w.file.sync_data()?,
            SyncPolicy::Interval(d) if w.last_sync.elapsed() >= d => {
                w.file.sync_data()?;
                w.last_sync = Instant::now();
            }
            _ => {}
        }
        self.state_w().account_append(w.active, len, tomb);
        Ok((w.active, off))
    }

    /// 换一个新的活动段。旧段总是先 fsync，压缩依赖这一点。
    fn roll(&self, w: &mut Writer) -> Result<()> {
        w.file.sync_data()?;
        let id = w.active + 1;
        let file = create_segment(&self.0.cfg.dir, id)?;
        {
            let mut st = self.state_w();
            st.segs.insert(id, Seg::new(file.clone()));
            st.disk += SEG_HEADER;
        }
        w.active = id;
        w.file = file;
        w.len = SEG_HEADER;
        w.last_sync = Instant::now();
        Ok(())
    }

    fn needs_compact(&self, st: &State) -> bool {
        let c = &self.0.cfg;
        st.segs.len() > 1
            && st.disk >= c.compact_min_bytes
            && (st.disk - st.live) as f64 >= c.compact_ratio * st.disk as f64
    }

    pub(super) fn maybe_maintain(&self) {
        let inner = &self.0;
        if !inner.cfg.background {
            return;
        }
        let (over, compact, disk, garbage) = {
            let st = self.state_r();
            let over = inner.cfg.max_bytes.is_some_and(|m| st.disk > m);
            (
                over,
                self.needs_compact(&st),
                st.disk,
                st.disk.saturating_sub(st.live),
            )
        };
        if !(over || compact) {
            return;
        }
        let now = to_ms(SystemTime::now());
        let elapsed = now.saturating_sub(inner.last_maint_ms.load(Ordering::Relaxed));
        let grown = grown_since(
            (
                inner.last_maint_disk.load(Ordering::Relaxed),
                inner.last_maint_garbage.load(Ordering::Relaxed),
            ),
            (disk, garbage),
        );
        if (elapsed < MAINT_MAX_GAP_MS && !grown) || inner.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let s = self.clone();
        let spawned = std::thread::Builder::new()
            .name("pagesift-store-gc".into())
            .spawn(move || {
                let r = if over {
                    s.gc().map(drop)
                } else {
                    s.compact_with(s.0.cfg.compact_ratio, false).map(drop)
                };
                if let Err(e) = r {
                    *lock(&s.0.last_error) = Some(e.to_string());
                }
                {
                    let st = s.state_r();
                    s.0.last_maint_disk.store(st.disk, Ordering::Relaxed);
                    s.0.last_maint_garbage
                        .store(st.disk.saturating_sub(st.live), Ordering::Relaxed);
                }
                s.0.last_maint_ms
                    .store(to_ms(SystemTime::now()), Ordering::Relaxed);
                s.0.busy.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            inner.busy.store(false, Ordering::Release);
        }
    }

    pub(super) fn compact_with(&self, min_ratio: f64, roll_active: bool) -> Result<usize> {
        let _m = lock(&self.0.maint);
        let targets: Vec<u32> = {
            let mut w = self.writer();
            if roll_active {
                let garbage = self.state_r().segs[&w.active].garbage();
                if garbage > 0 {
                    self.roll(&mut w)?;
                }
            }
            let st = self.state_r();
            st.segs
                .iter()
                .filter(|(id, s)| {
                    **id != w.active
                        && s.garbage() > 0
                        && s.garbage() as f64 >= min_ratio * s.total as f64
                })
                .map(|(id, _)| *id)
                .collect()
        };
        drop(_m);
        for &id in &targets {
            self.compact_segment(id)?;
        }
        Ok(targets.len())
    }

    /// 测试用：除活动段外的段编号。
    #[cfg(test)]
    pub(crate) fn sealed_segments(&self) -> Vec<u32> {
        let active = self.writer().active;
        self.state_r()
            .segs
            .keys()
            .copied()
            .filter(|id| *id != active)
            .collect()
    }

    /// 把一个段里存活的记录复制到活动段，然后删掉这个段。
    ///
    /// 删除标记只有在所有更旧的段都没有失效 put 时才能丢掉，否则重启重放时旧值会复活；
    /// 这时如果键仍然是删除状态，就把标记一并复制过去。
    pub(crate) fn compact_segment(&self, id: u32) -> Result<()> {
        let _m = lock(&self.0.maint);
        let (file, len, drop_tombs) = {
            let st = self.state_r();
            let Some(s) = st.segs.get(&id) else {
                return Ok(());
            };
            let drop_tombs = st.segs.range(..id).all(|(_, s)| s.dead_puts() == 0);
            (s.file.clone(), s.total, drop_tombs)
        };
        record::scan(&file, len, |off, raw, rec| -> Result<()> {
            let mut w = self.writer();
            match rec.op {
                Op::Put => {
                    if self.state_r().is_at(rec.ns, rec.key, id, off) {
                        let (seg, noff) = self.append(&mut w, raw, false)?;
                        self.state_w().relocate(rec.ns, rec.key, seg, noff);
                    }
                }
                Op::Del => {
                    if !drop_tombs && self.state_r().find(rec.ns, rec.key)?.is_none() {
                        self.append(&mut w, raw, true)?;
                    }
                }
            }
            Ok(())
        })?;
        // 复制出去的记录落盘之后才能删旧段。
        self.writer().file.sync_data()?;
        {
            let mut st = self.state_w();
            let seg = st.segs.remove(&id).expect("segment");
            debug_assert_eq!(seg.live, 0);
            st.disk -= seg.total;
            st.live -= seg.live;
        }
        fs::remove_file(seg_path(&self.0.cfg.dir, id))?;
        sync_dir(&self.0.cfg.dir)?;
        Ok(())
    }
}

/// 两次后台维护之间最长等多久。[策略] 先验包 v0：0.81 的现值 1000 ms，作为上限保留：
/// 维护做完仍超限（比如剩下的都是不可淘汰的任务状态）时，每秒最多重试一次，不空转。
const MAINT_MAX_GAP_MS: u64 = 1000;

/// 维护的触发按垃圾增长：上次维护以后，
/// 无效数据或磁盘总量的增长超过上次磁盘总量的 `δ`（以它为分辨率：小于它的变化改变不了
/// "要不要压缩/淘汰"的判断）就可以马上再维护，不必等满 [`MAINT_MAX_GAP_MS`]。
/// 写得越快，越早再维护；没有增长时退回 0.81 的每秒一次。改用"增长量"而不是"增长率 × 间隔"：
/// 两者等价（增长量 = 速率 × 时间），增长量不用估计速率，也就没有估计误差。
fn grown_since(last: (u64, u64), now: (u64, u64)) -> bool {
    let resolution = pagesift_stat::Delta::DEFAULT.get() * last.0.max(1) as f64;
    now.1.saturating_sub(last.1) as f64 >= resolution
        || now.0.saturating_sub(last.0) as f64 >= resolution
}

#[cfg(test)]
mod maint_tests {
    use super::grown_since;

    /// 增长不到上次磁盘总量的 δ 不算；垃圾或总量长够了就算；首次（上次为 0）任何增长都算。
    #[test]
    fn maintenance_triggers_on_garbage_growth() {
        let last = (1_000_000, 400_000);
        assert!(!grown_since(last, (1_010_000, 440_000)));
        assert!(grown_since(last, (1_010_000, 450_000)));
        assert!(grown_since(last, (1_050_000, 400_000)));
        assert!(grown_since((0, 0), (10, 0)));
    }
}
