use crate::learning::Score;
use std::collections::HashMap;

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct NativeFrame {
    timestamp: u64,
    swap_chain: u64,
    lost: u32,
    reserved: u32,
}
struct Samples {
    chains: HashMap<u64, Vec<u64>>,
    start: u64,
    end: u64,
    frequency: u64,
    count: usize,
}
impl Samples {
    fn new(origin: u64, frequency: u64) -> Result<Self, String> {
        if frequency == 0 {
            return Err("Relógio ETW inválido".into());
        }
        let start = frequency
            .checked_mul(15)
            .and_then(|delta| origin.checked_add(delta))
            .ok_or("Relógio ETW inválido")?;
        let end = frequency
            .checked_mul(45)
            .and_then(|delta| origin.checked_add(delta))
            .ok_or("Relógio ETW inválido")?;
        Ok(Self {
            chains: HashMap::new(),
            start,
            end,
            frequency,
            count: 0,
        })
    }
    fn record(&mut self, frame: NativeFrame) -> Result<(), String> {
        if frame.timestamp < self.start || frame.timestamp >= self.end {
            return Ok(());
        }
        if frame.lost != 0 {
            return Err("Perda de eventos de apresentação; medição descartada".into());
        }
        if self.count >= 180_000
            || (!self.chains.contains_key(&frame.swap_chain) && self.chains.len() >= 64)
        {
            return Err("Limite de amostras ETW excedido; medição descartada".into());
        }
        let times = self
            .chains
            .entry(frame.swap_chain)
            .or_insert_with(|| Vec::with_capacity(4096));
        if times
            .last()
            .is_some_and(|previous| frame.timestamp <= *previous)
        {
            return Err("Timestamps ETW fora de ordem; medição descartada".into());
        }
        times.push(frame.timestamp);
        self.count += 1;
        Ok(())
    }
    fn score(self) -> Result<Score, String> {
        let mut times = self
            .chains
            .into_values()
            .max_by_key(Vec::len)
            .ok_or("Nenhum frame do processo selecionado; mantenha o jogo renderizando")?;
        let span = times.last().unwrap() - times[0];
        if times.len() < 121 || span as f64 / (self.frequency as f64) < 25.0 {
            return Err(
                "Captura insuficiente: mantenha o jogo renderizando por 30 segundos".into(),
            );
        }
        for index in (1..times.len()).rev() {
            times[index] -= times[index - 1];
        }
        let deltas = &mut times[1..];
        let fps = self.frequency as f64 * deltas.len() as f64 / span as f64;
        deltas.sort_unstable_by(|a, b| b.cmp(a));
        let count = deltas.len().div_ceil(100);
        let slow_sum = deltas[..count]
            .iter()
            .map(|value| *value as f64)
            .sum::<f64>();
        Ok(Score {
            fps,
            low_fps: self.frequency as f64 * count as f64 / slow_sum,
        })
    }
}

#[cfg(windows)]
pub use integrated::Capture;
#[cfg(any(windows, test))]
mod integrated {
    use super::Score;
    #[cfg(windows)]
    use super::{NativeFrame, Samples};
    #[cfg(windows)]
    use std::ffi::c_void;
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc::{self, Receiver, SyncSender, TryRecvError},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };
    #[cfg(windows)]
    extern "C" {
        fn rapo_capture_start(
            pid: u32,
            frequency: *mut u64,
            origin: *mut u64,
            error: *mut u32,
            cancelled: extern "C" fn(*const c_void) -> bool,
            context: *const c_void,
        ) -> *mut c_void;
        fn rapo_capture_read(
            handle: *mut c_void,
            frames: *mut NativeFrame,
            capacity: u32,
            count: *mut u32,
        ) -> u32;
        fn rapo_capture_finish(handle: *mut c_void, lost: *mut u32) -> u32;
        fn rapo_capture_stop(handle: *mut c_void);
    }
    #[cfg(windows)]
    fn check(status: u32) -> Result<(), String> {
        if status == 0 {
            Ok(())
        } else if status == 5 || status == 1314 {
            Err("O Windows negou acesso à captura ETW integrada. Captura interrompida sem tentar contornar a proteção; confira as permissões ETW e as regras do jogo.".into())
        } else {
            Err(format!(
                "Captura ETW integrada: {} (código {status})",
                std::io::Error::from_raw_os_error(status as i32)
            ))
        }
    }
    #[cfg(windows)]
    struct Session(*mut c_void);
    #[cfg(windows)]
    impl Drop for Session {
        fn drop(&mut self) {
            unsafe {
                rapo_capture_stop(self.0);
            }
        }
    }
    #[cfg(windows)]
    impl Session {
        fn read(
            &self,
            frames: &mut [NativeFrame; 1024],
            samples: &mut Samples,
        ) -> Result<usize, String> {
            let mut count = 0;
            check(unsafe {
                rapo_capture_read(self.0, frames.as_mut_ptr(), frames.len() as u32, &mut count)
            })?;
            let batch = frames.get(..count as usize).ok_or("Lote ETW inválido")?;
            for frame in batch {
                samples.record(*frame)?;
            }
            Ok(batch.len())
        }
        fn finish(&self) -> Result<(), String> {
            let mut lost = 0;
            check(unsafe { rapo_capture_finish(self.0, &mut lost) })?;
            if lost != 0 {
                return Err("Perda de eventos/buffers ETW; medição descartada e perfil anterior preservado".into());
            }
            Ok(())
        }
    }
    #[cfg(windows)]
    extern "C" fn cancelled(context: *const c_void) -> bool {
        unsafe { &*context.cast::<AtomicBool>() }.load(Ordering::Acquire)
    }
    #[cfg(windows)]
    fn collect(
        pid: u32,
        cancel: &AtomicBool,
        events: &SyncSender<Event>,
    ) -> Result<Score, String> {
        if cancel.load(Ordering::Acquire) {
            return Err("Captura cancelada".into());
        }
        let (mut frequency, mut origin, mut error) = (0, 0, 0);
        let pointer = unsafe {
            rapo_capture_start(
                pid, &mut frequency, &mut origin, &mut error,
                cancelled, (cancel as *const AtomicBool).cast(),
            )
        };
        if pointer.is_null() {
            return Err(check(if error == 0 { 31 } else { error }).unwrap_err());
        }
        let session = Session(pointer);
        if cancel.load(Ordering::Acquire) {
            return Err("Captura cancelada".into());
        }
        let mut samples = Samples::new(origin, frequency)?;
        let started = Instant::now();
        if events.send(Event::Ready(started)).is_err() {
            return Err("Captura cancelada".into());
        }
        let mut frames = [NativeFrame::default(); 1024];
        while started.elapsed() < Duration::from_secs(45) {
            if cancel.load(Ordering::Acquire) {
                return Err("Captura cancelada".into());
            }
            session.read(&mut frames, &mut samples)?;
            thread::sleep(Duration::from_millis(20));
        }
        if cancel.load(Ordering::Acquire) {
            return Err("Captura cancelada".into());
        }
        session.finish()?;
        while session.read(&mut frames, &mut samples)? != 0 {
            if cancel.load(Ordering::Acquire) {
                return Err("Captura cancelada".into());
            }
        }
        let score = samples.score()?;
        // Check the held game instance again after scoring, not just before stopping ETW.
        session.finish()?;
        Ok(score)
    }
    enum Event {
        Ready(Instant),
        Finished(Result<Score, String>),
    }
    pub struct Capture {
        cancel: Arc<AtomicBool>,
        events: Receiver<Event>,
        started: Option<Instant>,
    }
    impl Capture {
        #[cfg(windows)]
        pub fn start(pid: u32) -> Result<Self, String> {
            Self::spawn(move |cancel, events| collect(pid, cancel, events))
        }
        fn spawn(
            collect: impl FnOnce(&AtomicBool, &SyncSender<Event>) -> Result<Score, String> + Send + 'static,
        ) -> Result<Self, String> {
            let cancel = Arc::new(AtomicBool::new(false));
            let flag = cancel.clone();
            // A worker emits at most one Ready and one Finished event.
            let (events_tx, events) = mpsc::sync_channel(2);
            thread::Builder::new()
                .name("rapo-etw".into())
                .spawn(move || {
                    let result = collect(&flag, &events_tx);
                    if !flag.load(Ordering::Acquire) {
                        let _ = events_tx.send(Event::Finished(result));
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(Self { cancel, events, started: None })
        }
        pub fn elapsed(&self) -> Option<Duration> {
            self.started.map(|started| started.elapsed())
        }
        pub fn poll(&mut self) -> Result<Option<Score>, String> {
            loop {
                match self.events.try_recv() {
                    Ok(Event::Ready(started)) => self.started = Some(started),
                    Ok(Event::Finished(result)) => return result.map(Some),
                    Err(TryRecvError::Empty) => return Ok(None),
                    Err(TryRecvError::Disconnected) => return Err(
                        "O monitor ETW integrado encerrou inesperadamente; nenhum perfil salvo".into(),
                    ),
                }
            }
        }
    }
    impl Drop for Capture {
        fn drop(&mut self) {
            // The detached worker retains the flag and owns all native cleanup.
            self.cancel.store(true, Ordering::Release);
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[cfg(windows)]
        #[test]
        fn invalid_process_fails_asynchronously_without_starting_etw() {
            // Windows rejects PID zero in OpenProcess before any logger is created.
            let mut capture = Capture::start(0).unwrap();
            assert!(capture.elapsed().is_none());
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "invalid process did not fail");
                match capture.poll() {
                    Ok(None) => thread::yield_now(),
                    Err(_) => break,
                    Ok(Some(_)) => panic!("invalid process returned a score"),
                }
            }
            assert!(capture.elapsed().is_none());
        }

        #[test]
        fn reports_preparation_failure_through_poll_without_becoming_ready() {
            let (release, wait) = mpsc::channel();
            let (done, completed) = mpsc::channel();
            let mut capture = Capture::spawn(move |_, _| {
                wait.recv().unwrap();
                done.send(()).unwrap();
                Err("start failed".into())
            }).unwrap();
            assert!(capture.elapsed().is_none());
            assert!(matches!(capture.poll(), Ok(None)));
            release.send(()).unwrap();
            completed.recv_timeout(Duration::from_secs(5)).unwrap();
            // The worker may still be publishing its terminal event.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                assert!(Instant::now() < deadline, "worker did not publish its error");
                match capture.poll() {
                    Ok(None) => thread::yield_now(),
                    Err(error) => { assert_eq!(error, "start failed"); break; }
                    Ok(Some(_)) => panic!("failed capture returned a score"),
                }
            }
            assert!(capture.elapsed().is_none());
        }

        #[test]
        fn consumes_ready_before_result_and_retains_worker_start_time() {
            let (events_tx, events) = mpsc::sync_channel(2);
            let start = Instant::now() - Duration::from_secs(2);
            events_tx.send(Event::Ready(start)).unwrap();
            events_tx.send(Event::Finished(Ok(Score { fps: 120.0, low_fps: 90.0 }))).unwrap();
            let mut capture = Capture {
                cancel: Arc::new(AtomicBool::new(false)), events, started: None,
            };
            assert!(capture.elapsed().is_none());
            let score = capture.poll().unwrap().unwrap();
            assert_eq!(score.fps, 120.0);
            assert!(capture.elapsed().unwrap() >= Duration::from_secs(2));
        }

        #[test]
        fn reports_disconnected_worker_instead_of_waiting() {
            let (sender, events) = mpsc::sync_channel(2);
            drop(sender);
            let mut capture = Capture {
                cancel: Arc::new(AtomicBool::new(false)), events, started: None,
            };
            assert!(capture.poll().is_err());
            assert!(capture.elapsed().is_none());
        }

        #[test]
        fn drop_signals_preparing_worker_without_waiting_for_cleanup() {
            let (entered, preparing) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let (done, completed) = mpsc::channel();
            let capture = Capture::spawn(move |cancel, _| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                done.send(cancel.load(Ordering::Acquire)).unwrap();
                Err("cancelled".into())
            }).unwrap();
            preparing.recv_timeout(Duration::from_secs(5)).unwrap();
            let (dropped, drop_complete) = mpsc::channel();
            let dropper = thread::spawn(move || {
                drop(capture);
                dropped.send(()).unwrap();
            });
            let returned_without_cleanup = drop_complete.recv_timeout(Duration::from_secs(5));
            release.send(()).unwrap();
            assert!(completed.recv_timeout(Duration::from_secs(5)).unwrap());
            dropper.join().unwrap();
            assert!(returned_without_cleanup.is_ok(), "drop waited for its worker");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(swap_chain: u64, timestamp: u64) -> NativeFrame {
        NativeFrame {
            timestamp,
            swap_chain,
            ..NativeFrame::default()
        }
    }
    #[test]
    fn measures_dominant_swapchain_without_hiding_stutters() {
        let mut samples = Samples::new(0, 1000).unwrap();
        let mut timestamp = 15_000;
        for i in 0..3001 {
            samples.record(frame(1, timestamp)).unwrap();
            if i % 10 == 0 {
                samples.record(frame(2, timestamp)).unwrap();
            }
            timestamp += if i == 1500 { 1000 } else { 9 };
        }
        let score = samples.score().unwrap();
        assert!((score.fps - 3_000_000.0 / 27_991.0).abs() < 0.001);
        assert!((score.low_fps - 30_000.0 / 1261.0).abs() < 0.001);
    }
    #[test]
    fn excludes_warmup_and_incomplete_capture_and_rejects_loss() {
        let mut samples = Samples::new(0, 1000).unwrap();
        samples
            .record(NativeFrame {
                lost: 1,
                ..frame(1, 14_999)
            })
            .unwrap();
        samples.record(frame(1, 45_000)).unwrap();
        assert!(samples.score().is_err());
        let mut samples = Samples::new(0, 1000).unwrap();
        assert!(samples
            .record(NativeFrame {
                lost: 1,
                ..frame(1, 20_000)
            })
            .is_err());
        let mut samples = Samples::new(0, 1000).unwrap();
        samples.record(frame(1, 20_000)).unwrap();
        assert!(samples.record(frame(1, 19_999)).is_err());
        assert!(Samples::new(0, 0).is_err());
    }
    #[test]
    fn rejects_excessive_streams_without_silently_dropping_samples() {
        let mut samples = Samples::new(0, 1000).unwrap();
        for chain in 0..64 {
            samples.record(frame(chain, 20_000)).unwrap();
        }
        samples.record(frame(0, 20_001)).unwrap();
        assert!(samples.record(frame(64, 20_002)).is_err());
        let mut samples = Samples::new(0, 1_000_000).unwrap();
        for index in 0..180_000 {
            samples.record(frame(1, 15_000_000 + index)).unwrap();
        }
        assert!(samples.record(frame(1, 15_180_000)).is_err());
    }
}
